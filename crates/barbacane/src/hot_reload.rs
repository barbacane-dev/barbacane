//! Hot-reload functionality for zero-downtime artifact updates.
//!
//! This module handles downloading, verifying, and applying new artifacts
//! while the gateway continues serving requests.

use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

/// Result of a hot-reload attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotReloadResult {
    /// Hot-reload completed successfully.
    Success { artifact_id: Uuid },
    /// Hot-reload failed with an error.
    Failed { artifact_id: Uuid, error: String },
}

/// Lock to prevent concurrent hot-reloads.
pub static HOT_RELOAD_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Download an artifact from the control plane and verify its checksum.
///
/// # Arguments
/// * `http_client` - The HTTP client to use for downloading
/// * `download_url` - URL to download the artifact from
/// * `expected_sha256` - Expected SHA256 hash of the artifact (hex-encoded)
/// * `artifact_dir` - Directory to store the downloaded artifact
///
/// # Returns
/// The path to the downloaded and verified artifact, or an error message.
pub async fn download_artifact(
    http_client: &reqwest::Client,
    download_url: &str,
    expected_sha256: &str,
    artifact_dir: &Path,
) -> Result<PathBuf, String> {
    // Create temp file path
    let temp_filename = format!("artifact-{}.bca.tmp", Uuid::new_v4());
    let temp_path = artifact_dir.join(&temp_filename);

    tracing::info!(
        download_url = %download_url,
        temp_path = %temp_path.display(),
        "Downloading artifact"
    );

    // Start download
    let response = http_client
        .get(download_url)
        .send()
        .await
        .map_err(|e| format!("download request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "download failed with status: {}",
            response.status()
        ));
    }

    // Create temp file
    let mut file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| format!("failed to create temp file: {}", e))?;

    // Stream to file while computing SHA256
    let mut hasher = Sha256::new();
    let mut stream = response.bytes_stream();
    let mut total_bytes = 0u64;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| format!("download stream error: {}", e))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("write error: {}", e))?;
        total_bytes += chunk.len() as u64;
    }

    file.flush()
        .await
        .map_err(|e| format!("flush error: {}", e))?;
    drop(file);

    // Verify checksum
    let computed_sha256 = hex::encode(hasher.finalize());
    if computed_sha256 != expected_sha256 {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(format!(
            "checksum mismatch: expected {}, got {}",
            expected_sha256, computed_sha256
        ));
    }

    // Atomic rename to final path
    let final_filename = format!("artifact-{}.bca", Uuid::new_v4());
    let final_path = artifact_dir.join(&final_filename);
    tokio::fs::rename(&temp_path, &final_path)
        .await
        .map_err(|e| format!("rename failed: {}", e))?;

    tracing::info!(
        final_path = %final_path.display(),
        size_bytes = total_bytes,
        sha256 = %computed_sha256,
        "Artifact downloaded and verified"
    );

    Ok(final_path)
}

