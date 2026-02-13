//! Routing benchmarks for the prefix-trie router.
//!
//! Run with: cargo bench -p barbacane

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use barbacane_lib::router::{RouteEntry, Router};

/// Generate a set of realistic API routes.
fn generate_routes(count: usize) -> Vec<(String, String)> {
    let resources = [
        "users",
        "orders",
        "products",
        "customers",
        "invoices",
        "payments",
    ];
    let methods = ["GET", "POST", "PUT", "DELETE"];

    let mut routes = Vec::new();

    // Add base resource routes
    for resource in &resources {
        routes.push((format!("/{}", resource), "GET".to_string()));
        routes.push((format!("/{}", resource), "POST".to_string()));
        routes.push((format!("/{}/{{id}}", resource), "GET".to_string()));
        routes.push((format!("/{}/{{id}}", resource), "PUT".to_string()));
        routes.push((format!("/{}/{{id}}", resource), "DELETE".to_string()));
    }

    // Add nested routes
    routes.push(("/users/{userId}/orders".to_string(), "GET".to_string()));
    routes.push((
        "/users/{userId}/orders/{orderId}".to_string(),
        "GET".to_string(),
    ));
    routes.push((
        "/products/{productId}/reviews".to_string(),
        "GET".to_string(),
    ));
    routes.push((
        "/products/{productId}/reviews/{reviewId}".to_string(),
        "GET".to_string(),
    ));

    // Fill to desired count with variations
    while routes.len() < count {
        let i = routes.len();
        let resource = resources[i % resources.len()];
        let method = methods[i % methods.len()];
        routes.push((format!("/api/v{}/{}", i / 10, resource), method.to_string()));
    }

    routes.truncate(count);
    routes
}

/// Build a router with the given routes.
fn build_router(routes: &[(String, String)]) -> Router {
    let mut router = Router::new();
    for (i, (path, method)) in routes.iter().enumerate() {
        router.insert(path, method, RouteEntry { operation_index: i });
    }
    router
}

fn bench_router_lookup(c: &mut Criterion) {
    let mut group = c.benchmark_group("router_lookup");

    for route_count in [10, 50, 100, 500, 1000] {
        let routes = generate_routes(route_count);
        let router = build_router(&routes);

        // Benchmark static path lookup
        group.bench_with_input(
            BenchmarkId::new("static_path", route_count),
            &router,
            |b, router| {
                b.iter(|| {
                    black_box(router.lookup("/users", "GET"));
                });
            },
        );

        // Benchmark parameterized path lookup
        group.bench_with_input(
            BenchmarkId::new("param_path", route_count),
            &router,
            |b, router| {
                b.iter(|| {
                    black_box(router.lookup("/users/12345", "GET"));
                });
            },
        );

        // Benchmark nested param path lookup
        group.bench_with_input(
            BenchmarkId::new("nested_param_path", route_count),
            &router,
            |b, router| {
                b.iter(|| {
                    black_box(router.lookup("/users/12345/orders/67890", "GET"));
                });
            },
        );

        // Benchmark not found path
        group.bench_with_input(
            BenchmarkId::new("not_found", route_count),
            &router,
            |b, router| {
                b.iter(|| {
                    black_box(router.lookup("/nonexistent/path/here", "GET"));
                });
            },
        );
    }

    group.finish();
}

fn bench_router_insert(c: &mut Criterion) {
    let mut group = c.benchmark_group("router_insert");

    for route_count in [10, 50, 100, 500] {
        let routes = generate_routes(route_count);

        group.bench_with_input(
            BenchmarkId::new("build_router", route_count),
            &routes,
            |b, routes| {
                b.iter(|| {
                    black_box(build_router(routes));
                });
            },
        );
    }

    group.finish();
}

criterion_group!(benches, bench_router_lookup, bench_router_insert);
criterion_main!(benches);
