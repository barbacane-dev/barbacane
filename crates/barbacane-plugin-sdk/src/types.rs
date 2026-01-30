use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An HTTP request as seen by plugins.
///
/// Uses BTreeMap instead of HashMap to avoid WASI random dependency in WASM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub query: Option<String>,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
    pub client_ip: String,
    pub path_params: BTreeMap<String, String>,
}

/// An HTTP response as produced by plugins.
///
/// Uses BTreeMap instead of HashMap to avoid WASI random dependency in WASM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Option<String>,
}

/// The action a middleware returns from `on_request`.
#[derive(Debug, Clone)]
pub enum Action<T> {
    /// Pass the (possibly modified) request to the next middleware.
    Continue(T),
    /// Stop the chain and return this response immediately.
    ShortCircuit(Response),
}
