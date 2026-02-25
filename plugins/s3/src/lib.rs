//! AWS S3 / S3-compatible object storage dispatcher plugin for Barbacane API gateway.
//!
//! Proxies requests to S3 (or any S3-compatible endpoint such as MinIO / Ceph)
//! with AWS Signature Version 4 signing.
//!
//! # Supported URL styles
//! - **Virtual-hosted** (default): `{bucket}.s3.{region}.amazonaws.com/{key}`
//! - **Path-style**: `s3.{region}.amazonaws.com/{bucket}/{key}`
//! - **Custom endpoint** (always path-style): `{endpoint}/{bucket}/{key}`
//!
//! # Binary response bodies
//! The plugin SDK's `Response` type uses `Option<String>`. Binary objects
//! (images, PDFs, etc.) whose bodies are not valid UTF-8 will be returned
//! with an empty body. Use a byte-range request or a pre-signed URL to
//! download binary content directly.

use barbacane_plugin_sdk::prelude::*;
use barbacane_sigv4 as sigv4;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// S3 dispatcher configuration.
#[barbacane_dispatcher]
#[derive(Deserialize)]
pub struct S3Dispatcher {
    // ── AWS credentials ────────────────────────────────────────────────────
    /// AWS access key ID.
    access_key_id: String,

    /// AWS secret access key.
    secret_access_key: String,

    /// Session token for temporary credentials (STS / AssumeRole / IRSA).
    #[serde(default)]
    session_token: Option<String>,

    // ── AWS / endpoint config ──────────────────────────────────────────────
    /// AWS region (e.g. `us-east-1`).
    region: String,

    /// Custom S3-compatible endpoint (e.g. `https://minio.internal:9000`).
    /// When set, path-style URLs are always used.
    #[serde(default)]
    endpoint: Option<String>,

    /// Use path-style URLs (`s3.{region}.amazonaws.com/{bucket}/{key}`)
    /// instead of virtual-hosted style.
    /// Custom endpoints always use path-style regardless of this flag.
    #[serde(default)]
    force_path_style: bool,

    // ── Bucket / key resolution ────────────────────────────────────────────
    /// Hard-coded bucket name.
    /// When set, `bucket_param` is ignored.
    /// Use for single-bucket routes such as `/assets/{key+}`.
    #[serde(default)]
    bucket: Option<String>,

    /// Name of the path parameter that holds the bucket (default: `"bucket"`).
    #[serde(default = "default_bucket_param")]
    bucket_param: String,

    /// Name of the path parameter that holds the object key (default: `"key"`).
    /// For multi-segment keys use a wildcard parameter (`{key+}` in the route).
    #[serde(default = "default_key_param")]
    key_param: String,

    // ── Request options ────────────────────────────────────────────────────
    /// Request timeout in seconds (default: 30).
    #[serde(default = "default_timeout")]
    timeout: f64,
}

fn default_bucket_param() -> String {
    "bucket".to_string()
}

fn default_key_param() -> String {
    "key".to_string()
}

fn default_timeout() -> f64 {
    30.0
}

/// HTTP request for `host_http_call`.
#[derive(Serialize)]
struct HttpRequest {
    method: String,
    url: String,
    headers: BTreeMap<String, String>,
    body: Option<String>,
    timeout_ms: Option<u64>,
}

/// HTTP response from `host_http_call`.
#[derive(Deserialize)]
struct HttpResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Option<Vec<u8>>,
}

