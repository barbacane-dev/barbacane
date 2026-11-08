//! `http-upstream` with `endpoints` (ADR-0035): weighted selection, retries,
//! per-endpoint circuit breakers, and their metrics.
//!
//! Each test starts its own upstreams, plain HTTP servers that count the
//! requests they get, and a gateway whose spec lists them.
//!
//! Run with: `cargo test -p barbacane-test --test upstream_pool`

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use barbacane_test::TestGateway;

/// A local upstream that answers every request with `status` and its name,
/// after `delay`, and counts the requests it gets.
struct Upstream {
    url: String,
    hits: Arc<AtomicUsize>,
}

impl Upstream {
    fn start(name: &str, status: u16, delay: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let name = name.to_string();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let counter = Arc::clone(&counter);
                let name = name.clone();
                std::thread::spawn(move || {
                    if read_request(&mut stream).is_none() {
                        return;
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(delay);
                    let _ = stream.write_all(
                        format!(
                            "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{name}",
                            name.len()
                        )
                        .as_bytes(),
                    );
                });
            }
        });
        Self { url, hits }
    }

    fn ok(name: &str) -> Self {
        Self::start(name, 200, Duration::ZERO)
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// Read a request's head and its `content-length` body.
fn read_request(stream: &mut std::net::TcpStream) -> Option<()> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    while data.len() < head_end + length {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    Some(())
}

/// A URL on which nothing listens.
fn unreachable_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    format!("http://127.0.0.1:{port}")
}

/// A project routing `GET` and `POST /pool` to `config` through `http-upstream`.
fn project(dir: &Path, config: &str) -> PathBuf {
    let plugin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/http-upstream/http-upstream.wasm");
    std::fs::write(
        dir.join("barbacane.yaml"),
        format!(
            "plugins:\n  http-upstream:\n    path: {}\n",
            plugin.display()
        ),
    )
    .expect("manifest");
    let dispatch = format!(
        "      x-barbacane-dispatch:\n        name: http-upstream\n        config:\n{}\n",
        config
            .lines()
            .map(|l| format!("          {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let spec = dir.join("api.yaml");
    std::fs::write(
        &spec,
        format!(
            "openapi: \"3.1.0\"\ninfo: {{ title: Pool, version: \"1.0.0\" }}\npaths:\n  /pool:\n    get:\n      operationId: poolGet\n{dispatch}      responses:\n        \"200\": {{ description: ok }}\n    post:\n      operationId: poolPost\n{dispatch}      responses:\n        \"200\": {{ description: ok }}\n"
        ),
    )
    .expect("spec");
    spec
}

fn endpoints(list: &[(&str, u32)]) -> String {
    list.iter()
        .map(|(url, weight)| format!("  - {{ url: \"{url}\", weight: {weight} }}"))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn gateway(config: &str) -> (tempfile::TempDir, TestGateway) {
    let dir = tempfile::TempDir::new().unwrap();
    let spec = project(dir.path(), config);
    let gateway = TestGateway::from_spec_with_env(
        spec.to_str().unwrap(),
        &[("BARBACANE_ALLOW_INTERNAL_EGRESS", "1")],
    )
    .await
    .expect("gateway starts");
    (dir, gateway)
}

#[tokio::test]
async fn requests_are_spread_by_weight() {
    let a = Upstream::ok("a");
    let b = Upstream::ok("b");
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}",
        endpoints(&[(&a.url, 1), (&b.url, 2)])
    ))
    .await;
    for _ in 0..30 {
        assert_eq!(gw.get("/pool").await.unwrap().status(), 200);
    }
    assert_eq!((a.hits(), b.hits()), (10, 20));
}

#[tokio::test]
async fn an_unreachable_endpoint_is_routed_around_whatever_the_method() {
    let ok = Upstream::ok("ok");
    let dead = unreachable_url();
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}",
        endpoints(&[(&dead, 1), (&ok.url, 1)])
    ))
    .await;
    // The first request goes to the unreachable endpoint, the second to `ok`.
    let response = gw.post("/pool", "{}").await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "ok");
    assert_eq!(gw.get("/pool").await.unwrap().status(), 200);
    assert_eq!(ok.hits(), 2);
}

#[tokio::test]
async fn a_503_is_retried_for_an_idempotent_method() {
    let busy = Upstream::start("busy", 503, Duration::ZERO);
    let ok = Upstream::ok("ok");
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}",
        endpoints(&[(&busy.url, 1), (&ok.url, 1)])
    ))
    .await;
    let response = gw.get("/pool").await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!((busy.hits(), ok.hits()), (1, 1));
}

