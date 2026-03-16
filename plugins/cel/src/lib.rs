//! CEL policy evaluation middleware plugin for Barbacane API gateway.
//!
//! Evaluates inline [CEL](https://cel.dev/) expressions against request context
//! for access control decisions. No external service needed — expressions are
//! compiled once and evaluated in-process per request.

use barbacane_plugin_sdk::prelude::*;
use cel_interpreter as cel;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::Arc;

/// Context keys to set when the expression matches.
/// Used by `on_match` to route requests to named AI targets or set other context.
#[derive(Deserialize, Default)]
struct OnMatch {
    /// Context key-value pairs to write via `host_context_set` when expression is true.
    #[serde(default)]
    set_context: BTreeMap<String, String>,
}

/// CEL policy evaluation middleware configuration.
#[barbacane_middleware]
#[derive(Deserialize)]
pub struct CelPolicy {
    /// CEL expression that must evaluate to a boolean.
    expression: String,

    /// Custom message returned in the 403 response when the expression is false.
    /// Ignored when `on_match` is set (false is a no-op in routing mode).
    #[serde(default = "default_deny_message")]
    deny_message: String,

    /// Pre-compiled CEL program (lazy-initialized on first request).
    #[serde(skip)]
    compiled: Option<cel::Program>,

    /// When present, switches from access-control mode to context-routing mode:
    /// - true  → write `set_context` keys into request context, then continue
    /// - false → continue unchanged (no 403)
    ///
    /// When absent (default), the original access-control behaviour applies:
    /// - true  → continue
    /// - false → 403 Forbidden
    #[serde(default)]
    on_match: Option<OnMatch>,
}

fn default_deny_message() -> String {
    "Access denied by policy".to_string()
}

// ---------------------------------------------------------------------------
// Middleware implementation
// ---------------------------------------------------------------------------

impl CelPolicy {
    pub fn on_request(&mut self, req: Request) -> Action<Request> {
        // Lazy-compile the expression on first request
        if let Err(msg) = self.ensure_compiled() {
            return Action::ShortCircuit(self.config_error_response(&msg));
        }

        let program = self.compiled.as_ref().expect("just compiled above");

        // Build CEL context from request
        let context = self.build_context(&req);

        // Execute the CEL program
        match program.execute(&context) {
            Ok(cel::Value::Bool(true)) => {
                if let Some(on_match) = &self.on_match {
                    for (key, value) in &on_match.set_context {
                        host::context_set(key, value);
                    }
                }
                Action::Continue(req)
            }
            Ok(cel::Value::Bool(false)) => {
                if self.on_match.is_some() {
                    // Routing mode: false is a no-op, let the request pass through
                    Action::Continue(req)
                } else {
                    // Access-control mode: false → 403
                    Action::ShortCircuit(self.denied_response())
                }
            }
            Ok(other) => Action::ShortCircuit(self.eval_error_response(&format!(
                "expression returned {}, expected bool",
                value_type_name(&other)
            ))),
            Err(e) => Action::ShortCircuit(self.eval_error_response(&format!("{}", e))),
        }
    }

    pub fn on_response(&mut self, resp: Response) -> Response {
        resp
    }

    /// Compile the CEL expression once, reuse on subsequent calls.
    fn ensure_compiled(&mut self) -> Result<(), String> {
        if self.compiled.is_none() {
            let program = cel::Program::compile(&self.expression)
                .map_err(|e| format!("CEL parse error: {}", e))?;
            self.compiled = Some(program);
        }
        Ok(())
    }

    /// Build a CEL evaluation context from the request.
    fn build_context(&self, req: &Request) -> cel::Context<'_> {
        let mut context = cel::Context::default();

        let mut request_map: HashMap<String, cel::Value> = HashMap::new();

        // Core request fields
        request_map.insert("method".to_string(), str_val(&req.method));
        request_map.insert("path".to_string(), str_val(&req.path));
        request_map.insert(
            "query".to_string(),
            str_val(req.query.as_deref().unwrap_or("")),
        );
        request_map.insert(
            "body".to_string(),
            str_val(req.body_str().unwrap_or("")),
        );
        request_map.insert("client_ip".to_string(), str_val(&req.client_ip));