impl S3Dispatcher {
    /// Build a signed S3 `HttpRequest` without performing any I/O.
    ///
    /// All inputs are passed explicitly (including `unix_secs`) so this function
    /// is pure and testable — callers can verify the URL, Host header, and signed
    /// headers without going through `host_http_call`.
    #[allow(clippy::too_many_arguments)] // all args are distinct; a wrapper struct adds boilerplate with no clarity gain
    fn build_s3_request(
        &self,
        bucket: &str,
        key: &str,
        method: &str,
        query: Option<&str>,
        body: Option<&str>,
        incoming_headers: &BTreeMap<String, String>,
        unix_secs: u64,
    ) -> HttpRequest {
        let (datetime, date) = sigv4::format_datetime(unix_secs);

        // ── Body hash ──────────────────────────────────────────────────────
        let body_bytes = body.unwrap_or("").as_bytes();
        let body_sha256 = sigv4::sha256_hex(body_bytes);

        // ── URL style + Host ───────────────────────────────────────────────
        let use_path_style = self.force_path_style || self.endpoint.is_some();

        let (host, s3_path, base_url) = if use_path_style {
            match &self.endpoint {
                Some(ep) => {
                    // Custom endpoint: extract host for the Host header; keep
                    // full endpoint URL as the base for the outbound request.
                    let without_scheme = ep
                        .trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .trim_end_matches('/');
                    let host = without_scheme.to_string();
                    let path = format!("/{}/{}", bucket, key);
                    let base = ep.trim_end_matches('/').to_string();
                    (host, path.clone(), format!("{}{}", base, path))
                }
                None => {
                    let host = format!("s3.{}.amazonaws.com", self.region);
                    let path = format!("/{}/{}", bucket, key);
                    let base = format!("https://s3.{}.amazonaws.com", self.region);
                    (host, path.clone(), format!("{}{}", base, path))
                }
            }
        } else {
            // Virtual-hosted style
            let host = format!("{}.s3.{}.amazonaws.com", bucket, self.region);
            let path = format!("/{}", key);
            let base = format!("https://{}", host);
            (host, path.clone(), format!("{}{}", base, path))
        };

        // Append query string to final URL
        let full_url = match query {
            Some(qs) if !qs.is_empty() => format!("{}?{}", base_url, qs),
            _ => base_url,
        };

        // ── Build headers to sign ──────────────────────────────────────────
        // Keys must be lowercase; BTreeMap ensures sorted order for SigV4.
        let mut headers_to_sign = BTreeMap::new();
        headers_to_sign.insert("host".to_string(), host.clone());
        headers_to_sign.insert("x-amz-content-sha256".to_string(), body_sha256.clone());
        headers_to_sign.insert("x-amz-date".to_string(), datetime.clone());
        if let Some(token) = &self.session_token {
            headers_to_sign.insert("x-amz-security-token".to_string(), token.clone());
        }

        // ── Sign ───────────────────────────────────────────────────────────
        let creds = sigv4::Credentials {
            access_key_id: self.access_key_id.clone(),
            secret_access_key: self.secret_access_key.clone(),
            session_token: self.session_token.clone(),
        };
        let signing_config = sigv4::SigningConfig {
            region: &self.region,
            service: "s3",
        };
        let canonical_uri_str = sigv4::canonical_uri(&s3_path);
        let canonical_query_str = sigv4::canonical_query(query);
        let signing_input = sigv4::SigningInput {
            method,
            canonical_uri: &canonical_uri_str,
            canonical_query: &canonical_query_str,
            headers_to_sign: &headers_to_sign,
            body_sha256: &body_sha256,
            datetime: &datetime,
            date: &date,
        };
        let signed = sigv4::sign(&signing_input, &creds, &signing_config);

        // ── Build outbound headers ─────────────────────────────────────────
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), host);
        headers.insert("x-amz-date".to_string(), signed.x_amz_date);
        headers.insert(
            "x-amz-content-sha256".to_string(),
            signed.x_amz_content_sha256,
        );
        headers.insert("authorization".to_string(), signed.authorization);
        if let Some(token) = signed.x_amz_security_token {
            headers.insert("x-amz-security-token".to_string(), token);
        }
        // Forward content-type for uploads (PUT / POST)
        if let Some(ct) = incoming_headers.get("content-type") {
            headers.insert("content-type".to_string(), ct.clone());
        }

        HttpRequest {
            method: method.to_string(),
            url: full_url,
            headers,
            body: body.map(|s| s.to_string()),
            timeout_ms: Some((self.timeout * 1000.0) as u64),
        }
    }

    /// Build and sign an S3 request, then proxy it via `host_http_call`.
    pub fn dispatch(&mut self, req: Request) -> Response {
        // ── 1. Resolve bucket ──────────────────────────────────────────────
        let bucket = match self
            .bucket
            .as_deref()
            .or_else(|| req.path_params.get(&self.bucket_param).map(|s| s.as_str()))
        {
            Some(b) => b.to_string(),
            None => {
                return self.error_response(
                    400,
                    "Bad Request",
                    "missing bucket",
                    &format!("path param '{}' not found", self.bucket_param),
                )
            }
        };

        // ── 2. Resolve key ─────────────────────────────────────────────────
        let key = match req.path_params.get(&self.key_param) {
            Some(k) => k.clone(),
            None => {
                return self.error_response(
                    400,
                    "Bad Request",
                    "missing key",
                    &format!("path param '{}' not found", self.key_param),
                )
            }
        };

        // ── 3. Build signed request ────────────────────────────────────────
        let unix_secs = current_timestamp();
        let http_request = self.build_s3_request(
            &bucket,
            &key,
            &req.method,
            req.query.as_deref(),
            req.body.as_deref(),
            &req.headers,
            unix_secs,
        );

        // ── 4. Serialize and call S3 ───────────────────────────────────────
        let request_json = match serde_json::to_vec(&http_request) {
            Ok(json) => json,
            Err(e) => {
                return self.error_response(
                    500,
                    "Internal Error",
                    "failed to serialize request",
                    &e.to_string(),
                )
            }
        };

        let result_len =
            unsafe { host_http_call(request_json.as_ptr() as i32, request_json.len() as i32) };

        if result_len < 0 {
            return self.error_response(
                502,
                "Bad Gateway",
                "S3 connection failed",
                "host_http_call returned error",
            );
        }

        let mut response_buf = vec![0u8; result_len as usize];
        let bytes_read =
            unsafe { host_http_read_result(response_buf.as_mut_ptr() as i32, result_len) };

        if bytes_read <= 0 {
            return self.error_response(
                502,
                "Bad Gateway",
                "S3 connection failed",
                "failed to read response",
            );
        }

        let http_response: HttpResponse =
            match serde_json::from_slice(&response_buf[..bytes_read as usize]) {
                Ok(resp) => resp,
                Err(e) => {
                    return self.error_response(
                        502,
                        "Bad Gateway",
                        "invalid S3 response",
                        &e.to_string(),
                    )
                }
            };

        // ── 5. Pass through response ───────────────────────────────────────
        // Filter hop-by-hop headers; pass S3 error codes through transparently.
        let mut response_headers = BTreeMap::new();
        for (key, value) in http_response.headers {
            let key_lower = key.to_lowercase();
            if !matches!(
                key_lower.as_str(),
                "connection" | "keep-alive" | "transfer-encoding" | "te" | "trailer" | "upgrade"
            ) {
                response_headers.insert(key, value);
            }
        }

        // Note: Binary response bodies that are not valid UTF-8 are omitted.
        // See module-level documentation for workarounds.
        let body = http_response.body.and_then(|b| String::from_utf8(b).ok());

        Response {
            status: http_response.status,
            headers: response_headers,
            body,
        }
    }

    /// Create an error response in RFC 9457 Problem Details format.
    fn error_response(&self, status: u16, title: &str, detail: &str, debug: &str) -> Response {
        let error_type = match status {
            400 => "urn:barbacane:error:bad-request",
            502 => "urn:barbacane:error:upstream-unavailable",
            _ => "urn:barbacane:error:internal",
        };

        let full_detail = format!("{}: {}", detail, debug);
        let body = serde_json::json!({
            "type": error_type,
            "title": title,
            "status": status,
            "detail": full_detail,
        });

        let mut headers = BTreeMap::new();
        headers.insert(
            "content-type".to_string(),
            "application/problem+json".to_string(),
        );

        Response {
            status,
            headers,
            body: Some(body.to_string()),
        }
    }
}

