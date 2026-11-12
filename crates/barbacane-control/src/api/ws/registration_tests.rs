//! Registration over the data-plane WebSocket against a database: what a
//! key may claim, and what a refused registration is told. Skips without
//! `DATABASE_URL`.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::api::{create_router, AdminAuth, ConnectionManager};
use crate::db::{
    ApiKeysRepository, ArtifactsRepository, DataPlanesRepository, NewApiKey, NewProject,
    ProjectsRepository,
};

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The pool and the WebSocket URL of a control plane served on loopback.
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
    Some((pool, format!("ws://{addr}/ws/data-plane")))
}

async fn project(pool: &PgPool) -> Uuid {
    ProjectsRepository::new(pool.clone())
        .create(NewProject {
            name: format!("ws-{}", Uuid::new_v4().simple()),
            description: None,
            production_mode: false,
        })
        .await
        .expect("project")
        .id
}

async fn key(pool: &PgPool, project_id: Uuid, scopes: &[&str]) -> String {
    ApiKeysRepository::new(pool.clone())
        .create(
            project_id,
            NewApiKey {
                name: "data-plane".into(),
                scopes: scopes.iter().map(|s| s.to_string()).collect(),
                expires_at: None,
            },
        )
        .await
        .expect("key")
        .key
}

async fn artifact(pool: &PgPool, project_id: Uuid) -> Uuid {
    ArtifactsRepository::new(pool.clone())
        .create(
            Some(project_id),
            serde_json::json!({}),
            vec![0x1f, 0x8b],
            "sha",
            "test",
        )
        .await
        .expect("artifact")
        .id
}

async fn connect(url: &str) -> Socket {
    tokio_tungstenite::connect_async(url)
        .await
        .expect("connect")
        .0
}

async fn send(ws: &mut Socket, message: serde_json::Value) {
    ws.send(Message::Text(message.to_string().into()))
        .await
        .expect("send");
}

async fn reply(ws: &mut Socket) -> serde_json::Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => return serde_json::from_str(&text).expect("json"),
            Ok(Some(Ok(_))) => continue,
            other => panic!("no reply: {other:?}"),
        }
    }
}

#[tokio::test]
async fn an_artifact_of_another_project_cannot_be_claimed() {
    let Some((pool, url)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let mine = project(&pool).await;
    let theirs = project(&pool).await;
    let own_artifact = artifact(&pool, mine).await;
    let foreign_artifact = artifact(&pool, theirs).await;
    let api_key = key(&pool, mine, &["data-plane:connect"]).await;
    let data_planes = DataPlanesRepository::new(pool.clone());

    // Claimed at registration: ignored.
    let mut ws = connect(&url).await;
    send(
        &mut ws,
        serde_json::json!({"type": "register", "project_id": mine, "api_key": api_key, "artifact_id": foreign_artifact}),
    )
    .await;
    let registered = reply(&mut ws).await;
    assert_eq!(registered["type"], "registered", "{registered}");
    let data_plane_id: Uuid = registered["data_plane_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let record = data_planes.get(data_plane_id).await.unwrap().unwrap();
    assert_eq!(record.artifact_id, None);

    // Claimed in a heartbeat: ignored, and the heartbeat is still acknowledged.
    send(
        &mut ws,
        serde_json::json!({"type": "heartbeat", "artifact_id": foreign_artifact, "uptime_secs": 1, "requests_total": 0}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "heartbeat_ack");
    assert_eq!(
        data_planes
            .get(data_plane_id)
            .await
            .unwrap()
            .unwrap()
            .artifact_id,
        None
    );

    // Claimed after a download: ignored; the project's own artifact is recorded.
    send(
        &mut ws,
        serde_json::json!({"type": "artifact_downloaded", "artifact_id": foreign_artifact, "success": true}),
    )
    .await;
    send(
        &mut ws,
        serde_json::json!({"type": "heartbeat", "artifact_id": own_artifact, "uptime_secs": 2, "requests_total": 0}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "heartbeat_ack");
    assert_eq!(
        data_planes
            .get(data_plane_id)
            .await
            .unwrap()
            .unwrap()
            .artifact_id,
        Some(own_artifact)
    );
}

#[tokio::test]
async fn a_refused_registration_is_told_one_reason() {
    let Some((pool, url)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let mine = project(&pool).await;
    let theirs = project(&pool).await;
    let foreign_key = key(&pool, theirs, &["data-plane:connect"]).await;
    let scopeless_key = key(&pool, mine, &[]).await;

    for api_key in ["bbk_unknown", foreign_key.as_str(), scopeless_key.as_str()] {
        let mut ws = connect(&url).await;
        send(
            &mut ws,
            serde_json::json!({"type": "register", "project_id": mine, "api_key": api_key}),
        )
        .await;
        let refused = reply(&mut ws).await;
        assert_eq!(refused["type"], "registration_failed", "{refused}");
        assert_eq!(refused["reason"], "registration refused", "{refused}");
    }
}
