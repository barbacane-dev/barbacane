//! Control plane client for connected mode.
//!
//! This module handles WebSocket communication with the control plane,
//! including registration, heartbeat, and artifact notifications.

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{connect_async_tls_with_config, tungstenite::Message, Connector};
use uuid::Uuid;

/// Messages sent from data plane to control plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DataPlaneMessage {
    /// Initial registration with authentication.
    Register {
        project_id: Uuid,
        api_key: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
        #[serde(default)]
        metadata: serde_json::Value,
    },
    /// Periodic heartbeat.
    Heartbeat {
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_id: Option<Uuid>,
        #[serde(skip_serializing_if = "Option::is_none")]
        artifact_hash: Option<String>,
        uptime_secs: u64,
        requests_total: u64,
    },
    /// Acknowledgment of artifact download.
    ArtifactDownloaded {
        artifact_id: Uuid,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Messages sent from control plane to data plane.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlPlaneMessage {
    /// Registration successful.
    Registered {
        data_plane_id: Uuid,
        heartbeat_interval_secs: u32,
    },
    /// Registration failed.
    RegistrationFailed { reason: String },
    /// New artifact available for download.
    ArtifactAvailable {
        artifact_id: Uuid,
        download_url: String,
        sha256: String,
    },
    /// Heartbeat acknowledgment with drift detection status.
    HeartbeatAck { drift_detected: bool },
    /// The session ends. With `reconnect` false the data plane does not
    /// register again until it restarts.
    Disconnect {
        reason: String,
        #[serde(default = "reconnect_by_default")]
        reconnect: bool,
    },
    /// Error message.
    Error { message: String },
}

fn reconnect_by_default() -> bool {
    true
}

/// The CAs the control plane's certificate is checked against, for the
/// WebSocket and the artifact downloads: those of `--control-plane-ca` in place
/// of the public roots, or the public roots.
#[derive(Clone, Default)]
pub struct ControlPlaneTrust {
    /// The PEM bundle, for the download client.
    pem: Option<Arc<Vec<u8>>>,
    /// The WebSocket's TLS configuration.
    tls: Option<Arc<rustls::ClientConfig>>,
}

impl ControlPlaneTrust {
    /// Trust the CAs of the PEM file at `path`, and only them.
    pub fn from_pem_file(path: &std::path::Path) -> Result<Self, String> {
        let pem = std::fs::read(path)
            .map_err(|e| format!("cannot read the control plane CA {}: {e}", path.display()))?;
        let mut roots = rustls::RootCertStore::empty();
        for cert in rustls_pemfile::certs(&mut pem.as_slice()) {
            let cert = cert
                .map_err(|e| format!("the control plane CA {} is not PEM: {e}", path.display()))?;
            roots.add(cert).map_err(|e| {
                format!(
                    "the control plane CA {} holds an invalid certificate: {e}",
                    path.display()
                )
            })?;
        }
        if roots.is_empty() {
            return Err(format!(
                "the control plane CA {} holds no certificate",
                path.display()
            ));
        }
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| format!("cannot build the control plane TLS configuration: {e}"))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            pem: Some(Arc::new(pem)),
            tls: Some(Arc::new(tls)),
        })
    }

    /// `builder` trusting these CAs.
    pub fn apply(&self, builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder, String> {
        let Some(pem) = &self.pem else {
            return Ok(builder);
        };
        let certs = reqwest::Certificate::from_pem_bundle(pem)
            .map_err(|e| format!("the control plane CA is not usable: {e}"))?;
        Ok(certs
            .into_iter()
            .fold(builder.tls_built_in_root_certs(false), |b, cert| {
                b.add_root_certificate(cert)
            }))
    }

    /// The WebSocket connector: these CAs, or the public roots.
    fn connector(&self) -> Option<Connector> {
        self.tls.clone().map(Connector::Rustls)
    }
}

