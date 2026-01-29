//! WASM trap handling and error recovery.
//!
//! Per SPEC-003 section 6.4, error handling differs by phase:
//! - Request phase (on_request, dispatch): traps produce 500
//! - Response phase (on_response): traps are fault-tolerant, log and continue

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

impl TrapContext {
    /// Returns true if this context is fault-tolerant.
    ///
    /// Only the response phase is fault-tolerant; traps during on_response
    /// should log the error and continue with the original response.
    pub fn is_fault_tolerant(&self) -> bool {
        matches!(self, TrapContext::OnResponse)
    }
}

/// The result of handling a WASM trap.
#[derive(Debug)]
pub enum TrapResult {
    /// Fatal error - should return 500 to client.
    Fatal {
        /// Human-readable error message.
        message: String,
        /// The context where the trap occurred.
        context: TrapContext,
    },

    /// Fault-tolerant - log and continue with original response.
    FaultTolerant {
        /// Human-readable error message for logging.
        message: String,
    },
}

impl TrapResult {
    /// Create a trap result from an error and context.
    pub fn from_error<E: std::fmt::Display>(error: E, context: TrapContext) -> Self {
        let message = error.to_string();

        if context.is_fault_tolerant() {
            TrapResult::FaultTolerant { message }
        } else {
            TrapResult::Fatal { message, context }
        }
    }

    /// Returns true if this is a fatal error.
    pub fn is_fatal(&self) -> bool {
        matches!(self, TrapResult::Fatal { .. })
    }

    /// Get the error message.
    pub fn message(&self) -> &str {
        match self {
            TrapResult::Fatal { message, .. } => message,
            TrapResult::FaultTolerant { message } => message,
        }
    }
}

/// Classify a wasmtime trap into a human-readable category.
pub fn classify_trap(trap: &wasmtime::Trap) -> &'static str {
    // wasmtime::Trap is now opaque, so we examine the message
    let msg = trap.to_string();

    if msg.contains("out of fuel") {
        "execution timeout"
    } else if msg.contains("unreachable") {
        "unreachable code executed (likely panic)"
    } else if msg.contains("memory") {
        "memory access error"
    } else if msg.contains("stack overflow") {
        "stack overflow"
    } else if msg.contains("indirect call") {
        "invalid indirect call"
    } else if msg.contains("integer") {
        "integer overflow or division error"
    } else {
        "unknown trap"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_is_fatal() {
        let result = TrapResult::from_error("init failed", TrapContext::Init);
        assert!(result.is_fatal());
    }

    #[test]
    fn on_request_is_fatal() {
        let result = TrapResult::from_error("request failed", TrapContext::OnRequest);
        assert!(result.is_fatal());
    }

    #[test]
    fn dispatch_is_fatal() {
        let result = TrapResult::from_error("dispatch failed", TrapContext::Dispatch);
        assert!(result.is_fatal());
    }

    #[test]
    fn on_response_is_fault_tolerant() {
        let result = TrapResult::from_error("response failed", TrapContext::OnResponse);
        assert!(!result.is_fatal());
    }

    #[test]
    fn trap_context_fault_tolerance() {
        assert!(!TrapContext::Init.is_fault_tolerant());
        assert!(!TrapContext::OnRequest.is_fault_tolerant());
        assert!(!TrapContext::Dispatch.is_fault_tolerant());
        assert!(TrapContext::OnResponse.is_fault_tolerant());
    }
}
