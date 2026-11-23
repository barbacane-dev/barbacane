//! Upstream paths built from a request's path parameters.
//!
//! Path parameter values arrive percent-decoded, so a value can hold `/`, `?`,
//! `#`, `%` or `..`. Pasted into a URL as is, it can leave the path it was
//! meant for. [`fill_path_template`] encodes each value before it goes in.

use std::collections::BTreeMap;
use std::fmt;

/// A path parameter whose value cannot go into a URL path: one of its
/// `/`-separated parts is `.` or `..`, which a URL parser resolves against the
/// parts before it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DotSegmentParam {
    /// The parameter's name.
    pub name: String,
}

impl fmt::Display for DotSegmentParam {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "path parameter `{}` holds a `.` or `..` segment",
            self.name
        )
    }
}

impl std::error::Error for DotSegmentParam {}

/// `template` with each `{name}` replaced by the value of the path parameter
/// `name`, in one pass.
///
/// A value is percent-encoded part by part: a `/` in it stays a path
/// separator, so a `{path+}` wildcard keeps its segments, and every other
/// character outside the unreserved set and `!$&'()*+,=:@` is encoded, so a
/// value cannot add a query, a fragment, a `;` path parameter or another
/// `{name}`. A part that is `.` or `..` is refused. A `{name}` with no
/// parameter of that name is left as written.
pub fn fill_path_template(
    template: &str,
    params: &BTreeMap<String, String>,
) -> Result<String, DotSegmentParam> {
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        filled.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            filled.push_str(&rest[open..]);
            return Ok(filled);
        };
        let name = &after[..close];
        match params.get(name) {
            Some(value) => encode_value(name, value, &mut filled)?,
            None => {
                filled.push('{');
                filled.push_str(name);
                filled.push('}');
            }
        }
        rest = &after[close + 1..];
    }
    filled.push_str(rest);
    Ok(filled)
}

/// Append `value` to `out`, encoded part by part.
fn encode_value(name: &str, value: &str, out: &mut String) -> Result<(), DotSegmentParam> {
    for (index, part) in value.split('/').enumerate() {
        if part == "." || part == ".." {
            return Err(DotSegmentParam {
                name: name.to_string(),
            });
        }
        if index > 0 {
            out.push('/');
        }
        for byte in part.bytes() {
            if byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,=:@".contains(&byte) {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_plain_value_is_inserted_as_is() {
        let filled =
            fill_path_template("/v1/users/{id}/orders", &params(&[("id", "abc-1_2.3~")])).unwrap();
        assert_eq!(filled, "/v1/users/abc-1_2.3~/orders");
    }

    #[test]
    fn a_slash_stays_a_separator_and_dot_segments_are_refused() {
        assert_eq!(
            fill_path_template("/{path}", &params(&[("path", "a/b/c.txt")])).unwrap(),
            "/a/b/c.txt"
        );
        for value in ["..", ".", "../admin", "a/../../admin", "a/./b", "x/.."] {
            let error =
                fill_path_template("/public/{name}", &params(&[("name", value)])).expect_err(value);
            assert_eq!(error.name, "name", "{value}");
        }
    }

    #[test]
    fn a_value_cannot_add_a_query_a_fragment_or_a_path_parameter() {
        let filled = fill_path_template(
            "/process/{id}",
            &params(&[("id", "x?amount=0&#frag;jsessionid=1")]),
        )
        .unwrap();
        assert_eq!(filled, "/process/x%3Famount=0&%23frag%3Bjsessionid=1");
    }

    #[test]
    fn encodings_a_url_parser_would_decode_again_are_encoded() {
        // `%2e%2e` is a dot segment to a WHATWG parser, `\` a separator, and
        // tabs and line breaks are dropped before parsing.
        let filled = fill_path_template(
            "/p/{a}/{b}/{c}",
            &params(&[("a", "%2e%2e"), ("b", "..\\..\\admin"), ("c", ".\t.")]),
        )
        .unwrap();
        assert_eq!(filled, "/p/%252e%252e/..%5C..%5Cadmin/.%09.");
    }

    #[test]
    fn a_value_cannot_fill_another_placeholder() {
        let filled = fill_path_template("/{a}/{b}", &params(&[("a", "{b}"), ("b", "x")])).unwrap();
        assert_eq!(filled, "/%7Bb%7D/x");
    }

    #[test]
    fn non_ascii_is_encoded_as_utf8() {
        let filled = fill_path_template("/{name}", &params(&[("name", "café bar")])).unwrap();
        assert_eq!(filled, "/caf%C3%A9%20bar");
    }

    #[test]
    fn unknown_and_unclosed_placeholders_are_left_as_written() {
        assert_eq!(
            fill_path_template("/{missing}/x", &params(&[])).unwrap(),
            "/{missing}/x"
        );
        assert_eq!(
            fill_path_template("/a/{open", &params(&[("open", "v")])).unwrap(),
            "/a/{open"
        );
    }
}
