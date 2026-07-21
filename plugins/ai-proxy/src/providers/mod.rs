//! Per-provider transport (HTTP request building, auth headers, URL composition).
//!
//! `openai` and `ollama` share the same OpenAI-compatible passthrough; `ollama`
//! is a thin re-export. `anthropic` builds its own request shape and pins the
//! API version. Translation between client and provider formats lives in the
//! protocol layer ([`crate::protocols`]), one above this one.

pub mod anthropic;
pub mod ollama;
pub mod openai;

use crate::Auth;
use std::collections::BTreeMap;

/// Attach the credential to an outbound request per the target's [`Auth`]
/// strategy. The single source of auth-attachment logic shared by every
/// transport (OpenAI, Anthropic, and the `/v1/models` aggregator) so a new
/// credential convention is added in one place, not three.
///
/// Header names are lowercased to match the transports' canonical header map
/// (the host canonicalizes on the wire, and HTTP header names are
/// case-insensitive per RFC 9110 §5.1). Query auth appends to the URL,
/// preserving any pre-existing query string.
pub(crate) fn apply_auth(
    auth: &Auth,
    key: &str,
    headers: &mut BTreeMap<String, String>,
    url: &mut String,
) {
    match auth {
        Auth::Bearer => {
            headers.insert("authorization".to_string(), format!("Bearer {}", key));
        }
        Auth::ApiKey => {
            headers.insert("x-api-key".to_string(), key.to_string());
        }
        Auth::Header(name) => {
            headers.insert(name.to_ascii_lowercase(), key.to_string());
        }
        Auth::Query(param) => {
            let sep = if url.contains('?') { '&' } else { '?' };
            url.push(sep);
            url.push_str(param);
            url.push('=');
            url.push_str(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(auth: Auth, url: &str) -> (BTreeMap<String, String>, String) {
        let mut headers = BTreeMap::new();
        let mut u = url.to_string();
        apply_auth(&auth, "SECRET", &mut headers, &mut u);
        (headers, u)
    }

    #[test]
    fn bearer_sets_authorization_header() {
        let (h, u) = apply(Auth::Bearer, "https://x/v1/chat/completions");
        assert_eq!(h.get("authorization").map(String::as_str), Some("Bearer SECRET"));
        assert_eq!(u, "https://x/v1/chat/completions");
    }

    #[test]
    fn api_key_sets_x_api_key_header() {
        let (h, _) = apply(Auth::ApiKey, "https://x");
        assert_eq!(h.get("x-api-key").map(String::as_str), Some("SECRET"));
    }

    #[test]
    fn header_variant_lowercases_name() {
        // Brave: X-Subscription-Token. Case is normalized to the canonical
        // lowercase header map; HTTP header names are case-insensitive.
        let (h, _) = apply(Auth::Header("X-Subscription-Token".to_string()), "https://x");
        assert_eq!(h.get("x-subscription-token").map(String::as_str), Some("SECRET"));
        assert!(h.get("authorization").is_none());
    }

    #[test]
    fn query_appends_to_url_preserving_existing_query() {
        let (h, u) = apply(Auth::Query("key".to_string()), "https://x/models?alt=json");
        assert!(h.is_empty());
        assert_eq!(u, "https://x/models?alt=json&key=SECRET");
    }

    #[test]
    fn query_adds_question_mark_when_no_existing_query() {
        let (_, u) = apply(Auth::Query("key".to_string()), "https://x/models");
        assert_eq!(u, "https://x/models?key=SECRET");
    }
}
