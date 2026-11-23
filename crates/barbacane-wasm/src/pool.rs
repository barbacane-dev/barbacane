//! Instance pooling for WASM plugins.
//!
//! Per SPEC-003 section 6.1, each (plugin name, config) pair produces a
//! separate WASM instance. Under load, instances are cloned from the
//! AOT-compiled module.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use dashmap::DashMap;
use sha2::{Digest, Sha256};

use crate::cache::ResponseCache;
use crate::engine::{CompiledModule, WasmEngine};
use crate::error::WasmError;
use crate::http_client::HttpClient;
use crate::instance::PluginInstance;
use crate::limits::PluginLimits;
use crate::rate_limiter::RateLimiter;
use crate::secrets::SecretsStore;
use crate::snapshot::MemorySnapshot;

/// Key for identifying a plugin instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceKey {
    /// Plugin name.
    pub name: String,

    /// Hash of the serialized config for deduplication.
    pub config_hash: String,
}

impl InstanceKey {
    /// Create an instance key from a plugin name and config.
    pub fn new(name: &str, config: &serde_json::Value) -> Self {
        let config_str = serde_json::to_string(config).unwrap_or_default();
        let config_hash = compute_hash(&config_str);

        Self {
            name: name.to_string(),
            config_hash,
        }
    }
}

/// Compute a short hash of the given string.
fn compute_hash(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    // Use first 16 chars of hex for reasonable uniqueness
    hex::encode(&result[..8])
}

/// Pool of WASM plugin instances.
///
/// Maintains a cache of compiled modules and manages instance creation.
pub struct InstancePool {
    /// The engine the modules were compiled with. Its epoch ticker enforces
    /// the instances' time limits, so it lives as long as the pool.
    _engine: Arc<WasmEngine>,

    /// Resource limits for a plugin without its own.
    limits: PluginLimits,

    /// Resource limits by plugin name.
    plugin_limits: HashMap<String, PluginLimits>,

    /// HTTP client for plugins that need outbound HTTP calls.
    http_client: Option<Arc<HttpClient>>,

    /// Resolved secrets store (shared across all instances).
    secrets: Option<SecretsStore>,

    /// Rate limiter (shared across all instances).
    rate_limiter: Option<RateLimiter>,

    /// Response cache (shared across all instances).
    response_cache: Option<ResponseCache>,

    /// NATS publisher (shared across all instances).
    nats_publisher: Option<Arc<crate::nats_client::NatsPublisher>>,

    /// Kafka publisher (shared across all instances).
    kafka_publisher: Option<Arc<crate::kafka_client::KafkaPublisher>>,

    /// LDAP client (shared across all instances).
    ldap_client: Option<Arc<crate::ldap_client::LdapClient>>,

    /// Cache of compiled modules by plugin name.
    modules: DashMap<String, CompiledModule>,

    /// The memory `init` left, by key, for modules that can start from it.
    snapshots: DashMap<InstanceKey, Arc<MemorySnapshot>>,

    /// Plugin configs by key.
    configs: DashMap<InstanceKey, Vec<u8>>,

    /// Secret references (env://… / file://…) each plugin declares in its config,
    /// keyed by plugin name. Used to scope `host_get_secret` to a plugin's own
    /// secrets (WA-3). A plugin absent from this map gets an empty secrets store.
    secret_refs_by_plugin: HashMap<String, HashSet<String>>,

    /// Registry the gateway's metrics go to: plugin telemetry, and pool
    /// endpoint metrics from the host's HTTP calls.
    metrics: Option<Arc<barbacane_telemetry::MetricsRegistry>>,
}

impl InstancePool {
    /// Create a new instance pool.
    pub fn new(engine: Arc<WasmEngine>, limits: PluginLimits) -> Self {
        Self {
            _engine: engine,
            limits,
            http_client: None,
            secrets: None,
            rate_limiter: None,
            response_cache: None,
            nats_publisher: None,
            kafka_publisher: None,
            ldap_client: None,
            modules: DashMap::new(),
            snapshots: DashMap::new(),
            configs: DashMap::new(),
            secret_refs_by_plugin: HashMap::new(),
            plugin_limits: HashMap::new(),
            metrics: None,
        }
    }

