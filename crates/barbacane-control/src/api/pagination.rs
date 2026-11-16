//! `limit` and `offset` on the list endpoints.

use serde::Deserialize;

use crate::db::Page;
use crate::error::ProblemDetails;

/// Rows a list returns when the request names no `limit`.
pub const DEFAULT_LIMIT: i64 = 100;

/// The largest `limit` a request may name.
pub const MAX_LIMIT: i64 = 1000;

/// The `limit` and `offset` query parameters. Read as text, so a value that
/// is not a number is answered like one out of range, as problem+json.
#[derive(Debug, Default, Deserialize)]
pub struct PageQuery {
    pub limit: Option<String>,
    pub offset: Option<String>,
}

impl PageQuery {
    /// The window the parameters ask for: `limit` from 1 to [`MAX_LIMIT`],
    /// [`DEFAULT_LIMIT`] when absent; `offset` from 0, 0 when absent.
    pub fn page(&self) -> Result<Page, ProblemDetails> {
        let limit = match self.limit.as_deref() {
            None => DEFAULT_LIMIT,
            Some(value) => value
                .parse::<i64>()
                .ok()
                .filter(|n| (1..=MAX_LIMIT).contains(n))
                .ok_or_else(|| {
                    ProblemDetails::bad_request(format!(
                        "limit must be a whole number from 1 to {MAX_LIMIT}"
                    ))
                })?,
        };
        let offset = match self.offset.as_deref() {
            None => 0,
            Some(value) => value
                .parse::<i64>()
                .ok()
                .filter(|n| *n >= 0)
                .ok_or_else(|| {
                    ProblemDetails::bad_request("offset must be a whole number from 0")
                })?,
        };
        Ok(Page { limit, offset })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(limit: Option<&str>, offset: Option<&str>) -> Result<Page, ProblemDetails> {
        PageQuery {
            limit: limit.map(str::to_string),
            offset: offset.map(str::to_string),
        }
        .page()
    }

    #[test]
    fn defaults_and_bounds() {
        assert_eq!(
            page(None, None).unwrap(),
            Page {
                limit: DEFAULT_LIMIT,
                offset: 0
            }
        );
        assert_eq!(page(Some("1"), Some("0")).unwrap().limit, 1);
        assert_eq!(page(Some("1000"), Some("5000")).unwrap().offset, 5000);
        for (limit, offset) in [
            (Some("0"), None),
            (Some("1001"), None),
            (Some("-1"), None),
            (Some("ten"), None),
            (Some(""), None),
            (None, Some("-1")),
            (None, Some("1.5")),
        ] {
            assert!(page(limit, offset).is_err(), "{limit:?} {offset:?}");
        }
    }
}
