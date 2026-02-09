//! WASM plugin instance management.
//!
//! Each plugin instance wraps a wasmtime Store and Instance with the
//! plugin state required for host function calls.

use std::collections::HashMap;
use std::sync::Arc;

use wasmtime::{Caller, Engine, Instance, Linker, Memory, Store, TypedFunc};

use serde::Deserialize;
use std::collections::BTreeMap;

use crate::broker::{BrokerMessage, BrokerRegistry};
use crate::engine::CompiledModule;
use crate::error::WasmError;
use crate::http_client::{
    HttpClient, HttpRequest as HttpClientRequest, HttpResponse as HttpClientResponse,
};
use crate::limits::PluginLimits;

/// HTTP request format from WASM plugins.
/// This matches the format used by http-upstream plugin.
#[derive(Debug, Deserialize)]
struct PluginHttpRequest {
    method: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// Per-request context passed to plugins.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    /// Key-value store for inter-middleware communication.
    pub values: HashMap<String, String>,

    /// Result buffer for host_context_get.
    pub last_get_result: Option<String>,

    /// Trace ID for distributed tracing.
    pub trace_id: String,

    /// Request ID.
    pub request_id: String,
}

impl RequestContext {
    /// Create a new request context.
    pub fn new(trace_id: String, request_id: String) -> Self {
        Self {
            values: HashMap::new(),
            last_get_result: None,
            trace_id,
            request_id,
        }
    }
}

/// State attached to each WASM store.
pub struct PluginState {
    /// Plugin name for logging.
    pub plugin_name: String,

    /// Output buffer for plugin results.
    pub output_buffer: Vec<u8>,

    /// Per-request context.
    pub context: RequestContext,

    /// Maximum memory in bytes.
    pub max_memory: usize,

    /// HTTP client for outbound requests (shared).
    pub http_client: Option<Arc<HttpClient>>,

    /// Result buffer for host_http_read_result.
    pub last_http_result: Option<Vec<u8>>,

    /// Resolved secrets store (shared across instances).
    pub secrets: crate::secrets::SecretsStore,

    /// Result buffer for host_secret_read_result.
    pub last_secret_result: Option<Vec<u8>>,

    /// Rate limiter (shared across instances).
    pub rate_limiter: Option<crate::rate_limiter::RateLimiter>,

    /// Result buffer for host_rate_limit_read_result.
    pub last_rate_limit_result: Option<Vec<u8>>,

    /// Response cache (shared across instances).
    pub response_cache: Option<crate::cache::ResponseCache>,

    /// Result buffer for host_cache_read_result.
    pub last_cache_result: Option<Vec<u8>>,

    /// Metrics registry for plugin telemetry (shared).
    pub metrics: Option<Arc<barbacane_telemetry::MetricsRegistry>>,

    /// Broker registry for Kafka/NATS publishing (shared).
    pub brokers: Option<Arc<BrokerRegistry>>,

    /// Result buffer for host_kafka_publish / host_nats_publish.
    pub last_broker_result: Option<Vec<u8>>,

    /// Result buffer for host_uuid_read_result.
    pub last_uuid_result: Option<Vec<u8>>,
}

impl PluginState {
    /// Create new plugin state.
    pub fn new(plugin_name: String, limits: &PluginLimits) -> Self {
        Self {
            plugin_name,
            output_buffer: Vec::new(),
            context: RequestContext::default(),
            max_memory: limits.max_memory_bytes,
            http_client: None,
            last_http_result: None,
            secrets: crate::secrets::SecretsStore::new(),
            last_secret_result: None,
            rate_limiter: None,
            last_rate_limit_result: None,
            response_cache: None,
            last_cache_result: None,
            metrics: None,
            brokers: None,
            last_broker_result: None,
            last_uuid_result: None,
        }
    }

    /// Create new plugin state with HTTP client.
    pub fn with_http_client(
        plugin_name: String,
        limits: &PluginLimits,
        http_client: Arc<HttpClient>,
    ) -> Self {
        Self {
            plugin_name,
            output_buffer: Vec::new(),
            context: RequestContext::default(),
            max_memory: limits.max_memory_bytes,
            http_client: Some(http_client),
            last_http_result: None,
            secrets: crate::secrets::SecretsStore::new(),
            last_secret_result: None,
            rate_limiter: None,
            last_rate_limit_result: None,
            response_cache: None,
            last_cache_result: None,
            metrics: None,
            brokers: None,
            last_broker_result: None,
            last_uuid_result: None,
        }
    }

