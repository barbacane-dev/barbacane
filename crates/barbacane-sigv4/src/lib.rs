//! AWS Signature Version 4 (SigV4) signing primitives.
//!
//! Standalone library crate shared by the `s3` dispatcher and the future
//! `aws-sigv4` middleware plugin. Has no WASM-specific dependencies — it
//! compiles for both native and `wasm32-wasip1` targets.

use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// --- Public types ---

/// AWS credentials.
pub struct Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
    /// For STS / AssumeRole / IRSA temporary credentials.
    pub session_token: Option<String>,
}

/// Signing configuration (region + service).
pub struct SigningConfig<'a> {
    pub region: &'a str,
    pub service: &'a str,
}

/// Input to the signing function.
pub struct SigningInput<'a> {
    /// HTTP method (will be uppercased).
    pub method: &'a str,
    /// Canonical URI (already percent-encoded, slashes preserved).
    pub canonical_uri: &'a str,
    /// Canonical query string (pre-sorted, pre-encoded). Empty string if none.
    pub canonical_query: &'a str,
    /// Headers to sign. Keys **must be lowercase**; must include `"host"`.
    /// `BTreeMap` guarantees lexicographic order required by SigV4.
    pub headers_to_sign: &'a BTreeMap<String, String>,
    /// Pre-computed SHA-256 hex of the request body.
    pub body_sha256: &'a str,
    /// `YYYYMMDDTHHMMSSZ`
    pub datetime: &'a str,
    /// `YYYYMMDD`
    pub date: &'a str,
}

/// Computed signed headers returned by [`sign`].
pub struct SignedHeaders {
    /// Value for the `Authorization` request header.
    pub authorization: String,
    /// Value for the `x-amz-date` request header.
    pub x_amz_date: String,
    /// Value for the `x-amz-content-sha256` request header.
    pub x_amz_content_sha256: String,
    /// Value for `x-amz-security-token` (only present with temporary credentials).
    pub x_amz_security_token: Option<String>,
}

// --- Public functions ---

/// Format a Unix timestamp (seconds since epoch) into SigV4 datetime strings.
///
/// Returns `("YYYYMMDDTHHMMSSZ", "YYYYMMDD")`.
/// Uses integer arithmetic only — no `chrono` or `std::time`.
pub fn format_datetime(unix_secs: u64) -> (String, String) {
    let secs_of_day = unix_secs % 86_400;
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;

    // Howard Hinnant's civil_from_days algorithm
    // Converts days since Unix epoch to (year, month, day).
    let days = unix_secs / 86_400;
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    let datetime = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        year, m, d, hour, minute, second
    );
    let date = format!("{:04}{:02}{:02}", year, m, d);
    (datetime, date)
}

/// Compute SHA-256 of `data` and return the lowercase hex string.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

/// Percent-encode a URI path component for a SigV4 canonical request.
///
/// Encodes all bytes except unreserved characters (`A-Z a-z 0-9 - _ . ~`)
/// and forward slash (`/`). Prepends `/` if the path does not start with one.
pub fn canonical_uri(path: &str) -> String {
    let path = if path.is_empty() || !path.starts_with('/') {
        format!("/{}", path)
    } else {
        path.to_string()
    };

    let mut result = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

/// Build a canonical query string for SigV4 signing.
///
/// Parses `query`, percent-encodes each key and value (unreserved chars only),
/// sorts pairs by key (then value), and joins with `&`.
/// Returns an empty string if `query` is `None` or empty.
pub fn canonical_query(query: Option<&str>) -> String {
    let qs = match query {
        None | Some("") => return String::new(),
        Some(q) => q,
    };

    let mut params: Vec<(String, String)> = qs
        .split('&')
        .filter_map(|part| {
            if part.is_empty() {
                return None;
            }
            let (key, value) = match part.find('=') {
                Some(pos) => (&part[..pos], &part[pos + 1..]),
                None => (part, ""),
            };
            Some((percent_encode_query(key), percent_encode_query(value)))
        })
        .collect();

    params.sort_unstable_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    params
        .iter()
        .map(|(k, v)| format!("{}={}", k, v))
        .collect::<Vec<_>>()
        .join("&")
}

/// Compute the SigV4 `Authorization` header and related signed headers.
///
/// # Panics
/// Never panics — HMAC accepts any key length.
pub fn sign(input: &SigningInput, creds: &Credentials, config: &SigningConfig) -> SignedHeaders {
    // BTreeMap guarantees keys are already sorted; keys must be lowercase.
    let canonical_headers_str: String = input
        .headers_to_sign
        .iter()
        .map(|(k, v)| format!("{}:{}\n", k, v.trim()))
        .collect();

    let signed_headers_str: String = input
        .headers_to_sign
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(";");

    // Canonical request (6 components separated by newlines)
    let canonical_request = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        input.method.to_uppercase(),
        input.canonical_uri,
        input.canonical_query,
        canonical_headers_str,
        signed_headers_str,
        input.body_sha256,
    );

    // Credential scope: YYYYMMDD/region/service/aws4_request
    let credential_scope = format!(
        "{}/{}/{}/aws4_request",
        input.date, config.region, config.service
    );

    // String to sign
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{}",
        input.datetime,
        credential_scope,
        sha256_hex(canonical_request.as_bytes()),
    );

    // Signing key: HMAC chain starting from "AWS4" + secret
    let k_secret = format!("AWS4{}", creds.secret_access_key);
    let k_date = hmac_sha256(k_secret.as_bytes(), input.date.as_bytes());
    let k_region = hmac_sha256(&k_date, config.region.as_bytes());
    let k_service = hmac_sha256(&k_region, config.service.as_bytes());
    let k_signing = hmac_sha256(&k_service, b"aws4_request");

    let signature = hex::encode(hmac_sha256(&k_signing, string_to_sign.as_bytes()));

    // Authorization header (no spaces around commas per AWS spec)
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={},Signature={}",
        creds.access_key_id, credential_scope, signed_headers_str, signature,
    );

    SignedHeaders {
        authorization,
        x_amz_date: input.datetime.to_string(),
        x_amz_content_sha256: input.body_sha256.to_string(),
        x_amz_security_token: creds.session_token.clone(),
    }
}