/// Compute SHA256 hash of data (hex-encoded).
pub fn compute_sha256(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    /// Simple HTTP server for testing downloads.
    async fn start_test_server(
        content: Vec<u8>,
        status: u16,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                // Read the request (we don't care about the content)
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;

                // Send response
                let response = format!(
                    "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    status,
                    content.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.write_all(&content).await;
            }
        });

        (addr, handle)
    }

    #[tokio::test]
    async fn test_download_artifact_success() {
        // Create test content
        let content = b"test artifact content for download verification";
        let expected_sha256 = compute_sha256(content);

        // Start test server
        let (addr, server_handle) = start_test_server(content.to_vec(), 200).await;
        let url = format!("http://{}/artifact.bca", addr);

        // Create temp directory
        let temp_dir = tempfile::tempdir().unwrap();

        // Download artifact
        let client = reqwest::Client::new();
        let result = download_artifact(&client, &url, &expected_sha256, temp_dir.path()).await;

        // Verify success
        assert!(result.is_ok(), "download should succeed: {:?}", result);
        let artifact_path = result.unwrap();
        assert!(artifact_path.exists(), "artifact file should exist");

        // Verify content
        let downloaded_content = tokio::fs::read(&artifact_path).await.unwrap();
        assert_eq!(downloaded_content, content);

        // Cleanup
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_download_artifact_checksum_mismatch() {
        // Create test content
        let content = b"test artifact content";
        let wrong_sha256 = "0000000000000000000000000000000000000000000000000000000000000000";

        // Start test server
        let (addr, server_handle) = start_test_server(content.to_vec(), 200).await;
        let url = format!("http://{}/artifact.bca", addr);

        // Create temp directory
        let temp_dir = tempfile::tempdir().unwrap();

        // Download artifact
        let client = reqwest::Client::new();
        let result = download_artifact(&client, &url, wrong_sha256, temp_dir.path()).await;

        // Verify failure
        assert!(
            result.is_err(),
            "download should fail with checksum mismatch"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("checksum mismatch"),
            "error should mention checksum: {}",
            error
        );

        // Verify temp file was cleaned up
        let files: Vec<_> = std::fs::read_dir(temp_dir.path()).unwrap().collect();
        assert!(files.is_empty(), "temp file should be cleaned up");

        // Cleanup
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_download_artifact_http_error() {
        // Start test server that returns 404
        let (addr, server_handle) = start_test_server(vec![], 404).await;
        let url = format!("http://{}/artifact.bca", addr);

        // Create temp directory
        let temp_dir = tempfile::tempdir().unwrap();

        // Download artifact
        let client = reqwest::Client::new();
        let result = download_artifact(&client, &url, "dummy", temp_dir.path()).await;

        // Verify failure
        assert!(result.is_err(), "download should fail with HTTP error");
        let error = result.unwrap_err();
        assert!(
            error.contains("404") || error.contains("status"),
            "error should mention status: {}",
            error
        );

        // Cleanup
        server_handle.abort();
    }

    #[tokio::test]
    async fn test_download_artifact_connection_refused() {
        // Use a URL that will fail to connect
        let url = "http://127.0.0.1:1/artifact.bca";

        // Create temp directory
        let temp_dir = tempfile::tempdir().unwrap();

        // Download artifact
        let client = reqwest::Client::new();
        let result = download_artifact(&client, url, "dummy", temp_dir.path()).await;

        // Verify failure
        assert!(
            result.is_err(),
            "download should fail with connection error"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("download request failed"),
            "error should mention request failed: {}",
            error
        );
    }

    #[tokio::test]
    async fn test_hot_reload_lock_prevents_concurrent_reloads() {
        // Acquire the lock
        let guard = HOT_RELOAD_LOCK.try_lock();
        assert!(guard.is_ok(), "first lock should succeed");

        // Try to acquire again
        let second_guard = HOT_RELOAD_LOCK.try_lock();
        assert!(second_guard.is_err(), "second lock should fail");

        // Drop first lock
        drop(guard);

        // Now should succeed
        let third_guard = HOT_RELOAD_LOCK.try_lock();
        assert!(third_guard.is_ok(), "third lock should succeed after drop");
    }

    #[test]
    fn test_compute_sha256() {
        let data = b"hello world";
        let hash = compute_sha256(data);
        // Known SHA256 of "hello world"
        assert_eq!(
            hash,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn test_compute_sha256_empty() {
        let data = b"";
        let hash = compute_sha256(data);
        // Known SHA256 of empty string
        assert_eq!(
            hash,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn test_hot_reload_result_equality() {
        let id = Uuid::new_v4();

        let success1 = HotReloadResult::Success { artifact_id: id };
        let success2 = HotReloadResult::Success { artifact_id: id };
        assert_eq!(success1, success2);

        let failed1 = HotReloadResult::Failed {
            artifact_id: id,
            error: "test error".to_string(),
        };
        let failed2 = HotReloadResult::Failed {
            artifact_id: id,
            error: "test error".to_string(),
        };
        assert_eq!(failed1, failed2);

        assert_ne!(success1, failed1);
    }
}