    /// Create a new instance pool with all options.
    #[allow(clippy::too_many_arguments)]
    pub fn with_all_options(
        engine: Arc<WasmEngine>,
        limits: PluginLimits,
        http_client: Option<Arc<HttpClient>>,
        secrets: Option<SecretsStore>,
        rate_limiter: Option<RateLimiter>,
        response_cache: Option<ResponseCache>,
        nats_publisher: Option<Arc<crate::nats_client::NatsPublisher>>,
        kafka_publisher: Option<Arc<crate::kafka_client::KafkaPublisher>>,
        ldap_client: Option<Arc<crate::ldap_client::LdapClient>>,
    ) -> Self {
        Self {
            _engine: engine,
            limits,
            http_client,
            secrets,
            rate_limiter,
            response_cache,
            nats_publisher,
            kafka_publisher,
            ldap_client,
            modules: DashMap::new(),
            snapshots: DashMap::new(),
            configs: DashMap::new(),
            secret_refs_by_plugin: HashMap::new(),
            plugin_limits: HashMap::new(),
            metrics: None,
        }
    }

    /// Give every instance the gateway's metrics registry. Chain after a
    /// constructor.
    pub fn with_metrics(mut self, metrics: Arc<barbacane_telemetry::MetricsRegistry>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Set the per-plugin secret reference scopes (WA-3). Chain after a
    /// constructor: `InstancePool::with_all_options(..).with_secret_scopes(map)`.
    pub fn with_secret_scopes(mut self, scopes: HashMap<String, HashSet<String>>) -> Self {
        self.secret_refs_by_plugin = scopes;
        self
    }

    /// Set each plugin's resource limits, by plugin name. A plugin absent from
    /// the map gets the pool's limits. Chain after a constructor.
    pub fn with_plugin_limits(mut self, limits: HashMap<String, PluginLimits>) -> Self {
        self.plugin_limits = limits;
        self
    }

    /// The resource limits instances of `plugin` run with.
    pub fn limits_for(&self, plugin: &str) -> &PluginLimits {
        self.plugin_limits.get(plugin).unwrap_or(&self.limits)
    }

    /// Register a compiled module in the pool.
    pub fn register_module(&self, module: CompiledModule) {
        self.modules.insert(module.name.clone(), module);
    }

    /// Register a plugin config.
    pub fn register_config(&self, key: InstanceKey, config_json: Vec<u8>) {
        self.configs.insert(key, config_json);
    }

    /// A new instance for `key`, in the state its plugin's `init(config)` left.
    ///
    /// The first instance for a key runs `init`. When the module allows it
    /// (`CompiledModule` reports whether), what `init` wrote to memory is kept,
    /// and every later instance is a fresh one with that memory written back,
    /// without running `init` again. Either way no instance carries anything
    /// from an earlier request.
    pub fn get_instance(&self, key: &InstanceKey) -> Result<PluginInstance, WasmError> {
        // Get the compiled module
        let module = self
            .modules
            .get(&key.name)
            .ok_or_else(|| WasmError::PluginNotFound(key.name.clone()))?;

        // Get the config
        let config_json = self
            .configs
            .get(key)
            .ok_or_else(|| WasmError::ConfigNotRegistered(key.name.clone()))?;

        // Scope host_get_secret to this plugin's own config references (WA-3):
        // a plugin can resolve the secrets it declares, not another plugin's.
        let scoped_secrets = self.secrets.as_ref().map(|store| {
            match self.secret_refs_by_plugin.get(&key.name) {
                Some(refs) => store.scoped(refs),
                // Plugin declared no secret references -> empty store (default-deny).
                None => store.scoped(&HashSet::new()),
            }
        });

        // Create a new instance with all options
        let mut instance = PluginInstance::new_with_all_options(
            &module,
            self.limits_for(&key.name).clone(),
            self.http_client.clone(),
            scoped_secrets,
            self.rate_limiter.clone(),
            self.response_cache.clone(),
            self.nats_publisher.clone(),
            self.kafka_publisher.clone(),
            self.ldap_client.clone(),
        )?;
        instance.set_metrics(self.metrics.clone());
        instance.set_config_hash(&key.config_hash);
        if let Some(metrics) = &self.metrics {
            instance.count_in(Arc::clone(metrics));
        }

        if let Some(snapshot) = self.snapshots.get(key) {
            snapshot.restore(&mut instance)?;
            return Ok(instance);
        }

        let fresh = module.replayable().then(|| instance.memory().to_vec());
        let result = instance.init(&config_json)?;
        if result != 0 {
            return Err(WasmError::InitFailed(format!(
                "plugin {} init returned {}",
                key.name, result
            )));
        }
        if let Some(fresh) = fresh {
            let snapshot = MemorySnapshot::diff(&fresh, instance.memory());
            self.snapshots.insert(key.clone(), Arc::new(snapshot));
        }

        Ok(instance)
    }

    /// Number of keys whose instances start from a memory snapshot.
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Check if a plugin is registered.
    pub fn has_plugin(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// Check if a plugin has body_access capability.
    pub fn body_access(&self, name: &str) -> bool {
        self.modules
            .get(name)
            .map(|m| m.body_access)
            .unwrap_or(false)
    }

    /// Get the number of registered modules.
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn instance_key_from_config() {
        let key1 = InstanceKey::new("rate-limit", &json!({"quota": 100, "window": 60}));
        let key2 = InstanceKey::new("rate-limit", &json!({"quota": 100, "window": 60}));
        let key3 = InstanceKey::new("rate-limit", &json!({"quota": 200, "window": 60}));

        // Same config should produce same key
        assert_eq!(key1, key2);

        // Different config should produce different key
        assert_ne!(key1, key3);
    }

    #[test]
    fn instance_key_different_plugins() {
        let key1 = InstanceKey::new("plugin-a", &json!({}));
        let key2 = InstanceKey::new("plugin-b", &json!({}));

        assert_ne!(key1, key2);
    }

    #[test]
    fn create_pool() {
        let engine = Arc::new(WasmEngine::new().unwrap());
        let pool = InstancePool::new(engine, PluginLimits::default());

        assert_eq!(pool.module_count(), 0);
    }

    /// A middleware whose `init` grows its memory to 3 MiB and returns 1 when
    /// the growth is refused.
    const GROWS_TO_3_MIB: &str = r#"(module
      (memory (export "memory") 1)
      (func (export "alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "init") (param i32 i32) (result i32)
        (i32.eq (memory.grow (i32.const 47)) (i32.const -1)))
      (func (export "on_request") (param i32 i32) (result i32) (i32.const 0))
      (func (export "on_response") (param i32 i32) (result i32) (i32.const 0)))"#;

    #[test]
    fn each_plugin_runs_with_its_own_limits() {
        let engine = Arc::new(WasmEngine::new().unwrap());
        let wasm = wat::parse_str(GROWS_TO_3_MIB).expect("valid WAT");
        let small = PluginLimits::default().with_memory(2 * 1024 * 1024);
        let pool = InstancePool::new(Arc::clone(&engine), PluginLimits::default())
            .with_plugin_limits(HashMap::from([("small".to_string(), small)]));
        for name in ["small", "roomy"] {
            let module = engine
                .compile(&wasm, name.into(), "0.1.0".into(), false)
                .expect("compile");
            pool.register_module(module);
            pool.register_config(InstanceKey::new(name, &json!({})), b"{}".to_vec());
        }

        assert_eq!(pool.limits_for("small").max_memory_bytes, 2 * 1024 * 1024);
        assert_eq!(
            pool.limits_for("roomy").max_memory_bytes,
            PluginLimits::default().max_memory_bytes
        );
        let refused = pool
            .get_instance(&InstanceKey::new("small", &json!({})))
            .err()
            .expect("3 MiB does not fit in 2 MiB");
        assert!(refused.to_string().contains("init returned 1"), "{refused}");
        pool.get_instance(&InstanceKey::new("roomy", &json!({})))
            .expect("3 MiB fits in the default");
    }
    /// A middleware whose `init` stores 7 at 60000 and whose `on_request`
    /// returns 0 when it finds that and no earlier mark, 1 when an earlier
    /// call's mark is in memory or in its global, and 2 when `init`'s value is
    /// missing. Each call leaves a mark in both.
    const MARKS_EACH_CALL: &str = r#"(module
      (memory (export "memory") 1)
      (global $seen (mut i32) (i32.const 0))
      (func (export "alloc") (param i32) (result i32) (i32.const 1024))
      (func (export "init") (param i32 i32) (result i32)
        (i32.store8 (i32.const 60000) (i32.const 7))
        (i32.const 0))
      (func (export "on_request") (param i32 i32) (result i32)
        (if (i32.or (i32.load8_u (i32.const 60001)) (global.get $seen))
          (then (return (i32.const 1))))
        (if (i32.ne (i32.load8_u (i32.const 60000)) (i32.const 7))
          (then (return (i32.const 2))))
        (i32.store8 (i32.const 60001) (i32.const 1))
        (global.set $seen (i32.const 1))
        (i32.const 0))
      (func (export "on_response") (param i32 i32) (result i32) (i32.const 0)))"#;

    fn pool_with(name: &str, wat: &str) -> (InstancePool, InstanceKey) {
        let engine = Arc::new(WasmEngine::new().unwrap());
        let pool = InstancePool::new(Arc::clone(&engine), PluginLimits::default());
        let module = engine
            .compile(
                &wat::parse_str(wat).expect("valid WAT"),
                name.into(),
                "0.1.0".into(),
                false,
            )
            .expect("compile");
        pool.register_module(module);
        let key = InstanceKey::new(name, &json!({"k": "v"}));
        pool.register_config(key.clone(), br#"{"k":"v"}"#.to_vec());
        (pool, key)
    }

    #[test]
    fn each_instance_starts_where_init_left_off_and_sees_no_earlier_call() {
        let (pool, key) = pool_with("marker", MARKS_EACH_CALL);
        for call in 1..=3 {
            let mut instance = pool.get_instance(&key).expect("instance");
            assert_eq!(instance.on_request(b"{}").unwrap(), 0, "call {call}");
        }
        assert_eq!(pool.snapshot_count(), 1);

        // Control: an instance used twice sees its own earlier mark.
        let mut instance = pool.get_instance(&key).expect("instance");
        assert_eq!(instance.on_request(b"{}").unwrap(), 0);
        assert_eq!(instance.on_request(b"{}").unwrap(), 1);
    }

    #[test]
    fn a_restored_instance_has_the_memory_init_left() {
        // `init` grows memory by two pages and writes into the new one.
        let (pool, key) = pool_with(
            "grower",
            r#"(module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "init") (param i32 i32) (result i32)
                (drop (memory.grow (i32.const 2)))
                (i32.store (i32.const 140000) (i32.const 0x01020304))
                (i32.const 0))
              (func (export "on_request") (param i32 i32) (result i32) (i32.const 0))
              (func (export "on_response") (param i32 i32) (result i32) (i32.const 0)))"#,
        );
        let initialised = pool.get_instance(&key).expect("first instance");
        let restored = pool.get_instance(&key).expect("restored instance");
        assert_eq!(pool.snapshot_count(), 1);
        assert_eq!(restored.memory().len(), 3 * 65536);
        assert!(initialised.memory() == restored.memory());
    }