    /// Create new plugin state with HTTP client and secrets.
    pub fn with_http_client_and_secrets(
        plugin_name: String,
        limits: &PluginLimits,
        http_client: Arc<HttpClient>,
        secrets: crate::secrets::SecretsStore,
    ) -> Self {
        Self {
            plugin_name,
            output_buffer: Vec::new(),
            context: RequestContext::default(),
            max_memory: limits.max_memory_bytes,
            http_client: Some(http_client),
            last_http_result: None,
            secrets,
            last_secret_result: None,
            rate_limiter: None,
            last_rate_limit_result: None,
            response_cache: None,
            last_cache_result: None,
            metrics: None,
            brokers: None,
            last_broker_result: None,
            last_uuid_result: None,
        }
    }

    /// Create new plugin state with all options.
    pub fn with_all_options(
        plugin_name: String,
        limits: &PluginLimits,
        http_client: Option<Arc<HttpClient>>,
        secrets: crate::secrets::SecretsStore,
        rate_limiter: Option<crate::rate_limiter::RateLimiter>,
        response_cache: Option<crate::cache::ResponseCache>,
    ) -> Self {
        Self {
            plugin_name,
            output_buffer: Vec::new(),
            context: RequestContext::default(),
            max_memory: limits.max_memory_bytes,
            http_client,
            last_http_result: None,
            secrets,
            last_secret_result: None,
            rate_limiter,
            last_rate_limit_result: None,
            response_cache,
            last_cache_result: None,
            metrics: None,
            brokers: None,
            last_broker_result: None,
            last_uuid_result: None,
        }
    }

    /// Create new plugin state with all options including metrics.
    pub fn with_all_options_and_metrics(
        plugin_name: String,
        limits: &PluginLimits,
        http_client: Option<Arc<HttpClient>>,
        secrets: crate::secrets::SecretsStore,
        rate_limiter: Option<crate::rate_limiter::RateLimiter>,
        response_cache: Option<crate::cache::ResponseCache>,
        metrics: Option<Arc<barbacane_telemetry::MetricsRegistry>>,
    ) -> Self {
        Self {
            plugin_name,
            output_buffer: Vec::new(),
            context: RequestContext::default(),
            max_memory: limits.max_memory_bytes,
            http_client,
            last_http_result: None,
            secrets,
            last_secret_result: None,
            rate_limiter,
            last_rate_limit_result: None,
            response_cache,
            last_cache_result: None,
            metrics,
            brokers: None,
            last_broker_result: None,
            last_uuid_result: None,
        }
    }

    /// Get the output buffer contents.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output_buffer)
    }

    /// Set the request context for this call.
    pub fn set_context(&mut self, context: RequestContext) {
        self.context = context;
    }
}

impl wasmtime::ResourceLimiter for PluginState {
    fn memory_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        Ok(desired <= self.max_memory)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> anyhow::Result<bool> {
        // Allow reasonable table growth
        Ok(desired <= 10_000)
    }
}

/// A WASM plugin instance ready for execution.
pub struct PluginInstance {
    store: Store<PluginState>,
    #[allow(dead_code)]
    instance: Instance,
    limits: PluginLimits,

    // Cached function references
    init_func: Option<TypedFunc<(i32, i32), i32>>,
    on_request_func: Option<TypedFunc<(i32, i32), i32>>,
    on_response_func: Option<TypedFunc<(i32, i32), i32>>,
    dispatch_func: Option<TypedFunc<(i32, i32), i32>>,
    memory: Memory,
}

impl PluginInstance {
    /// Create a new plugin instance from a compiled module.
    pub fn new(
        engine: &Engine,
        module: &CompiledModule,
        limits: PluginLimits,
    ) -> Result<Self, WasmError> {
        Self::new_with_options(engine, module, limits, None, None)
    }

    /// Create a new plugin instance with an HTTP client for outbound calls.
    pub fn new_with_http_client(
        engine: &Engine,
        module: &CompiledModule,
        limits: PluginLimits,
        http_client: Option<Arc<HttpClient>>,
    ) -> Result<Self, WasmError> {
        Self::new_with_options(engine, module, limits, http_client, None)
    }

    /// Create a new plugin instance with HTTP client and secrets.
    pub fn new_with_options(
        engine: &Engine,
        module: &CompiledModule,
        limits: PluginLimits,
        http_client: Option<Arc<HttpClient>>,
        secrets: Option<crate::secrets::SecretsStore>,
    ) -> Result<Self, WasmError> {
        Self::new_with_all_options(engine, module, limits, http_client, secrets, None, None)
    }

    /// Create a new plugin instance with all options including rate limiter and cache.
    pub fn new_with_all_options(
        engine: &Engine,
        module: &CompiledModule,
        limits: PluginLimits,
        http_client: Option<Arc<HttpClient>>,
        secrets: Option<crate::secrets::SecretsStore>,
        rate_limiter: Option<crate::rate_limiter::RateLimiter>,
        response_cache: Option<crate::cache::ResponseCache>,
    ) -> Result<Self, WasmError> {
        let state = PluginState::with_all_options(
            module.name.clone(),
            &limits,
            http_client,
            secrets.unwrap_or_default(),
            rate_limiter,
            response_cache,
        );
        let mut store = Store::new(engine, state);

        // Set fuel for execution limiting
        store
            .set_fuel(limits.max_fuel)
            .map_err(|e| WasmError::Instantiation(format!("failed to set fuel: {}", e)))?;

        // Enable resource limiting
        store.limiter(|state| state);

        // Create linker and add host functions
        let mut linker = Linker::new(engine);
        add_host_functions(&mut linker)?;

        // Instantiate the module
        let instance = linker
            .instantiate(&mut store, module.module())
            .map_err(|e| WasmError::Instantiation(e.to_string()))?;

        // Get memory
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| WasmError::MissingExport("memory".into()))?;

