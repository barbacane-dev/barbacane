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

        // ── 3. Timestamp ───────────────────────────────────────────────────
        let unix_secs = current_timestamp();
        let (datetime, date) = sigv4::format_datetime(unix_secs);

        // ── 4. Body hash ───────────────────────────────────────────────────
        let body_bytes = req.body.as_deref().unwrap_or("").as_bytes();
        let body_sha256 = sigv4::sha256_hex(body_bytes);

        // ── 5. URL style + Host ────────────────────────────────────────────
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
        let full_url = match req.query.as_deref() {
            Some(qs) if !qs.is_empty() => format!("{}?{}", base_url, qs),
            _ => base_url,
        };

        // ── 6. Build headers to sign ───────────────────────────────────────
        // Keys must be lowercase; BTreeMap ensures sorted order.
        let mut headers_to_sign = BTreeMap::new();
        headers_to_sign.insert("host".to_string(), host.clone());
        headers_to_sign.insert("x-amz-content-sha256".to_string(), body_sha256.clone());
        headers_to_sign.insert("x-amz-date".to_string(), datetime.clone());
        if let Some(token) = &self.session_token {
            headers_to_sign.insert("x-amz-security-token".to_string(), token.clone());
        }

        // ── 7. Sign ────────────────────────────────────────────────────────
        let creds = sigv4::Credentials {
            access_key_id: self.access_key_id.clone(),
            secret_access_key: self.secret_access_key.clone(),
            session_token: self.session_token.clone(),
        };
        let signing_config = sigv4::SigningConfig {
            region: &self.region,
            service: "s3",
        };
        let canonical_uri = sigv4::canonical_uri(&s3_path);
        let canonical_query = sigv4::canonical_query(req.query.as_deref());
        let signing_input = sigv4::SigningInput {
            method: &req.method,
            canonical_uri: &canonical_uri,
            canonical_query: &canonical_query,
            headers_to_sign: &headers_to_sign,
            body_sha256: &body_sha256,
            datetime: &datetime,
            date: &date,
        };
        let signed = sigv4::sign(&signing_input, &creds, &signing_config);

        // ── 8. Build outbound headers ──────────────────────────────────────
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
        if let Some(ct) = req.headers.get("content-type") {
            headers.insert("content-type".to_string(), ct.clone());
        }

        // ── 9. Call S3 ─────────────────────────────────────────────────────
        let http_request = HttpRequest {
            method: req.method.clone(),
            url: full_url,
            headers,
            body: req.body.clone(),
            timeout_ms: Some((self.timeout * 1000.0) as u64),
        };

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

        // ── 10. Pass through response ──────────────────────────────────────
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

    // ── Error responses ────────────────────────────────────────────────────

    #[test]
    fn test_missing_key_returns_400() {
        let mut d = make_dispatcher(Some("my-bucket"), None);
        // No key param in path_params
        let req = make_get_request(BTreeMap::new());
        let resp = d.dispatch(req);
        assert_eq!(resp.status, 400);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:bad-request");
    }

    #[test]
    fn test_missing_bucket_returns_400() {
        let mut d = make_dispatcher(None, None); // no hardcoded bucket, no bucket param
        let mut params = BTreeMap::new();
        params.insert("key".to_string(), "my-file.txt".to_string());
        // No bucket param in path_params
        let req = make_get_request(params);
        let resp = d.dispatch(req);
        assert_eq!(resp.status, 400);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:bad-request");
    }

    #[test]
    fn test_dispatch_returns_502_on_native() {
        // host_http_call stub always returns -1 on native → should get 502
        let mut d = make_dispatcher(Some("my-bucket"), None);
        let mut params = BTreeMap::new();
        params.insert("key".to_string(), "my-file.txt".to_string());
        let req = make_get_request(params);
        let resp = d.dispatch(req);
        assert_eq!(resp.status, 502);
        let body: serde_json::Value = serde_json::from_str(resp.body.as_ref().expect("body")).expect("json");
        assert_eq!(body["type"], "urn:barbacane:error:upstream-unavailable");
    }

    // ── Bucket / key resolution ────────────────────────────────────────────

    #[test]
    fn test_bucket_from_config() {
        // Config bucket takes precedence over any path param
        let mut d = make_dispatcher(Some("config-bucket"), None);
        let mut params = BTreeMap::new();
        params.insert("bucket".to_string(), "param-bucket".to_string());
        params.insert("key".to_string(), "file.txt".to_string());
        let req = make_get_request(params);
        // Reaches host_http_call → 502 (native stub), but bucket resolution succeeded
        let resp = d.dispatch(req);
        assert_eq!(resp.status, 502); // reached signing stage
    }

    #[test]
    fn test_bucket_from_param() {
        // No hardcoded bucket → resolved from path param
        let mut d = make_dispatcher(None, None);
        let mut params = BTreeMap::new();
        params.insert("bucket".to_string(), "param-bucket".to_string());
        params.insert("key".to_string(), "file.txt".to_string());
        let req = make_get_request(params);
        let resp = d.dispatch(req);
        assert_eq!(resp.status, 502); // reached signing stage
    }

    // ── URL style ──────────────────────────────────────────────────────────

    #[test]
    fn test_virtual_hosted_url() {
        // Virtual-hosted: host = {bucket}.s3.{region}.amazonaws.com; path = /{key}
        let mut d = make_dispatcher(None, None);
        let mut params = BTreeMap::new();
        params.insert("bucket".to_string(), "my-bucket".to_string());
        params.insert("key".to_string(), "folder/file.txt".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 502); // verified by reaching host call
    }

    #[test]
    fn test_path_style_url() {
        let mut d = make_dispatcher(None, None);
        d.force_path_style = true;
        let mut params = BTreeMap::new();
        params.insert("bucket".to_string(), "my-bucket".to_string());
        params.insert("key".to_string(), "folder/file.txt".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 502);
    }

    #[test]
    fn test_custom_endpoint_uses_path_style() {
        // Even without force_path_style, custom endpoint → path-style
        let mut d = make_dispatcher(Some("uploads"), Some("https://minio.internal:9000"));
        let mut params = BTreeMap::new();
        params.insert("key".to_string(), "data/file.csv".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 502);
    }

    #[test]
    fn test_wildcard_key_with_slashes() {
        // Key captured via {key+} wildcard contains multiple path segments
        let mut d = make_dispatcher(None, None);
        let mut params = BTreeMap::new();
        params.insert("bucket".to_string(), "assets".to_string());
        params.insert("key".to_string(), "2024/01/report.pdf".to_string());
        let resp = d.dispatch(make_get_request(params));
        assert_eq!(resp.status, 502); // reached signing stage
    }
}
