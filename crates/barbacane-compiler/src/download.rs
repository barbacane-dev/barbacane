//! HTTP download logic for remote plugins.

use std::io::Read;
use std::time::Duration;

use crate::error::CompileError;

/// Maximum size of a downloaded plugin `.wasm` body. A remote (untrusted) host
/// could otherwise stream an arbitrarily large body and OOM the compile/CI host,
/// since the whole body is buffered in memory.
const MAX_PLUGIN_WASM_BYTES: u64 = 64 * 1024 * 1024;

/// Maximum size of a downloaded `plugin.toml`. It is small config; cap it so a
/// hostile host cannot exhaust memory via the sidecar fetch either.
const MAX_PLUGIN_TOML_BYTES: u64 = 1024 * 1024;

/// Result of downloading a remote plugin.
#[derive(Debug)]
pub struct DownloadResult {
    /// WASM binary content.
    pub wasm_bytes: Vec<u8>,
    /// Raw plugin.toml content (if found at sibling URL).
    pub plugin_toml: Option<String>,
}

/// Build a blocking HTTP client with appropriate defaults.
fn build_client() -> Result<reqwest::blocking::Client, CompileError> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(120))
        .user_agent(format!("barbacane-compiler/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| CompileError::PluginResolution(format!("failed to build HTTP client: {e}")))
}

/// Derive candidate plugin.toml URLs from a .wasm URL.
///
/// Returns up to two candidates:
/// 1. `<name>.plugin.toml` — sibling file convention (GitHub release assets are flat)
///    e.g. `https://github.com/.../v0.5.0/jwt-auth.wasm` -> `https://github.com/.../v0.5.0/jwt-auth.plugin.toml`
/// 2. `plugin.toml` — directory convention (self-hosted / structured URLs)
///    e.g. `https://example.com/plugins/jwt-auth/1.0.0/jwt-auth.wasm` -> `https://example.com/plugins/jwt-auth/1.0.0/plugin.toml`
fn derive_plugin_toml_urls(wasm_url: &str) -> Vec<String> {
    let mut urls = Vec::new();
    if let Some(stripped) = wasm_url.strip_suffix(".wasm") {
        urls.push(format!("{stripped}.plugin.toml"));
    }
    if let Some(last_slash) = wasm_url.rfind('/') {
        urls.push(format!("{}/plugin.toml", &wasm_url[..last_slash]));
    }
    urls
}

/// Download a plugin from a remote HTTPS URL.
///
/// Fetches the .wasm binary and attempts to fetch `plugin.toml` from
/// the same directory (best-effort — 404 is fine).
pub fn download_plugin(url: &str) -> Result<DownloadResult, CompileError> {
    if !url.starts_with("https://") {
        return Err(CompileError::PluginResolution(format!(
            "plugin URL must use HTTPS: {url}"
        )));
    }

    let client = build_client()?;

    tracing::info!(url, "downloading remote plugin");

    // Download the .wasm file
    let response = client.get(url).send().map_err(|e| {
        CompileError::PluginResolution(format!("failed to download plugin from {url}: {e}"))
    })?;

    if !response.status().is_success() {
        return Err(CompileError::PluginResolution(format!(
            "failed to download plugin from {url}: HTTP {}",
            response.status()
        )));
    }

    // Reject early if the server declares an oversize body...
    if let Some(len) = response.content_length() {
        if len > MAX_PLUGIN_WASM_BYTES {
            return Err(CompileError::PluginResolution(format!(
                "plugin at {url} is {len} bytes, exceeds the {MAX_PLUGIN_WASM_BYTES}-byte limit"
            )));
        }
    }
    // ...and cap the actual read so a missing or lying Content-Length can't OOM
    // the host: read at most limit+1 bytes, then reject if the limit is exceeded.
    let mut wasm_bytes = Vec::new();
    response
        .take(MAX_PLUGIN_WASM_BYTES + 1)
        .read_to_end(&mut wasm_bytes)
        .map_err(|e| {
            CompileError::PluginResolution(format!(
                "failed to read plugin response body from {url}: {e}"
            ))
        })?;
    if wasm_bytes.len() as u64 > MAX_PLUGIN_WASM_BYTES {
        return Err(CompileError::PluginResolution(format!(
            "plugin body from {url} exceeds the {MAX_PLUGIN_WASM_BYTES}-byte limit"
        )));
    }

    // Best-effort: try to fetch plugin.toml from candidate URLs (also bounded).
    let plugin_toml = derive_plugin_toml_urls(url)
        .into_iter()
        .find_map(|toml_url| {
            tracing::debug!(url = %toml_url, "attempting to fetch plugin.toml");
            let resp = client.get(&toml_url).send().ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let mut buf = String::new();
            resp.take(MAX_PLUGIN_TOML_BYTES)
                .read_to_string(&mut buf)
                .ok()?;
            Some(buf)
        });

    Ok(DownloadResult {
        wasm_bytes,
        plugin_toml,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reject_http_url() {
        let result = download_plugin("http://example.com/plugin.wasm");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("HTTPS"));
    }

    #[test]
    fn derive_plugin_toml_urls_from_wasm() {
        let urls =
            derive_plugin_toml_urls("https://example.com/plugins/jwt-auth/1.0.0/jwt-auth.wasm");
        assert_eq!(urls.len(), 2);
        // First: sibling <name>.plugin.toml
        assert_eq!(
            urls[0],
            "https://example.com/plugins/jwt-auth/1.0.0/jwt-auth.plugin.toml"
        );
        // Second: directory plugin.toml
        assert_eq!(
            urls[1],
            "https://example.com/plugins/jwt-auth/1.0.0/plugin.toml"
        );
    }

    #[test]
    fn derive_plugin_toml_urls_github_release() {
        let urls = derive_plugin_toml_urls(
            "https://github.com/barbacane-dev/barbacane/releases/download/v0.5.0/jwt-auth.wasm",
        );
        assert_eq!(
            urls[0],
            "https://github.com/barbacane-dev/barbacane/releases/download/v0.5.0/jwt-auth.plugin.toml"
        );
    }
}