/// Connect to the control plane's WebSocket at `url`, presenting `api_key`.
pub(crate) async fn connect(
    url: &str,
    api_key: &str,
    trust: &ControlPlaneTrust,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    String,
> {
    let request = upgrade_request(url, api_key)?;
    let (stream, _response) =
        connect_async_tls_with_config(request, None, false, trust.connector())
            .await
            .map_err(|e| format!("WebSocket connection failed: {e}"))?;
    Ok(stream)
}

/// Requests the data plane has handled since it started, reported in each
/// heartbeat. Process-wide, so a hot-reload does not reset it.
pub static REQUESTS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// The upgrade request to the control plane at `url`, carrying `api_key` as a
/// bearer token: the control plane checks it before upgrading.
fn upgrade_request(
    url: &str,
    api_key: &str,
) -> Result<tokio_tungstenite::tungstenite::handshake::client::Request, String> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::{header::AUTHORIZATION, HeaderValue};

    let mut request = url
        .into_client_request()
        .map_err(|e| format!("invalid control plane URL: {e}"))?;
    let bearer = HeaderValue::from_str(&format!("Bearer {api_key}"))
        .map_err(|_| "the API key is not a valid header value".to_string())?;
    request.headers_mut().insert(AUTHORIZATION, bearer);
    Ok(request)
}

/// The heartbeat sent every period.
fn heartbeat_message(
    artifact_id: Option<Uuid>,
    artifact_hash: Option<String>,
    uptime_secs: u64,
) -> DataPlaneMessage {
    DataPlaneMessage::Heartbeat {
        artifact_id,
        artifact_hash,
        uptime_secs,
        requests_total: REQUESTS_TOTAL.load(Ordering::Relaxed),
    }
}

/// Configuration for the control plane client.
#[derive(Clone)]
pub struct ControlPlaneConfig {
    pub control_plane_url: String,
    pub project_id: Uuid,
    pub api_key: String,
    pub data_plane_name: Option<String>,
    pub initial_artifact_id: Option<Uuid>,
    pub trust: ControlPlaneTrust,
}

/// Notification that a new artifact is available.
#[derive(Debug, Clone)]
pub struct ArtifactNotification {
    pub artifact_id: Uuid,
    pub download_url: String,
    pub sha256: String,
}

/// Response to send back to the control plane after downloading an artifact.
#[derive(Debug, Clone)]
pub struct ArtifactDownloadedResponse {
    pub artifact_id: Uuid,
    pub success: bool,
    pub error: Option<String>,
}

/// Result of a connection attempt.
enum ConnectOutcome {
    /// Clean shutdown via signal — exit loop.
    Shutdown,
    /// Successfully registered but connection was later lost — reset backoff.
    ConnectionLost(String),
    /// Failed before completing registration — increase backoff.
    ConnectionFailed(String),
    /// The control plane deleted this data plane's record: it does not
    /// register again until it restarts.
    Removed(String),
}

/// Control plane client that maintains connection and handles messages.
pub struct ControlPlaneClient {
    config: ControlPlaneConfig,
}

impl ControlPlaneClient {
    /// Create a new control plane client.
    pub fn new(config: ControlPlaneConfig) -> Self {
        Self { config }
    }

    /// Start the connection loop in a background task.
    /// Returns a receiver for artifact notifications and a sender for download responses.
    pub fn start(
        self,
        shutdown_rx: watch::Receiver<bool>,
        artifact_hash_rx: watch::Receiver<Option<String>>,
        artifact_id_rx: watch::Receiver<Option<Uuid>>,
        drift_flag: Arc<AtomicBool>,
    ) -> (
        mpsc::Receiver<ArtifactNotification>,
        mpsc::Sender<ArtifactDownloadedResponse>,
    ) {
        let (artifact_tx, artifact_rx) = mpsc::channel::<ArtifactNotification>(16);
        let (response_tx, response_rx) = mpsc::channel::<ArtifactDownloadedResponse>(16);

        tokio::spawn(async move {
            self.connection_loop(
                shutdown_rx,
                artifact_tx,
                response_rx,
                artifact_hash_rx,
                artifact_id_rx,
                drift_flag,
            )
            .await;
        });

        (artifact_rx, response_tx)
    }

