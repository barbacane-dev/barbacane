//! Ollama transport. Ollama exposes an OpenAI-compatible API so it shares
//! the [`super::openai`] passthrough today; this module exists as a slot for
//! Ollama-specific divergence when ADR-0030's `/v1/responses` adapter lands
//! (Ollama has no Responses API as of 2026-04, so the future call site will
//! reject Responses requests against an Ollama target rather than passing
//! them through silently).
