//! The kafka dispatcher against a real broker: what a request publishes, and
//! what the egress guard keeps from the broker.
//!
//! CI provides the broker as a job service (`apache/kafka-native`, one node,
//! advertised as `localhost:9092`). Locally, the tests skip when nothing
//! listens at `BARBACANE_TEST_KAFKA_BROKERS` (default `localhost:9092`); start
//! one with `podman run --rm -p 9092:9092 apache/kafka-native:4.4.0`.
//!
//! Run with: `cargo test -p barbacane-test --test kafka`

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use barbacane_test::TestGateway;
use rskafka::client::partition::{PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::BackoffConfig;

const ORDER: &str = r#"{"orderId":"550e8400-e29b-41d4-a716-446655440000","total":99.99}"#;

fn fixture(name: &str) -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the workspace root")
        .join("tests/fixtures")
        .join(name)
        .display()
        .to_string()
}

fn brokers() -> String {
    std::env::var("BARBACANE_TEST_KAFKA_BROKERS").unwrap_or_else(|_| "localhost:9092".to_string())
}

/// True when something accepts TCP connections at one of the brokers.
fn reachable(brokers: &str) -> bool {
    brokers.split(',').any(|broker| {
        broker
            .trim()
            .to_socket_addrs()
            .map(|mut addrs| {
                addrs.any(|addr| TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok())
            })
            .unwrap_or(false)
    })
}

/// The broker list and a client of it, or `None` (after logging) when no
/// broker is reachable.
async fn broker() -> Option<(String, Client)> {
    let brokers = brokers();
    if !barbacane_test::require(
        reachable(&brokers),
        &format!("a Kafka broker at {brokers} (`BARBACANE_TEST_KAFKA_BROKERS`)"),
    ) {
        return None;
    }
    let bootstrap = brokers.split(',').map(|b| b.trim().to_string()).collect();
    let client = ClientBuilder::new(bootstrap)
        .backoff_config(BackoffConfig {
            deadline: Some(Duration::from_secs(30)),
            ..Default::default()
        })
        .build()
        .await
        .expect("connect to the broker");
    Some((brokers, client))
}

/// A new topic with one partition, so a test reads only its own records.
async fn new_topic(client: &Client) -> String {
    let topic = format!("barbacane-test-{}", uuid::Uuid::new_v4().simple());
    client
        .controller_client()
        .expect("controller client")
        .create_topic(&topic, 1, 1, 5_000)
        .await
        .expect("create the topic");
    topic
}

async fn partition(client: &Client, topic: &str) -> PartitionClient {
    client
        .partition_client(topic, 0, UnknownTopicHandling::Retry)
        .await
        .expect("partition client")
}

async fn gateway(brokers: &str, topic: &str, extra_env: &[(&str, &str)]) -> TestGateway {
    let mut env = vec![
        ("BARBACANE_TEST_KAFKA_BROKERS", brokers),
        ("BARBACANE_TEST_KAFKA_TOPIC", topic),
    ];
    env.extend_from_slice(extra_env);
    TestGateway::from_spec_with_env(&fixture("kafka-publish.yaml"), &env)
        .await
        .expect("failed to start gateway")
}

/// A request becomes one record on the configured topic: the body is its
/// value, the key comes from the header the spec names, the listed headers and
/// the request id become record headers, and the 202 names where it landed.
#[tokio::test]
async fn a_request_is_published_as_a_record() {
    let Some((brokers, client)) = broker().await else {
        return;
    };
    let topic = new_topic(&client).await;
    let gateway = gateway(&brokers, &topic, &[]).await;
    let request_id = uuid::Uuid::new_v4().to_string();

    let resp = gateway
        .request_builder(reqwest::Method::POST, "/events/orders")
        .header("content-type", "application/json")
        .header("x-order-key", "order-42")
        .header("x-tenant", "acme")
        .header("x-request-id", &request_id)
        .body(ORDER)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 202);
    let ack: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(ack["status"], "accepted", "{ack}");
    assert_eq!(ack["topic"], topic.as_str(), "{ack}");
    assert_eq!(ack["partition"], 0, "{ack}");
    let offset = ack["offset"].as_i64().expect("an offset");

    let (records, high_watermark) = partition(&client, &topic)
        .await
        .fetch_records(offset, 1..1_000_000, 1_000)
        .await
        .expect("fetch");
    assert_eq!(high_watermark, offset + 1, "one record on the topic");
    let published = records.first().expect("the published record");
    assert_eq!(published.offset, offset);
    let record = &published.record;
    assert_eq!(record.value.as_deref(), Some(ORDER.as_bytes()));
    assert_eq!(record.key.as_deref(), Some(b"order-42".as_slice()));
    assert_eq!(
        record.headers.get("x-tenant").map(Vec::as_slice),
        Some(b"acme".as_slice())
    );
    assert_eq!(
        record.headers.get("x-request-id").map(Vec::as_slice),
        Some(request_id.as_bytes())
    );
}

/// With the egress guard on, the plugin may not reach a broker on loopback:
/// the request fails and the topic stays empty.
#[tokio::test]
async fn the_egress_guard_keeps_the_plugin_from_a_loopback_broker() {
    let Some((brokers, client)) = broker().await else {
        return;
    };
    let topic = new_topic(&client).await;
    // Set after the harness's own value, so the guard is on.
    let gateway = gateway(
        &brokers,
        &topic,
        &[("BARBACANE_ALLOW_INTERNAL_EGRESS", "0")],
    )
    .await;

    let resp = gateway
        .post("/events/orders", ORDER)
        .await
        .expect("request");
    assert_eq!(resp.status(), 502);
    let body: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(body["type"], "urn:barbacane:error:kafka-publish-failed");

    let (records, high_watermark) = partition(&client, &topic)
        .await
        .fetch_records(0, 1..1_000_000, 500)
        .await
        .expect("fetch");
    assert!(records.is_empty(), "nothing published: {records:?}");
    assert_eq!(high_watermark, 0);
}
