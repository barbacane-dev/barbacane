//! A plugin's HTTP calls may use only the TLS files and endpoint pools its own
//! config names.
//!
//! The TLS files sit under `BARBACANE_SECRETS_DIR`, which this binary sets for
//! all its tests, and the upstream is plain HTTP: a call the host lets through
//! reaches it, so a refused call is one that never connects.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use barbacane_wasm::{
    HttpClient, HttpClientConfig, InstanceKey, InstancePool, PluginLimits, WasmEngine,
};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use serde_json::json;

/// Where `ca.pem` and `other-ca.pem` are, set as `BARBACANE_SECRETS_DIR`.
fn secrets_dir() -> &'static PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        for name in ["ca.pem", "other-ca.pem"] {
            let key = KeyPair::generate().expect("key");
            let mut params = CertificateParams::new(Vec::<String>::new()).expect("params");
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            let cert = params.self_signed(&key).expect("certificate");
            std::fs::write(dir.join(name), cert.pem()).expect("write");
        }
        std::env::set_var("BARBACANE_SECRETS_DIR", &dir);
        dir
    })
}

/// A plain HTTP upstream answering 200; counts the connections it accepts.
fn upstream() -> (u16, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            counter.fetch_add(1, Ordering::SeqCst);
            let mut head = [0u8; 4096];
            let _ = stream.read(&mut head);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        }
    });
    (port, accepted)
}

/// A dispatcher whose `dispatch` sends `request` with `host_function`
/// (`host_http_call` or `host_http_pool_call`) and returns its result code.
fn calling_module(host_function: &str, request: &str) -> String {
    let escaped: String = request.bytes().map(|b| format!("\\{b:02x}")).collect();
    format!(
        r#"(module
  (import "barbacane" "{host_function}" (func $call (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 1024) "{escaped}")
  (func (export "alloc") (param i32) (result i32) (i32.const 8192))
  (func (export "init") (param i32 i32) (result i32) (i32.const 0))
  (func (export "dispatch") (param i32 i32) (result i32)
    (call $call (i32.const 1024) (i32.const {len}))))"#,
        len = request.len(),
    )
}

/// Connections the upstream accepted while a plugin with `config` called it
/// naming `tls`.
fn connections_for(config: serde_json::Value, tls: serde_json::Value) -> usize {
    let (port, accepted) = upstream();
    let request = json!({
        "method": "GET",
        "url": format!("http://127.0.0.1:{port}/"),
        "headers": {},
        "tls": tls,
    });
    call(&config, "host_http_call", &request);
    accepted.load(Ordering::SeqCst)
}

/// Connections the upstream accepted while a plugin whose config lists
/// `listed` (a pool of `upstream` endpoints, by weight) made a pool call to
/// `called`.
fn pool_connections_for(listed: &[u32], called: &[u32]) -> usize {
    let (port, accepted) = upstream();
    let pool = |weights: &[u32]| -> serde_json::Value {
        weights
            .iter()
            .map(|weight| json!({"url": format!("http://127.0.0.1:{port}"), "weight": weight}))
            .collect()
    };
    let config = json!({"endpoints": pool(listed)});
    let request = json!({
        "method": "GET",
        "url": "/",
        "headers": {},
        "endpoints": pool(called),
    });
    call(&config, "host_http_pool_call", &request);
    accepted.load(Ordering::SeqCst)
}

/// Runs a plugin with `config` whose `dispatch` sends `request` through
/// `host_function`.
fn call(config: &serde_json::Value, host_function: &str, request: &serde_json::Value) {
    secrets_dir();
    let request = request.to_string();

    let engine = Arc::new(WasmEngine::new().expect("engine"));
    let client = HttpClient::new(HttpClientConfig {
        allow_plaintext: true,
        allow_internal_egress: true,
        ..Default::default()
    })
    .expect("client");
    let pool = InstancePool::with_all_options(
        Arc::clone(&engine),
        PluginLimits::default(),
        Some(Arc::new(client)),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let wasm = wat::parse_str(calling_module(host_function, &request)).expect("valid WAT");
    let module = engine
        .compile(&wasm, "caller".into(), "0.1.0".into(), false)
        .expect("compile");
    pool.register_module(module);
    let key = InstanceKey::new("caller", config);
    pool.register_config(key.clone(), config.to_string().into_bytes());

    let mut instance = pool.get_instance(&key).expect("instance");
    instance.dispatch(b"{}").expect("dispatch");
}

#[test]
fn a_call_uses_the_tls_files_its_config_names() {
    let config = json!({"tls": {"ca": "ca.pem"}});
    assert_eq!(connections_for(config, json!({"ca": "ca.pem"})), 1);
}

#[test]
fn a_call_naming_a_tls_file_its_config_does_not_is_refused() {
    let config = json!({"tls": {"ca": "ca.pem"}});
    assert_eq!(connections_for(config, json!({"ca": "other-ca.pem"})), 0);

    let config = json!({"url": "https://upstream.example"});
    assert_eq!(connections_for(config, json!({"ca": "ca.pem"})), 0);
}

#[test]
fn a_pool_call_uses_a_pool_its_config_lists() {
    assert_eq!(pool_connections_for(&[1], &[1]), 1);
}

#[test]
fn a_pool_call_to_a_pool_its_config_does_not_list_is_refused() {
    // Another weight is another pool, with a rotation of its own.
    assert_eq!(pool_connections_for(&[1], &[2]), 0);
    assert_eq!(pool_connections_for(&[1], &[1, 1]), 0);
}
