//! The cost of an instance of each official plugin, as a request gets one
//! from the pool, with a representative config.
//!
//! Needs the plugins built: `make plugins`, or
//! `cargo build --release --target wasm32-unknown-unknown` in each
//! `plugins/<name>` with the result copied to `plugins/<name>/<name>.wasm`.
//! A plugin whose `.wasm` is missing is skipped.
//!
//! Run with: cargo bench -p barbacane-wasm --bench plugins

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use serde_json::{json, Value};

use barbacane_wasm::{InstanceKey, InstancePool, PluginLimits, WasmEngine};

/// Each official plugin with a config its `init` accepts.
fn plugins() -> Vec<(&'static str, Value)> {
    vec![
        ("acl", json!({"allow": ["admins", "ops"]})),
        ("apikey-auth", json!({})),
        ("basic-auth", json!({"realm": "api"})),
        (
            "bot-detection",
            json!({"deny": ["curl", "python-requests"]}),
        ),
        ("cache", json!({"ttl": 60})),
        ("cel", json!({"expression": "request.method == 'GET'"})),
        ("correlation-id", json!({})),
        (
            "cors",
            json!({"allowed_origins": ["https://app.example.com"]}),
        ),
        (
            "fire-and-forget",
            json!({"url": "https://hooks.example.com/in"}),
        ),
        (
            "http-log",
            json!({"endpoint": "https://logs.example.com/ingest"}),
        ),
        ("http-upstream", json!({"url": "https://api.example.com"})),
        ("ip-restriction", json!({"allow": ["10.0.0.0/8"]})),
        ("jwt-auth", json!({"issuer": "https://auth.example.com"})),
        ("kafka", json!({"brokers": "kafka:9092", "topic": "events"})),
        ("lambda", json!({"url": "https://lambda.example.com/fn"})),
        (
            "ldap-auth",
            json!({"url": "ldaps://ldap.example.com", "user_base_dn": "ou=people,dc=example,dc=com"}),
        ),
        ("mock", json!({"status": 200, "body": "{\"ok\":true}"})),
        (
            "nats",
            json!({"url": "nats://nats:4222", "subject": "events"}),
        ),
        (
            "oauth2-auth",
            json!({
                "introspection_endpoint": "https://auth.example.com/introspect",
                "client_id": "gateway",
                "client_secret": "secret"
            }),
        ),
        ("observability", json!({})),
        (
            "oidc-auth",
            json!({"issuer_url": "https://auth.example.com"}),
        ),
        (
            "opa-authz",
            json!({"opa_url": "https://opa.example.com/v1/data/authz/allow"}),
        ),
        ("rate-limit", json!({"quota": 100, "window": 60})),
        (
            "redirect",
            json!({"rules": [{"path": "/old", "target": "/new"}]}),
        ),
        ("request-size-limit", json!({"max_bytes": 1048576})),
        (
            "request-transformer",
            json!({"headers": {"add": {"x-gateway": "barbacane"}}}),
        ),
        (
            "response-transformer",
            json!({"headers": {"remove": ["server"]}}),
        ),
        (
            "s3",
            json!({"access_key_id": "AKIAEXAMPLE", "secret_access_key": "secret", "region": "eu-west-1"}),
        ),
        ("ws-upstream", json!({"url": "wss://ws.example.com"})),
    ]
}

fn wasm_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
        .join(name)
        .join(format!("{name}.wasm"))
}

fn bench_plugin_instances(c: &mut Criterion) {
    let mut group = c.benchmark_group("plugin_instance");
    group
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2));
    let engine = Arc::new(WasmEngine::new().expect("engine"));
    let pool = InstancePool::new(Arc::clone(&engine), PluginLimits::default());

    for (name, config) in plugins() {
        let Ok(wasm) = std::fs::read(wasm_path(name)) else {
            eprintln!("skip {name}: not built");
            continue;
        };
        let module = engine
            .compile(&wasm, name.to_string(), "bench".to_string(), false)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        pool.register_module(module);
        let key = InstanceKey::new(name, &config);
        pool.register_config(key.clone(), serde_json::to_vec(&config).expect("config"));
        pool.get_instance(&key)
            .unwrap_or_else(|e| panic!("{name} does not initialise with {config}: {e}"));

        group.bench_function(name, |b| {
            b.iter(|| black_box(pool.get_instance(&key).expect("instance")));
        });
    }
    group.finish();
}

criterion_group!(benches, bench_plugin_instances);
criterion_main!(benches);
