//! In-process tests of the data-plane WebSocket. The router is served on a
//! loopback port with a pool that never connects: everything checked here
//! happens before the first database query.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::api::{create_router, AdminAuth, ConnectionManager};

async fn serve() -> String {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(1))
        .connect_lazy("postgres://nobody@127.0.0.1:1/unreachable")
        .expect("a lazy pool needs no server");
    let app = create_router(
        pool,
        None,
        Arc::new(ConnectionManager::new()),
        AdminAuth::Disabled,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("ws://{addr}/ws/data-plane")
}

/// The next text frame within five seconds, or `None` once the server closes.
async fn next_text(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<String> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => return Some(text.to_string()),
            Ok(Some(Ok(Message::Close(_)))) | Ok(Some(Err(_))) | Ok(None) => return None,
            Ok(Some(Ok(_))) => continue,
            Err(_) => panic!("no reply within five seconds"),
        }
    }
}

#[tokio::test]
async fn a_message_larger_than_the_cap_ends_the_connection_before_registration() {
    let url = serve().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    let oversize = format!(
        r#"{{"type":"register","pad":"{}"}}"#,
        "x".repeat(1024 * 1024)
    );
    // The send may succeed before the server reads: it is the read that is refused.
    let _ = ws.send(Message::Text(oversize.into())).await;
    assert_eq!(
        next_text(&mut ws).await,
        None,
        "the server must close the socket"
    );
}

#[tokio::test]
async fn a_registration_with_an_oversized_name_is_refused() {
    let url = serve().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    let register = serde_json::json!({
        "type": "register",
        "project_id": uuid::Uuid::new_v4(),
        "api_key": "bbk_test",
        "name": "n".repeat(10_000),
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .expect("send");
    let reply = next_text(&mut ws).await.expect("a reply");
    assert!(reply.contains("registration_failed"), "{reply}");
    assert!(reply.contains("invalid registration"), "{reply}");
}

#[tokio::test]
async fn a_registration_within_the_limits_reaches_key_validation() {
    let url = serve().await;
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect");
    let register = serde_json::json!({
        "type": "register",
        "project_id": uuid::Uuid::new_v4(),
        "api_key": "bbk_test",
        "name": "edge-1",
    });
    ws.send(Message::Text(register.to_string().into()))
        .await
        .expect("send");
    // The pool never connects, so the key lookup fails: the fields were accepted.
    let reply = next_text(&mut ws).await.expect("a reply");
    assert!(reply.contains("Internal error"), "{reply}");
}
