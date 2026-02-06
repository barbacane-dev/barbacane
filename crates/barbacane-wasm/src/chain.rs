//! Middleware chain execution.
//!
//! Per SPEC-003 section 5, middlewares execute in order for on_request,
//! and reverse order for on_response. A middleware returning 1 from
//! on_request short-circuits the chain with an immediate response.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::WasmError;
use crate::instance::{PluginInstance, RequestContext};
use crate::trap::{TrapContext, TrapResult};

/// Callback for recording middleware metrics.
/// Parameters: middleware_name, phase ("request" or "response"), duration_secs, short_circuit
pub type MetricsCallback<'a> = Option<&'a dyn Fn(&str, &str, f64, bool)>;

/// The result of executing on_request on a single middleware.
#[derive(Debug)]
pub enum OnRequestResult {
    /// Continue to the next middleware with possibly modified request.
    Continue(Vec<u8>),
    /// Short-circuit with an immediate response.
    ShortCircuit(Vec<u8>),
}

/// Output format from middleware on_request.
///
/// The plugin sets output as JSON with this structure.
#[derive(Debug, Serialize, Deserialize)]
struct MiddlewareOutput {
    /// 0 = continue, 1 = short-circuit
    action: i32,
    /// The request (if continue) or response (if short-circuit)
    data: serde_json::Value,
}

/// A configured middleware in the chain.
#[derive(Debug, Clone)]
pub struct MiddlewareConfig {
    /// Plugin name.
    pub name: String,
    /// Plugin config as JSON.
    pub config: serde_json::Value,
}

impl MiddlewareConfig {
    /// Create a new middleware config.
    pub fn new(name: impl Into<String>, config: serde_json::Value) -> Self {
        Self {
            name: name.into(),
            config,
        }
    }
}

/// A middleware chain that executes multiple middlewares in sequence.
pub struct MiddlewareChain {
    /// Middleware configs in order of execution.
    configs: Vec<MiddlewareConfig>,
}

impl MiddlewareChain {
    /// Create a new empty middleware chain.
    pub fn new() -> Self {
        Self {
            configs: Vec::new(),
        }
    }

    /// Create a chain from a list of middleware configs.
    pub fn from_configs(configs: Vec<MiddlewareConfig>) -> Self {
        Self { configs }
    }

    /// Add a middleware to the chain.
    pub fn push(&mut self, config: MiddlewareConfig) {
        self.configs.push(config);
    }

    /// Get the number of middlewares in the chain.
    pub fn len(&self) -> usize {
        self.configs.len()
    }

    /// Check if the chain is empty.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// Get the middleware configs.
    pub fn configs(&self) -> &[MiddlewareConfig] {
        &self.configs
    }
}

