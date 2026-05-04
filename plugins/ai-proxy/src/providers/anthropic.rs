//! Anthropic Messages API transport. The protocol-specific translation
//! (Chat Completions, Responses) lives in [`crate::protocols`]; this module
//! handles only the wire format — building the request, sending it, and
//! returning the raw upstream response.
//!
//! API version pinned to 2024-10-22 (ADR-0024 contract-test-and-bump).

use crate::protocols::chat_completion::translate_from_anthropic;
use crate::protocols::chat_completion::translate_to_anthropic;
use crate::{build_response, http_call, AiProxy, HttpRequest, TargetConfig};
use barbacane_plugin_sdk::prelude::*;
use std::collections::BTreeMap;

/// Anthropic API version pinned for translation. Bumping requires updating
/// the contract tests (ADR-0024).
pub(crate) const ANTHROPIC_API_VERSION: &str = "2024-10-22";

impl AiProxy {
    /// Send a pre-built Anthropic Messages body to the target's `/v1/messages`
    /// endpoint and return the raw upstream `Response` (status + headers +
    /// body). Translation in/out lives in the calling protocol module — this
    /// helper only handles authentication, the version header, and transport.
    pub(crate) fn anthropic_messages_call_raw(
        &self,
        target: &TargetConfig,
        body: &[u8],
    ) -> Result<Response, String> {
        let base = target.effective_base_url().trim_end_matches('/');
        let url = format!("{}/v1/messages", base);

        let mut headers = BTreeMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        headers.insert(
            "anthropic-version".to_string(),
            ANTHROPIC_API_VERSION.to_string(),
        );
        if let Some(key) = &target.api_key {
            headers.insert("x-api-key".to_string(), key.clone());
        }

        set_http_request_body(body);

        let http_req = HttpRequest {
            method: "POST".to_string(),
            url,
            headers,
            timeout_ms: Some(self.timeout * 1000),
        };

        let resp_bytes = http_call(&http_req)?;
        Ok(build_response(resp_bytes))
    }

    /// Chat Completions ↔ Messages dispatch entrypoint. Builds the Anthropic
    /// Messages body from the OpenAI-format request, calls the upstream, and
    /// translates the response back. Used by [`crate::protocols::chat_completion`].
    pub(crate) fn anthropic_call(
        &self,
        target: &TargetConfig,
        req: &Request,
        client_model: &str,
        stream: bool,
    ) -> Result<Response, String> {
        let body = translate_to_anthropic(&req.body, client_model, stream, self.max_tokens)?;
        let resp = self.anthropic_messages_call_raw(target, body.as_bytes())?;

        // Only translate 2xx responses; pass error responses through as-is
        if resp.status >= 200 && resp.status < 300 {
            let translated_body = resp
                .body_str()
                .map(translate_from_anthropic)
                .transpose()?
                .map(|s| s.into_bytes());
            Ok(Response {
                status: resp.status,
                headers: resp.headers,
                body: translated_body,
            })
        } else {
            Ok(resp)
        }
    }
}