// ── Host function declarations ─────────────────────────────────────────────

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "barbacane")]
extern "C" {
    /// Make an HTTP request. Returns the response length, or -1 on error.
    fn host_http_call(req_ptr: i32, req_len: i32) -> i32;

    /// Read the HTTP response into the provided buffer. Returns bytes read.
    fn host_http_read_result(buf_ptr: i32, buf_len: i32) -> i32;
}

// Native stubs for testing (non-WASM targets)
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_call(_req_ptr: i32, _req_len: i32) -> i32 {
    -1
}

#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_http_read_result(_buf_ptr: i32, _buf_len: i32) -> i32 {
    0
}

// ── Unix timestamp ─────────────────────────────────────────────────────────

/// Get current Unix timestamp (WASM: host function; native: system clock).
#[cfg(target_arch = "wasm32")]
fn current_timestamp() -> u64 {
    #[link(wasm_import_module = "barbacane")]
    extern "C" {
        fn host_get_unix_timestamp() -> u64;
    }
    unsafe { host_get_unix_timestamp() }
}

#[cfg(not(target_arch = "wasm32"))]
fn current_timestamp() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time is after Unix epoch")
        .as_secs()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Fixed timestamp: 2013-05-24T00:00:00Z — same as AWS SigV4 test suite.
    // Use this in all build_s3_request tests for deterministic output.
    const TEST_TS: u64 = 1_369_353_600;

    fn make_dispatcher(bucket: Option<&str>, endpoint: Option<&str>) -> S3Dispatcher {
        S3Dispatcher {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            session_token: None,
            region: "us-east-1".to_string(),
            endpoint: endpoint.map(|s| s.to_string()),
            force_path_style: false,
            bucket: bucket.map(|s| s.to_string()),
            bucket_param: "bucket".to_string(),
            key_param: "key".to_string(),
            timeout: 30.0,
        }
    }

    fn make_get_request(path_params: BTreeMap<String, String>) -> Request {
        Request {
            method: "GET".to_string(),
            path: "/".to_string(),
            headers: BTreeMap::new(),
            body: None,
            query: None,
            path_params,
            client_ip: "127.0.0.1".to_string(),
        }
    }

    // ── Config deserialization ─────────────────────────────────────────────

    #[test]
    fn test_config_minimal() {
        let json = r#"{
            "access_key_id": "AKIA...",
            "secret_access_key": "secret",
            "region": "us-east-1"
        }"#;
        let cfg: S3Dispatcher = serde_json::from_str(json).expect("deserialize");
        assert_eq!(cfg.access_key_id, "AKIA...");
        assert_eq!(cfg.region, "us-east-1");
        assert!(cfg.bucket.is_none());
        assert_eq!(cfg.bucket_param, "bucket");
        assert_eq!(cfg.key_param, "key");
        assert!(!cfg.force_path_style);
        assert_eq!(cfg.timeout, 30.0);
    }

    #[test]
    fn test_config_full() {
        let json = r#"{
            "access_key_id": "AKIA...",
            "secret_access_key": "secret",
            "session_token": "token",
            "region": "eu-west-1",
            "endpoint": "https://minio.internal:9000",
            "force_path_style": true,
            "bucket": "my-bucket",
            "bucket_param": "bkt",
            "key_param": "obj",
            "timeout": 60.0
        }"#;
        let cfg: S3Dispatcher = serde_json::from_str(json).expect("deserialize");
        assert_eq!(cfg.session_token, Some("token".to_string()));
        assert_eq!(cfg.endpoint, Some("https://minio.internal:9000".to_string()));
        assert!(cfg.force_path_style);
        assert_eq!(cfg.bucket, Some("my-bucket".to_string()));
        assert_eq!(cfg.bucket_param, "bkt");
        assert_eq!(cfg.key_param, "obj");
        assert_eq!(cfg.timeout, 60.0);
    }

    #[test]
    fn test_config_missing_required_fields() {
        // region is required
        let json = r#"{"access_key_id": "AKIA...", "secret_access_key": "secret"}"#;
        let result: Result<S3Dispatcher, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }

    // ── Error / validation (dispatch level) ───────────────────────────────

    #[test]
    fn test_missing_key_returns_400() {
        let mut d = make_dispatcher(Some("my-bucket"), None);
        let req = make_get_request(BTreeMap::new());
        let resp = d.dispatch(req);
        assert_eq!(resp.status, 400);
        let body: serde_json::Value =
            serde_json::from_str(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:bad-request");
    }

    #[test]
    fn test_missing_bucket_returns_400() {
        let mut d = make_dispatcher(None, None);
        let mut params = BTreeMap::new();
        params.insert("key".to_string(), "my-file.txt".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 400);
        let body: serde_json::Value =
            serde_json::from_str(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:bad-request");
    }

    #[test]
    fn test_dispatch_returns_502_on_native() {
        // host_http_call stub always returns -1 on native → 502
        let mut d = make_dispatcher(Some("my-bucket"), None);
        let mut params = BTreeMap::new();
        params.insert("key".to_string(), "my-file.txt".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 502);
        let body: serde_json::Value =
            serde_json::from_str(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:upstream-unavailable");
    }

    // ── Bucket resolution (dispatch level) ────────────────────────────────

    #[test]
    fn test_bucket_from_config_overrides_param() {
        // Config bucket takes precedence; reaching 502 proves bucket resolved OK.
        let mut d = make_dispatcher(Some("config-bucket"), None);
        let mut params = BTreeMap::new();
        params.insert("bucket".to_string(), "param-bucket".to_string());
        params.insert("key".to_string(), "file.txt".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 502);
    }

    #[test]
    fn test_bucket_from_param() {
        let mut d = make_dispatcher(None, None);
        let mut params = BTreeMap::new();
        params.insert("bucket".to_string(), "param-bucket".to_string());
        params.insert("key".to_string(), "file.txt".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 502);
    }

    // ── URL construction (build_s3_request) ───────────────────────────────

    #[test]
    fn test_virtual_hosted_url_construction() {
        let d = make_dispatcher(None, None);
        let req = d.build_s3_request(
            "my-bucket",
            "my-key.txt",
            "GET",
            None,
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        assert_eq!(
            req.url,
            "https://my-bucket.s3.us-east-1.amazonaws.com/my-key.txt"
        );
        assert_eq!(
            req.headers["host"],
            "my-bucket.s3.us-east-1.amazonaws.com"
        );
        // Credential scope must reference correct region and service
        assert!(req.headers["authorization"]
            .contains("Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"));
        assert!(req.headers["authorization"]
            .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date"));
        assert_eq!(req.method, "GET");
    }

    #[test]
    fn test_path_style_url_construction() {
        let mut d = make_dispatcher(None, None);
        d.force_path_style = true;
        let req = d.build_s3_request(
            "my-bucket",
            "my-key.txt",
            "GET",
            None,
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        assert_eq!(
            req.url,
            "https://s3.us-east-1.amazonaws.com/my-bucket/my-key.txt"
        );
        assert_eq!(req.headers["host"], "s3.us-east-1.amazonaws.com");
        assert!(req.headers["authorization"].contains("/us-east-1/s3/aws4_request"));
    }

    #[test]
    fn test_custom_endpoint_url_construction() {
        // Custom endpoint always uses path-style
        let d = make_dispatcher(Some("uploads"), Some("https://minio.internal:9000"));
        let req = d.build_s3_request(
            "uploads",
            "data/file.csv",
            "GET",
            None,
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        assert_eq!(
            req.url,
            "https://minio.internal:9000/uploads/data/file.csv"
        );
        assert_eq!(req.headers["host"], "minio.internal:9000");
    }

    #[test]
    fn test_custom_endpoint_http_scheme() {
        // http:// (not https://) endpoint — host extraction must strip the right prefix
        let d = make_dispatcher(Some("test"), Some("http://localhost:9000"));
        let req = d.build_s3_request(
            "test",
            "file.txt",
            "GET",
            None,
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        assert_eq!(req.url, "http://localhost:9000/test/file.txt");
        assert_eq!(req.headers["host"], "localhost:9000");
    }

    // ── Request content (build_s3_request) ────────────────────────────────

    #[test]
    fn test_timestamp_propagated_to_amz_date() {
        let d = make_dispatcher(Some("bucket"), None);
        let req =
            d.build_s3_request("bucket", "k", "GET", None, None, &BTreeMap::new(), TEST_TS);
        assert_eq!(req.headers["x-amz-date"], "20130524T000000Z");
    }

    #[test]
    fn test_put_body_sha256() {
        let d = make_dispatcher(Some("bucket"), None);
        let body = "Hello from Barbacane!";
        let req = d.build_s3_request(
            "bucket",
            "hello.txt",
            "PUT",
            None,
            Some(body),
            &BTreeMap::new(),
            TEST_TS,
        );
        let expected_hash = sigv4::sha256_hex(body.as_bytes());
        // Body hash must reflect the actual body content, not the empty-body hash
        assert_eq!(req.headers["x-amz-content-sha256"], expected_hash);
        assert_ne!(
            expected_hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            "non-empty body must not produce the empty-string hash"
        );
        assert_eq!(req.body, Some(body.to_string()));
        assert_eq!(req.method, "PUT");
    }

    #[test]
    fn test_empty_body_hash_for_get() {
        let d = make_dispatcher(Some("bucket"), None);
        let req =
            d.build_s3_request("bucket", "k", "GET", None, None, &BTreeMap::new(), TEST_TS);
        assert_eq!(
            req.headers["x-amz-content-sha256"],
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_query_string_appended_to_url() {
        let d = make_dispatcher(Some("bucket"), None);
        let req = d.build_s3_request(
            "bucket",
            "prefix/",
            "GET",
            Some("list-type=2&prefix=logs%2F"),
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        assert!(
            req.url.contains("?list-type=2&prefix=logs%2F"),
            "query string must be appended to the URL: {}",
            req.url
        );
    }

    #[test]
    fn test_content_type_forwarded() {
        let d = make_dispatcher(Some("bucket"), None);
        let mut incoming = BTreeMap::new();
        incoming.insert("content-type".to_string(), "image/png".to_string());
        let req = d.build_s3_request(
            "bucket",
            "photo.png",
            "PUT",
            None,
            Some("PNG data"),
            &incoming,
            TEST_TS,
        );
        assert_eq!(
            req.headers.get("content-type"),
            Some(&"image/png".to_string())
        );
    }

    #[test]
    fn test_session_token_in_request() {
        let mut d = make_dispatcher(None, None);
        d.session_token = Some("my-session-token".to_string());
        let req = d.build_s3_request(
            "bucket",
            "key.txt",
            "GET",
            None,
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        assert_eq!(
            req.headers.get("x-amz-security-token"),
            Some(&"my-session-token".to_string())
        );
        // Session token must be included in SignedHeaders
        assert!(
            req.headers["authorization"].contains("x-amz-security-token"),
            "x-amz-security-token must appear in SignedHeaders: {}",
            req.headers["authorization"]
        );
    }

    #[test]
    fn test_wildcard_key_slashes_preserved_in_url() {
        // {key+} captures "2024/01/report.pdf" as a single string — slashes must be preserved
        let d = make_dispatcher(None, None);
        let req = d.build_s3_request(
            "assets",
            "2024/01/report.pdf",
            "GET",
            None,
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        assert_eq!(
            req.url,
            "https://assets.s3.us-east-1.amazonaws.com/2024/01/report.pdf"
        );
    }

    #[test]
    fn test_key_with_special_chars_percent_encoded_in_signing() {
        // Spaces and special characters in the key must be percent-encoded in the
        // canonical URI (used for signing), but the raw key appears in the URL.
        let d = make_dispatcher(Some("bucket"), None);
        let req = d.build_s3_request(
            "bucket",
            "my file (1).txt",
            "GET",
            None,
            None,
            &BTreeMap::new(),
            TEST_TS,
        );
        // The URL should contain the key as-is (gateway already decoded it)
        assert!(req.url.contains("my file (1).txt"), "url: {}", req.url);
        // Authorization header must still be present (signing did not panic)
        assert!(req.headers["authorization"].starts_with("AWS4-HMAC-SHA256 "));
    }
}
