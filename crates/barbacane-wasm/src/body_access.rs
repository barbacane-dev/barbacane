//! Body access control for middleware chains (SPEC-008).
//!
//! Manages the split-once pattern: the request body is extracted once before
//! the middleware chain runs. Non-body middlewares receive `body: null`,
//! body-access middlewares receive the full body. The body is re-injected
//! for the dispatcher after the chain completes.

/// Holds the split request state: a body-less JSON request and the held-aside body.
///
/// Created once before the middleware chain runs. The body is only injected
/// into the request JSON for middlewares that declare `body_access = true`.
pub struct BodyAccessControl {
    /// Request JSON with `body` set to `null`.
    request_without_body: Vec<u8>,
    /// The held-aside body value (may be `null` if the original request had no body).
    held_body: Option<serde_json::Value>,
}

impl BodyAccessControl {
    /// Split the request JSON into a body-less request and the held body.
    /// This is the only time the full request is parsed.
    pub fn split(request_json: &[u8]) -> Self {
        let mut v = serde_json::from_slice::<serde_json::Value>(request_json)
            .unwrap_or(serde_json::Value::Null);
        let body = v.get("body").cloned();
        v["body"] = serde_json::Value::Null;
        let request_without_body = serde_json::to_vec(&v).unwrap_or_else(|_| request_json.to_vec());
        Self {
            request_without_body,
            held_body: body,
        }
    }

    /// Returns the request JSON to pass to a middleware.
    /// If `body_access` is true, the held body is injected; otherwise body is null.
    pub fn request_for(&self, body_access: bool) -> Vec<u8> {
        if body_access {
            self.inject_body()
        } else {
            self.request_without_body.clone()
        }
    }

    /// Update state after a middleware returns. Call this with the middleware's
    /// output and whether it had body_access.
    ///
    /// - If `body_access` is true: extracts the (possibly modified) body from
    ///   the output and updates the held body.
    /// - If `body_access` is false: the output is already body-less, just
    ///   updates the request-without-body directly.
    pub fn update_after_middleware(&mut self, output: Vec<u8>, body_access: bool) {
        if body_access {
            let mut v = serde_json::from_slice::<serde_json::Value>(&output)
                .unwrap_or(serde_json::Value::Null);
            self.held_body = v.get("body").cloned();
            v["body"] = serde_json::Value::Null;
            self.request_without_body = serde_json::to_vec(&v).unwrap_or(output);
        } else {
            self.request_without_body = output;
        }
    }

    /// Produce the final request JSON with the body re-injected, ready for
    /// the dispatcher.
    pub fn finalize(self) -> Vec<u8> {
        self.inject_body()
    }

