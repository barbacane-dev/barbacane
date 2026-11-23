//! Kafka publisher for the Barbacane gateway.
//!
//! Provides a connection-caching Kafka client for the `host_kafka_publish` host function.
//! Connections are lazily established on first publish and cached by broker URL.
//! A dedicated tokio runtime keeps Kafka background tasks alive between publishes.

use crate::broker::{BrokerError, PublishResult};
use crate::egress_relay::{self, Permit};
use crate::http_client::{ip_is_internal, resolve_addrs_where, HostGuardError};
use chrono::Utc;
use parking_lot::Mutex;
use rskafka::client::partition::{Compression, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use rskafka::BackoffConfig;
use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// Default Kafka broker port, used when an address omits one.
const DEFAULT_KAFKA_PORT: u16 = 9092;

/// Upper bound on cached broker connections, so a plugin can't force unbounded
/// connection growth by publishing to many distinct broker strings.
const MAX_KAFKA_CONNECTIONS: usize = 256;

/// Timeout for an individual produce operation.
const PRODUCE_TIMEOUT: Duration = Duration::from_secs(10);

/// Kafka publisher with connection caching.
///
/// Owns a dedicated tokio runtime so that `rskafka::Client` background tasks
/// stay alive between publish calls. Connections are created lazily on first
/// publish and reused for subsequent messages to the same broker.
pub struct KafkaPublisher {
    runtime: Option<tokio::runtime::Runtime>,
    clients: Mutex<HashMap<String, Arc<Client>>>,
    /// The addresses a broker connection may reach (SSRF guard); `None` when
    /// the operator allows internal egress.
    permit: Option<Permit>,
    /// The egress relay every broker connection goes through while `permit` is
    /// set, started with the first connection.
    relay: Mutex<Option<SocketAddr>>,
}

impl KafkaPublisher {
    /// Create a new Kafka publisher with its own background runtime.
    pub fn new(allow_internal_egress: bool) -> Result<Self, BrokerError> {
        let permit: Option<Permit> = if allow_internal_egress {
            None
        } else {
            Some(Arc::new(|addr: &SocketAddr| !ip_is_internal(&addr.ip())))
        };
        Self::with_permit(permit)
    }

    fn with_permit(permit: Option<Permit>) -> Result<Self, BrokerError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("kafka-runtime")
            .enable_all()
            .build()
            .map_err(|e| BrokerError::ConnectionFailed(format!("failed to create runtime: {e}")))?;
        Ok(Self {
            runtime: Some(runtime),
            clients: Mutex::new(HashMap::new()),
            permit,
            relay: Mutex::new(None),
        })
    }

    /// Blocking publish for use from sync WASM host functions.
    ///
    /// Must be called from a thread that is NOT inside a tokio runtime context
    /// (e.g. from within `std::thread::scope`).
    pub fn publish_blocking(
        &self,
        brokers: &str,
        topic: &str,
        key: Option<String>,
        payload: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<PublishResult, BrokerError> {
        self.runtime()
            .block_on(self.publish(brokers, topic, key, payload, headers))
    }

    /// Publish a message to a Kafka topic (async).
    async fn publish(
        &self,
        brokers: &str,
        topic: &str,
        key: Option<String>,
        payload: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<PublishResult, BrokerError> {
        let client = self.get_or_connect(brokers).await?;

        let partition_client = client
            .partition_client(topic.to_string(), 0, UnknownTopicHandling::Retry)
            .await
            .map_err(|e| BrokerError::ConnectionFailed(e.to_string()))?;

        let record = Record {
            key: key.map(|k| k.into_bytes()),
            value: Some(payload.as_bytes().to_vec()),
            headers: headers
                .into_iter()
                .map(|(k, v)| (k, v.into_bytes()))
                .collect(),
            timestamp: Utc::now(),
        };

        let offsets = tokio::time::timeout(
            PRODUCE_TIMEOUT,
            partition_client.produce(vec![record], Compression::NoCompression),
        )
        .await
        .map_err(|_| BrokerError::Timeout)?
        .map_err(|e| BrokerError::PublishFailed(e.to_string()))?;

        let offset = offsets.first().copied();

        Ok(PublishResult {
            success: true,
            error: None,
            topic: topic.to_string(),
            partition: Some(0),
            offset,
        })
    }

    /// Get a cached client or establish a new one.
    async fn get_or_connect(&self, brokers: &str) -> Result<Arc<Client>, BrokerError> {
        // Check cache (lock is held briefly, no await while locked)
        {
            let clients = self.clients.lock();
            if let Some(client) = clients.get(brokers) {
                return Ok(client.clone());
            }
        }

        let mut bootstrap = Vec::new();
        for broker in brokers.split(',') {
            let (host, port) = crate::broker::split_host_port(broker.trim(), DEFAULT_KAFKA_PORT);
            let is_ipv6 = host.contains(':');
            if let Some(permit) = &self.permit {
                // The relay enforces the guard on every connection; checking the
                // bootstrap brokers here reports a blocked one at once.
                match resolve_addrs_where(&host, port, |addr| permit(addr)).await {
                    Ok(_) => {}
                    Err(HostGuardError::Blocked(h)) => return Err(BrokerError::Blocked(h)),
                    Err(HostGuardError::Resolve(m)) => {
                        return Err(BrokerError::ConnectionFailed(m));
                    }
                }
                // The client's SOCKS5 transport splits host and port at the first colon.
                if is_ipv6 {
                    return Err(BrokerError::ConnectionFailed(format!(
                        "Kafka broker {broker}: an IPv6 address cannot name a broker while the \
                         egress guard is on; use a host name"
                    )));
                }
            }
            bootstrap.push(if is_ipv6 {
                format!("[{host}]:{port}")
            } else {
                format!("{host}:{port}")
            });
        }

        // Connect (outside the lock) with a 5s deadline to avoid blocking the host function
        let backoff = BackoffConfig {
            deadline: Some(Duration::from_secs(5)),
            ..Default::default()
        };
        let mut builder = ClientBuilder::new(bootstrap).backoff_config(backoff);
        if let Some(permit) = &self.permit {
            // The client dials the brokers the cluster advertises by itself, so
            // every connection goes through the relay that applies the guard.
            builder = builder.socks5_proxy(self.relay(permit)?.to_string());
        }
        let client = builder
            .build()
            .await
            .map_err(|e| BrokerError::ConnectionFailed(e.to_string()))?;

        let client = Arc::new(client);
        tracing::info!(brokers = %brokers, "established Kafka connection");

        // Cache the new connection, bounding the cache size.
        {
            let mut clients = self.clients.lock();
            if clients.len() >= MAX_KAFKA_CONNECTIONS && !clients.contains_key(brokers) {
                return Err(BrokerError::ConnectionFailed(
                    "Kafka connection cache is full".to_string(),
                ));
            }
            clients.insert(brokers.to_string(), client.clone());
        }

        Ok(client)
    }
}

impl KafkaPublisher {
    /// The address of the egress relay, started on this publisher's runtime at
    /// the first call. Must be called on that runtime.
    fn relay(&self, permit: &Permit) -> Result<SocketAddr, BrokerError> {
        let mut relay = self.relay.lock();
        if let Some(addr) = *relay {
            return Ok(addr);
        }
        let addr = egress_relay::start(permit.clone()).map_err(|e| {
            BrokerError::ConnectionFailed(format!("cannot start the egress relay: {e}"))
        })?;
        *relay = Some(addr);
        Ok(addr)
    }

    /// The runtime, which is present for the whole life of the value and taken
    /// only by `Drop`.
    fn runtime(&self) -> &tokio::runtime::Runtime {
        self.runtime
            .as_ref()
            .expect("the runtime is taken only while dropping")
    }
}

impl Drop for KafkaPublisher {
    /// Hand the runtime to tokio's background shutdown instead of waiting for
    /// it here.
    ///
    /// Dropping a runtime blocks until its workers stop, which tokio refuses
    /// inside an async context. This type is reachable from tasks running on
    /// the gateway's runtime, so the last reference can fall anywhere, and
    /// `shutdown_background` is safe wherever that happens.
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

#[cfg(test)]
mod drop_safety_tests {
    use super::*;

    /// The last reference can fall on a runtime thread, since the gateway holds
    /// this type behind an `Arc` that background tasks clone. Dropping a
    /// runtime there is what tokio refuses, so the drop must not do it.
    #[test]
    fn dropping_inside_a_runtime_does_not_panic() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let client = KafkaPublisher::new(true).expect("kafka publisher");
            drop(client);
        });
    }

    /// And from a spawned task, which is where the gateway's eviction and
    /// hot-reload tasks would drop it.
    #[test]
    fn dropping_inside_a_spawned_task_does_not_panic() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("outer runtime");

        outer.block_on(async {
            let shared = std::sync::Arc::new(KafkaPublisher::new(true).expect("kafka publisher"));
            let held = shared.clone();
            let task = tokio::spawn(async move {
                // The task outlives the local reference, so its drop is last.
                drop(held);
            });
            drop(shared);
            task.await.expect("task");
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn publisher_starts_empty() {
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
        let clients = publisher.clients.lock();
        assert!(clients.is_empty());
    }

    #[test]
    fn blocks_internal_broker_when_egress_disallowed() {
        let publisher = KafkaPublisher::new(false).expect("kafka publisher");
        let result = publisher.publish_blocking(
            "169.254.169.254:9092",
            "test-topic",
            None,
            "hello",
            BTreeMap::new(),
        );
        assert!(matches!(result, Err(BrokerError::Blocked(_))));
    }

    #[test]
    fn publish_blocking_connection_refused() {
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
        let result = publisher.publish_blocking(
            "127.0.0.1:19092",
            "test-topic",
            None,
            "hello",
            BTreeMap::new(),
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, BrokerError::ConnectionFailed(_)));
    }

    #[test]
    fn publish_blocking_from_thread_scope() {
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
        let result = std::thread::scope(|s| {
            s.spawn(|| {
                publisher.publish_blocking(
                    "127.0.0.1:19092",
                    "test-topic",
                    None,
                    "hello",
                    BTreeMap::new(),
                )
            })
            .join()
            .unwrap()
        });
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }

    #[test]
    fn publish_blocking_with_key_and_headers() {
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
        let mut headers = BTreeMap::new();
        headers.insert("x-request-id".to_string(), "req-123".to_string());

        let result = publisher.publish_blocking(
            "127.0.0.1:19092",
            "test-topic",
            Some("order-456".to_string()),
            r#"{"orderId":"456"}"#,
            headers,
        );
        // Connection refused, but validates the key/headers path compiles and runs
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }

    /// Answers a Kafka client as a one-broker cluster whose broker, the leader
    /// of every partition, is advertised at `advertised`: ApiVersions v3
    /// (offering Metadata v0 only) and Metadata v0.
    fn serve_bootstrap(advertised: SocketAddr) -> (SocketAddr, Arc<AtomicUsize>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let connections = Arc::new(AtomicUsize::new(0));
        let seen = connections.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                seen.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || answer_as_broker(stream, advertised));
            }
        });
        (addr, connections)
    }

    fn answer_as_broker(mut stream: std::net::TcpStream, advertised: SocketAddr) {
        use std::io::{Read, Write};
        let i16_be = |v: i16| v.to_be_bytes();
        let i32_be = |v: i32| v.to_be_bytes();
        loop {
            let mut len = [0u8; 4];
            if stream.read_exact(&mut len).is_err() {
                return;
            }
            let mut request = vec![0u8; u32::from_be_bytes(len) as usize];
            if stream.read_exact(&mut request).is_err() {
                return;
            }
            let api_key = i16::from_be_bytes([request[0], request[1]]);
            let mut body = request[4..8].to_vec(); // correlation id
            match api_key {
                18 => {
                    body.extend(i16_be(0));
                    body.push(3); // compact array of two
                    for (key, max) in [(18, 3), (3, 0)] {
                        body.extend(i16_be(key));
                        body.extend(i16_be(0));
                        body.extend(i16_be(max));
                        body.push(0);
                    }
                    body.extend(i32_be(0));
                    body.push(0);
                }
                3 => {
                    let host = advertised.ip().to_string();
                    body.extend(i32_be(1));
                    body.extend(i32_be(1)); // node id
                    body.extend(i16_be(host.len() as i16));
                    body.extend(host.as_bytes());
                    body.extend(i32_be(i32::from(advertised.port())));
                    body.extend(i32_be(1));
                    body.extend(i16_be(0));
                    body.extend(i16_be("test-topic".len() as i16));
                    body.extend(b"test-topic");
                    body.extend(i32_be(1));
                    body.extend(i16_be(0));
                    body.extend(i32_be(0)); // partition 0
                    body.extend(i32_be(1)); // leader: node 1
                    body.extend(i32_be(1));
                    body.extend(i32_be(1)); // replicas
                    body.extend(i32_be(1));
                    body.extend(i32_be(1)); // in-sync replicas
                }
                _ => return,
            }
            let mut frame = (body.len() as u32).to_be_bytes().to_vec();
            frame.extend(body);
            if stream.write_all(&frame).is_err() {
                return;
            }
        }
    }

    #[test]
    fn an_advertised_broker_passes_the_egress_guard() {
        let advertised = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let advertised_addr = advertised.local_addr().expect("addr");
        let dialed = Arc::new(AtomicUsize::new(0));
        let counter = dialed.clone();
        std::thread::spawn(move || {
            for _ in advertised.incoming().flatten() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        let (bootstrap, answered) = serve_bootstrap(advertised_addr);

        // The guard admits the bootstrap broker and nothing else.
        let publisher = KafkaPublisher::with_permit(Some(Arc::new(move |addr: &SocketAddr| {
            *addr == bootstrap
        })))
        .expect("kafka publisher");
        let result = publisher.publish_blocking(
            &bootstrap.to_string(),
            "test-topic",
            None,
            "hello",
            BTreeMap::new(),
        );

        assert!(
            answered.load(Ordering::SeqCst) > 0,
            "the bootstrap broker was not reached"
        );
        assert!(result.is_err(), "published through a refused broker");
        assert_eq!(
            dialed.load(Ordering::SeqCst),
            0,
            "the client dialed an advertised broker the guard refuses"
        );
    }

    #[test]
    fn an_ipv6_broker_address_is_refused_while_the_guard_is_on() {
        let publisher =
            KafkaPublisher::with_permit(Some(Arc::new(|_: &SocketAddr| true))).expect("publisher");
        let result =
            publisher.publish_blocking("[::1]:9092", "test-topic", None, "hello", BTreeMap::new());
        assert!(
            matches!(&result, Err(BrokerError::ConnectionFailed(m)) if m.contains("use a host name")),
            "{result:?}"
        );
    }

    #[test]
    fn publish_blocking_comma_separated_brokers() {
        let publisher = KafkaPublisher::new(true).expect("kafka publisher");
        let result = publisher.publish_blocking(
            "127.0.0.1:19092, 127.0.0.1:19093",
            "test-topic",
            None,
            "hello",
            BTreeMap::new(),
        );
        // Connection refused, but validates comma-separated broker parsing
        assert!(matches!(result, Err(BrokerError::ConnectionFailed(_))));
    }
}
