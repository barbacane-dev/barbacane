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

/// Actions to take when the expression matches (evaluates to `true`).
///
/// Either or both fields can be present. When both are set, `deny` wins —
/// a denied request shouldn't also have its context mutated.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct OnMatch {
    /// Context key-value pairs to write via `host_context_set` when expression is true.
    #[serde(default)]
    set_context: BTreeMap<String, String>,
    /// Reject the request with the configured status / code when expression is true.
    #[serde(default)]
    deny: Option<DenyAction>,
}

/// Configurable deny response for `on_match.deny`. The error code is embedded
/// into a `urn:barbacane:error:<code>` problem+json type and exposed alongside
/// `status` and `detail` so clients can introspect the policy decision.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DenyAction {
    /// HTTP status code. Defaults to 403; must be 4xx (5xx would mask a policy
    /// decision as a server fault).
    #[serde(default = "default_deny_status")]
    status: u16,
    /// Machine-readable error code, snake_case. Becomes the URN suffix and the
    /// `code` field on the response body — the convention used by `ai-proxy`
    /// for `model_not_permitted` and similar.
    code: String,
    /// Human-readable detail message. Falls back to `code` when omitted.
    #[serde(default)]
    message: Option<String>,
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

    /// When present, switches from access-control mode to match-and-act mode:
    /// - true  → take the configured `on_match` actions (`set_context` and/or
    ///           `deny`); `deny` wins when both are set
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

