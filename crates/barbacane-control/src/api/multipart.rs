//! Shared multipart form parsing helpers.

use axum::extract::Multipart;

use crate::error::ProblemDetails;

/// Reduce an untrusted upload filename to a safe basename.
///
/// Strips any directory components (defeating `../` path traversal when the name
/// is later joined onto a filesystem path) and replaces any character outside a
/// conservative allowlist (defeating CRLF/quote injection when the name is
/// reflected into a `Content-Disposition` header). Falls back to `upload` when
/// nothing usable remains.
pub fn safe_filename(raw: &str) -> String {
    let base = std::path::Path::new(raw)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        "upload".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Extract a single `file` field from a multipart upload.
///
/// Returns `(file_bytes, filename)`. Fails with 400 if the field is missing
/// or cannot be read.
pub async fn extract_file_field(
    mut multipart: Multipart,
) -> Result<(Vec<u8>, String), ProblemDetails> {
    let mut file_data: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| ProblemDetails::bad_request(format!("Invalid multipart data: {}", e)))?
    {
        if field.name().unwrap_or_default() == "file" {
            filename = field.file_name().map(String::from);
            file_data = Some(
                field
                    .bytes()
                    .await
                    .map_err(|e| {
                        ProblemDetails::bad_request(format!("Failed to read file: {}", e))
                    })?
                    .to_vec(),
            );
        }
    }

    let content = file_data.ok_or_else(|| ProblemDetails::bad_request("Missing 'file' field"))?;
    let filename = filename.ok_or_else(|| ProblemDetails::bad_request("Missing filename"))?;

    // Sanitize before the name is stored and later used as a filesystem path
    // and reflected into response headers.
    Ok((content, safe_filename(&filename)))
}

#[cfg(test)]
mod tests {
    use super::safe_filename;

    #[test]
    fn keeps_ordinary_names() {
        assert_eq!(safe_filename("api.yaml"), "api.yaml");
        assert_eq!(safe_filename("my-api.v2.json"), "my-api.v2.json");
    }

    #[test]
    fn strips_directory_traversal() {
        assert_eq!(safe_filename("../../etc/passwd"), "passwd");
        assert_eq!(safe_filename("/abs/path/spec.yaml"), "spec.yaml");
        // A bare traversal segment leaves no basename.
        assert_eq!(safe_filename(".."), "upload");
        assert_eq!(safe_filename(""), "upload");
        // Backslashes are not path separators on Unix, but are still neutralized
        // to a safe character so the result can never form a traversal or inject.
        let win = safe_filename("..\\..\\win.yaml");
        assert!(!win.contains('\\') && !win.contains('/'));
    }

    #[test]
    fn strips_header_injection_chars() {
        // CRLF / quotes that would break a Content-Disposition header.
        assert_eq!(
            safe_filename("a\"; drop\r\nSet-Cookie: x.yaml"),
            "a___drop__Set-Cookie__x.yaml"
        );
    }
}