        // Cache function references
        let init_func = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "init")
            .ok();
        let on_request_func = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "on_request")
            .ok();
        let on_response_func = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "on_response")
            .ok();
        let dispatch_func = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, "dispatch")
            .ok();

        Ok(Self {
            store,
            instance,
            limits,
            init_func,
            on_request_func,
            on_response_func,
            dispatch_func,
            memory,
        })
    }

    /// Get the plugin name.
    pub fn name(&self) -> &str {
        &self.store.data().plugin_name
    }

    /// Write data to the plugin's memory and return the pointer.
    pub fn write_to_memory(&mut self, data: &[u8]) -> Result<i32, WasmError> {
        let mem_size = self.memory.data_size(&self.store);

        // Find space in memory (simple bump allocator from end of memory)
        // In a real implementation, we'd use the plugin's allocator
        if data.len() > mem_size {
            return Err(WasmError::MemoryLimitExceeded {
                requested: data.len(),
                limit: mem_size,
            });
        }

        let ptr = (mem_size - data.len()) as i32;
        if ptr < 0 {
            return Err(WasmError::MemoryLimitExceeded {
                requested: data.len(),
                limit: mem_size,
            });
        }

        self.memory
            .write(&mut self.store, ptr as usize, data)
            .map_err(|e| WasmError::Trap(format!("memory write failed: {}", e)))?;

        Ok(ptr)
    }

    /// Call the init function with the given config.
    pub fn init(&mut self, config_json: &[u8]) -> Result<i32, WasmError> {
        let init_func = self
            .init_func
            .clone()
            .ok_or_else(|| WasmError::MissingExport("init".into()))?;

        // Write config to memory
        let ptr = self.write_to_memory(config_json)?;
        let len = config_json.len() as i32;

        // Reset fuel for this call
        self.store.set_fuel(self.limits.max_fuel).ok();

        // Call init
        let result = init_func
            .call(&mut self.store, (ptr, len))
            .map_err(|e| WasmError::Trap(e.to_string()))?;

        Ok(result)
    }

    /// Call on_request with the given request data.
    pub fn on_request(&mut self, request_json: &[u8]) -> Result<i32, WasmError> {
        let func = self
            .on_request_func
            .clone()
            .ok_or_else(|| WasmError::MissingExport("on_request".into()))?;

        self.call_handler(func, request_json)
    }

    /// Call on_response with the given response data.
    pub fn on_response(&mut self, response_json: &[u8]) -> Result<i32, WasmError> {
        let func = self
            .on_response_func
            .clone()
            .ok_or_else(|| WasmError::MissingExport("on_response".into()))?;

        self.call_handler(func, response_json)
    }

    /// Call dispatch with the given request data.
    pub fn dispatch(&mut self, request_json: &[u8]) -> Result<i32, WasmError> {
        let func = self
            .dispatch_func
            .clone()
            .ok_or_else(|| WasmError::MissingExport("dispatch".into()))?;

        self.call_handler(func, request_json)
    }

    /// Call a handler function with data.
    fn call_handler(
        &mut self,
        func: TypedFunc<(i32, i32), i32>,
        data: &[u8],
    ) -> Result<i32, WasmError> {
        // Clear output buffer
        self.store.data_mut().output_buffer.clear();

        // Write data to memory
        let ptr = self.write_to_memory(data)?;
        let len = data.len() as i32;

        // Reset fuel
        self.store.set_fuel(self.limits.max_fuel).ok();

        // Call function
        let result = func
            .call(&mut self.store, (ptr, len))
            .map_err(|e| WasmError::Trap(e.to_string()))?;

        Ok(result)
    }

    /// Get the output buffer contents.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.store.data_mut().take_output()
    }

    /// Set the request context for the next call.
    pub fn set_context(&mut self, context: RequestContext) {
        self.store.data_mut().set_context(context);
    }

    /// Get the current request context (after modifications by host functions).
    pub fn get_context(&self) -> RequestContext {
        self.store.data().context.clone()
    }
}