#[tokio::test]
async fn a_503_is_not_retried_for_a_post() {
    let busy = Upstream::start("busy", 503, Duration::ZERO);
    let ok = Upstream::ok("ok");
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}",
        endpoints(&[(&busy.url, 1), (&ok.url, 1)])
    ))
    .await;
    let response = gw.post("/pool", "{}").await.unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!((busy.hits(), ok.hits()), (1, 0));
}

#[tokio::test]
async fn a_failing_endpoint_is_taken_out_of_rotation() {
    let busy = Upstream::start("busy", 503, Duration::ZERO);
    let ok = Upstream::ok("ok");
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}\nretries: 0",
        endpoints(&[(&busy.url, 1), (&ok.url, 1)])
    ))
    .await;
    for _ in 0..20 {
        gw.get("/pool").await.unwrap();
    }
    // Five failures within the window open the breaker; then `busy` is skipped.
    assert_eq!(busy.hits(), 5);
    assert_eq!(ok.hits(), 15);
    let log = gw.log();
    assert!(
        log.wait_for("circuit breaker opened", Duration::from_secs(2)),
        "{}",
        log.text()
    );
    let line = log.find_line("circuit breaker opened").unwrap();
    // The upstream's URL is its `scheme://host:port`.
    assert!(line.contains(&busy.url), "names the endpoint: {line}");
}

#[tokio::test]
async fn when_every_endpoint_is_failing_requests_still_go_out() {
    let a = Upstream::start("a", 503, Duration::ZERO);
    let b = Upstream::start("b", 503, Duration::ZERO);
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}\nretries: 0",
        endpoints(&[(&a.url, 1), (&b.url, 1)])
    ))
    .await;
    for _ in 0..20 {
        assert_eq!(gw.get("/pool").await.unwrap().status(), 503);
    }
    assert_eq!(a.hits() + b.hits(), 20);
}

#[tokio::test]
async fn retries_stay_within_the_timeout() {
    let slow_a = Upstream::start("a", 200, Duration::from_secs(3));
    let slow_b = Upstream::start("b", 200, Duration::from_secs(3));
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}\ntimeout: 1",
        endpoints(&[(&slow_a.url, 1), (&slow_b.url, 1)])
    ))
    .await;
    let started = Instant::now();
    let response = gw.get("/pool").await.unwrap();
    let elapsed = started.elapsed();
    assert_eq!(response.status(), 504);
    assert!(elapsed < Duration::from_millis(2500), "took {elapsed:?}");
}

#[tokio::test]
async fn metrics_and_the_request_span_name_endpoints_by_scheme_host_and_port() {
    let busy = Upstream::start("busy", 503, Duration::ZERO);
    let ok = Upstream::ok("ok");
    let (_dir, gw) = gateway(&format!(
        "endpoints:\n{}\npath: /deep/path",
        endpoints(&[(&busy.url, 1), (&ok.url, 1)])
    ))
    .await;
    assert_eq!(gw.get("/pool?secret=1").await.unwrap().status(), 200);

    let metrics = gw
        .admin_get("/metrics")
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    for line in [
        format!(
            "barbacane_upstream_endpoint_requests_total{{endpoint=\"{}\",outcome=\"failure\"}} 1",
            busy.url
        ),
        format!(
            "barbacane_upstream_endpoint_requests_total{{endpoint=\"{}\",outcome=\"success\"}} 1",
            ok.url
        ),
        format!(
            "barbacane_upstream_endpoint_retries_total{{endpoint=\"{}\"}} 1",
            ok.url
        ),
    ] {
        assert!(metrics.contains(&line), "{line} in:\n{metrics}");
    }
    assert!(!metrics.contains("secret=1") && !metrics.contains("/deep/path"));
}

#[tokio::test]
async fn url_and_endpoints_are_exclusive_and_retries_needs_endpoints() {
    let ok = Upstream::ok("ok");
    for config in [
        format!(
            "url: \"{}\"\nendpoints:\n{}",
            ok.url,
            endpoints(&[(&ok.url, 1)])
        ),
        format!("url: \"{}\"\nretries: 1", ok.url),
        "timeout: 5".to_string(),
    ] {
        let dir = tempfile::TempDir::new().unwrap();
        let spec = project(dir.path(), &config);
        let error = match TestGateway::from_spec(spec.to_str().unwrap()).await {
            Ok(_) => panic!("compiled {config}"),
            Err(e) => e.to_string(),
        };
        assert!(error.contains("E1023"), "{config}: {error}");
    }
}
