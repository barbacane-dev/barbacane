//! WebSocket module for data plane connections.

pub mod handler;
pub mod manager;
pub mod protocol;

pub use handler::ws_handler;
pub use manager::ConnectionManager;

/// The data-plane socket at `url`, its upgrade presenting `key` when given.
#[cfg(test)]
async fn connect(
    url: &str,
    key: Option<&str>,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request()?;
    if let Some(key) = key {
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {key}").parse().expect("header"),
        );
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(ws, _)| ws)
}

/// The status a refused upgrade was answered with.
#[cfg(test)]
fn refused_status(error: tokio_tungstenite::tungstenite::Error) -> u16 {
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => response.status().as_u16(),
        other => panic!("not an HTTP refusal: {other}"),
    }
}

#[cfg(test)]
mod registration_tests;
#[cfg(test)]
mod session_tests;
#[cfg(test)]
mod socket_tests;
