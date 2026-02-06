//! Middleware chain execution.
//!
//! Per SPEC-003 section 5, middlewares execute in order for on_request,
//! and reverse order for on_response. A middleware returning 1 from
//! on_request short-circuits the chain with an immediate response.

use serde::{Deserialize, Serialize};

use crate::error::WasmError;
use crate::instance::{PluginInstance, RequestContext};
use crate::trap::{TrapContext, TrapResult};

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
    let mut current_request = initial_request.to_vec();
    let mut current_context = context;

    for (index, instance) in instances.iter_mut().enumerate() {
        // Set context for this middleware
        instance.set_context(current_context.clone());

        // Call on_request
        match instance.on_request(&current_request) {
            Ok(result_code) => {
                let output = instance.take_output();

                // Parse the output to determine action
                match parse_middleware_output(&output, result_code) {
                    Ok(OnRequestResult::Continue(new_request)) => {
                        current_request = new_request;
                        // Get context modifications from the middleware
                        current_context = instance.get_context();
                    }
                    Ok(OnRequestResult::ShortCircuit(response)) => {
                        // Get context modifications before short-circuit
                        let final_context = instance.get_context();
                        return ChainResult::ShortCircuit {
                            response,
                            middleware_index: index,
                            context: final_context,
                        };
                    }
                    Err(e) => {
                        return ChainResult::Error {
                            trap_result: TrapResult::from_error(&e, TrapContext::OnRequest),
                            error: e,
                        };
                    }
                }
            }
            Err(e) => {
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
    let mut current_response = initial_response.to_vec();

    // Process in reverse order
    for instance in instances.iter_mut().rev() {
        instance.set_context(context.clone());

        match instance.on_response(&current_response) {
            Ok(_result_code) => {
                let output = instance.take_output();
                if !output.is_empty() {
                    current_response = output;
                }
            }
            Err(e) => {
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
}
