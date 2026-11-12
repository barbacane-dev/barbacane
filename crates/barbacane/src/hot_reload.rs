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

/// The client artifacts are downloaded with. It does not decompress, so the
/// bytes written are the bytes the checksum covers.
pub fn download_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_gzip()
        .build()
        .map_err(|e| format!("failed to build the download client: {e}"))
}

/// Download an artifact from the control plane and verify its checksum.
///
/// # Arguments
/// * `http_client` - The HTTP client to use for downloading
/// * `download_url` - URL to download the artifact from
/// * `bearer` - Credential sent as a bearer token, the data plane's API key
/// * `expected_sha256` - Expected SHA256 hash of the artifact (hex-encoded)
/// * `artifact_dir` - Directory to store the downloaded artifact
/// * `max_bytes` - Size past which the download is abandoned
///
/// # Returns
/// The path to the downloaded and verified artifact, or an error message. On
/// failure, or if the download is cancelled, nothing is left in `artifact_dir`.
pub async fn download_artifact(
    http_client: &reqwest::Client,
    download_url: &str,
    bearer: Option<&str>,
    expected_sha256: &str,
    artifact_dir: &Path,
    max_bytes: u64,
) -> Result<PathBuf, String> {
    // Create temp file path
    let temp_filename = format!("artifact-{}.bca.tmp", Uuid::new_v4());
    let temp_path = artifact_dir.join(&temp_filename);
    let temp_guard = RemoveOnDrop(Some(temp_path.clone()));

    tracing::info!(
        download_url = %download_url,
        temp_path = %temp_path.display(),
        "Downloading artifact"
    );

    // Start download
    let mut request = http_client.get(download_url);
    if let Some(token) = bearer {
        request = request.bearer_auth(token);
    }
    let response = request
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
        total_bytes += chunk.len() as u64;
        if total_bytes > max_bytes {
            return Err(format!("the artifact is larger than {max_bytes} bytes"));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("write error: {}", e))?;
    }

    file.flush()
        .await
        .map_err(|e| format!("flush error: {}", e))?;
    drop(file);

    // Verify checksum
    let computed_sha256 = hex::encode(hasher.finalize());
    if computed_sha256 != expected_sha256 {
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
    temp_guard.disarm();

    tracing::info!(
        final_path = %final_path.display(),
        size_bytes = total_bytes,
        sha256 = %computed_sha256,
        "Artifact downloaded and verified"
    );

    Ok(final_path)
}

/// Removes the file at its path when dropped, unless disarmed.
struct RemoveOnDrop(Option<PathBuf>);

impl RemoveOnDrop {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
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

    const MAX: u64 = barbacane_compiler::MAX_ARTIFACT_BYTES;

    /// Simple HTTP server for testing downloads.
    async fn start_test_server(
        content: Vec<u8>,
        status: u16,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let mut response = format!(
            "HTTP/1.1 {} OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status,
            content.len()
        )
        .into_bytes();
        response.extend_from_slice(&content);
        start_raw_server(response).await
    }

    /// A server that answers one request with `response`, written as is.
    async fn start_raw_server(response: Vec<u8>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                // Read the request (we don't care about the content)
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(&response).await;
            }
        });

        (addr, handle)
    }

    fn is_empty(dir: &Path) -> bool {
        std::fs::read_dir(dir).unwrap().next().is_none()
    }

    #[tokio::test]
    async fn a_download_past_the_limit_is_abandoned_and_removed() {
        let content = vec![7u8; 4096];
        let (addr, server_handle) = start_test_server(content.clone(), 200).await;
        let url = format!("http://{}/artifact.bca", addr);
        let temp_dir = tempfile::tempdir().unwrap();

        let client = download_client().unwrap();
        let sha = compute_sha256(&content);
        let error = download_artifact(&client, &url, None, &sha, temp_dir.path(), 1024)
            .await
            .unwrap_err();
        assert!(error.contains("larger than 1024"), "{error}");
        assert!(is_empty(temp_dir.path()), "the partial file is removed");
        server_handle.abort();
    }

    #[tokio::test]
    async fn a_download_cut_short_leaves_nothing_behind() {
        let mut response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\n".to_vec();
        response.extend_from_slice(&[1u8; 100]);
        let (addr, server_handle) = start_raw_server(response).await;
        let url = format!("http://{}/artifact.bca", addr);
        let temp_dir = tempfile::tempdir().unwrap();

        let client = download_client().unwrap();
        let result = download_artifact(&client, &url, None, "dummy", temp_dir.path(), MAX).await;
        assert!(result.is_err());
        assert!(is_empty(temp_dir.path()), "the partial file is removed");
        server_handle.abort();
    }

    #[tokio::test]
    async fn a_compressed_response_is_written_as_sent() {
        use std::io::Write as _;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&vec![0u8; 64 << 10]).unwrap();
        let sent = encoder.finish().unwrap();
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            sent.len()
        )
        .into_bytes();
        response.extend_from_slice(&sent);
        let (addr, server_handle) = start_raw_server(response).await;
        let url = format!("http://{}/artifact.bca", addr);
        let temp_dir = tempfile::tempdir().unwrap();

        let client = download_client().unwrap();
        let path = download_artifact(
            &client,
            &url,
            None,
            &compute_sha256(&sent),
            temp_dir.path(),
            MAX,
        )
        .await
        .expect("the bytes sent match their checksum");
        assert_eq!(std::fs::read(path).unwrap(), sent);
        server_handle.abort();
    }

    /// A server that answers 200 with `content` when the request carries
    /// `Authorization: Bearer <token>`, and 401 otherwise.
    async fn start_bearer_server(
        content: Vec<u8>,
        token: &str,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let expected = format!("authorization: Bearer {token}\r\n");
        let handle = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                let response = if head.contains(&expected.to_ascii_lowercase()) {
                    let mut r = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        content.len()
                    )
                    .into_bytes();
                    r.extend_from_slice(&content);
                    r
                } else {
                    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec()
                };
                let _ = socket.write_all(&response).await;
            }
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn the_bearer_token_is_sent_with_the_download() {
        let content = b"artifact".to_vec();
        let sha = compute_sha256(&content);
        let client = download_client().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();

        let (addr, server) = start_bearer_server(content.clone(), "bbk_secret").await;
        let url = format!("http://{}/artifacts/1/download", addr);
        let path = download_artifact(
            &client,
            &url,
            Some("bbk_secret"),
            &sha,
            temp_dir.path(),
            MAX,
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), content);
        server.abort();

        let (addr, server) = start_bearer_server(content, "bbk_secret").await;
        let url = format!("http://{}/artifacts/1/download", addr);
        let error = download_artifact(&client, &url, None, &sha, temp_dir.path(), MAX)
            .await
            .unwrap_err();
        assert!(error.contains("401"), "{error}");
        server.abort();
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
        let result =
            download_artifact(&client, &url, None, &expected_sha256, temp_dir.path(), MAX).await;

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
        let result =
            download_artifact(&client, &url, None, wrong_sha256, temp_dir.path(), MAX).await;

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
        let result = download_artifact(&client, &url, None, "dummy", temp_dir.path(), MAX).await;

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
        let result = download_artifact(&client, url, None, "dummy", temp_dir.path(), MAX).await;

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
