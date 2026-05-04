//! Per-provider transport (HTTP request building, auth headers, URL composition).
//!
//! `openai` and `ollama` share the same OpenAI-compatible passthrough; `ollama`
//! is a thin re-export. `anthropic` builds its own request shape and pins the
//! API version. Translation between client and provider formats lives in the
//! protocol layer ([`crate::protocols`]), one above this one.

pub mod anthropic;
pub mod ollama;
pub mod openai;
