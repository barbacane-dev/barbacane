//! OpenAI-compatible transport. Used directly by `Provider::OpenAI` and
//! shared (via [`super::ollama`]) by `Provider::Ollama`.
//!
//! The translation layer (e.g. [`crate::protocols::chat_completion`]) decides
//! whether to call here or into [`super::anthropic`] based on the resolved
//! provider; this module deals only with HTTP wire-level concerns.

use crate::{build_response, host_http_stream, http_call, AiProxy, HttpRequest, TargetConfig};
use barbacane_plugin_sdk::prelude::*;
use std::collections::BTreeMap;

impl AiProxy {
    pub(crate) fn openai_call(
        &self,
        target: &TargetConfig,
        req: &Request,
    ) -> Result<Response, String> {
        let url = openai_url(target, &req.path);
        let headers = openai_headers(target);

        let body = self.maybe_inject_max_tokens(&req.body);
        if let Some(ref b) = body {
            set_http_request_body(b);
        }

        let http_req = HttpRequest {
            method: req.method.clone(),
            url,
            headers,
            timeout_ms: Some(self.timeout * 1000),
        };

        let resp_bytes = http_call(&http_req)?;
        Ok(build_response(resp_bytes))
    }

    pub(crate) fn openai_stream(
        &self,
        target: &TargetConfig,
        req: &Request,
    ) -> Result<Response, String> {
        let url = openai_url(target, &req.path);
        let mut headers = openai_headers(target);
        // Ensure Accept header for SSE
        headers.insert("accept".to_string(), "text/event-stream".to_string());

        let body = self.maybe_inject_max_tokens(&req.body);
        if let Some(ref b) = body {
            set_http_request_body(b);
        }

        let http_req = HttpRequest {
            method: req.method.clone(),
            url,
            headers,
            timeout_ms: Some(self.timeout * 1000),
        };

        let req_json = serde_json::to_vec(&http_req).map_err(|e| e.to_string())?;
        let result = unsafe { host_http_stream(req_json.as_ptr() as i32, req_json.len() as i32) };

        if result < 0 {
            return Err("upstream stream failed".to_string());
        }

        Ok(streamed_response())
    }

    /// Inject a default `max_tokens` into the request body when the client
    /// didn't send one — required for Anthropic (field is mandatory) and
    /// useful as a cost guardrail for OpenAI.
    pub(crate) fn maybe_inject_max_tokens(&self, body: &Option<Vec<u8>>) -> Option<Vec<u8>> {
        let Some(max) = self.max_tokens else {
            return body.clone();
        };
        let Some(raw) = body.as_deref() else {
            return body.clone();
        };
        let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(raw) else {
            return body.clone();
        };
        if let Some(obj) = v.as_object_mut() {
            if !obj.contains_key("max_tokens") {
                obj.insert("max_tokens".to_string(), serde_json::json!(max));
                return Some(serde_json::to_vec(&v).unwrap_or_default());
            }
        }
        body.clone()
    }
}

pub(crate) fn openai_url(target: &TargetConfig, req_path: &str) -> String {
    let base = target.effective_base_url().trim_end_matches('/');
    format!("{}{}", base, req_path)
}

pub(crate) fn openai_headers(target: &TargetConfig) -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    if let Some(key) = &target.api_key {
        headers.insert("authorization".to_string(), format!("Bearer {}", key));
    }
    headers
}