fn default_deny_status() -> u16 {
    403
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
                    if let Some(deny) = &on_match.deny {
                        // Deny wins over set_context — a denied request shouldn't
                        // also have its context mutated.
                        return Action::ShortCircuit(self.deny_action_response(deny));
                    }
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

    /// Compile the CEL expression once, reuse on subsequent calls. Also
    /// validates `on_match.deny.code` against the snake_case rule that the
    /// JSON schema declares — vacuum's auto-generated validator only recurses
    /// to top-level fields, so nested-field constraints are enforced here.
    fn ensure_compiled(&mut self) -> Result<(), String> {
        if self.compiled.is_none() {
            if let Some(on_match) = &self.on_match {
                if let Some(deny) = &on_match.deny {
                    if !is_snake_case_code(&deny.code) {
                        return Err(format!(
                            "on_match.deny.code must match ^[a-z][a-z0-9_]*$, got {:?}",
                            deny.code
                        ));
                    }
                }
            }
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
        request_map.insert("body_json".to_string(), parse_body_json(req));
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

    /// problem+json response for `on_match.deny`. The configured `code` becomes
    /// the URN suffix and the `code` field on the body. Status defaults to 403
    /// and is clamped into the 4xx range — a `cel` policy denial that returned
    /// 5xx would mask an operator decision as a server fault.
    fn deny_action_response(&self, action: &DenyAction) -> Response {
        let status = if (400..500).contains(&action.status) {
            action.status
        } else {
            403
        };
        let title = http_reason_phrase(status);
        let detail = action
            .message
            .clone()
            .unwrap_or_else(|| action.code.clone());

        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        let body = serde_json::json!({
            "type": format!("urn:barbacane:error:{}", action.code),
            "title": title,
            "status": status,
            "code": action.code,
            "detail": detail,
        });

        Response {
            status,
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

/// Empty CEL map. Returned as the `request.body_json` value when the body
/// can't be parsed as JSON — keeps `has(request.body_json.x)` semantics clean.
fn empty_cel_map() -> cel::Value {
    HashMap::<String, cel::Value>::new().into()
}

/// Parse the request body as JSON when the inbound `content-type` advertises it
/// (`application/json` or any `application/*+json` vendor type, ignoring `;`-suffixed
/// parameters). Returns an empty map for non-JSON content-types and for malformed
/// bodies; the latter logs a warning so operators see policy mis-applies, but never
/// short-circuits the request — a CEL plugin that rejected on every malformed JSON
/// would let an attacker take down every downstream policy by sending one bad byte.
fn parse_body_json(req: &Request) -> cel::Value {
    let content_type = match req.headers.get("content-type") {
        Some(v) => v.split(';').next().unwrap_or("").trim().to_ascii_lowercase(),
        None => return empty_cel_map(),
    };
    let is_json = content_type == "application/json"
        || (content_type.starts_with("application/") && content_type.ends_with("+json"));
    if !is_json {
        return empty_cel_map();
    }
    let body = match req.body_str() {
        Some(s) if !s.is_empty() => s,
        _ => return empty_cel_map(),
    };
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(v) => json_to_cel(v),
        Err(e) => {
            host::log_warn(&format!(
                "cel: request body advertised {} but could not be parsed as JSON: {}",
                content_type, e
            ));
            empty_cel_map()
        }
    }
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

/// Validate a `code` value against the schema-declared `^[a-z][a-z0-9_]*$`.
/// Local function rather than pulling in `regex` for one tiny check.
fn is_snake_case_code(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// HTTP reason-phrase for the small set of 4xx codes a `cel` deny is likely
/// to use (RFC 9110 §15.5). Falls back to "Forbidden" — denying access is the
/// dominant case and the reason-phrase is not load-bearing for clients anyway.
fn http_reason_phrase(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        _ => "Forbidden",
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

    pub fn log_warn(msg: &str) {
        #[link(wasm_import_module = "barbacane")]
        extern "C" {
            fn host_log(level: i32, msg_ptr: i32, msg_len: i32);
        }
        unsafe { host_log(2, msg.as_ptr() as i32, msg.len() as i32) }
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod host {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    thread_local! {
        static CONTEXT: RefCell<BTreeMap<String, String>> = const { RefCell::new(BTreeMap::new()) };
        static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub fn context_set(key: &str, value: &str) {
        CONTEXT.with(|ctx| {
            ctx.borrow_mut().insert(key.to_string(), value.to_string());
        });
    }

    pub fn log_warn(msg: &str) {
        WARNINGS.with(|w| w.borrow_mut().push(msg.to_string()));
    }

    #[cfg(test)]
    pub fn get_context() -> BTreeMap<String, String> {
        CONTEXT.with(|ctx| ctx.borrow().clone())
    }

    #[cfg(test)]
    pub fn reset_context() {
        CONTEXT.with(|ctx| ctx.borrow_mut().clear());
    }

    #[cfg(test)]
    pub fn take_warnings() -> Vec<String> {
        WARNINGS.with(|w| std::mem::take(&mut *w.borrow_mut()))
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
            on_match: Some(OnMatch {
                set_context,
                deny: None,
            }),
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

    // --- request.body_json access (ADR-0030) ---

    #[test]
    fn eval_body_json_field_access() {
        let mut config = create_config("request.body_json.foo == 'bar'");
        let mut req = create_request();
        req.body = Some(br#"{"foo":"bar"}"#.to_vec());
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_body_json_ai_consumer_policy_example() {
        // The motivating ADR-0030 example: per-tier model gating using `on_match.deny`.
        let json = r#"{
            "expression": "request.body_json.model.startsWith('gpt-4o') && request.claims.tier != 'premium'",
            "on_match": {
                "deny": {
                    "status": 403,
                    "code": "model_not_permitted"
                }
            }
        }"#;
        let mut config: CelPolicy = serde_json::from_str(json).expect("config parses");

        // Free tier asking for gpt-4o-mini → expression matches → 403 model_not_permitted.
        let mut req_blocked = create_request();
        req_blocked.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"tier":"free"}"#.to_string(),
        );
        req_blocked.body = Some(br#"{"model":"gpt-4o-mini"}"#.to_vec());
        match config.on_request(req_blocked) {
            Action::Continue(_) => panic!("expected 403 for free tier on gpt-4o-mini"),
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 403);
                let body: serde_json::Value =
                    serde_json::from_slice(&resp.body.unwrap()).expect("problem+json");
                assert_eq!(body["code"], "model_not_permitted");
                assert_eq!(body["type"], "urn:barbacane:error:model_not_permitted");
                assert_eq!(body["status"], 403);
            }
        }

        // Premium tier asking for gpt-4o → expression false → continue.
        let mut req_allowed = create_request();
        req_allowed.headers.insert(
            "x-auth-claims".to_string(),
            r#"{"tier":"premium"}"#.to_string(),
        );
        req_allowed.body = Some(br#"{"model":"gpt-4o"}"#.to_vec());
        match config.on_request(req_allowed) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn on_match_deny_default_status_is_403() {
        // `status` omitted → defaults to 403.
        let json = r#"{
            "expression": "true",
            "on_match": { "deny": { "code": "model_not_permitted" } }
        }"#;
        let mut config: CelPolicy = serde_json::from_str(json).expect("config parses");
        match config.on_request(create_request()) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn on_match_deny_honors_custom_status() {
        // Operator can pick a non-403 status (e.g. 429 for budget exhaustion).
        let json = r#"{
            "expression": "true",
            "on_match": { "deny": { "status": 429, "code": "budget_exhausted" } }
        }"#;
        let mut config: CelPolicy = serde_json::from_str(json).expect("config parses");
        match config.on_request(create_request()) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => {
                assert_eq!(resp.status, 429);
                let body: serde_json::Value =
                    serde_json::from_slice(&resp.body.unwrap()).expect("problem+json");
                assert_eq!(body["title"], "Too Many Requests");
                assert_eq!(body["code"], "budget_exhausted");
            }
        }
    }

    #[test]
    fn on_match_deny_falls_back_to_403_for_non_4xx_status() {
        // 500 would mask a policy decision as a server fault — clamp to 403.
        let mut config = CelPolicy {
            expression: "true".to_string(),
            deny_message: default_deny_message(),
            compiled: None,
            on_match: Some(OnMatch {
                set_context: BTreeMap::new(),
                deny: Some(DenyAction {
                    status: 500,
                    code: "oops".to_string(),
                    message: None,
                }),
            }),
        };
        match config.on_request(create_request()) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
    }

    #[test]
    fn on_match_deny_uses_message_when_provided() {
        let json = r#"{
            "expression": "true",
            "on_match": {
                "deny": {
                    "code": "model_not_permitted",
                    "message": "gpt-4o is reserved for premium tier"
                }
            }
        }"#;
        let mut config: CelPolicy = serde_json::from_str(json).expect("config parses");
        match config.on_request(create_request()) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => {
                let body: serde_json::Value =
                    serde_json::from_slice(&resp.body.unwrap()).expect("problem+json");
                assert_eq!(body["detail"], "gpt-4o is reserved for premium tier");
            }
        }
    }

    #[test]
    fn on_match_deny_falls_back_to_code_for_detail_when_message_omitted() {
        let json = r#"{
            "expression": "true",
            "on_match": { "deny": { "code": "model_not_permitted" } }
        }"#;
        let mut config: CelPolicy = serde_json::from_str(json).expect("config parses");
        match config.on_request(create_request()) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => {
                let body: serde_json::Value =
                    serde_json::from_slice(&resp.body.unwrap()).expect("problem+json");
                assert_eq!(body["detail"], "model_not_permitted");
            }
        }
    }

    #[test]
    fn on_match_deny_wins_over_set_context() {
        // When both are configured, a denied request must NOT have its context
        // mutated — operators rely on this to avoid leaking partial state to
        // downstream plugins for a request that was rejected.
        host::reset_context();
        let json = r#"{
            "expression": "true",
            "on_match": {
                "set_context": { "ai.policy": "should-not-be-set" },
                "deny": { "code": "model_not_permitted" }
            }
        }"#;
        let mut config: CelPolicy = serde_json::from_str(json).expect("config parses");
        match config.on_request(create_request()) {
            Action::Continue(_) => panic!("expected deny"),
            Action::ShortCircuit(resp) => assert_eq!(resp.status, 403),
        }
        let ctx = host::get_context();
        assert!(
            !ctx.contains_key("ai.policy"),
            "deny should not write context, found {:?}",
            ctx
        );
    }

    #[test]
    fn on_match_deny_no_op_when_expression_false() {
        // Expression false → continue regardless of `on_match.deny`. Matches the
        // existing `set_context` semantics and the ADR's "match-and-take-action"
        // reading.
        let json = r#"{
            "expression": "false",
            "on_match": { "deny": { "code": "model_not_permitted" } }
        }"#;
        let mut config: CelPolicy = serde_json::from_str(json).expect("config parses");
        match config.on_request(create_request()) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got {}", resp.status),
        }
    }

    #[test]
    fn on_match_deny_invalid_code_returns_500() {
        // Schema declares ^[a-z][a-z0-9_]*$ for code, but the auto-generated
        // vacuum validator doesn't recurse into on_match.deny — so the regex
        // is enforced in Rust at first-request time. Returns a config error.
        for bad in [
            "Bad-Code",          // hyphen
            "BadCode",           // PascalCase
            "1leading_digit",    // digit start
            "_leading_underscore",
            "has space",
            "",
        ] {
            let mut config = CelPolicy {
                expression: "true".to_string(),
                deny_message: default_deny_message(),
                compiled: None,
                on_match: Some(OnMatch {
                    set_context: BTreeMap::new(),
                    deny: Some(DenyAction {
                        status: 403,
                        code: bad.to_string(),
                        message: None,
                    }),
                }),
            };
            match config.on_request(create_request()) {
                Action::Continue(_) => panic!("expected 500 for invalid code {:?}", bad),
                Action::ShortCircuit(resp) => {
                    assert_eq!(resp.status, 500, "code {:?} should be rejected", bad);
                    let body: serde_json::Value =
                        serde_json::from_slice(&resp.body.unwrap()).expect("problem+json");
                    assert_eq!(body["type"], "urn:barbacane:error:cel-config");
                }
            }
        }
    }

    #[test]
    fn on_match_deny_accepts_valid_snake_case_codes() {
        for ok in [
            "model_not_permitted",
            "x",
            "a1",
            "z9_a_b_c",
        ] {
            let mut config = CelPolicy {
                expression: "true".to_string(),
                deny_message: default_deny_message(),
                compiled: None,
                on_match: Some(OnMatch {
                    set_context: BTreeMap::new(),
                    deny: Some(DenyAction {
                        status: 403,
                        code: ok.to_string(),
                        message: None,
                    }),
                }),
            };
            match config.on_request(create_request()) {
                Action::Continue(_) => panic!("expected deny short-circuit for code {:?}", ok),
                Action::ShortCircuit(resp) => {
                    assert_eq!(resp.status, 403, "code {:?} should be accepted", ok);
                }
            }
        }
    }

    #[test]
    fn on_match_unknown_field_is_rejected() {
        // Regression test: previously OnMatch silently accepted unknown fields,
        // so `on_match: { deny: {...} }` was a no-op against a plugin that only
        // knew `set_context`. Now both OnMatch and DenyAction reject unknown
        // fields explicitly so operator typos surface at config-load time.
        let json = r#"{
            "expression": "true",
            "on_match": { "deny_typo": { "code": "x" } }
        }"#;
        let err = match serde_json::from_str::<CelPolicy>(json) {
            Ok(_) => panic!("expected unknown field rejection"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("deny_typo"),
            "error should mention the unknown field: {}",
            err
        );
    }

    #[test]
    fn eval_body_json_vendor_plus_json_content_type() {
        let mut config = create_config("request.body_json.kind == 'event'");
        let mut req = create_request();
        req.headers.insert(
            "content-type".to_string(),
            "application/vnd.api+json".to_string(),
        );
        req.body = Some(br#"{"kind":"event"}"#.to_vec());
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_body_json_content_type_with_charset_param() {
        let mut config = create_config("request.body_json.foo == 'bar'");
        let mut req = create_request();
        req.headers.insert(
            "content-type".to_string(),
            "application/json; charset=utf-8".to_string(),
        );
        req.body = Some(br#"{"foo":"bar"}"#.to_vec());
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_body_json_non_json_content_type_yields_empty_map() {
        // text/plain → body_json is an empty map → has() returns false, not an error.
        let mut config = create_config("!has(request.body_json.foo)");
        let mut req = create_request();
        req.headers
            .insert("content-type".to_string(), "text/plain".to_string());
        req.body = Some(b"this is not json".to_vec());
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }
    }

    #[test]
    fn eval_body_json_malformed_body_logs_warning_and_yields_empty_map() {
        // Malformed JSON with a JSON content-type must NOT short-circuit the request —
        // a CEL plugin that 500s on every garbled body would let an attacker take down
        // every downstream policy with one bad byte. Instead: empty map + log warning.
        let _ = host::take_warnings(); // clear any prior test's warnings

        let mut config = create_config("!has(request.body_json.foo)");
        let mut req = create_request();
        req.body = Some(b"not-actually-json{".to_vec());
        match config.on_request(req) {
            Action::Continue(_) => {}
            Action::ShortCircuit(resp) => panic!("expected continue, got status {}", resp.status),
        }

        let warnings = host::take_warnings();
        assert!(
            warnings.iter().any(|w| w.contains("could not be parsed as JSON")),
            "expected a parse-failure warning, got {:?}",
            warnings
        );
    }

    #[test]
    fn eval_body_json_empty_body_yields_empty_map() {
        let mut config = create_config("!has(request.body_json.foo)");
        let req = create_request(); // body is None
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
