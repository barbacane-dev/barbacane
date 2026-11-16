//! The listener's TLS configuration and its reload while serving.
//!
//! A reload builds a new configuration from the files and swaps it in: new
//! handshakes use it, established connections keep theirs. A configuration
//! that does not build is not swapped in.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig};
use sha2::{Digest, Sha256};

/// The files and settings a listener's TLS configuration is built from.
#[derive(Debug, Clone)]
pub struct TlsSettings {
    /// Certificate chain, PEM.
    pub cert_path: PathBuf,
    /// Private key, PEM.
    pub key_path: PathBuf,
    /// Minimum TLS version: "1.2" or "1.3".
    pub min_version: String,
    /// CA certificates, PEM, that client certificates must chain to. When set,
    /// every client presents one.
    pub client_ca_path: Option<PathBuf>,
}

impl TlsSettings {
    fn files(&self) -> impl Iterator<Item = &Path> {
        [self.cert_path.as_path(), self.key_path.as_path()]
            .into_iter()
            .chain(self.client_ca_path.as_deref())
    }
}

/// The configuration a listener shares with every handshake.
pub type SharedServerConfig = Arc<ArcSwap<ServerConfig>>;

/// Build the server configuration `settings` describe.
pub fn server_config(settings: &TlsSettings) -> Result<Arc<ServerConfig>, String> {
    let certs = read_certs(&settings.cert_path, "certificate")?;
    let key_file = File::open(&settings.key_path).map_err(|e| {
        format!(
            "failed to open key file '{}': {e}",
            settings.key_path.display()
        )
    })?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut BufReader::new(key_file))
        .map_err(|e| {
            format!(
                "failed to parse key file '{}': {e}",
                settings.key_path.display()
            )
        })?
        .ok_or_else(|| format!("no private key found in '{}'", settings.key_path.display()))?;

    let versions: Vec<&'static rustls::SupportedProtocolVersion> =
        match settings.min_version.as_str() {
            "1.3" => vec![&rustls::version::TLS13],
            _ => vec![&rustls::version::TLS13, &rustls::version::TLS12],
        };
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_protocol_versions(&versions)
        .map_err(|e| format!("failed to build TLS config: {e}"))?;
    let builder = match &settings.client_ca_path {
        Some(path) => {
            let mut roots = RootCertStore::empty();
            for cert in read_certs(path, "client CA")? {
                roots.add(cert).map_err(|e| {
                    format!("invalid client CA certificate in '{}': {e}", path.display())
                })?;
            }
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .map_err(|e| format!("client CA '{}': {e}", path.display()))?;
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let mut config = builder
        .with_single_cert(certs, key)
        .map_err(|e| format!("failed to build TLS config: {e}"))?;
    // Prefer HTTP/2, fall back to HTTP/1.1.
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn read_certs(path: &Path, what: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let file = File::open(path)
        .map_err(|e| format!("failed to open {what} file '{}': {e}", path.display()))?;
    let certs = rustls_pemfile::certs(&mut BufReader::new(file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to parse {what} file '{}': {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("no certificates found in '{}'", path.display()));
    }
    Ok(certs)
}

/// A digest of the files `settings` names; `None` while one cannot be read.
pub fn fingerprint(settings: &TlsSettings) -> Option<[u8; 32]> {
    let mut digest = Sha256::new();
    for path in settings.files() {
        let content = std::fs::read(path).ok()?;
        digest.update((content.len() as u64).to_le_bytes());
        digest.update(content);
    }
    Some(digest.finalize().into())
}

/// Rebuild `config` from `settings` on SIGHUP, and when the files change,
/// checked every `interval` (never when it is zero). Runs until the task is
/// dropped.
pub async fn reload(settings: TlsSettings, config: SharedServerConfig, interval: Duration) {
    let mut last = fingerprint(&settings);
    let mut hangup = hangup_signal();
    let mut ticker = (!interval.is_zero()).then(|| {
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        ticker
    });
    loop {
        let signalled = tokio::select! {
            () = next_hangup(&mut hangup) => true,
            () = next_tick(&mut ticker) => false,
        };
        let current = fingerprint(&settings);
        if !signalled && current == last {
            continue;
        }
        // The same files are not tried twice: a broken set is reported once,
        // and retried when it changes or on SIGHUP.
        last = current;
        let rebuilt = {
            let settings = settings.clone();
            tokio::task::spawn_blocking(move || server_config(&settings)).await
        };
        match rebuilt {
            Ok(Ok(new)) => {
                config.store(new);
                tracing::info!(
                    cert = %settings.cert_path.display(),
                    trigger = if signalled { "SIGHUP" } else { "file change" },
                    "TLS configuration reloaded"
                );
            }
            Ok(Err(e)) => tracing::error!(
                error = %e,
                "TLS configuration not reloaded; serving the previous one"
            ),
            Err(e) => tracing::error!(
                error = %e,
                "TLS configuration not reloaded; serving the previous one"
            ),
        }
    }
}

#[cfg(unix)]
type Hangup = Option<tokio::signal::unix::Signal>;
#[cfg(not(unix))]
type Hangup = ();

#[cfg(unix)]
fn hangup_signal() -> Hangup {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup()) {
        Ok(signal) => Some(signal),
        Err(e) => {
            tracing::warn!(error = %e, "SIGHUP does not reload TLS: no handler");
            None
        }
    }
}

#[cfg(not(unix))]
fn hangup_signal() -> Hangup {}

#[cfg(unix)]
async fn next_hangup(hangup: &mut Hangup) {
    if let Some(signal) = hangup {
        if signal.recv().await.is_some() {
            return;
        }
    }
    std::future::pending().await
}

#[cfg(not(unix))]
async fn next_hangup(_: &mut Hangup) {
    std::future::pending().await
}

async fn next_tick(ticker: &mut Option<tokio::time::Interval>) {
    match ticker {
        Some(ticker) => {
            ticker.tick().await;
        }
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dir: &Path, client_ca: bool) -> TlsSettings {
        let rcgen::CertifiedKey { cert, key_pair } =
            rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        std::fs::write(dir.join("server.crt"), cert.pem()).unwrap();
        std::fs::write(dir.join("server.key"), key_pair.serialize_pem()).unwrap();
        if client_ca {
            std::fs::write(dir.join("ca.crt"), cert.pem()).unwrap();
        }
        TlsSettings {
            cert_path: dir.join("server.crt"),
            key_path: dir.join("server.key"),
            min_version: "1.2".to_string(),
            client_ca_path: client_ca.then(|| dir.join("ca.crt")),
        }
    }

    #[test]
    fn the_fingerprint_follows_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(dir.path(), true);
        let first = fingerprint(&settings).expect("readable");
        assert_eq!(fingerprint(&settings), Some(first));

        for path in [&settings.cert_path, &settings.key_path] {
            let before = fingerprint(&settings);
            let mut content = std::fs::read(path).unwrap();
            content.push(b'\n');
            std::fs::write(path, content).unwrap();
            assert_ne!(fingerprint(&settings), before, "{}", path.display());
        }
        let ca = settings.client_ca_path.clone().unwrap();
        let before = fingerprint(&settings);
        std::fs::write(&ca, b"changed").unwrap();
        assert_ne!(fingerprint(&settings), before);

        std::fs::remove_file(&ca).unwrap();
        assert_eq!(fingerprint(&settings), None);
    }

    #[test]
    fn a_client_ca_without_certificates_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut settings = settings(dir.path(), true);
        assert!(server_config(&settings).is_ok());

        let ca = settings.client_ca_path.clone().unwrap();
        std::fs::write(&ca, b"").unwrap();
        let error = server_config(&settings).unwrap_err();
        assert!(error.contains("no certificates found"), "{error}");

        settings.client_ca_path = Some(dir.path().join("absent.crt"));
        let error = server_config(&settings).unwrap_err();
        assert!(error.contains("failed to open client CA file"), "{error}");
    }

    #[test]
    fn a_key_that_does_not_match_the_certificate_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(dir.path(), false);
        let other = rcgen::KeyPair::generate().unwrap();
        std::fs::write(&settings.key_path, other.serialize_pem()).unwrap();
        assert!(server_config(&settings).is_err());
    }
}
