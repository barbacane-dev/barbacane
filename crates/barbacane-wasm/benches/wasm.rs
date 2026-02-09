//! WASM runtime benchmarks.
//!
//! Benchmarks engine compilation, instance lifecycle, middleware chain execution,
//! and context cloning — the per-request hot paths through the WASM layer.
//!
//! Run with: cargo bench -p barbacane-wasm --bench wasm

use std::collections::BTreeMap;
use std::sync::Arc;

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use barbacane_wasm::{
    execute_on_request_with_metrics, execute_on_response_with_metrics, ChainResult, PluginInstance,
    PluginLimits, RequestContext, WasmEngine,
};

/// Noop WASM middleware: accepts calls, returns 0 (continue), produces no output.
const NOOP_WAT: &str = r#"
(module
  (memory (export "memory") 4)
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_request") (param i32 i32) (result i32) (i32.const 0))
  (func (export "on_response") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32) (i32.const 0))
)
"#;

fn noop_wasm_bytes() -> Vec<u8> {
    wat::parse_str(NOOP_WAT).expect("valid WAT")
}

fn create_engine() -> Arc<WasmEngine> {
    Arc::new(WasmEngine::new().expect("engine creation"))
}

fn create_test_request() -> Vec<u8> {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("authorization".to_string(), "Bearer token123".to_string());

    let request = barbacane_wasm::Request {
        method: "GET".to_string(),
        path: "/api/users/12345".to_string(),
        query: Some("limit=10&offset=0".to_string()),
        headers,
        body: None,
        client_ip: "127.0.0.1".to_string(),
        path_params: BTreeMap::from([("id".to_string(), "12345".to_string())]),
    };

    serde_json::to_vec(&request).unwrap()
}

fn create_test_response() -> Vec<u8> {
    let response = barbacane_wasm::Response {
        status: 200,
        headers: BTreeMap::from([("content-type".to_string(), "application/json".to_string())]),
        body: Some(r#"{"id":"12345","name":"test"}"#.to_string()),
    };

    serde_json::to_vec(&response).unwrap()
}

fn create_noop_instance(engine: &WasmEngine) -> PluginInstance {
    let wasm = noop_wasm_bytes();
    let module = engine
        .compile(&wasm, "noop".to_string(), "0.1.0".to_string())
        .expect("compile");
    PluginInstance::new(engine.engine(), &module, PluginLimits::default()).expect("instance")
}

// ---------------------------------------------------------------------------
// Engine benchmarks
// ---------------------------------------------------------------------------

fn bench_wasm_engine(c: &mut Criterion) {
    let mut group = c.benchmark_group("wasm_engine");
    let engine = create_engine();
    let wasm = noop_wasm_bytes();

    group.bench_function("compile_module", |b| {
        b.iter(|| {
            black_box(
                engine
                    .compile(&wasm, "noop".to_string(), "0.1.0".to_string())
                    .unwrap(),
            );
        });
    });

    group.bench_function("validate_module", |b| {
        b.iter(|| {
            engine.validate(&wasm).unwrap();
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Instance benchmarks
// ---------------------------------------------------------------------------

fn bench_wasm_instance(c: &mut Criterion) {
    let mut group = c.benchmark_group("wasm_instance");
    let engine = create_engine();
    let wasm = noop_wasm_bytes();
    let module = engine
        .compile(&wasm, "noop".to_string(), "0.1.0".to_string())
        .unwrap();
    let request = create_test_request();

    group.bench_function("create_instance", |b| {
        b.iter(|| {
            black_box(
                PluginInstance::new(engine.engine(), &module, PluginLimits::default()).unwrap(),
            );
        });
    });

    group.bench_function("init_instance", |b| {
        let config = serde_json::to_vec(&serde_json::json!({"key": "value"})).unwrap();
        b.iter(|| {
            let mut instance =
                PluginInstance::new(engine.engine(), &module, PluginLimits::default()).unwrap();
            black_box(instance.init(&config).unwrap());
        });
    });

    for size in [1024, 10 * 1024, 100 * 1024] {
        let label = match size {
            1024 => "1KB",
            10240 => "10KB",
            _ => "100KB",
        };
        let data = vec![0u8; size];

        group.bench_with_input(
            BenchmarkId::new("write_to_memory", label),
            &data,
            |b, data| {
                let mut instance =
                    PluginInstance::new(engine.engine(), &module, PluginLimits::default()).unwrap();
                b.iter(|| {
                    black_box(instance.write_to_memory(data).unwrap());
                });
            },
        );
    }

    group.bench_function("on_request", |b| {
        b.iter(|| {
            let mut instance =
                PluginInstance::new(engine.engine(), &module, PluginLimits::default()).unwrap();
            black_box(instance.on_request(&request).unwrap());
        });
    });

    group.bench_function("dispatch", |b| {
        b.iter(|| {
            let mut instance =
                PluginInstance::new(engine.engine(), &module, PluginLimits::default()).unwrap();
            black_box(instance.dispatch(&request).unwrap());
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Middleware chain benchmarks
// ---------------------------------------------------------------------------

fn bench_middleware_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("middleware_chain");
    let engine = create_engine();
    let request = create_test_request();
    let response = create_test_response();

    for chain_size in [1, 3, 5] {
        group.bench_with_input(
            BenchmarkId::new("on_request", chain_size),
            &chain_size,
            |b, &size| {
                b.iter(|| {
                    let mut instances: Vec<_> =
                        (0..size).map(|_| create_noop_instance(&engine)).collect();
                    let ctx = RequestContext::new("trace-1".into(), "req-1".into());
                    let result = execute_on_request_with_metrics(
                        &mut instances,
                        black_box(&request),
                        ctx,
                        None,
                    );
                    assert!(matches!(result, ChainResult::Continue { .. }));
                    black_box(result);
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("on_response", chain_size),
            &chain_size,
            |b, &size| {
                b.iter(|| {
                    let mut instances: Vec<_> =
                        (0..size).map(|_| create_noop_instance(&engine)).collect();
                    let ctx = RequestContext::new("trace-1".into(), "req-1".into());
                    let result = execute_on_response_with_metrics(
                        &mut instances,
                        black_box(&response),
                        ctx,
                        None,
                    );
                    black_box(result);
                });
            },
        );
    }

    // Benchmark with metrics callback to measure callback overhead
    group.bench_function("on_request_with_callback/3", |b| {
        let callback = |_name: &str, _phase: &str, _duration: f64, _sc: bool| {};
        b.iter(|| {
            let mut instances: Vec<_> = (0..3).map(|_| create_noop_instance(&engine)).collect();
            let ctx = RequestContext::new("trace-1".into(), "req-1".into());
            let result = execute_on_request_with_metrics(
                &mut instances,
                black_box(&request),
                ctx,
                Some(&callback),
            );
            black_box(result);
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Context clone benchmarks
// ---------------------------------------------------------------------------

fn bench_context_clone(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_clone");

    group.bench_function("empty", |b| {
        let ctx = RequestContext::new("trace-1".into(), "req-1".into());
        b.iter(|| black_box(ctx.clone()));
    });

    for count in [10, 50] {
        let mut ctx = RequestContext::new("trace-1".into(), "req-1".into());
        for i in 0..count {
            ctx.values
                .insert(format!("key-{}", i), format!("value-{}", i));
        }

        group.bench_with_input(BenchmarkId::new("with_values", count), &ctx, |b, ctx| {
            b.iter(|| black_box(ctx.clone()));
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_wasm_engine,
    bench_wasm_instance,
    bench_middleware_chain,
    bench_context_clone
);
criterion_main!(benches);