/// Register a `host_*_read_result` function that copies data from plugin state to WASM memory.
///
/// All read_result host functions follow the same pattern: take a result buffer from state,
/// get the WASM memory export, copy bytes into the provided buffer, return bytes written.
fn add_read_result_fn(
    linker: &mut Linker<PluginState>,
    name: &str,
    extract: impl Fn(&mut PluginState) -> Option<Vec<u8>> + Send + Sync + 'static,
) -> Result<(), WasmError> {
    linker
        .func_wrap(
            "barbacane",
            name,
            move |mut caller: Caller<'_, PluginState>, buf_ptr: i32, buf_len: i32| -> i32 {
                let result = extract(caller.data_mut());
                if let Some(data) = result {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return 0,
                    };
                    let copy_len = std::cmp::min(data.len(), buf_len as usize);
                    if memory
                        .write(&mut caller, buf_ptr as usize, &data[..copy_len])
                        .is_ok()
                    {
                        return copy_len as i32;
                    }
                }
                0
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add {}: {}", name, e)))?;
    Ok(())
}

/// Add host functions to the linker.
fn add_host_functions(linker: &mut Linker<PluginState>) -> Result<(), WasmError> {
    // host_set_output - always available
    linker
        .func_wrap(
            "barbacane",
            "host_set_output",
            |mut caller: Caller<'_, PluginState>, ptr: i32, len: i32| {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return,
                };

                let start = ptr as usize;
                let end = start + len as usize;
                let data = memory.data(&caller);

                if end <= data.len() {
                    let bytes = data[start..end].to_vec();
                    caller.data_mut().output_buffer = bytes;
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_set_output: {}", e)))?;

    // host_log
    linker
        .func_wrap(
            "barbacane",
            "host_log",
            |mut caller: Caller<'_, PluginState>, level: i32, msg_ptr: i32, msg_len: i32| {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return,
                };

                let start = msg_ptr as usize;
                let end = start + msg_len as usize;
                let data = memory.data(&caller);

                if end <= data.len() {
                    if let Ok(message) = std::str::from_utf8(&data[start..end]) {
                        let plugin_name = caller.data().plugin_name.clone();
                        match level {
                            0 => tracing::error!(plugin = %plugin_name, "{}", message),
                            1 => tracing::warn!(plugin = %plugin_name, "{}", message),
                            2 => tracing::info!(plugin = %plugin_name, "{}", message),
                            _ => tracing::debug!(plugin = %plugin_name, "{}", message),
                        }
                    }
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_log: {}", e)))?;

    // host_context_get
    linker
        .func_wrap(
            "barbacane",
            "host_context_get",
            |mut caller: Caller<'_, PluginState>, key_ptr: i32, key_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let start = key_ptr as usize;
                let end = start + key_len as usize;
                let data = memory.data(&caller);

                if end > data.len() {
                    return -1;
                }

                let key = match std::str::from_utf8(&data[start..end]) {
                    Ok(k) => k.to_string(),
                    Err(_) => return -1,
                };

                match caller.data().context.values.get(&key).cloned() {
                    Some(value) => {
                        let len = value.len() as i32;
                        caller.data_mut().context.last_get_result = Some(value);
                        len
                    }
                    None => -1,
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_context_get: {}", e)))?;

    // host_context_read_result
    add_read_result_fn(linker, "host_context_read_result", |state| {
        state.context.last_get_result.take().map(String::into_bytes)
    })?;

    // host_context_set
    linker
        .func_wrap(
            "barbacane",
            "host_context_set",
            |mut caller: Caller<'_, PluginState>,
             key_ptr: i32,
             key_len: i32,
             val_ptr: i32,
             val_len: i32| {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return,
                };

                let key_start = key_ptr as usize;
                let key_end = key_start + key_len as usize;
                let val_start = val_ptr as usize;
                let val_end = val_start + val_len as usize;

                // Read data first, then mutate
                let data = memory.data(&caller);
                if key_end <= data.len() && val_end <= data.len() {
                    let key_result =
                        std::str::from_utf8(&data[key_start..key_end]).map(String::from);
                    let val_result =
                        std::str::from_utf8(&data[val_start..val_end]).map(String::from);

                    if let (Ok(key), Ok(value)) = (key_result, val_result) {
                        caller.data_mut().context.values.insert(key, value);
                    }
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_context_set: {}", e)))?;

    // host_clock_now
    linker
        .func_wrap(
            "barbacane",
            "host_clock_now",
            |_caller: Caller<'_, PluginState>| -> i64 {
                use std::time::Instant;

                static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
                let start = START.get_or_init(Instant::now);

                start.elapsed().as_millis() as i64
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_clock_now: {}", e)))?;

    // host_time_now - alias for host_clock_now (deprecated, use host_clock_now)
    linker
        .func_wrap(
            "barbacane",
            "host_time_now",
            |_caller: Caller<'_, PluginState>| -> i64 {
                use std::time::Instant;

                static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
                let start = START.get_or_init(Instant::now);

                start.elapsed().as_millis() as i64
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_time_now: {}", e)))?;

    // host_get_unix_timestamp - returns current Unix timestamp in seconds
    linker
        .func_wrap(
            "barbacane",
            "host_get_unix_timestamp",
            |_caller: Caller<'_, PluginState>| -> u64 {
                use std::time::{SystemTime, UNIX_EPOCH};

                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_get_unix_timestamp: {}", e))
        })?;

    // host_uuid_generate - generates UUID v7 and returns length
    linker
        .func_wrap(
            "barbacane",
            "host_uuid_generate",
            |mut caller: Caller<'_, PluginState>| -> i32 {
                let uuid = uuid::Uuid::now_v7().to_string();
                let len = uuid.len() as i32;
                caller.data_mut().last_uuid_result = Some(uuid.into_bytes());
                len
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_uuid_generate: {}", e))
        })?;

    // host_uuid_read_result - copies generated UUID to WASM memory
    add_read_result_fn(linker, "host_uuid_read_result", |state| {
        state.last_uuid_result.take()
    })?;

    // host_http_call - make outbound HTTP request
    linker
        .func_wrap(
            "barbacane",
            "host_http_call",
            |mut caller: Caller<'_, PluginState>, req_ptr: i32, req_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let start = req_ptr as usize;
                let end = start + req_len as usize;
                let data = memory.data(&caller);

                if end > data.len() {
                    return -1;
                }

                // Parse the request JSON from plugin format
                let plugin_request: PluginHttpRequest =
                    match serde_json::from_slice(&data[start..end]) {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::error!("failed to parse HTTP request: {}", e);
                            return -1;
                        }
                    };

                // Convert to HttpClientRequest format
                let request = HttpClientRequest {
                    method: plugin_request.method,
                    url: plugin_request.url,
                    headers: plugin_request.headers.into_iter().collect(),
                    body: plugin_request.body.map(|s| s.into_bytes()),
                    timeout: plugin_request
                        .timeout_ms
                        .map(std::time::Duration::from_millis),
                };

                // Get the HTTP client
                let http_client = match caller.data().http_client.clone() {
                    Some(c) => c,
                    None => {
                        tracing::error!("HTTP client not available");
                        return -1;
                    }
                };

                // Use a separate runtime to avoid deadlock with the main runtime.
                // The main runtime is blocked waiting for the WASM call to complete,
                // so we can't schedule work on it. Create a new runtime just for this call.
                // TODO: Optimize by using a thread-local runtime or worker pool instead of
                // creating a new runtime per call (performance improvement for high throughput).
                let response_json = std::thread::scope(|s| {
                    let handle = s.spawn(|| {
                        let rt = match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                tracing::error!("failed to create runtime: {}", e);
                                return None;
                            }
                        };

                        rt.block_on(async {
                            match http_client.call(request).await {
                                Ok(response) => serde_json::to_vec(&response).ok(),
                                Err(e) => {
                                    tracing::error!("HTTP call failed: {}", e);
                                    // Return error response
                                    let error_response = match e {
                                        crate::http_client::HttpClientError::Timeout => {
                                            HttpClientResponse::error(
                                                504,
                                                "urn:barbacane:error:upstream-timeout",
                                                "Gateway Timeout",
                                                "Upstream request timed out",
                                            )
                                        }
                                        crate::http_client::HttpClientError::CircuitOpen(host) => {
                                            HttpClientResponse::error(
                                                503,
                                                "urn:barbacane:error:circuit-open",
                                                "Service Unavailable",
                                                &format!("Circuit breaker open for {}", host),
                                            )
                                        }
                                        crate::http_client::HttpClientError::ConnectionFailed(
                                            _,
                                        ) => HttpClientResponse::error(
                                            502,
                                            "urn:barbacane:error:upstream-unavailable",
                                            "Bad Gateway",
                                            "Failed to connect to upstream",
                                        ),
                                        _ => HttpClientResponse::error(
                                            502,
                                            "urn:barbacane:error:upstream-unavailable",
                                            "Bad Gateway",
                                            &e.to_string(),
                                        ),
                                    };
                                    serde_json::to_vec(&error_response).ok()
                                }
                            }
                        })
                    });

                    match handle.join() {
                        Ok(result) => result,
                        Err(e) => {
                            tracing::error!("worker thread panicked: {:?}", e);
                            None
                        }
                    }
                });

                match response_json {
                    Some(json) => {
                        let len = json.len() as i32;
                        caller.data_mut().last_http_result = Some(json);
                        len
                    }
                    None => -1,
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_http_call: {}", e)))?;

    // host_http_read_result - read HTTP response
    add_read_result_fn(linker, "host_http_read_result", |state| {
        state.last_http_result.take()
    })?;

    // host_get_secret - get a secret by reference
    linker
        .func_wrap(
            "barbacane",
            "host_get_secret",
            |mut caller: Caller<'_, PluginState>, ref_ptr: i32, ref_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let start = ref_ptr as usize;
                let end = start + ref_len as usize;
                let data = memory.data(&caller);

                if end > data.len() {
                    return -1;
                }

                // Read the secret reference from plugin memory
                let secret_ref = match std::str::from_utf8(&data[start..end]) {
                    Ok(r) => r.to_string(),
                    Err(_) => return -1,
                };

                // Look up in secrets store
                match caller.data().secrets.get(&secret_ref) {
                    Some(value) => {
                        let bytes = value.as_bytes().to_vec();
                        let len = bytes.len() as i32;
                        caller.data_mut().last_secret_result = Some(bytes);
                        len
                    }
                    None => {
                        tracing::warn!(
                            plugin = %caller.data().plugin_name,
                            reference = %secret_ref,
                            "secret not found in store"
                        );
                        -1
                    }
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_get_secret: {}", e)))?;

    // host_secret_read_result - read secret value into plugin memory
    add_read_result_fn(linker, "host_secret_read_result", |state| {
        state.last_secret_result.take()
    })?;

    // host_rate_limit_check - check rate limit for a key
    linker
        .func_wrap(
            "barbacane",
            "host_rate_limit_check",
            |mut caller: Caller<'_, PluginState>,
             key_ptr: i32,
             key_len: i32,
             quota: u32,
             window_secs: u32|
             -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let start = key_ptr as usize;
                let end = start + key_len as usize;
                let data = memory.data(&caller);

                if end > data.len() {
                    return -1;
                }

                // Read the partition key from plugin memory
                let key = match std::str::from_utf8(&data[start..end]) {
                    Ok(k) => k.to_string(),
                    Err(_) => return -1,
                };

                // Get the rate limiter
                let rate_limiter = match &caller.data().rate_limiter {
                    Some(rl) => rl.clone(),
                    None => {
                        tracing::error!("rate limiter not available");
                        return -1;
                    }
                };

                // Check the rate limit
                let result = rate_limiter.check(&key, quota, window_secs as u64);

                // Serialize the result
                match serde_json::to_vec(&result) {
                    Ok(json) => {
                        let len = json.len() as i32;
                        caller.data_mut().last_rate_limit_result = Some(json);
                        len
                    }
                    Err(e) => {
                        tracing::error!("failed to serialize rate limit result: {}", e);
                        -1
                    }
                }
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_rate_limit_check: {}", e))
        })?;

    // host_rate_limit_read_result - read rate limit result into plugin memory
    add_read_result_fn(linker, "host_rate_limit_read_result", |state| {
        state.last_rate_limit_result.take()
    })?;

    // host_cache_get - look up a cached response
    linker
        .func_wrap(
            "barbacane",
            "host_cache_get",
            |mut caller: Caller<'_, PluginState>, key_ptr: i32, key_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let start = key_ptr as usize;
                let end = start + key_len as usize;
                let data = memory.data(&caller);

                if end > data.len() {
                    return -1;
                }

                // Read the cache key from plugin memory
                let key = match std::str::from_utf8(&data[start..end]) {
                    Ok(k) => k.to_string(),
                    Err(_) => return -1,
                };

                // Get the response cache
                let cache = match &caller.data().response_cache {
                    Some(c) => c.clone(),
                    None => {
                        tracing::error!("response cache not available");
                        return -1;
                    }
                };

                // Check the cache
                let result = cache.get(&key);

                // Serialize the result
                match serde_json::to_vec(&result) {
                    Ok(json) => {
                        let len = json.len() as i32;
                        caller.data_mut().last_cache_result = Some(json);
                        len
                    }
                    Err(e) => {
                        tracing::error!("failed to serialize cache result: {}", e);
                        -1
                    }
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_cache_get: {}", e)))?;

    // host_cache_set - store a response in the cache
    linker
        .func_wrap(
            "barbacane",
            "host_cache_set",
            |mut caller: Caller<'_, PluginState>,
             key_ptr: i32,
             key_len: i32,
             entry_ptr: i32,
             entry_len: i32,
             ttl_secs: u32|
             -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let key_start = key_ptr as usize;
                let key_end = key_start + key_len as usize;
                let entry_start = entry_ptr as usize;
                let entry_end = entry_start + entry_len as usize;
                let data = memory.data(&caller);

                if key_end > data.len() || entry_end > data.len() {
                    return -1;
                }

                // Read the cache key
                let key = match std::str::from_utf8(&data[key_start..key_end]) {
                    Ok(k) => k.to_string(),
                    Err(_) => return -1,
                };

                // Parse the cache entry JSON
                let entry: crate::cache::CacheEntry =
                    match serde_json::from_slice(&data[entry_start..entry_end]) {
                        Ok(e) => e,
                        Err(e) => {
                            tracing::error!("failed to parse cache entry: {}", e);
                            return -1;
                        }
                    };

                // Get the response cache
                let cache = match &caller.data().response_cache {
                    Some(c) => c.clone(),
                    None => {
                        tracing::error!("response cache not available");
                        return -1;
                    }
                };

                // Store in cache
                cache.set(&key, entry, ttl_secs as u64);
                0 // Success
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_cache_set: {}", e)))?;

    // host_cache_read_result - read cache lookup result into plugin memory
    add_read_result_fn(linker, "host_cache_read_result", |state| {
        state.last_cache_result.take()
    })?;

    // === Telemetry Host Functions ===

    // host_metric_counter_inc - increment a plugin counter metric
    linker
        .func_wrap(
            "barbacane",
            "host_metric_counter_inc",
            |mut caller: Caller<'_, PluginState>,
             name_ptr: i32,
             name_len: i32,
             labels_ptr: i32,
             labels_len: i32,
             value: f64| {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return,
                };

                let data = memory.data(&caller);
                let name_start = name_ptr as usize;
                let name_end = name_start + name_len as usize;
                let labels_start = labels_ptr as usize;
                let labels_end = labels_start + labels_len as usize;

                if name_end > data.len() || labels_end > data.len() {
                    return;
                }

                let name = match std::str::from_utf8(&data[name_start..name_end]) {
                    Ok(n) => n.to_string(),
                    Err(_) => return,
                };

                let labels_json = match std::str::from_utf8(&data[labels_start..labels_end]) {
                    Ok(l) => l.to_string(),
                    Err(_) => return,
                };

                let plugin_name = caller.data().plugin_name.clone();
                if let Some(metrics) = &caller.data().metrics {
                    metrics.plugin_counter_inc(&plugin_name, &name, &labels_json, value as u64);
                }
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_metric_counter_inc: {}", e))
        })?;

    // host_metric_histogram_observe - observe a plugin histogram metric
    linker
        .func_wrap(
            "barbacane",
            "host_metric_histogram_observe",
            |mut caller: Caller<'_, PluginState>,
             name_ptr: i32,
             name_len: i32,
             labels_ptr: i32,
             labels_len: i32,
             value: f64| {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return,
                };

                let data = memory.data(&caller);
                let name_start = name_ptr as usize;
                let name_end = name_start + name_len as usize;
                let labels_start = labels_ptr as usize;
                let labels_end = labels_start + labels_len as usize;

                if name_end > data.len() || labels_end > data.len() {
                    return;
                }

                let name = match std::str::from_utf8(&data[name_start..name_end]) {
                    Ok(n) => n.to_string(),
                    Err(_) => return,
                };

                let labels_json = match std::str::from_utf8(&data[labels_start..labels_end]) {
                    Ok(l) => l.to_string(),
                    Err(_) => return,
                };

                let plugin_name = caller.data().plugin_name.clone();
                if let Some(metrics) = &caller.data().metrics {
                    metrics.plugin_histogram_observe(&plugin_name, &name, &labels_json, value);
                }
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!(
                "failed to add host_metric_histogram_observe: {}",
                e
            ))
        })?;

    // host_span_start - start a child span (stub - returns span ID)
    // Full implementation requires passing span context through RequestContext
    linker
        .func_wrap(
            "barbacane",
            "host_span_start",
            |mut caller: Caller<'_, PluginState>, name_ptr: i32, name_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let data = memory.data(&caller);
                let start = name_ptr as usize;
                let end = start + name_len as usize;

                if end > data.len() {
                    return -1;
                }

                let span_name = match std::str::from_utf8(&data[start..end]) {
                    Ok(n) => n,
                    Err(_) => return -1,
                };

                // Log the span start for now (full tracing integration in Phase 9)
                let plugin_name = &caller.data().plugin_name;
                tracing::debug!(plugin = %plugin_name, span = %span_name, "plugin span started");

                // Return a placeholder span ID
                1
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_span_start: {}", e)))?;

    // host_span_end - end the current span
    linker
        .func_wrap(
            "barbacane",
            "host_span_end",
            |caller: Caller<'_, PluginState>| {
                let plugin_name = &caller.data().plugin_name;
                tracing::debug!(plugin = %plugin_name, "plugin span ended");
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_span_end: {}", e)))?;

    // host_span_set_attribute - set an attribute on the current span
    linker
        .func_wrap(
            "barbacane",
            "host_span_set_attribute",
            |mut caller: Caller<'_, PluginState>,
             key_ptr: i32,
             key_len: i32,
             val_ptr: i32,
             val_len: i32| {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return,
                };

                let data = memory.data(&caller);
                let key_start = key_ptr as usize;
                let key_end = key_start + key_len as usize;
                let val_start = val_ptr as usize;
                let val_end = val_start + val_len as usize;

                if key_end > data.len() || val_end > data.len() {
                    return;
                }

                let key = match std::str::from_utf8(&data[key_start..key_end]) {
                    Ok(k) => k,
                    Err(_) => return,
                };

                let value = match std::str::from_utf8(&data[val_start..val_end]) {
                    Ok(v) => v,
                    Err(_) => return,
                };

                let plugin_name = &caller.data().plugin_name;
                tracing::debug!(plugin = %plugin_name, %key, %value, "plugin span attribute set");
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_span_set_attribute: {}", e))
        })?;

    // === Broker Host Functions (M10) ===

    // host_kafka_publish - publish a message to Kafka
    linker
        .func_wrap(
            "barbacane",
            "host_kafka_publish",
            |mut caller: Caller<'_, PluginState>, msg_ptr: i32, msg_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let start = msg_ptr as usize;
                let end = start + msg_len as usize;
                let data = memory.data(&caller);

                if end > data.len() {
                    return -1;
                }

                // Parse the broker message from plugin memory
                let message: BrokerMessage = match serde_json::from_slice(&data[start..end]) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!("failed to parse broker message: {}", e);
                        return -1;
                    }
                };

                // Get the broker registry
                let brokers = match &caller.data().brokers {
                    Some(b) => b.clone(),
                    None => {
                        tracing::error!("broker registry not available");
                        return -1;
                    }
                };

                // Publish to Kafka
                let result = brokers.publish_kafka(message);

                // Serialize the result
                let result_json = match result {
                    Ok(r) => serde_json::to_vec(&r),
                    Err(e) => {
                        let error_result = crate::broker::PublishResult::failure(
                            "unknown".to_string(),
                            e.to_string(),
                        );
                        serde_json::to_vec(&error_result)
                    }
                };

                match result_json {
                    Ok(json) => {
                        let len = json.len() as i32;
                        caller.data_mut().last_broker_result = Some(json);
                        len
                    }
                    Err(e) => {
                        tracing::error!("failed to serialize broker result: {}", e);
                        -1
                    }
                }
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_kafka_publish: {}", e))
        })?;

    // host_nats_publish - publish a message to NATS
    linker
        .func_wrap(
            "barbacane",
            "host_nats_publish",
            |mut caller: Caller<'_, PluginState>, msg_ptr: i32, msg_len: i32| -> i32 {
                let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                    Some(m) => m,
                    None => return -1,
                };

                let start = msg_ptr as usize;
                let end = start + msg_len as usize;
                let data = memory.data(&caller);

                if end > data.len() {
                    return -1;
                }

                // Parse the broker message from plugin memory
                let message: BrokerMessage = match serde_json::from_slice(&data[start..end]) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::error!("failed to parse broker message: {}", e);
                        return -1;
                    }
                };

                // Get the broker registry
                let brokers = match &caller.data().brokers {
                    Some(b) => b.clone(),
                    None => {
                        tracing::error!("broker registry not available");
                        return -1;
                    }
                };

                // Publish to NATS
                let result = brokers.publish_nats(message);

                // Serialize the result
                let result_json = match result {
                    Ok(r) => serde_json::to_vec(&r),
                    Err(e) => {
                        let error_result = crate::broker::PublishResult::failure(
                            "unknown".to_string(),
                            e.to_string(),
                        );
                        serde_json::to_vec(&error_result)
                    }
                };

                match result_json {
                    Ok(json) => {
                        let len = json.len() as i32;
                        caller.data_mut().last_broker_result = Some(json);
                        len
                    }
                    Err(e) => {
                        tracing::error!("failed to serialize broker result: {}", e);
                        -1
                    }
                }
            },
        )
        .map_err(|e| WasmError::Instantiation(format!("failed to add host_nats_publish: {}", e)))?;

    // host_broker_read_result - read broker publish result into plugin memory
    add_read_result_fn(linker, "host_broker_read_result", |state| {
        state.last_broker_result.take()
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_context_new() {
        let ctx = RequestContext::new("trace-123".into(), "req-456".into());
        assert_eq!(ctx.trace_id, "trace-123");
        assert_eq!(ctx.request_id, "req-456");
        assert!(ctx.values.is_empty());
    }

    #[test]
    fn plugin_state_take_output() {
        let limits = PluginLimits::default();
        let mut state = PluginState::new("test".into(), &limits);
        state.output_buffer = vec![1, 2, 3];

        let output = state.take_output();
        assert_eq!(output, vec![1, 2, 3]);
        assert!(state.output_buffer.is_empty());
    }

    #[test]
    fn plugin_state_uuid_result_initialized() {
        let limits = PluginLimits::default();
        let state = PluginState::new("test".into(), &limits);
        assert!(state.last_uuid_result.is_none());
    }

    #[test]
    fn uuid_v7_format() {
        // Test that UUID v7 generates valid format
        let uuid = uuid::Uuid::now_v7().to_string();
        assert_eq!(uuid.len(), 36); // UUID string format: 8-4-4-4-12
        assert!(uuid.chars().nth(14) == Some('7')); // Version 7 marker
    }
}
