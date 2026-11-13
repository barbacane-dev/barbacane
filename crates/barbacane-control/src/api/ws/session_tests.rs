//! A data-plane session over the WebSocket against a database: what ends
//! it. Skips without `DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::api::{create_router, AdminAuth, ConnectionManager};
use crate::db::{ApiKeysRepository, NewApiKey, NewProject, ProjectsRepository};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The pool and the base URL of a control plane served on loopback.
async fn serve() -> Option<(PgPool, String)> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let pool = crate::db::create_pool(&url).await.ok()?;
    crate::db::run_migrations(&pool).await.ok()?;
    let app = create_router(
        pool.clone(),
        None,
        Arc::new(ConnectionManager::new()),
        AdminAuth::Disabled,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.ok()?;
    let addr = listener.local_addr().ok()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Some((pool, format!("http://{addr}")))
}

async fn registered_session(pool: &PgPool, base: &str) -> (Uuid, Uuid, Socket) {
    let project = ProjectsRepository::new(pool.clone())
        .create(NewProject {
            name: format!("ws-{}", Uuid::new_v4().simple()),
            description: None,
            production_mode: false,
        })
        .await
        .expect("project")
        .id;
    let key = ApiKeysRepository::new(pool.clone())
        .create(
            project,
            NewApiKey {
                name: "data-plane".into(),
                scopes: vec!["data-plane:connect".into()],
                expires_at: None,
            },
        )
        .await
        .expect("key")
        .key;
    let ws_url = format!("{}/ws/data-plane", base.replacen("http://", "ws://", 1));
    let (mut ws, _) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .expect("connect");
    let register = serde_json::json!({"type": "register", "project_id": project, "api_key": key});
    ws.send(Message::Text(register.to_string().into()))
        .await
        .expect("send");
    let registered: serde_json::Value = loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => break serde_json::from_str(&text).expect("json"),
            Ok(Some(Ok(_))) => continue,
            other => panic!("no registration reply: {other:?}"),
        }
    };
    assert_eq!(registered["type"], "registered", "{registered}");
    let data_plane_id = registered["data_plane_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    (project, data_plane_id, ws)
}

/// Deleting a data plane's record ends its session.
#[tokio::test]
async fn deleting_a_data_plane_ends_its_session() {
    let Some((pool, base)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let (project, data_plane_id, mut ws) = registered_session(&pool, &base).await;

    let deleted = reqwest::Client::new()
        .delete(format!(
            "{base}/projects/{project}/data-planes/{data_plane_id}"
        ))
        .send()
        .await
        .expect("delete");
    assert_eq!(deleted.status(), 204);

    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert!(
        ended.is_ok(),
        "the session must end once the record is deleted"
    );
}
