//! Instance pooling for WASM plugins.
//!
//! Per SPEC-003 section 6.1, each (plugin name, config) pair produces a
//! separate WASM instance. Under load, instances are cloned from the
//! AOT-compiled module.

use std::sync::Arc;

use dashmap::DashMap;
use sha2::{Digest, Sha256};

use crate::engine::{CompiledModule, WasmEngine};
use crate::error::WasmError;
use crate::instance::PluginInstance;
use crate::limits::PluginLimits;

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

/// A resolved plugin ready for instantiation.
pub struct ResolvedPlugin {
    /// The compiled WASM module.
    pub module: CompiledModule,

    /// The plugin config (JSON).
    pub config: serde_json::Value,

    /// Pre-serialized config for passing to init.
    pub config_json: Vec<u8>,
}

/// Pool of WASM plugin instances.
///
/// Maintains a cache of compiled modules and manages instance creation.
pub struct InstancePool {
    /// The WASM engine.
    engine: Arc<WasmEngine>,

    /// Resource limits for instances.
    limits: PluginLimits,

    /// Cache of compiled modules by plugin name.
    modules: DashMap<String, CompiledModule>,

    /// Cache of initialized instances by key.
    /// In a production implementation, this would be a proper pool with
    /// checkout/return semantics. For now, we create new instances.
    instances: DashMap<InstanceKey, ()>,

    /// Plugin configs by key.
    configs: DashMap<InstanceKey, Vec<u8>>,
}

impl InstancePool {
    /// Create a new instance pool.
    pub fn new(engine: Arc<WasmEngine>, limits: PluginLimits) -> Self {
        Self {
            engine,
            limits,
            modules: DashMap::new(),
            instances: DashMap::new(),
            configs: DashMap::new(),
        }
    }

    /// Register a compiled module in the pool.
    pub fn register_module(&self, module: CompiledModule) {
        self.modules.insert(module.name.clone(), module);
    }

    /// Register a plugin config.
    pub fn register_config(&self, key: InstanceKey, config_json: Vec<u8>) {
        self.configs.insert(key.clone(), config_json);
        self.instances.insert(key, ());
    }

    /// Get or create an instance for the given key.
    pub fn get_instance(&self, key: &InstanceKey) -> Result<PluginInstance, WasmError> {
        // Get the compiled module
        let module = self
            .modules
            .get(&key.name)
            .ok_or_else(|| WasmError::InitFailed(format!("plugin not found: {}", key.name)))?;

        // Get the config
        let config_json = self
            .configs
            .get(key)
            .ok_or_else(|| WasmError::InitFailed(format!("config not found for: {}", key.name)))?;

        // Create a new instance
        let mut instance = PluginInstance::new(self.engine.engine(), &module, self.limits.clone())?;

        // Initialize with config
        let result = instance.init(&config_json)?;
        if result != 0 {
            return Err(WasmError::InitFailed(format!(
                "plugin {} init returned {}",
                key.name, result
            )));
        }

        Ok(instance)
    }

    /// Check if a plugin is registered.
    pub fn has_plugin(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// Get the number of registered modules.
    pub fn module_count(&self) -> usize {
        self.modules.len()
    }

    /// Get the number of registered instance keys.
    pub fn instance_key_count(&self) -> usize {
        self.instances.len()
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
        assert_eq!(pool.instance_key_count(), 0);
    }
}
