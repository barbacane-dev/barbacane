//! The headers a dispatcher sends upstream on a client's behalf.
//!
//! The upstream learns the client's address from the gateway alone: the
//! client's `X-Forwarded-For`, `X-Real-IP` and `Forwarded` are dropped, and
//! `X-Forwarded-For` and `X-Real-IP` are set to [`Request::client_ip`], the
//! client the data plane resolved through its trusted proxies.
//! `X-Forwarded-Proto` and `X-Forwarded-Host` are sent as the data plane set
//! them: the scheme and host the client used.
//!
//! ```
//! use barbacane_plugin_sdk::{forwarding, prelude::*};
//! let req = Request::new("GET", "/")
//!     .with_client_ip("198.51.100.7")
//!     .with_header("x-forwarded-for", "203.0.113.9")
//!     .with_header("accept", "*/*");
//! let headers = forwarding::upstream_headers(&req);
//! assert_eq!(headers["x-forwarded-for"], "198.51.100.7");
//! assert_eq!(headers["x-real-ip"], "198.51.100.7");
//! assert_eq!(headers["accept"], "*/*");
//! ```

use std::collections::BTreeMap;

use crate::types::Request;

/// Headers that describe a single connection, which the host's HTTP client
/// sets itself.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
];

/// Headers through which a proxy reports the client's address.
const CLIENT_ADDRESS: &[&str] = &["x-forwarded-for", "x-real-ip", "forwarded"];

/// The request's headers to send upstream: hop-by-hop headers and the client's
/// own forwarding headers removed, then `x-forwarded-for` and `x-real-ip` set
/// to `req.client_ip`. Header names are matched case-insensitively.
pub fn upstream_headers(req: &Request) -> BTreeMap<String, String> {
    let mut headers: BTreeMap<String, String> = req
        .headers
        .iter()
        .filter(|(name, _)| {
            !HOP_BY_HOP
                .iter()
                .chain(CLIENT_ADDRESS)
                .any(|h| name.eq_ignore_ascii_case(h))
        })
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    headers.insert("x-forwarded-for".to_string(), req.client_ip.clone());
    headers.insert("x-real-ip".to_string(), req.client_ip.clone());
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn the_upstream_learns_the_client_from_the_gateway_alone() {
        let req = Request::new("GET", "/")
            .with_client_ip("198.51.100.7")
            .with_headers(map(&[
                ("X-Forwarded-For", "6.6.6.6, 203.0.113.9"),
                ("x-real-ip", "6.6.6.6"),
                ("Forwarded", "for=6.6.6.6"),
                ("connection", "close"),
                ("Upgrade", "h2c"),
                ("accept", "application/json"),
            ]));

        assert_eq!(
            upstream_headers(&req),
            map(&[
                ("accept", "application/json"),
                ("x-forwarded-for", "198.51.100.7"),
                ("x-real-ip", "198.51.100.7"),
            ])
        );
    }
}