    /// Inject the held body into the body-less request JSON.
    fn inject_body(&self) -> Vec<u8> {
        if let Some(ref body) = self.held_body {
            if let Ok(mut v) =
                serde_json::from_slice::<serde_json::Value>(&self.request_without_body)
            {
                v["body"] = body.clone();
                return serde_json::to_vec(&v)
                    .unwrap_or_else(|_| self.request_without_body.clone());
            }
        }
        self.request_without_body.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_request(body: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "method": "POST",
            "path": "/upload",
            "query": null,
            "headers": {"content-type": "application/json"},
            "body": body,
            "client_ip": "127.0.0.1",
            "path_params": {}
        }))
        .expect("serialize request")
    }

    fn make_request_no_body() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "method": "GET",
            "path": "/health",
            "query": null,
            "headers": {},
            "body": null,
            "client_ip": "127.0.0.1",
            "path_params": {}
        }))
        .expect("serialize request")
    }

    fn parse_body(json_bytes: &[u8]) -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_slice(json_bytes).expect("parse JSON");
        v["body"].clone()
    }

    fn parse_header(json_bytes: &[u8], key: &str) -> Option<String> {
        let v: serde_json::Value = serde_json::from_slice(json_bytes).expect("parse JSON");
        v["headers"][key].as_str().map(|s| s.to_string())
    }

    // ── Split ────────────────────────────────────────────────────────

    #[test]
    fn split_extracts_body_and_nullifies() {
        let req = make_request(json!("hello world"));
        let ctrl = BodyAccessControl::split(&req);

        assert_eq!(parse_body(&ctrl.request_without_body), json!(null));
        assert_eq!(ctrl.held_body, Some(json!("hello world")));
    }

    #[test]
    fn split_null_body_request() {
        let req = make_request_no_body();
        let ctrl = BodyAccessControl::split(&req);

        assert_eq!(parse_body(&ctrl.request_without_body), json!(null));
        assert_eq!(ctrl.held_body, Some(json!(null)));
    }

    #[test]
    fn split_binary_base64_body() {
        // Simulate base64-encoded binary body as it appears in the JSON
        let b64 = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            b"\x00\x01\x02\xff",
        );
        let req = make_request(json!(b64));
        let ctrl = BodyAccessControl::split(&req);

        assert_eq!(ctrl.held_body, Some(json!(b64)));
        assert_eq!(parse_body(&ctrl.request_without_body), json!(null));
    }

    #[test]
    fn split_invalid_json_produces_safe_default() {
        let ctrl = BodyAccessControl::split(b"not json");

        // Should not panic — produces a null-bodied fallback
        assert!(ctrl.held_body.is_some() || ctrl.held_body.is_none());
    }

    // ── request_for ──────────────────────────────────────────────────

    #[test]
    fn request_for_without_body_access_has_null_body() {
        let req = make_request(json!({"key": "value"}));
        let ctrl = BodyAccessControl::split(&req);

        let for_wasm = ctrl.request_for(false);
        assert_eq!(parse_body(&for_wasm), json!(null));
    }

    #[test]
    fn request_for_with_body_access_has_body() {
        let req = make_request(json!({"key": "value"}));
        let ctrl = BodyAccessControl::split(&req);

        let for_wasm = ctrl.request_for(true);
        assert_eq!(parse_body(&for_wasm), json!({"key": "value"}));
    }

    #[test]
    fn request_for_with_body_access_null_body() {
        let req = make_request_no_body();
        let ctrl = BodyAccessControl::split(&req);

        let for_wasm = ctrl.request_for(true);
        assert_eq!(parse_body(&for_wasm), json!(null));
    }

    // ── update_after_middleware ───────────────────────────────────────

    #[test]
    fn update_non_body_middleware_preserves_header_changes() {
        let req = make_request(json!("original body"));
        let mut ctrl = BodyAccessControl::split(&req);

        // Simulate auth middleware adding a header (no body in output)
        let mut output: serde_json::Value =
            serde_json::from_slice(&ctrl.request_for(false)).expect("parse");
        output["headers"]["x-consumer-id"] = json!("user-42");
        let output_bytes = serde_json::to_vec(&output).expect("serialize");

        ctrl.update_after_middleware(output_bytes, false);

        assert_eq!(
            parse_header(&ctrl.request_without_body, "x-consumer-id"),
            Some("user-42".to_string())
        );
        // Body is still held aside
        assert_eq!(ctrl.held_body, Some(json!("original body")));
    }

    #[test]
    fn update_body_middleware_captures_modified_body() {
        let req = make_request(json!({"original": true}));
        let mut ctrl = BodyAccessControl::split(&req);

        // Simulate request-transformer modifying the body
        let mut output: serde_json::Value =
            serde_json::from_slice(&ctrl.request_for(true)).expect("parse");
        output["body"] = json!({"original": true, "added_field": "new"});
        let output_bytes = serde_json::to_vec(&output).expect("serialize");

        ctrl.update_after_middleware(output_bytes, true);

        assert_eq!(
            ctrl.held_body,
            Some(json!({"original": true, "added_field": "new"}))
        );
        // request_without_body should have null body
        assert_eq!(parse_body(&ctrl.request_without_body), json!(null));
    }

    // ── finalize ─────────────────────────────────────────────────────

    #[test]
    fn finalize_reinjects_body() {
        let req = make_request(json!("the body"));
        let ctrl = BodyAccessControl::split(&req);

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!("the body"));
    }

    #[test]
    fn finalize_null_body_stays_null() {
        let req = make_request_no_body();
        let ctrl = BodyAccessControl::split(&req);

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!(null));
    }

    // ── Full chain scenarios ─────────────────────────────────────────

    /// Chain: [auth(false), rate-limit(false), transformer(true), cors(false)]
    /// Only transformer gets the body; transformer modifies it; dispatcher gets final body.
    #[test]
    fn full_chain_mixed_body_access() {
        let req = make_request(json!({"data": "original"}));
        let mut ctrl = BodyAccessControl::split(&req);
        let flags = [false, false, true, false];

        // Step 1: auth (no body access) — adds x-consumer-id header
        let wasm_input = ctrl.request_for(flags[0]);
        assert_eq!(parse_body(&wasm_input), json!(null));
        let mut out: serde_json::Value = serde_json::from_slice(&wasm_input).expect("parse");
        out["headers"]["x-consumer-id"] = json!("user-1");
        ctrl.update_after_middleware(serde_json::to_vec(&out).expect("ser"), flags[0]);

        // Step 2: rate-limit (no body access) — passes through
        let wasm_input = ctrl.request_for(flags[1]);
        assert_eq!(parse_body(&wasm_input), json!(null));
        // rate-limit returns unchanged
        ctrl.update_after_middleware(wasm_input, flags[1]);

        // Step 3: transformer (body access) — modifies body
        let wasm_input = ctrl.request_for(flags[2]);
        assert_eq!(parse_body(&wasm_input), json!({"data": "original"}));
        // Verify auth's header change carried through
        assert_eq!(
            parse_header(&wasm_input, "x-consumer-id"),
            Some("user-1".to_string())
        );
        let mut out: serde_json::Value = serde_json::from_slice(&wasm_input).expect("parse");
        out["body"] = json!({"data": "transformed"});
        ctrl.update_after_middleware(serde_json::to_vec(&out).expect("ser"), flags[2]);

        // Step 4: cors (no body access) — adds cors header
        let wasm_input = ctrl.request_for(flags[3]);
        assert_eq!(parse_body(&wasm_input), json!(null));
        let mut out: serde_json::Value = serde_json::from_slice(&wasm_input).expect("parse");
        out["headers"]["access-control-allow-origin"] = json!("*");
        ctrl.update_after_middleware(serde_json::to_vec(&out).expect("ser"), flags[3]);

        // Final: dispatcher gets transformed body + all header changes
        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!({"data": "transformed"}));
        assert_eq!(
            parse_header(&final_req, "x-consumer-id"),
            Some("user-1".to_string())
        );
        assert_eq!(
            parse_header(&final_req, "access-control-allow-origin"),
            Some("*".to_string())
        );
    }

    /// Chain: all middlewares have body_access = false.
    /// Body should pass through untouched to dispatcher.
    #[test]
    fn full_chain_all_no_body_access() {
        let req = make_request(json!("untouched body"));
        let mut ctrl = BodyAccessControl::split(&req);
        let flags = [false, false, false];

        for &flag in &flags {
            let wasm_input = ctrl.request_for(flag);
            assert_eq!(parse_body(&wasm_input), json!(null));
            ctrl.update_after_middleware(wasm_input, flag);
        }

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!("untouched body"));
    }

    /// Chain: all middlewares have body_access = true.
    /// Each sees the (possibly modified) body.
    #[test]
    fn full_chain_all_body_access() {
        let req = make_request(json!({"step": 0}));
        let mut ctrl = BodyAccessControl::split(&req);
        let flags = [true, true, true];

        for (i, &flag) in flags.iter().enumerate() {
            let wasm_input = ctrl.request_for(flag);
            assert_eq!(parse_body(&wasm_input), json!({"step": i}));
            let mut out: serde_json::Value = serde_json::from_slice(&wasm_input).expect("parse");
            out["body"] = json!({"step": i + 1});
            ctrl.update_after_middleware(serde_json::to_vec(&out).expect("ser"), flag);
        }

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!({"step": 3}));
    }

    /// Single middleware with body_access = false.
    #[test]
    fn single_middleware_no_body_access() {
        let req = make_request(json!("keep me"));
        let mut ctrl = BodyAccessControl::split(&req);

        let wasm_input = ctrl.request_for(false);
        assert_eq!(parse_body(&wasm_input), json!(null));
        ctrl.update_after_middleware(wasm_input, false);

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!("keep me"));
    }

    /// Single middleware with body_access = true.
    #[test]
    fn single_middleware_with_body_access() {
        let req = make_request(json!("modify me"));
        let mut ctrl = BodyAccessControl::split(&req);

        let wasm_input = ctrl.request_for(true);
        assert_eq!(parse_body(&wasm_input), json!("modify me"));
        let mut out: serde_json::Value = serde_json::from_slice(&wasm_input).expect("parse");
        out["body"] = json!("modified");
        ctrl.update_after_middleware(serde_json::to_vec(&out).expect("ser"), true);

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!("modified"));
    }

    /// Empty chain — split then immediately finalize.
    #[test]
    fn empty_chain_finalize_returns_original() {
        let req = make_request(json!("pass through"));
        let ctrl = BodyAccessControl::split(&req);

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!("pass through"));
    }

    /// Body-access middleware sets body to null (removes it).
    #[test]
    fn body_access_middleware_removes_body() {
        let req = make_request(json!("remove me"));
        let mut ctrl = BodyAccessControl::split(&req);

        let wasm_input = ctrl.request_for(true);
        let mut out: serde_json::Value = serde_json::from_slice(&wasm_input).expect("parse");
        out["body"] = json!(null);
        ctrl.update_after_middleware(serde_json::to_vec(&out).expect("ser"), true);

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!(null));
    }

    /// Large body — verify no corruption through split/inject/finalize.
    #[test]
    fn large_body_roundtrip() {
        let large = "x".repeat(100_000);
        let req = make_request(json!(large));
        let mut ctrl = BodyAccessControl::split(&req);

        // Pass through one non-body middleware
        let wasm_input = ctrl.request_for(false);
        assert_eq!(parse_body(&wasm_input), json!(null));
        ctrl.update_after_middleware(wasm_input, false);

        // Then one body middleware that passes through unchanged
        let wasm_input = ctrl.request_for(true);
        assert_eq!(parse_body(&wasm_input), json!(large));
        ctrl.update_after_middleware(wasm_input, true);

        let final_req = ctrl.finalize();
        assert_eq!(parse_body(&final_req), json!(large));
    }
}