        // Headers as a map
        request_map.insert("headers".to_string(), btree_to_cel_map(&req.headers));

        // Path params as a map
        request_map.insert(
            "path_params".to_string(),
            btree_to_cel_map(&req.path_params),
        );

        // Consumer identity (from x-auth-consumer header)
        let consumer = req
            .headers
            .get("x-auth-consumer")
            .map(|s| s.as_str())
            .unwrap_or("");
        request_map.insert("consumer".to_string(), str_val(consumer));

        // Parsed claims (from x-auth-claims header, or empty map)
        let claims = req
            .headers
            .get("x-auth-claims")
            .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        request_map.insert("claims".to_string(), json_to_cel(claims));

        context.add_variable_from_value("request", request_map);
        context
    }

    /// 403 Forbidden response for policy denial.
    fn denied_response(&self) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": "urn:barbacane:error:cel-denied",
            "title": "Forbidden",
            "status": 403,
            "detail": self.deny_message
        });

        Response {
            status: 403,
            headers,
            body: Some(body.to_string().into_bytes()),
        }
    }

    /// 500 Internal Server Error for CEL configuration errors (bad expression).
    fn config_error_response(&self, detail: &str) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": "urn:barbacane:error:cel-config",
            "title": "Internal Server Error",
            "status": 500,
            "detail": detail
        });

        Response {
            status: 500,
            headers,
            body: Some(body.to_string().into_bytes()),
        }
    }

    /// 500 Internal Server Error for CEL evaluation errors.
    fn eval_error_response(&self, detail: &str) -> Response {
        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": "urn:barbacane:error:cel-evaluation",
            "title": "Internal Server Error",
            "status": 500,
            "detail": detail
        });

        Response {
            status: 500,
            headers,
            body: Some(body.to_string().into_bytes()),
        }
    }
}

// ---------------------------------------------------------------------------
// Value conversion helpers
// ---------------------------------------------------------------------------

/// Create a CEL string value.
fn str_val(s: &str) -> cel::Value {
    cel::Value::String(Arc::new(s.to_string()))
}

/// Convert a BTreeMap<String, String> to a CEL map value.
fn btree_to_cel_map(map: &BTreeMap<String, String>) -> cel::Value {
    let cel_map: HashMap<String, cel::Value> =
        map.iter().map(|(k, v)| (k.clone(), str_val(v))).collect();
    cel_map.into()
}

/// Convert a serde_json::Value to a CEL value.
fn json_to_cel(value: serde_json::Value) -> cel::Value {
    match value {
        serde_json::Value::Null => cel::Value::Null,
        serde_json::Value::Bool(b) => cel::Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                cel::Value::Int(i)
            } else if let Some(u) = n.as_u64() {
                cel::Value::UInt(u)
            } else if let Some(f) = n.as_f64() {
                cel::Value::Float(f)
            } else {
                cel::Value::Null
            }
        }
        serde_json::Value::String(s) => cel::Value::String(Arc::new(s)),
        serde_json::Value::Array(arr) => {
            let items: Vec<cel::Value> = arr.into_iter().map(json_to_cel).collect();
            cel::Value::List(Arc::new(items))
        }
        serde_json::Value::Object(obj) => {
            let map: HashMap<String, cel::Value> =
                obj.into_iter().map(|(k, v)| (k, json_to_cel(v))).collect();
            map.into()
        }
    }
}

/// Get a human-readable type name for a CEL value (for error messages).
fn value_type_name(value: &cel::Value) -> &'static str {
    match value {
        cel::Value::List(_) => "list",
        cel::Value::Map(_) => "map",
        cel::Value::Function(_, _) => "function",
        cel::Value::Int(_) => "int",
        cel::Value::UInt(_) => "uint",
        cel::Value::Float(_) => "float",
        cel::Value::String(_) => "string",
        cel::Value::Bytes(_) => "bytes",
        cel::Value::Bool(_) => "bool",
        cel::Value::Null => "null",
    }
}