    /// Main connection loop with reconnection logic.
    async fn connection_loop(
        &self,
        mut shutdown_rx: watch::Receiver<bool>,
        artifact_tx: mpsc::Sender<ArtifactNotification>,
        mut response_rx: mpsc::Receiver<ArtifactDownloadedResponse>,
        artifact_hash_rx: watch::Receiver<Option<String>>,
        artifact_id_rx: watch::Receiver<Option<Uuid>>,
        drift_flag: Arc<AtomicBool>,
    ) {
        const INITIAL_BACKOFF_MS: u64 = 1000;
        const MAX_BACKOFF_MS: u64 = 60000;
        const BACKOFF_MULTIPLIER: f64 = 2.0;

        let mut backoff_ms = INITIAL_BACKOFF_MS;

        loop {
            // Check for shutdown
            if *shutdown_rx.borrow() {
                tracing::info!("Control plane client shutting down");
                return;
            }

            tracing::info!(url = %self.config.control_plane_url, "Connecting to control plane");

            match self
                .try_connect(
                    &mut shutdown_rx,
                    &artifact_tx,
                    &mut response_rx,
                    &artifact_hash_rx,
                    &artifact_id_rx,
                    &drift_flag,
                )
                .await
            {
                ConnectOutcome::Shutdown => {
                    return;
                }
                ConnectOutcome::Removed(reason) => {
                    tracing::error!(
                        reason = %reason,
                        "The control plane removed this data plane; it keeps serving its artifact and registers again when it restarts"
                    );
                    eprintln!(
                        "barbacane: the control plane removed this data plane ({reason}); not reconnecting until restarted"
                    );
                    return;
                }
                ConnectOutcome::ConnectionLost(e) => {
                    // Was registered, connection dropped — reset backoff for fast reconnect
                    tracing::warn!(
                        error = %e,
                        "Control plane connection lost, reconnecting immediately"
                    );
                    backoff_ms = INITIAL_BACKOFF_MS;
                }
                ConnectOutcome::ConnectionFailed(e) => {
                    tracing::warn!(
                        error = %e,
                        backoff_ms = backoff_ms,
                        "Control plane connection failed, will retry"
                    );
                }
            }

            // Wait before reconnecting (or abort if shutdown)
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(backoff_ms)) => {}
            }

