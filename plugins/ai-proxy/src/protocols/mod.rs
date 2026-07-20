//! Per-protocol translation adapters.
//!
//! Each protocol module exposes a `handle(plugin, target, req, streaming)`
//! function that adapts the inbound client format to the resolved upstream
//! provider. Targets, fallback, metrics, and context propagation live one
//! layer up in [`crate::dispatch`] so they're shared across protocols.

pub mod chat_completion;
pub mod models;
pub mod responses;
pub mod tools;
