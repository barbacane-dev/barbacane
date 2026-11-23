//! In-process tests of the data-plane WebSocket. The router is served on a
//! loopback port with a pool that never connects: everything checked here
//! happens before an upgrade.

use std::sync::Arc;
use std::time::Duration;

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

#[tokio::test]
async fn an_upgrade_without_a_key_is_refused_before_it_starts() {
    let url = serve().await;
    let error = super::connect(&url, None)
        .await
        .expect_err("upgraded without a key");
    assert_eq!(super::refused_status(error), 401);
}

#[tokio::test]
async fn an_upgrade_whose_key_cannot_be_checked_is_refused() {
    let url = serve().await;
    // The pool never connects, so the key cannot be looked up.
    let error = super::connect(&url, Some("bbk_test"))
        .await
        .expect_err("upgraded with an unchecked key");
    assert_eq!(super::refused_status(error), 503);
}
