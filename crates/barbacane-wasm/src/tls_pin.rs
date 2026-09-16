//! TLS client configuration for plugin egress opened to a vetted address.
//!
//! The SSRF guard resolves an egress target once and the client connects to
//! one of the vetted addresses. A library that derives the TLS server name
//! from the address it was handed would then validate the certificate against
//! an IP literal. The verifier here delegates to the WebPKI verifier with the
//! hostname of the original URL fixed, whatever server name the connection was
//! opened with. The ClientHello carries no SNI when the connection is opened
//! with an IP literal.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error, RootCertStore, SignatureScheme};
use std::sync::Arc;

/// Validates the server certificate against a fixed hostname.
#[derive(Debug)]
struct PinnedHostVerifier {
    inner: Arc<WebPkiServerVerifier>,
    server_name: ServerName<'static>,
}

impl ServerCertVerifier for PinnedHostVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        self.inner.verify_server_cert(
            end_entity,
            intermediates,
            &self.server_name,
            ocsp_response,
            now,
        )
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

/// The crypto provider the gateway's TLS clients run on.
pub(crate) fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::aws_lc_rs::default_provider())
}

/// Root store holding the platform CA certificates.
fn native_roots() -> Result<RootCertStore, String> {
    let loaded = rustls_native_certs::load_native_certs();
    let mut roots = RootCertStore::empty();
    roots.add_parsable_certificates(loaded.certs);
    if roots.is_empty() {
        let errors: Vec<String> = loaded.errors.iter().map(|e| e.to_string()).collect();
        return Err(format!(
            "no platform CA certificates loaded: {}",
            errors.join("; ")
        ));
    }
    Ok(roots)
}

/// Client configuration trusting the platform CA store and validating the
/// server certificate against `hostname` regardless of the address the
/// connection was opened with.
pub(crate) fn pinned_client_config(hostname: &str) -> Result<ClientConfig, String> {
    pinned_client_config_with_roots(hostname, native_roots()?)
}

/// [`pinned_client_config`] with an explicit root store.
pub(crate) fn pinned_client_config_with_roots(
    hostname: &str,
    roots: RootCertStore,
) -> Result<ClientConfig, String> {
    let bare = hostname
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(hostname);
    let server_name = ServerName::try_from(bare)
        .map_err(|e| format!("invalid TLS server name '{hostname}': {e}"))?
        .to_owned();
    let provider = provider();
    let inner = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|e| format!("TLS verifier: {e}"))?;
    let verifier = Arc::new(PinnedHostVerifier { inner, server_name });
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS protocol versions: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::PrivateKeyDer;
    use std::net::{IpAddr, Ipv4Addr};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    /// Self-signed certificate for `host`, returned with a root store trusting it.
    fn cert_for(host: &str) -> (rustls::ServerConfig, RootCertStore) {
        let certified = rcgen::generate_simple_self_signed(vec![host.to_string()])
            .expect("self-signed certificate");
        let cert: CertificateDer<'static> = certified.cert.der().clone();
        let key = PrivateKeyDer::Pkcs8(certified.key_pair.serialize_der().into());
        let server = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("protocol versions")
            .with_no_client_auth()
            .with_single_cert(vec![cert.clone()], key)
            .expect("server config");
        let mut roots = RootCertStore::empty();
        roots.add(cert).expect("trust anchor");
        (server, roots)
    }

    /// Complete a handshake against a local server presenting `cert_host`,
    /// connecting by IP with a client pinned to `pinned_host`.
    async fn handshake(cert_host: &str, pinned_host: &str) -> Result<(), String> {
        let (server, roots) = cert_for(cert_host);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let acceptor = TlsAcceptor::from(Arc::new(server));
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let _ = acceptor.accept(stream).await;
            }
        });

        let config = pinned_client_config_with_roots(pinned_host, roots)?;
        let tcp = TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
        TlsConnector::from(Arc::new(config))
            .connect(ServerName::from(IpAddr::V4(Ipv4Addr::LOCALHOST)), tcp)
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    #[tokio::test]
    async fn validates_certificate_against_pinned_hostname_not_connect_address() {
        handshake("broker.test", "broker.test")
            .await
            .expect("handshake with the pinned hostname");
    }

    #[tokio::test]
    async fn rejects_certificate_for_another_hostname() {
        let err = handshake("broker.test", "other.test")
            .await
            .expect_err("certificate for another name must be rejected");
        assert!(
            err.contains("certificate") || err.contains("Certificate"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn rejects_certificate_from_untrusted_root() {
        let (server, _) = cert_for("broker.test");
        let (_, other_roots) = cert_for("broker.test");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("local addr");
        let acceptor = TlsAcceptor::from(Arc::new(server));
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let _ = acceptor.accept(stream).await;
            }
        });
        let config = pinned_client_config_with_roots("broker.test", other_roots).expect("config");
        let tcp = TcpStream::connect(addr).await.expect("connect");
        let result = TlsConnector::from(Arc::new(config))
            .connect(ServerName::from(IpAddr::V4(Ipv4Addr::LOCALHOST)), tcp)
            .await;
        assert!(result.is_err(), "untrusted issuer must be rejected");
    }

    #[test]
    fn accepts_bracketed_ipv6_and_rejects_invalid_names() {
        let (_, roots) = cert_for("h.test");
        assert!(pinned_client_config_with_roots("[::1]", roots.clone()).is_ok());
        assert!(pinned_client_config_with_roots("bad name", roots).is_err());
    }
}
