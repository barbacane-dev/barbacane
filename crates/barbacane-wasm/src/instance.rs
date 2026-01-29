//! WASM plugin instance management.
//!
//! Each plugin instance wraps a wasmtime Store and Instance with the
//! plugin state required for host function calls.

use std::collections::HashMap;
use std::sync::Arc;

use wasmtime::{Caller, Engine, Instance, Linker, Memory, Store, TypedFunc};

use crate::engine::CompiledModule;
use crate::error::WasmError;
use crate::http_client::{HttpClient, HttpRequest as HttpClientRequest, HttpResponse as HttpClientResponse};
use crate::limits::PluginLimits;

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
        Self::new_with_http_client(engine, module, limits, None)
    }

    /// Create a new plugin instance with an HTTP client for outbound calls.
    pub fn new_with_http_client(
        engine: &Engine,
        module: &CompiledModule,
        limits: PluginLimits,
        http_client: Option<Arc<HttpClient>>,
    ) -> Result<Self, WasmError> {
        let state = match http_client {
            Some(client) => PluginState::with_http_client(module.name.clone(), &limits, client),
            None => PluginState::new(module.name.clone(), &limits),
        };
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
    linker
        .func_wrap(
            "barbacane",
            "host_context_read_result",
            |mut caller: Caller<'_, PluginState>, buf_ptr: i32, buf_len: i32| -> i32 {
                let result = caller.data_mut().context.last_get_result.take();
                if let Some(value) = result {
                    let memory = match caller.get_export("memory").and_then(|e| e.into_memory()) {
                        Some(m) => m,
                        None => return 0,
                    };

                    let bytes = value.as_bytes();
                    let copy_len = std::cmp::min(bytes.len(), buf_len as usize);

                    if memory
                        .write(&mut caller, buf_ptr as usize, &bytes[..copy_len])
                        .is_ok()
                    {
                        return copy_len as i32;
                    }
                }
                0
            },
        )
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_context_read_result: {}", e))
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
                    let key_result = std::str::from_utf8(&data[key_start..key_end]).map(String::from);
                    let val_result = std::str::from_utf8(&data[val_start..val_end]).map(String::from);

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

                // Parse the request JSON
                let request: HttpClientRequest = match serde_json::from_slice(&data[start..end]) {
                    Ok(r) => r,
                    Err(e) => {
                        tracing::error!("failed to parse HTTP request: {}", e);
                        return -1;
                    }
                };

                // Get the HTTP client
                let http_client = match caller.data().http_client.clone() {
                    Some(c) => c,
                    None => {
                        tracing::error!("HTTP client not available");
                        return -1;
                    }
                };

                // Execute the request (blocking)
                let response_json = match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        handle.block_on(async {
                            match http_client.call(request).await {
                                Ok(response) => {
                                    serde_json::to_vec(&response).ok()
                                }
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
                                        crate::http_client::HttpClientError::ConnectionFailed(_) => {
                                            HttpClientResponse::error(
                                                502,
                                                "urn:barbacane:error:upstream-unavailable",
                                                "Bad Gateway",
                                                "Failed to connect to upstream",
                                            )
                                        }
                                        _ => {
                                            HttpClientResponse::error(
                                                502,
                                                "urn:barbacane:error:upstream-unavailable",
                                                "Bad Gateway",
                                                &e.to_string(),
                                            )
                                        }
                                    };
                                    serde_json::to_vec(&error_response).ok()
                                }
                            }
                        })
                    }
                    Err(_) => {
                        tracing::error!("no tokio runtime available");
                        None
                    }
                };

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
    linker
        .func_wrap(
            "barbacane",
            "host_http_read_result",
            |mut caller: Caller<'_, PluginState>, buf_ptr: i32, buf_len: i32| -> i32 {
                let result = caller.data_mut().last_http_result.take();
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
        .map_err(|e| {
            WasmError::Instantiation(format!("failed to add host_http_read_result: {}", e))
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
}