impl Default for MiddlewareChain {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of executing the full request chain.
#[derive(Debug)]
pub enum ChainResult {
    /// Chain completed, continue to dispatch.
    Continue {
        /// The final request after all middlewares.
        request: Vec<u8>,
        /// Context to pass to on_response chain.
        context: RequestContext,
    },
    /// Chain short-circuited with a response.
    ShortCircuit {
        /// The response from the short-circuiting middleware.
        response: Vec<u8>,
        /// Index of the middleware that short-circuited.
        middleware_index: usize,
        /// Context for response chain (partial).
        context: RequestContext,
    },
    /// Chain failed with an error.
    Error {
        /// The error that occurred.
        error: WasmError,
        /// The trap result for error handling.
        trap_result: TrapResult,
    },
}

/// Execute the on_request chain.
///
/// Processes middlewares in order. If any middleware returns 1 (short-circuit),
/// stops and returns the response. If all middlewares return 0 (continue),
/// returns the final request for dispatch.
pub fn execute_on_request(
    instances: &mut [PluginInstance],
    initial_request: &[u8],
    context: RequestContext,
) -> ChainResult {
    execute_on_request_with_metrics(instances, initial_request, context, None)
}

/// Execute the on_request chain with optional metrics recording.
pub fn execute_on_request_with_metrics(
    instances: &mut [PluginInstance],
    initial_request: &[u8],
    context: RequestContext,
    metrics_callback: MetricsCallback<'_>,
) -> ChainResult {
    let mut current_request = initial_request.to_vec();
    let mut current_context = context;

    for (index, instance) in instances.iter_mut().enumerate() {
        // Set context for this middleware
        instance.set_context(current_context.clone());

        // Record start time
        let start = Instant::now();
        let middleware_name = instance.name().to_string();

        // Call on_request
        match instance.on_request(&current_request) {
            Ok(result_code) => {
                let output = instance.take_output();

                // Parse the output to determine action
                match parse_middleware_output(&output, result_code) {
                    Ok(OnRequestResult::Continue(new_request)) => {
                        // Record metrics (not a short-circuit)
                        if let Some(callback) = metrics_callback {
                            callback(
                                &middleware_name,
                                "request",
                                start.elapsed().as_secs_f64(),
                                false,
                            );
                        }
                        current_request = new_request;
                        // Get context modifications from the middleware
                        current_context = instance.get_context();
                    }
                    Ok(OnRequestResult::ShortCircuit(response)) => {
                        // Record metrics (short-circuit)
                        if let Some(callback) = metrics_callback {
                            callback(
                                &middleware_name,
                                "request",
                                start.elapsed().as_secs_f64(),
                                true,
                            );
                        }
                        // Get context modifications before short-circuit
                        let final_context = instance.get_context();
                        return ChainResult::ShortCircuit {
                            response,
                            middleware_index: index,
                            context: final_context,
                        };
                    }
                    Err(e) => {
                        // Record metrics for error case
                        if let Some(callback) = metrics_callback {
                            callback(
                                &middleware_name,
                                "request",
                                start.elapsed().as_secs_f64(),
                                false,
                            );
                        }
                        return ChainResult::Error {
                            trap_result: TrapResult::from_error(&e, TrapContext::OnRequest),
                            error: e,
                        };
                    }
                }
            }
            Err(e) => {
                // Record metrics for error case
                if let Some(callback) = metrics_callback {
                    callback(
                        &middleware_name,
                        "request",
                        start.elapsed().as_secs_f64(),
                        false,
                    );
                }
                return ChainResult::Error {
                    trap_result: TrapResult::from_error(&e, TrapContext::OnRequest),
                    error: e,
                };
            }
        }
    }

    ChainResult::Continue {
        request: current_request,
        context: current_context,
    }
}

/// Execute the on_response chain.
///
/// Processes middlewares in reverse order. If any middleware fails,
/// logs the error and continues with the original response (fault-tolerant).
pub fn execute_on_response(
    instances: &mut [PluginInstance],
    initial_response: &[u8],
    context: RequestContext,
) -> Vec<u8> {
    execute_on_response_with_metrics(instances, initial_response, context, None)
}

/// Execute the on_response chain with optional metrics recording.
pub fn execute_on_response_with_metrics(
    instances: &mut [PluginInstance],
    initial_response: &[u8],
    context: RequestContext,
    metrics_callback: MetricsCallback<'_>,
) -> Vec<u8> {
    let mut current_response = initial_response.to_vec();

    // Process in reverse order
    for instance in instances.iter_mut().rev() {
        instance.set_context(context.clone());

        // Record start time
        let start = Instant::now();
        let middleware_name = instance.name().to_string();

        match instance.on_response(&current_response) {
            Ok(_result_code) => {
                // Record metrics
                if let Some(callback) = metrics_callback {
                    callback(
                        &middleware_name,
                        "response",
                        start.elapsed().as_secs_f64(),
                        false,
                    );
                }
                let output = instance.take_output();
                if !output.is_empty() {
                    current_response = output;
                }
            }
            Err(e) => {
                // Record metrics for error case
                if let Some(callback) = metrics_callback {
                    callback(
                        &middleware_name,
                        "response",
                        start.elapsed().as_secs_f64(),
                        false,
                    );
                }
                // Fault-tolerant: log and continue with current response
                let trap_result = TrapResult::from_error(&e, TrapContext::OnResponse);
                tracing::warn!(
                    error = %trap_result.message(),
                    "Middleware on_response failed, continuing with original response"
                );
            }
        }
    }

    current_response
}

/// Execute on_response for a partial chain (after short-circuit).
///
/// Only processes middlewares up to (but not including) the short-circuiting one.
pub fn execute_on_response_partial(
    instances: &mut [PluginInstance],
    response: &[u8],
    short_circuit_index: usize,
    context: RequestContext,
) -> Vec<u8> {
    if short_circuit_index == 0 {
        return response.to_vec();
    }

    let partial_instances = &mut instances[..short_circuit_index];
    execute_on_response(partial_instances, response, context)
}

/// Parse middleware output to determine the action.
fn parse_middleware_output(output: &[u8], result_code: i32) -> Result<OnRequestResult, WasmError> {
    // If no output, use result code as simple continue/short-circuit
    if output.is_empty() {
        return if result_code == 0 {
            Ok(OnRequestResult::Continue(Vec::new()))
        } else {
            Err(WasmError::InitFailed(
                "middleware returned short-circuit without output".into(),
            ))
        };
    }

    // Try to parse as MiddlewareOutput JSON
    match serde_json::from_slice::<MiddlewareOutput>(output) {
        Ok(parsed) => {
            let data = serde_json::to_vec(&parsed.data)
                .map_err(|e| WasmError::InitFailed(format!("failed to serialize output: {}", e)))?;

            if parsed.action == 0 || result_code == 0 {
                Ok(OnRequestResult::Continue(data))
            } else {
                Ok(OnRequestResult::ShortCircuit(data))
            }
        }
        Err(_) => {
            // If not structured output, use raw output with result code
            if result_code == 0 {
                Ok(OnRequestResult::Continue(output.to_vec()))
            } else {
                Ok(OnRequestResult::ShortCircuit(output.to_vec()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn middleware_config_new() {
        let config = MiddlewareConfig::new("rate-limit", json!({"quota": 100}));
        assert_eq!(config.name, "rate-limit");
        assert_eq!(config.config["quota"], 100);
    }

    #[test]
    fn chain_new_is_empty() {
        let chain = MiddlewareChain::new();
        assert!(chain.is_empty());
        assert_eq!(chain.len(), 0);
    }

    #[test]
    fn chain_push() {
        let mut chain = MiddlewareChain::new();
        chain.push(MiddlewareConfig::new("auth", json!({})));
        chain.push(MiddlewareConfig::new("rate-limit", json!({})));

        assert_eq!(chain.len(), 2);
        assert_eq!(chain.configs()[0].name, "auth");
        assert_eq!(chain.configs()[1].name, "rate-limit");
    }

    #[test]
    fn chain_from_configs() {
        let configs = vec![
            MiddlewareConfig::new("auth", json!({})),
            MiddlewareConfig::new("cors", json!({})),
        ];
        let chain = MiddlewareChain::from_configs(configs);

        assert_eq!(chain.len(), 2);
    }

    #[test]
    fn parse_continue_output() {
        let output = serde_json::to_vec(&json!({
            "action": 0,
            "data": {"method": "GET", "path": "/api"}
        }))
        .unwrap();

        let result = parse_middleware_output(&output, 0).unwrap();
        assert!(matches!(result, OnRequestResult::Continue(_)));
    }

    #[test]
    fn parse_short_circuit_output() {
        let output = serde_json::to_vec(&json!({
            "action": 1,
            "data": {"status": 401, "body": "Unauthorized"}
        }))
        .unwrap();

        let result = parse_middleware_output(&output, 1).unwrap();
        assert!(matches!(result, OnRequestResult::ShortCircuit(_)));
    }

    #[test]
    fn parse_raw_output_continue() {
        let output = b"raw request data";
        let result = parse_middleware_output(output, 0).unwrap();
        assert!(matches!(result, OnRequestResult::Continue(_)));
    }

    #[test]
    fn parse_raw_output_short_circuit() {
        let output = b"error response";
        let result = parse_middleware_output(output, 1).unwrap();
        assert!(matches!(result, OnRequestResult::ShortCircuit(_)));
    }

    #[test]
    fn parse_empty_continue() {
        let result = parse_middleware_output(&[], 0).unwrap();
        assert!(matches!(result, OnRequestResult::Continue(data) if data.is_empty()));
    }

    #[test]
    fn parse_empty_short_circuit_fails() {
        let result = parse_middleware_output(&[], 1);
        assert!(result.is_err());
    }

    #[test]
    fn metrics_callback_type_accepts_closure() {
        use std::cell::RefCell;
        use std::rc::Rc;

        // Verify the callback type works with a recording closure
        let invocations = Rc::new(RefCell::new(Vec::new()));
        let invocations_clone = invocations.clone();

        let callback = move |name: &str, phase: &str, duration: f64, short_circuit: bool| {
            invocations_clone.borrow_mut().push((
                name.to_string(),
                phase.to_string(),
                duration,
                short_circuit,
            ));
        };

        // Verify the callback can be used as MetricsCallback
        let metrics_callback: MetricsCallback<'_> = Some(&callback);
        assert!(metrics_callback.is_some());

        // Invoke the callback
        if let Some(cb) = metrics_callback {
            cb("test-middleware", "request", 0.001, false);
            cb("test-middleware", "response", 0.002, true);
        }

        // Verify invocations were recorded
        let recorded = invocations.borrow();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].0, "test-middleware");
        assert_eq!(recorded[0].1, "request");
        assert!(!recorded[0].3); // not short-circuit
        assert_eq!(recorded[1].1, "response");
        assert!(recorded[1].3); // short-circuit
    }

    #[test]
    fn execute_on_request_empty_instances_returns_continue() {
        let mut instances: Vec<PluginInstance> = vec![];
        let request = b"test request";
        let context = RequestContext::default();

        let result = execute_on_request(&mut instances, request, context);
        assert!(matches!(result, ChainResult::Continue { .. }));

        if let ChainResult::Continue {
            request: req,
            context: _,
        } = result
        {
            assert_eq!(req, request.to_vec());
        }
    }

    #[test]
    fn execute_on_response_empty_instances_returns_input() {
        let mut instances: Vec<PluginInstance> = vec![];
        let response = b"test response";
        let context = RequestContext::default();

        let result = execute_on_response(&mut instances, response, context);
        assert_eq!(result, response.to_vec());
    }

    #[test]
    fn execute_with_metrics_none_callback_works() {
        let mut instances: Vec<PluginInstance> = vec![];
        let request = b"test";
        let context = RequestContext::default();

        // Verify None callback doesn't cause issues
        let result =
            execute_on_request_with_metrics(&mut instances, request, context.clone(), None);
        assert!(matches!(result, ChainResult::Continue { .. }));

        let response = execute_on_response_with_metrics(&mut instances, request, context, None);
        assert_eq!(response, request.to_vec());
    }
}