    #[test]
    fn a_module_init_does_not_fully_describe_runs_init_for_each_instance() {
        let two_globals = MARKS_EACH_CALL.replace(
            "(global $seen (mut i32) (i32.const 0))",
            "(global $seen (mut i32) (i32.const 0)) (global $other (mut i32) (i32.const 0))",
        );
        let (pool, key) = pool_with("marker", &two_globals);
        for call in 1..=3 {
            let mut instance = pool.get_instance(&key).expect("instance");
            assert_eq!(instance.on_request(b"{}").unwrap(), 0, "call {call}");
        }
        assert_eq!(pool.snapshot_count(), 0);
    }

    #[test]
    fn a_failed_init_leaves_no_snapshot() {
        let (pool, key) = pool_with(
            "refuser",
            r#"(module
              (memory (export "memory") 1)
              (func (export "alloc") (param i32) (result i32) (i32.const 1024))
              (func (export "init") (param i32 i32) (result i32) (i32.const 1))
              (func (export "on_request") (param i32 i32) (result i32) (i32.const 0))
              (func (export "on_response") (param i32 i32) (result i32) (i32.const 0)))"#,
        );
        for _ in 0..2 {
            let error = pool.get_instance(&key).err().expect("init fails");
            assert!(error.to_string().contains("init returned 1"), "{error}");
        }
        assert_eq!(pool.snapshot_count(), 0);
    }

    #[test]
    fn an_unknown_plugin_and_an_unregistered_configuration_are_told_apart() {
        let (pool, _) = pool_with("marker", MARKS_EACH_CALL);
        let unknown = pool
            .get_instance(&InstanceKey::new("absent", &json!({"k": "v"})))
            .err()
            .expect("no such plugin");
        assert!(
            matches!(&unknown, WasmError::PluginNotFound(name) if name == "absent"),
            "{unknown:?}"
        );
        let unregistered = pool
            .get_instance(&InstanceKey::new("marker", &json!({"other": 1})))
            .err()
            .expect("no such configuration");
        assert!(
            matches!(&unregistered, WasmError::ConfigNotRegistered(name) if name == "marker"),
            "{unregistered:?}"
        );
    }

    /// A pool over an engine whose pool holds `instances`, with `name`
    /// compiled from `wat` and one configuration registered.
    fn pooled_with(
        name: &str,
        wat: &str,
        instances: u32,
        max_memory_bytes: usize,
    ) -> (
        InstancePool,
        InstanceKey,
        Arc<barbacane_telemetry::MetricsRegistry>,
    ) {
        let pool_size = crate::engine::PoolSize {
            instances,
            max_memory_bytes,
        };
        let engine =
            Arc::new(WasmEngine::with_pool(PluginLimits::default(), Some(pool_size)).unwrap());
        assert_eq!(engine.pool_instances(), instances);
        let metrics = Arc::new(barbacane_telemetry::MetricsRegistry::new());
        let pool = InstancePool::new(Arc::clone(&engine), PluginLimits::default())
            .with_metrics(Arc::clone(&metrics));
        let module = engine
            .compile(
                &wat::parse_str(wat).expect("valid WAT"),
                name.into(),
                "0.1.0".into(),
                false,
            )
            .expect("compile");
        pool.register_module(module);
        let key = InstanceKey::new(name, &json!({"k": "v"}));
        pool.register_config(key.clone(), br#"{"k":"v"}"#.to_vec());
        (pool, key, metrics)
    }

    const MIB: usize = 1024 * 1024;

    #[test]
    fn a_full_pool_allocates_the_next_instances_per_request() {
        let (pool, key, metrics) = pooled_with("marker", MARKS_EACH_CALL, 2, 16 * MIB);
        let mut alive: Vec<PluginInstance> = (0..5)
            .map(|n| {
                pool.get_instance(&key)
                    .unwrap_or_else(|e| panic!("instance {n}: {e}"))
            })
            .collect();

        let full: Vec<bool> = alive.iter().map(PluginInstance::pool_was_full).collect();
        assert_eq!(full, [false, false, true, true, true]);
        for instance in &mut alive {
            assert_eq!(instance.on_request(b"{}").unwrap(), 0);
        }
        assert_eq!(metrics.wasm_instances.get(), 5);
        assert_eq!(metrics.wasm_instances_unpooled_total.get(), 3);

        drop(alive);
        assert_eq!(metrics.wasm_instances.get(), 0);
    }

    #[test]
    fn a_dropped_instance_gives_its_slot_back() {
        let (pool, key, metrics) = pooled_with("marker", MARKS_EACH_CALL, 2, 16 * MIB);
        for round in 0..50 {
            let pair = [pool.get_instance(&key), pool.get_instance(&key)];
            for instance in pair {
                let instance = instance.unwrap_or_else(|e| panic!("round {round}: {e}"));
                assert!(!instance.pool_was_full(), "round {round}: a slot was kept");
            }
        }
        assert_eq!(metrics.wasm_instances_unpooled_total.get(), 0);
        assert_eq!(metrics.wasm_instances.get(), 0);
    }

    #[test]
    fn a_reused_slot_holds_nothing_from_its_last_instance() {
        // One slot: every instance after the first reuses the memory the
        // previous one wrote to.
        let (pool, key, metrics) = pooled_with("marker", MARKS_EACH_CALL, 1, 16 * MIB);
        for call in 1..=5 {
            let mut instance = pool.get_instance(&key).expect("instance");
            assert!(!instance.pool_was_full(), "call {call}");
            assert_eq!(instance.on_request(b"{}").unwrap(), 0, "call {call}");
        }
        assert_eq!(pool.snapshot_count(), 1);
        assert_eq!(metrics.wasm_instances_unpooled_total.get(), 0);
    }

    #[test]
    fn a_pooled_instance_keeps_its_plugin_memory_limit() {
        let small = PluginLimits::default().with_memory(2 * MIB);
        let engine = Arc::new(
            WasmEngine::with_pool(
                PluginLimits::default(),
                Some(crate::engine::PoolSize {
                    instances: 4,
                    max_memory_bytes: 16 * MIB,
                }),
            )
            .unwrap(),
        );
        let pool = InstancePool::new(Arc::clone(&engine), PluginLimits::default())
            .with_plugin_limits(HashMap::from([("small".to_string(), small)]));
        let wasm = wat::parse_str(GROWS_TO_3_MIB).expect("valid WAT");
        for name in ["small", "roomy"] {
            let module = engine
                .compile(&wasm, name.into(), "0.1.0".into(), false)
                .expect("compile");
            assert!(module.pooled(), "{name} fits the pool");
            pool.register_module(module);
            pool.register_config(InstanceKey::new(name, &json!({})), b"{}".to_vec());
        }

        let refused = pool
            .get_instance(&InstanceKey::new("small", &json!({})))
            .err()
            .expect("3 MiB does not fit in 2 MiB");
        assert!(refused.to_string().contains("init returned 1"), "{refused}");
        let roomy = pool
            .get_instance(&InstanceKey::new("roomy", &json!({})))
            .expect("3 MiB fits in the default");
        assert!(!roomy.pool_was_full());
    }

    #[test]
    fn a_module_the_pool_cannot_hold_runs_per_request() {
        // Its memory starts at 2 pages; the pool's slots hold 1.
        let two_pages = MARKS_EACH_CALL.replace(
            "(memory (export \"memory\") 1)",
            "(memory (export \"memory\") 2)",
        );
        let (pool, key, metrics) = pooled_with("marker", &two_pages, 4, 65536);
        assert!(!pool.modules.get("marker").expect("registered").pooled());
        for call in 1..=3 {
            let mut instance = pool.get_instance(&key).expect("instance");
            assert!(!instance.pool_was_full(), "call {call}");
            assert_eq!(instance.on_request(b"{}").unwrap(), 0, "call {call}");
        }
        assert_eq!(metrics.wasm_instances_unpooled_total.get(), 0);
    }

    #[test]
    fn a_pool_of_zero_is_no_pool() {
        let (pool, key, metrics) = pooled_with("marker", MARKS_EACH_CALL, 0, 16 * MIB);
        assert!(!pool.modules.get("marker").expect("registered").pooled());
        let mut instance = pool.get_instance(&key).expect("instance");
        assert_eq!(instance.on_request(b"{}").unwrap(), 0);
        assert_eq!(metrics.wasm_instances_unpooled_total.get(), 0);
    }
}