            // Increase backoff for next attempt, capped at MAX_BACKOFF_MS
            backoff_ms =
                ((backoff_ms as f64) * BACKOFF_MULTIPLIER).min(MAX_BACKOFF_MS as f64) as u64;
        }
    }

    /// Attempt to connect and handle messages.
    async fn try_connect(
        &self,
        shutdown_rx: &mut watch::Receiver<bool>,
        artifact_tx: &mpsc::Sender<ArtifactNotification>,
        response_rx: &mut mpsc::Receiver<ArtifactDownloadedResponse>,
        artifact_hash_rx: &watch::Receiver<Option<String>>,
        artifact_id_rx: &watch::Receiver<Option<Uuid>>,
        drift_flag: &Arc<AtomicBool>,
    ) -> ConnectOutcome {
        // Connect to WebSocket, presenting the API key on the upgrade.
        let ws_stream = match connect(
            &self.config.control_plane_url,
            &self.config.api_key,
            &self.config.trust,
        )
        .await
        {
            Ok(stream) => stream,
            Err(e) => return ConnectOutcome::ConnectionFailed(e),
        };

        let (mut sender, mut receiver) = ws_stream.split();

        // Send registration message
        let register_msg = DataPlaneMessage::Register {
            project_id: self.config.project_id,
            api_key: self.config.api_key.clone(),
            name: self.config.data_plane_name.clone(),
            artifact_id: self.config.initial_artifact_id,
            metadata: serde_json::json!({}),
        };

        let register_json = match serde_json::to_string(&register_msg) {
            Ok(j) => j,
            Err(e) => {
                return ConnectOutcome::ConnectionFailed(format!(
                    "Failed to serialize register message: {}",
                    e
                ))
            }
        };

        if let Err(e) = sender.send(Message::Text(register_json.into())).await {
            return ConnectOutcome::ConnectionFailed(format!(
                "Failed to send register message: {}",
                e
            ));
        }

        // Wait for registration response
        let registration_response =
            match tokio::time::timeout(Duration::from_secs(30), receiver.next()).await {
                Ok(Some(Ok(msg))) => msg,
                Ok(Some(Err(e))) => {
                    return ConnectOutcome::ConnectionFailed(format!("WebSocket error: {}", e))
                }
                Ok(None) => {
                    return ConnectOutcome::ConnectionFailed(
                        "Connection closed before registration".to_string(),
                    )
                }
                Err(_) => {
                    return ConnectOutcome::ConnectionFailed("Registration timeout".to_string())
                }
            };

        let heartbeat_interval_secs = match registration_response {
            Message::Text(text) => {
                let msg: ControlPlaneMessage = match serde_json::from_str(&text) {
                    Ok(m) => m,
                    Err(e) => {
                        return ConnectOutcome::ConnectionFailed(format!(
                            "Failed to parse registration response: {}",
                            e
                        ))
                    }
                };

                match msg {
                    ControlPlaneMessage::Registered {
                        data_plane_id,
                        heartbeat_interval_secs,
                    } => {
                        tracing::info!(
                            data_plane_id = %data_plane_id,
                            heartbeat_interval_secs,
                            "Registered with control plane"
                        );
                        heartbeat_interval_secs
                    }
                    ControlPlaneMessage::RegistrationFailed { reason } => {
                        return ConnectOutcome::ConnectionFailed(format!(
                            "Registration failed: {}",
                            reason
                        ));
                    }
                    other => {
                        return ConnectOutcome::ConnectionFailed(format!(
                            "Unexpected registration response: {:?}",
                            other
                        ));
                    }
                }
            }
            other => {
                return ConnectOutcome::ConnectionFailed(format!(
                    "Unexpected message type: {:?}",
                    other
                ));
            }
        };

        // Start heartbeat timer
        let mut heartbeat_interval =
            tokio::time::interval(heartbeat_period(heartbeat_interval_secs));
        let start_time = std::time::Instant::now();

        // Main message loop — we are now registered, so any disconnect
        // should trigger reconnection (ConnectionLost), not give up.
        loop {
            tokio::select! {
                // Shutdown signal
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::info!("Disconnecting from control plane");
                        let _ = sender.close().await;
                        return ConnectOutcome::Shutdown;
                    }
                }

                // Heartbeat timer
                _ = heartbeat_interval.tick() => {
                    let heartbeat = heartbeat_message(
                        *artifact_id_rx.borrow(),
                        artifact_hash_rx.borrow().clone(),
                        start_time.elapsed().as_secs(),
                    );

                    let json = match serde_json::to_string(&heartbeat) {
                        Ok(j) => j,
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to serialize heartbeat");
                            continue;
                        }
                    };

                    if let Err(e) = sender.send(Message::Text(json.into())).await {
                        return ConnectOutcome::ConnectionLost(format!(
                            "Failed to send heartbeat: {}", e
                        ));
                    }

                    tracing::debug!("Heartbeat sent");
                }

                // Artifact download response from main loop
                Some(response) = response_rx.recv() => {
                    let msg = DataPlaneMessage::ArtifactDownloaded {
                        artifact_id: response.artifact_id,
                        success: response.success,
                        error: response.error,
                    };

                    let json = match serde_json::to_string(&msg) {
                        Ok(j) => j,
                        Err(e) => {
                            tracing::error!(error = %e, "Failed to serialize artifact downloaded");
                            continue;
                        }
                    };

                    if let Err(e) = sender.send(Message::Text(json.into())).await {
                        tracing::warn!(error = %e, "Failed to send artifact downloaded response");
                    } else {
                        tracing::info!(
                            artifact_id = %response.artifact_id,
                            success = response.success,
                            "Sent artifact downloaded response to control plane"
                        );
                    }
                }

                // Incoming messages
                result = receiver.next() => {
                    match result {
                        Some(Ok(Message::Text(text))) => {
                            match serde_json::from_str::<ControlPlaneMessage>(&text) {
                                Ok(ControlPlaneMessage::Disconnect { reason, reconnect }) => {
                                    tracing::info!(reason = %reason, reconnect, "Disconnected by the control plane");
                                    let _ = sender.close().await;
                                    return if reconnect {
                                        ConnectOutcome::ConnectionLost(format!("Disconnected by the control plane: {reason}"))
                                    } else {
                                        ConnectOutcome::Removed(reason)
                                    };
                                }
                                Ok(msg) => {
                                    if let Err(e) = self.handle_message(msg, artifact_tx, &mut sender, drift_flag).await {
                                        tracing::warn!(error = %e, "Error handling control plane message");
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(error = %e, "Failed to parse control plane message");
                                }
                            }
                        }
                        Some(Ok(Message::Ping(data))) => {
                            let _ = sender.send(Message::Pong(data)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            return ConnectOutcome::ConnectionLost(
                                "Connection closed by control plane".to_string()
                            );
                        }
                        Some(Err(e)) => {
                            return ConnectOutcome::ConnectionLost(format!(
                                "WebSocket error: {}", e
                            ));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Handle a message from the control plane.
    async fn handle_message(
        &self,
        msg: ControlPlaneMessage,
        artifact_tx: &mpsc::Sender<ArtifactNotification>,
        _sender: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        drift_flag: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        match msg {
            ControlPlaneMessage::HeartbeatAck { drift_detected } => {
                drift_flag.store(drift_detected, Ordering::Relaxed);
                if drift_detected {
                    tracing::warn!("Control plane detected configuration drift");
                }
                tracing::debug!(drift_detected, "Heartbeat acknowledged");
            }
            ControlPlaneMessage::ArtifactAvailable {
                artifact_id,
                download_url,
                sha256,
            } => {
                tracing::info!(
                    artifact_id = %artifact_id,
                    download_url = %download_url,
                    "New artifact available"
                );

                // Notify the main loop about the new artifact
                if let Err(e) = artifact_tx
                    .send(ArtifactNotification {
                        artifact_id,
                        download_url,
                        sha256,
                    })
                    .await
                {
                    tracing::warn!(error = %e, "Failed to send artifact notification");
                }
            }
            ControlPlaneMessage::Error { message } => {
                tracing::warn!(message = %message, "Error from control plane");
            }
            // These shouldn't happen after registration; a disconnect is
            // handled by the message loop.
            ControlPlaneMessage::Registered { .. }
            | ControlPlaneMessage::RegistrationFailed { .. }
            | ControlPlaneMessage::Disconnect { .. } => {
                tracing::warn!("Unexpected registration message after already registered");
            }
        }

        Ok(())
    }
}

/// The heartbeat period the control plane asked for, at least one second: a
/// zero period is not a valid timer interval, and would end the connection
/// task.
fn heartbeat_period(interval_secs: u32) -> Duration {
    Duration::from_secs(u64::from(interval_secs.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A CA and a server configuration for `localhost` with a certificate it
    /// issued; the CA as PEM.
    fn private_ca() -> (String, Arc<rustls::ServerConfig>) {
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = params.self_signed(&ca_key).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let leaf = rcgen::CertificateParams::new(vec!["localhost".to_string()])
            .unwrap()
            .signed_by(&key, &ca, &ca_key)
            .unwrap();
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            )
            .unwrap();
        (ca.pem(), Arc::new(config))
    }

    /// Serve TLS with `config` on loopback: a WebSocket that holds each
    /// session when `websocket`, else `200` to every request. The port.
    async fn tls_server(config: Arc<rustls::ServerConfig>, websocket: bool) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    if websocket {
                        if let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await {
                            let _ = ws.next().await;
                        }
                        return;
                    }
                    let mut request = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match tls.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => request.extend_from_slice(&buf[..n]),
                        }
                    }
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn a_private_ca_is_trusted_for_the_socket_and_the_downloads() {
        // As `barbacane` does at start, for the clients that use the public roots.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let (ca_pem, config) = private_ca();
        let dir = tempfile::tempdir().unwrap();
        let ca_path = dir.path().join("ca.pem");
        std::fs::write(&ca_path, ca_pem).unwrap();
        let trust = ControlPlaneTrust::from_pem_file(&ca_path).unwrap();
        let public = ControlPlaneTrust::default();

        let port = tls_server(Arc::clone(&config), true).await;
        let url = format!("wss://localhost:{port}/ws/data-plane");
        let connected = connect(&url, "key", &trust).await;
        assert!(connected.is_ok(), "{:?}", connected.err());
        assert!(
            connect(&url, "key", &public).await.is_err(),
            "the public roots do not know the private CA"
        );

        let port = tls_server(config, false).await;
        let url = format!("https://localhost:{port}/artifacts");
        let downloaded = crate::hot_reload::download_client(&trust)
            .unwrap()
            .get(&url)
            .send()
            .await;
        assert!(
            downloaded.as_ref().is_ok_and(|r| r.status() == 200),
            "{downloaded:?}"
        );
        let refused = crate::hot_reload::download_client(&public)
            .unwrap()
            .get(&url)
            .send()
            .await;
        assert!(refused.is_err(), "{refused:?}");

        std::fs::write(&ca_path, "not a certificate").unwrap();
        assert!(ControlPlaneTrust::from_pem_file(&ca_path).is_err());
    }

    /// Run a data plane against a control plane that registers it, then ends
    /// the session with a `disconnect` saying `reconnect`. How many times it
    /// connected after `wait`.
    async fn connections_after_a_disconnect(reconnect: bool, wait: Duration) -> u64 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let connections = Arc::new(AtomicU64::new(0));
        let counted = Arc::clone(&connections);
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                counted.fetch_add(1, Ordering::SeqCst);
                let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else {
                    continue;
                };
                let _register = ws.next().await;
                let registered = serde_json::json!({
                    "type": "registered",
                    "data_plane_id": Uuid::new_v4(),
                    "heartbeat_interval_secs": 30,
                });
                let disconnect = serde_json::json!({
                    "type": "disconnect",
                    "reason": "ended by the test",
                    "reconnect": reconnect,
                });
                let _ = ws.send(Message::Text(registered.to_string().into())).await;
                let _ = ws.send(Message::Text(disconnect.to_string().into())).await;
                let _ = ws.close(None).await;
            }
        });

        let client = ControlPlaneClient::new(ControlPlaneConfig {
            control_plane_url: format!("ws://127.0.0.1:{port}/ws/data-plane"),
            project_id: Uuid::new_v4(),
            api_key: "key".to_string(),
            data_plane_name: None,
            initial_artifact_id: None,
            trust: ControlPlaneTrust::default(),
        });
        let (_shutdown_tx, shutdown_rx) = watch::channel(false);
        let (_hash_tx, hash_rx) = watch::channel(None);
        let (_id_tx, id_rx) = watch::channel(None);
        let _channels = client.start(
            shutdown_rx,
            hash_rx,
            id_rx,
            Arc::new(AtomicBool::new(false)),
        );
        tokio::time::sleep(wait).await;
        connections.load(Ordering::SeqCst)
    }

    #[tokio::test]
    async fn a_removed_data_plane_does_not_register_again() {
        let wait = Duration::from_millis(2500);
        let (removed, ended) = tokio::join!(
            connections_after_a_disconnect(false, wait),
            connections_after_a_disconnect(true, wait),
        );
        assert_eq!(removed, 1, "removed: it stays disconnected");
        assert!(ended >= 2, "any other end: it registers again ({ended})");
    }

    #[test]
    fn the_upgrade_presents_the_api_key() {
        let request = upgrade_request("wss://control.example/ws/data-plane", "bbk_key").unwrap();
        assert_eq!(request.headers()["authorization"], "Bearer bbk_key");
        assert!(upgrade_request("wss://control.example/ws/data-plane", "bad\nkey").is_err());
    }

    #[test]
    fn a_zero_heartbeat_interval_becomes_one_second() {
        assert_eq!(heartbeat_period(0), Duration::from_secs(1));
        assert_eq!(heartbeat_period(30), Duration::from_secs(30));
        // A timer with this period must exist: a zero period panics.
        let _ = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async { tokio::time::interval(heartbeat_period(0)) });
    }

    #[test]
    fn a_heartbeat_carries_the_artifact_and_the_request_count() {
        REQUESTS_TOTAL.fetch_add(3, Ordering::Relaxed);
        let id = Uuid::new_v4();
        let DataPlaneMessage::Heartbeat {
            artifact_id,
            artifact_hash,
            requests_total,
            ..
        } = heartbeat_message(Some(id), Some("abc".into()), 7)
        else {
            panic!("a heartbeat");
        };
        assert_eq!(artifact_id, Some(id));
        assert_eq!(artifact_hash.as_deref(), Some("abc"));
        assert!(requests_total >= 3);
    }

    #[test]
    fn test_data_plane_message_register_serialization() {
        let msg = DataPlaneMessage::Register {
            project_id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            api_key: "test-key".to_string(),
            name: Some("my-data-plane".to_string()),
            artifact_id: None,
            metadata: serde_json::json!({"version": "1.0"}),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"register\""));
        assert!(json.contains("\"project_id\":"));
        assert!(json.contains("\"api_key\":\"test-key\""));
        assert!(json.contains("\"name\":\"my-data-plane\""));
    }

    #[test]
    fn test_data_plane_message_heartbeat_serialization() {
        let msg = DataPlaneMessage::Heartbeat {
            artifact_id: Some(Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()),
            artifact_hash: Some("sha256:abc123".to_string()),
            uptime_secs: 3600,
            requests_total: 1000,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"heartbeat\""));
        assert!(json.contains("\"uptime_secs\":3600"));
        assert!(json.contains("\"requests_total\":1000"));
        assert!(json.contains("\"artifact_hash\":\"sha256:abc123\""));
    }

    #[test]
    fn test_data_plane_message_artifact_downloaded_success() {
        let artifact_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let msg = DataPlaneMessage::ArtifactDownloaded {
            artifact_id,
            success: true,
            error: None,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"artifact_downloaded\""));
        assert!(json.contains("\"success\":true"));
        assert!(!json.contains("\"error\":")); // None should be skipped
    }

    #[test]
    fn test_data_plane_message_artifact_downloaded_failure() {
        let artifact_id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let msg = DataPlaneMessage::ArtifactDownloaded {
            artifact_id,
            success: false,
            error: Some("checksum mismatch".to_string()),
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"artifact_downloaded\""));
        assert!(json.contains("\"success\":false"));
        assert!(json.contains("\"error\":\"checksum mismatch\""));
    }

    #[test]
    fn test_control_plane_message_registered_deserialization() {
        let json = r#"{
            "type": "registered",
            "data_plane_id": "550e8400-e29b-41d4-a716-446655440000",
            "heartbeat_interval_secs": 30
        }"#;

        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::Registered {
                data_plane_id,
                heartbeat_interval_secs,
            } => {
                assert_eq!(
                    data_plane_id,
                    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
                );
                assert_eq!(heartbeat_interval_secs, 30);
            }
            _ => panic!("Expected Registered message"),
        }
    }

    #[test]
    fn test_control_plane_message_artifact_available_deserialization() {
        let json = r#"{
            "type": "artifact_available",
            "artifact_id": "550e8400-e29b-41d4-a716-446655440000",
            "download_url": "http://localhost:9090/artifacts/123/download",
            "sha256": "abc123def456"
        }"#;

        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::ArtifactAvailable {
                artifact_id,
                download_url,
                sha256,
            } => {
                assert_eq!(
                    artifact_id,
                    Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap()
                );
                assert_eq!(download_url, "http://localhost:9090/artifacts/123/download");
                assert_eq!(sha256, "abc123def456");
            }
            _ => panic!("Expected ArtifactAvailable message"),
        }
    }

    #[test]
    fn test_control_plane_message_disconnect_deserialization() {
        let json = r#"{
            "type": "disconnect",
            "reason": "server shutting down"
        }"#;

        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::Disconnect { reason, reconnect } => {
                assert_eq!(reason, "server shutting down");
                assert!(reconnect, "a notice without the field lets it reconnect");
            }
            _ => panic!("Expected Disconnect message"),
        }

        let msg: ControlPlaneMessage = serde_json::from_str(
            r#"{"type": "disconnect", "reason": "data plane removed", "reconnect": false}"#,
        )
        .unwrap();
        assert!(matches!(
            msg,
            ControlPlaneMessage::Disconnect {
                reconnect: false,
                ..
            }
        ));
    }

    #[test]
    fn test_artifact_downloaded_response_creation() {
        let artifact_id = Uuid::new_v4();

        let success_response = ArtifactDownloadedResponse {
            artifact_id,
            success: true,
            error: None,
        };
        assert!(success_response.success);
        assert!(success_response.error.is_none());

        let failure_response = ArtifactDownloadedResponse {
            artifact_id,
            success: false,
            error: Some("download failed".to_string()),
        };
        assert!(!failure_response.success);
        assert_eq!(failure_response.error.as_deref(), Some("download failed"));
    }

    #[test]
    fn test_artifact_notification_creation() {
        let notification = ArtifactNotification {
            artifact_id: Uuid::new_v4(),
            download_url: "http://example.com/artifact.bca".to_string(),
            sha256: "abc123".to_string(),
        };

        assert!(!notification.download_url.is_empty());
        assert!(!notification.sha256.is_empty());
    }

    #[test]
    fn test_control_plane_config_creation() {
        let config = ControlPlaneConfig {
            control_plane_url: "ws://localhost:9090/ws/data-plane".to_string(),
            project_id: Uuid::new_v4(),
            api_key: "test-api-key".to_string(),
            data_plane_name: Some("test-plane".to_string()),
            initial_artifact_id: None,
            trust: ControlPlaneTrust::default(),
        };

        assert!(config.control_plane_url.starts_with("ws://"));
        assert_eq!(config.api_key, "test-api-key");
        assert_eq!(config.data_plane_name.as_deref(), Some("test-plane"));
    }

    #[test]
    fn test_heartbeat_ack_with_drift_serialization() {
        let json = r#"{"type":"heartbeat_ack","drift_detected":true}"#;
        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::HeartbeatAck { drift_detected } => {
                assert!(drift_detected);
            }
            _ => panic!("Expected HeartbeatAck message"),
        }
    }

    #[test]
    fn test_heartbeat_ack_without_drift_serialization() {
        let json = r#"{"type":"heartbeat_ack","drift_detected":false}"#;
        let msg: ControlPlaneMessage = serde_json::from_str(json).unwrap();
        match msg {
            ControlPlaneMessage::HeartbeatAck { drift_detected } => {
                assert!(!drift_detected);
            }
            _ => panic!("Expected HeartbeatAck message"),
        }
    }

    #[test]
    fn test_heartbeat_with_artifact_hash_serialization() {
        let msg = DataPlaneMessage::Heartbeat {
            artifact_id: None,
            artifact_hash: Some("sha256:abc123def".to_string()),
            uptime_secs: 120,
            requests_total: 50,
        };

        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"artifact_hash\":\"sha256:abc123def\""));

        // Round-trip
        let deserialized: DataPlaneMessage = serde_json::from_str(&json).unwrap();
        match deserialized {
            DataPlaneMessage::Heartbeat {
                artifact_hash,
                uptime_secs,
                ..
            } => {
                assert_eq!(artifact_hash, Some("sha256:abc123def".to_string()));
                assert_eq!(uptime_secs, 120);
            }
            _ => panic!("Expected Heartbeat message"),
        }
    }

    #[test]
    fn test_heartbeat_without_artifact_hash() {
        let msg = DataPlaneMessage::Heartbeat {
            artifact_id: None,
            artifact_hash: None,
            uptime_secs: 0,
            requests_total: 0,
        };

        let json = serde_json::to_string(&msg).unwrap();
        // artifact_hash should be omitted when None (skip_serializing_if)
        assert!(
            !json.contains("artifact_hash"),
            "artifact_hash should be omitted when None"
        );
    }

    #[test]
    fn test_drift_flag_updated_by_heartbeat_ack() {
        let drift_flag = Arc::new(AtomicBool::new(false));

        // Simulate receiving drift_detected = true
        drift_flag.store(true, Ordering::Relaxed);
        assert!(drift_flag.load(Ordering::Relaxed));

        // Simulate receiving drift_detected = false
        drift_flag.store(false, Ordering::Relaxed);
        assert!(!drift_flag.load(Ordering::Relaxed));
    }
}
