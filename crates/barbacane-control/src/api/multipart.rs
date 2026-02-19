//! Shared multipart form parsing helpers.

use axum::extract::Multipart;

use crate::error::ProblemDetails;

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

    Ok((content, filename))
}
