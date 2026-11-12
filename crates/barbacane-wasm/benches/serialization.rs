//! Request/Response serialization benchmarks.
//!
//! Measures serde_json serialization cost for the plugin Request and Response
//! types with varying payload sizes — this runs on every request.
//!
//! Run with: cargo bench -p barbacane-wasm --bench serialization

use std::collections::BTreeMap;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use barbacane_wasm::{Request, Response};

// ---------------------------------------------------------------------------
// Request serialization
// ---------------------------------------------------------------------------

fn bench_request_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("request_serialization");

    // Minimal request
    group.bench_function("minimal", |b| {
        let req = Request::new("GET", "/health").with_client_ip("127.0.0.1");
        b.iter(|| black_box(serde_json::to_vec(&req).unwrap()));
    });

    // With varying header counts
    for count in [5, 20] {
        let mut headers = BTreeMap::new();
        for i in 0..count {
            headers.insert(format!("x-header-{}", i), format!("value-{}", i));
        }
        let req = Request::new("POST", "/api/users")
            .with_client_ip("127.0.0.1")
            .with_headers(headers);

        group.bench_with_input(BenchmarkId::new("with_headers", count), &req, |b, req| {
            b.iter(|| black_box(serde_json::to_vec(req).unwrap()));
        });
    }

    // With varying body sizes
    for (label, size) in [("1KB", 1024), ("10KB", 10 * 1024)] {
        let body = "x".repeat(size);
        let req = {
            let mut req = Request::new("POST", "/api/users")
                .with_body(body.into_bytes())
                .with_client_ip("127.0.0.1");
            req.headers =
                BTreeMap::from([("content-type".to_string(), "application/json".to_string())]);
            req
        };

        group.bench_with_input(BenchmarkId::new("with_body", label), &req, |b, req| {
            b.iter(|| black_box(serde_json::to_vec(req).unwrap()));
        });
    }

    // Full realistic request
    group.bench_function("full", |b| {
        let mut headers = BTreeMap::new();
        for i in 0..10 {
            headers.insert(format!("x-header-{}", i), format!("value-{}", i));
        }
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert(
            "authorization".to_string(),
            "Bearer eyJhbGciOi...".to_string(),
        );

        let mut path_params = BTreeMap::new();
        for i in 0..5 {
            path_params.insert(format!("param{}", i), format!("val{}", i));
        }

        let req = {
            let mut req = Request::new("POST", "/api/v1/users/{userId}/orders/{orderId}")
                .with_query("include=items&sort=date&limit=10")
                .with_body(vec![b'x'; 1024])
                .with_client_ip("192.168.1.100");
            req.headers = headers;
            req.path_params = path_params;
            req
        };
        b.iter(|| black_box(serde_json::to_vec(&req).unwrap()));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Response serialization
// ---------------------------------------------------------------------------

fn bench_response_serialization(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_serialization");

    group.bench_function("minimal", |b| {
        let resp = Response::new(200);
        b.iter(|| black_box(serde_json::to_vec(&resp).unwrap()));
    });

    for (label, size) in [("1KB", 1024), ("10KB", 10 * 1024)] {
        let resp = Response::new(200)
            .with_header("content-type", "application/json")
            .with_body(vec![b'x'; size]);

        group.bench_with_input(BenchmarkId::new("with_body", label), &resp, |b, resp| {
            b.iter(|| black_box(serde_json::to_vec(resp).unwrap()));
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_request_serialization,
    bench_response_serialization
);
criterion_main!(benches);
