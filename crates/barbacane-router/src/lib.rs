//! Prefix-trie HTTP request router.
//!
//! Compiles OpenAPI `paths` into a prefix trie with static/param segments
//! and method sets. Supports path parameter capture, static-over-param
//! precedence, and path normalization.

pub mod trie;

pub use trie::Router;
