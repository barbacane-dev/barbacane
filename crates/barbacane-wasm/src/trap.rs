//! WASM trap handling.
//!
//! Per SPEC-003 section 6.4, a trap in any phase fails the request with a 500.

/// The context in which a WASM trap occurred.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrapContext {
    /// During plugin initialization.
    Init,
    /// During request processing (on_request).
    OnRequest,
    /// During response processing (on_response).
    OnResponse,
    /// During dispatch.
    Dispatch,
}

/// A WASM trap and the phase it occurred in.
#[derive(Debug)]
pub struct TrapResult {
    message: String,
    context: TrapContext,
}

impl TrapResult {
    /// Create a trap result from an error and context.
    pub fn from_error<E: std::fmt::Display>(error: E, context: TrapContext) -> Self {
        TrapResult {
            message: error.to_string(),
            context,
        }
    }

    /// The phase the trap occurred in.
    pub fn context(&self) -> TrapContext {
        self.context
    }

    /// Get the error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trap_result_keeps_its_message_and_phase() {
        let result = TrapResult::from_error("response failed", TrapContext::OnResponse);
        assert_eq!(result.message(), "response failed");
        assert_eq!(result.context(), TrapContext::OnResponse);
    }
}