// ---------------------------------------------------------------------------
// Host functions
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
mod host {
    pub fn context_set(key: &str, value: &str) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_context_set(key_ptr: i32, key_len: i32, val_ptr: i32, val_len: i32);
        }
        unsafe {
            host_context_set(
                key.as_ptr() as i32,
                key.len() as i32,
                value.as_ptr() as i32,
                value.len() as i32,
            );
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static CONTEXT: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
    }

    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|ctx| {
            ctx.borrow_mut().insert(key.to_string(), value.to_string());
        });
    }

    #[cfg(test)]
    pub fn get_context() -> BTreeMap<String, String> {
        CONTEXT.with(|ctx| ctx.borrow().clone())
    }

    #[cfg(test)]
    pub fn reset_context() {
        CONTEXT.with(|ctx| ctx.borrow_mut().clear());
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn create_config(expression: &str) -> CelPolicy {
        CelPolicy {
            expression: expression.to_string(),
            deny_message: default_deny_message(),
            compiled: None,
            on_match: None,
        }
    }

    fn create_config_with_on_match(expression: &str, set_context: BTreeMap<String, String>) -> CelPolicy {
        CelPolicy {
            expression: expression.to_string(),
            deny_message: default_deny_message(),
            compiled: None,
            on_match: Some(OnMatch { set_context }),
        }
    }

    fn create_request() -> Request {
        let mut headers = BTreeMap::new();
        headers.insert("x-auth-consumer".to_string(), "alice".to_string());
        headers.insert("content-type".to_string(), "application/json".to_string());

        Request {
            method: "GET".to_string(),
            path: "/api/users".to_string(),
            headers,
            body: None,
            query: Some("page=1".to_string()),
            path_params: BTreeMap::new(),
            client_ip: "10.0.0.1".to_string(),
        }
    }

    // --- Config deserialization ---

    #[test]
    fn config_deserialization_minimal() {
        let json = r#"{"expression": "true"}"#;
        let config: CelPolicy = serde_json::from_str(json).expect("should parse");

        assert_eq!(config.expression, "true");
        assert_eq!(config.deny_message, "Access denied by policy");
        assert!(config.compiled.is_none());
    }

    #[test]
    fn config_deserialization_full() {
        let json = r#"{
            "expression": "request.method == 'GET'",
            "deny_message": "Custom deny"
        }"#;
        let config: CelPolicy = serde_json::from_str(json).expect("should parse");

        assert_eq!(config.expression, "request.method == 'GET'");
        assert_eq!(config.deny_message, "Custom deny");
    }

    #[test]
    fn config_deserialization_missing_expression() {
        let json = r#"{"deny_message": "test"}"#;
        let result: Result<CelPolicy, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // --- Expression evaluation ---

    #[test]
    fn eval_method_check_allowed() {
        let mut config = create_config("request.method == 'GET'");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_method_check_denied() {
        let mut config = create_config("request.method == 'POST'");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn eval_path_starts_with() {
        let mut config = create_config("request.path.startsWith('/api/')");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_path_starts_with_denied() {
        let mut config = create_config("request.path.startsWith('/admin/')");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn eval_consumer_check() {
        let mut config = create_config("request.consumer == 'alice'");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_consumer_check_denied() {
        let mut config = create_config("request.consumer == 'bob'");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn eval_header_access() {
        let mut config = create_config("request.headers['content-type'] == 'application/json'");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_client_ip() {
        let mut config = create_config("request.client_ip.startsWith('10.')");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_query_string() {
        let mut config = create_config("request.query == 'page=1'");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_logical_operators() {
        let mut config =
            create_config("request.method == 'GET' && request.path.startsWith('/api/')");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_logical_or() {
        let mut config = create_config("request.method == 'POST' || request.consumer == 'alice'");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_negation() {
        let mut config = create_config("!(request.method == 'DELETE')");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    // --- Claims ---

    #[test]
    fn eval_claims_role_check() {
        let mut config = create_config("'admin' in request.claims.roles");
        let mut req = create_request();
        req.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"sub":"alice","roles":["admin","editor"]}"#.to_string(),
        );
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_claims_role_check_denied() {
        let mut config = create_config("'admin' in request.claims.roles");
        let mut req = create_request();
        req.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"sub":"bob","roles":["viewer"]}"#.to_string(),
        );
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn eval_claims_has_field() {
        let mut config = create_config("has(request.claims.email)");
        let mut req = create_request();
        req.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"email":"alice@example.com"}"#.to_string(),
        );
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_claims_has_field_missing() {
        let mut config = create_config("has(request.claims.email)");
        let mut req = create_request();
        req.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"sub":"alice"}"#.to_string(),
        );
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn eval_claims_missing_header_empty_map() {
        // No x-auth-claims header → claims is empty map, has() returns false
        let mut config = create_config("has(request.claims.roles)");
        let mut req = create_request();
        req.headers.remove("x-auth-claims");
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn eval_claims_invalid_json_falls_back_to_empty() {
        let mut config = create_config("has(request.claims.roles)");
        let mut req = create_request();
        req.headers
            .insert("x-auth-claims".to_string(), "not-json".to_string());
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn eval_claims_exists_macro() {
        let mut config = create_config("request.claims.roles.exists(r, r == 'editor')");
        let mut req = create_request();
        req.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"roles":["admin","editor"]}"#.to_string(),
        );
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    // --- Body ---

    #[test]
    fn eval_body_access() {
        let mut config = create_config("request.body != ''");
        let mut req = create_request();
        req.body = Some(br#"{"data":"test"}"#.to_vec());
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_body_empty_when_none() {
        let mut config = create_config("request.body == ''");
        let req = create_request(); // body is None → ""
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    // --- Error handling ---

    #[test]
    fn eval_invalid_expression_returns_500() {
        let mut config = create_config("this is not valid CEL !!!");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected error"),
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 500);
                let body: serde_json::Value =
                    serde_json::from_slice(resp.body.as_ref().expect("body")).expect("json");
                assert_eq!(body["type"], "urn:barbacane:error:cel-config");
            }
        }
    }

    #[test]
    fn eval_non_bool_result_returns_500() {
        let mut config = create_config("request.method");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected error"),
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 500);
                let body: serde_json::Value =
                    serde_json::from_slice(resp.body.as_ref().expect("body")).expect("json");
                assert_eq!(body["type"], "urn:barbacane:error:cel-evaluation");
            }
        }
    }

    // --- Response format ---

    #[test]
    fn denied_response_format() {
        let config = create_config("false");
        let resp = config.denied_response();

        assert_eq!(resp.status, 403);
        assert_eq!(
            resp.headers.get("content-type").expect("content-type"),
            "application/problem+json"
        );

        let body: serde_json::Value =
            serde_json::from_slice(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:cel-denied");
        assert_eq!(body["title"], "Forbidden");
        assert_eq!(body["status"], 403);
        assert_eq!(body["detail"], "Access denied by policy");
    }

    #[test]
    fn denied_response_custom_message() {
        let mut config = create_config("false");
        config.deny_message = "Custom deny".to_string();

        let resp = config.denied_response();
        let body: serde_json::Value =
            serde_json::from_slice(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["detail"], "Custom deny");
    }

    #[test]
    fn config_error_response_format() {
        let config = create_config("bad");
        let resp = config.config_error_response("CEL parse error: unexpected token");

        assert_eq!(resp.status, 500);
        let body: serde_json::Value =
            serde_json::from_slice(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:cel-config");
        assert_eq!(body["title"], "Internal Server Error");
    }

    #[test]
    fn eval_error_response_format() {
        let config = create_config("bad");
        let resp = config.eval_error_response("expression returned string, expected bool");

        assert_eq!(resp.status, 500);
        let body: serde_json::Value =
            serde_json::from_slice(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:cel-evaluation");
    }

    // --- Lazy compilation ---

    #[test]
    fn lazy_compilation_reuses_program() {
        let mut config = create_config("true");
        assert!(config.compiled.is_none());

        let req = create_request();
        let _ = config.on_request(req);
        assert!(config.compiled.is_some());

        // Second request reuses compiled program
        let req2 = create_request();
        let _ = config.on_request(req2);
        assert!(config.compiled.is_some());
    }

    // --- JSON to CEL conversion ---

    #[test]
    fn json_to_cel_converts_all_types() {
        let json = serde_json::json!({
            "string": "hello",
            "int": 42,
            "float": 1.5,
            "bool": true,
            "null": null,
            "array": [1, 2, 3],
            "object": {"nested": "value"}
        });

        let cel_val = json_to_cel(json);
        // Should produce a map — just verify it doesn't panic
        match cel_val {
            cel::Value::Map(_) => {}
            other => panic!("expected map, got {:?}", other),
        }
    }

    // --- on_response passthrough ---

    #[test]
    fn on_response_passthrough() {
        let mut config = create_config("true");
        let response = Response {
            status: 200,
            headers: BTreeMap::new(),
            body: Some(b"ok".to_vec()),
        };

        let result = config.on_response(response);
        assert_eq!(result.status, 200);
        assert_eq!(result.body, Some(b"ok".to_vec()));
    }

    // --- Default values ---

    #[test]
    fn default_deny_message_value() {
        assert_eq!(default_deny_message(), "Access denied by policy");
    }

    // --- on_match routing mode ---

    #[test]
    fn on_match_true_sets_context_and_continues() {
        host::reset_context();
        let mut ctx_map = BTreeMap::new();
        ctx_map.insert("ai.target".to_string(), "premium".to_string());
        let mut config = create_config_with_on_match("request.consumer == 'alice'", ctx_map);
        let req = create_request();

        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }

        let context = host::get_context();
        assert_eq!(context.get("ai.target").map(|s| s.as_str()), Some("premium"));
    }

    #[test]
    fn on_match_false_continues_without_403() {
        host::reset_context();
        let mut ctx_map = BTreeMap::new();
        ctx_map.insert("ai.target".to_string(), "premium".to_string());
        let mut config = create_config_with_on_match("request.consumer == 'bob'", ctx_map);
        let req = create_request(); // consumer = alice, not bob

        match config.on_request(req) {
            Action::Continue(_) => {} // no 403 — routing mode
            Action::ShortCircuit(resp) => panic!("expected continue in routing mode, got {}", resp.status),
        }

        // Context was NOT set (expression was false)
        let context = host::get_context();
        assert!(!context.contains_key("ai.target"));
    }

    #[test]
    fn on_match_sets_multiple_context_keys() {
        host::reset_context();
        let mut ctx_map = BTreeMap::new();
        ctx_map.insert("ai.target".to_string(), "premium".to_string());
        ctx_map.insert("ai.priority".to_string(), "high".to_string());
        let mut config = create_config_with_on_match("true", ctx_map);
        let req = create_request();

        let _ = config.on_request(req);

        let context = host::get_context();
        assert_eq!(context.get("ai.target").map(|s| s.as_str()), Some("premium"));
        assert_eq!(context.get("ai.priority").map(|s| s.as_str()), Some("high"));
    }

    #[test]
    fn on_match_deserializes_from_json() {
        let json = r#"{
            "expression": "request.claims.tier == 'premium'",
            "on_match": {
                "set_context": {
                    "ai.target": "premium"
                }
            }
        }"#;
        let config: CelPolicy = serde_json::from_str(json).expect("should parse");
        assert!(config.on_match.is_some());
        let on_match = config.on_match.unwrap();
        assert_eq!(on_match.set_context.get("ai.target").map(|s| s.as_str()), Some("premium"));
    }

    #[test]
    fn without_on_match_false_still_returns_403() {
        let mut config = create_config("false");
        let req = create_request();
        match config.on_request(req) {
            Action::Continue(_) => panic!("expected 403"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }
}