// --- Private helpers ---

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Percent-encode a query string key or value (SigV4 rules).
/// Encodes all bytes except unreserved characters (`A-Z a-z 0-9 - _ . ~`).
fn percent_encode_query(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    result
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    // AWS SigV4 test suite credentials (from AWS documentation)
    const TEST_ACCESS_KEY: &str = "AKIAIOSFODNN7EXAMPLE";
    const TEST_SECRET_KEY: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    // Unix timestamp for 2013-05-24T00:00:00Z = 1369353600
    const TEST_TIMESTAMP: u64 = 1_369_353_600;

    #[test]
    fn test_format_datetime_known_date() {
        // 2013-05-24T00:00:00Z
        let (datetime, date) = format_datetime(TEST_TIMESTAMP);
        assert_eq!(datetime, "20130524T000000Z");
        assert_eq!(date, "20130524");
    }

    #[test]
    fn test_format_datetime_midnight_seconds() {
        // 2013-05-24T01:02:03Z = 1369353600 + 3723
        let (datetime, _) = format_datetime(TEST_TIMESTAMP + 3723);
        assert_eq!(datetime, "20130524T010203Z");
    }

    #[test]
    fn test_format_datetime_epoch() {
        let (datetime, date) = format_datetime(0);
        assert_eq!(datetime, "19700101T000000Z");
        assert_eq!(date, "19700101");
    }

    #[test]
    fn test_format_datetime_leap_year() {
        // 2024-02-29T00:00:00Z (leap day)
        // Days: 2024-01-01 is 19723 days after epoch; Jan=31, Feb=29 so day 31+29-1=59 → 19723+59=19782
        // 19782 * 86400 = 1709164800
        let (datetime, date) = format_datetime(1_709_164_800);
        assert_eq!(datetime, "20240229T000000Z");
        assert_eq!(date, "20240229");
    }

    #[test]
    fn test_sha256_hex_empty() {
        // SHA-256 of empty string
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_sha256_hex_hello() {
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn test_canonical_uri_simple() {
        assert_eq!(canonical_uri("/test.txt"), "/test.txt");
    }

    #[test]
    fn test_canonical_uri_preserves_slashes() {
        assert_eq!(
            canonical_uri("/folder/subfolder/file.txt"),
            "/folder/subfolder/file.txt"
        );
    }

    #[test]
    fn test_canonical_uri_encodes_spaces() {
        assert_eq!(canonical_uri("/my file.txt"), "/my%20file.txt");
    }

    #[test]
    fn test_canonical_uri_encodes_special_chars() {
        // = + are encoded; - _ . ~ are not
        assert_eq!(canonical_uri("/a=b+c-d_e.f~g"), "/a%3Db%2Bc-d_e.f~g");
    }

    #[test]
    fn test_canonical_uri_prepends_slash() {
        assert_eq!(canonical_uri("test.txt"), "/test.txt");
    }

    #[test]
    fn test_canonical_uri_empty() {
        assert_eq!(canonical_uri(""), "/");
    }

    #[test]
    fn test_canonical_query_empty() {
        assert_eq!(canonical_query(None), "");
        assert_eq!(canonical_query(Some("")), "");
    }

    #[test]
    fn test_canonical_query_single_param() {
        assert_eq!(canonical_query(Some("foo=bar")), "foo=bar");
    }

    #[test]
    fn test_canonical_query_sorted() {
        // AWS SigV4 requires query params sorted by key
        assert_eq!(canonical_query(Some("z=3&a=1&m=2")), "a=1&m=2&z=3");
    }

    #[test]
    fn test_canonical_query_encodes_values() {
        assert_eq!(
            canonical_query(Some("key=hello world")),
            "key=hello%20world"
        );
    }

    #[test]
    fn test_canonical_query_no_value() {
        // Key without value
        assert_eq!(canonical_query(Some("uploads")), "uploads=");
    }

    #[test]
    fn test_sigv4_signing_get_object() {
        // AWS SigV4 test suite: GET Object (byte-range request)
        // From: https://docs.aws.amazon.com/AmazonS3/latest/API/sig-v4-header-based-auth.html
        //
        // Expected Authorization header (no spaces around commas):
        //   AWS4-HMAC-SHA256
        //   Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,
        //   SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,
        //   Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41
        let empty_body_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        // BTreeMap sorts keys: host < range < x-amz-content-sha256 < x-amz-date
        let mut headers = BTreeMap::new();
        headers.insert(
            "host".to_string(),
            "examplebucket.s3.amazonaws.com".to_string(),
        );
        headers.insert("range".to_string(), "bytes=0-9".to_string());
        headers.insert(
            "x-amz-content-sha256".to_string(),
            empty_body_hash.to_string(),
        );
        headers.insert("x-amz-date".to_string(), "20130524T000000Z".to_string());

        let creds = Credentials {
            access_key_id: TEST_ACCESS_KEY.to_string(),
            secret_access_key: TEST_SECRET_KEY.to_string(),
            session_token: None,
        };
        let config = SigningConfig {
            region: "us-east-1",
            service: "s3",
        };
        let input = SigningInput {
            method: "GET",
            canonical_uri: "/test.txt",
            canonical_query: "",
            headers_to_sign: &headers,
            body_sha256: empty_body_hash,
            datetime: "20130524T000000Z",
            date: "20130524",
        };

        let signed = sign(&input, &creds, &config);

        let expected_sig = "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41";
        assert!(
            signed.authorization.contains(expected_sig),
            "Expected signature not found in: {}",
            signed.authorization
        );
        assert!(signed.authorization.starts_with("AWS4-HMAC-SHA256 "));
        assert!(signed
            .authorization
            .contains("Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request"));
        assert!(signed
            .authorization
            .contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
        assert_eq!(signed.x_amz_date, "20130524T000000Z");
        assert_eq!(signed.x_amz_content_sha256, empty_body_hash);
        assert!(signed.x_amz_security_token.is_none());
    }

    #[test]
    fn test_sigv4_signing_with_session_token() {
        // Session token must appear in signed headers and in x_amz_security_token output
        let empty_body_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

        let mut headers = BTreeMap::new();
        headers.insert(
            "host".to_string(),
            "my-bucket.s3.us-east-1.amazonaws.com".to_string(),
        );
        headers.insert(
            "x-amz-content-sha256".to_string(),
            empty_body_hash.to_string(),
        );
        headers.insert("x-amz-date".to_string(), "20130524T000000Z".to_string());
        headers.insert(
            "x-amz-security-token".to_string(),
            "session-token-value".to_string(),
        );

        let creds = Credentials {
            access_key_id: TEST_ACCESS_KEY.to_string(),
            secret_access_key: TEST_SECRET_KEY.to_string(),
            session_token: Some("session-token-value".to_string()),
        };
        let config = SigningConfig {
            region: "us-east-1",
            service: "s3",
        };
        let input = SigningInput {
            method: "GET",
            canonical_uri: "/my-key.txt",
            canonical_query: "",
            headers_to_sign: &headers,
            body_sha256: empty_body_hash,
            datetime: "20130524T000000Z",
            date: "20130524",
        };

        let signed = sign(&input, &creds, &config);

        assert!(signed
            .authorization
            .contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"));
        assert_eq!(
            signed.x_amz_security_token,
            Some("session-token-value".to_string())
        );
    }
}
