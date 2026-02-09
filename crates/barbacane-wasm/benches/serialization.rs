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
        let req = Request {
            method: "GET".to_string(),
            path: "/health".to_string(),
            query: None,
            headers: BTreeMap::new(),
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        };
        b.iter(|| black_box(serde_json::to_vec(&req).unwrap()));
    });

    // With varying header counts
    for count in [5, 20] {
        let mut headers = BTreeMap::new();
        for i in 0..count {
            headers.insert(format!("x-header-{}", i), format!("value-{}", i));
        }
        let req = Request {
            method: "POST".to_string(),
            path: "/api/users".to_string(),
            query: None,
            headers,
            body: None,
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
        };

        group.bench_with_input(BenchmarkId::new("with_headers", count), &req, |b, req| {
            b.iter(|| black_box(serde_json::to_vec(req).unwrap()));
        });
    }

    // With varying body sizes
    for (label, size) in [("1KB", 1024), ("10KB", 10 * 1024)] {
        let body = "x".repeat(size);
        let req = Request {
            method: "POST".to_string(),
            path: "/api/users".to_string(),
            query: None,
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: Some(body),
            client_ip: "127.0.0.1".to_string(),
            path_params: BTreeMap::new(),
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

        let req = Request {
            method: "POST".to_string(),
            path: "/api/v1/users/{userId}/orders/{orderId}".to_string(),
            query: Some("include=items&sort=date&limit=10".to_string()),
            headers,
            body: Some("x".repeat(1024)),
            client_ip: "192.168.1.100".to_string(),
            path_params,
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
        let resp = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: None,
        };
        b.iter(|| black_box(serde_json::to_vec(&resp).unwrap()));
    });

    for (label, size) in [("1KB", 1024), ("10KB", 10 * 1024)] {
        let resp = Response {
            status: 200,
            headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: Some("x".repeat(size)),
        };

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
