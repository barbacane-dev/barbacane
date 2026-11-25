//! Registration over the data-plane WebSocket against a database: what a
//! key may claim, and what a refused registration is told. Needs PostgreSQL
//! ([`crate::api::test_pool`]).

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
    serve_with(ConnectionManager::new()).await
}

async fn serve_with(manager: ConnectionManager) -> Option<(PgPool, String)> {
    let pool = crate::api::test_pool().await?;
    let app = create_router(pool.clone(), None, Arc::new(manager), AdminAuth::Disabled);
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
                name: format!("data-plane-{}", Uuid::new_v4().simple()),
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

async fn connect(url: &str, key: &str) -> Socket {
    super::connect(url, Some(key)).await.expect("connect")
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
    let mut ws = connect(&url, &api_key).await;
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

    let own_key = key(&pool, mine, &["data-plane:connect"]).await;
    let second_key = key(&pool, mine, &["data-plane:connect"]).await;

    // Refused before the upgrade.
    for api_key in ["bbk_unknown", scopeless_key.as_str()] {
        let error = super::connect(&url, Some(api_key))
            .await
            .expect_err("upgraded");
        assert_eq!(super::refused_status(error), 401, "{api_key}");
    }

    // Refused at registration: a key of another project, or a registration
    // naming another key than the upgrade's.
    for (upgrade_key, registered_key) in [
        (foreign_key.as_str(), foreign_key.as_str()),
        (own_key.as_str(), second_key.as_str()),
    ] {
        let mut ws = connect(&url, upgrade_key).await;
        send(
            &mut ws,
            serde_json::json!({"type": "register", "project_id": mine, "api_key": registered_key}),
        )
        .await;
        let refused = reply(&mut ws).await;
        assert_eq!(refused["type"], "registration_failed", "{refused}");
        assert_eq!(refused["reason"], "registration refused", "{refused}");
    }
}

/// The next text frame within five seconds, or `None` once the server closes.
async fn next_text(ws: &mut Socket) -> Option<String> {
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
    let Some((pool, url)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let mine = project(&pool).await;
    let mut ws = connect(&url, &key(&pool, mine, &["data-plane:connect"]).await).await;
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
    let Some((pool, url)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let mine = project(&pool).await;
    let api_key = key(&pool, mine, &["data-plane:connect"]).await;
    let mut ws = connect(&url, &api_key).await;
    send(
        &mut ws,
        serde_json::json!({"type": "register", "project_id": mine, "api_key": api_key, "name": "n".repeat(10_000)}),
    )
    .await;
    let reply = next_text(&mut ws).await.expect("a reply");
    assert!(reply.contains("registration_failed"), "{reply}");
    assert!(reply.contains("invalid registration"), "{reply}");
}

/// A registered data plane that sends nothing for the silence limit is
/// ended, as one gone without closing its connection is.
#[tokio::test]
async fn a_silent_session_is_ended() {
    let Some((pool, url)) = serve_with(ConnectionManager::with_silence_limit(
        Duration::from_millis(500),
    ))
    .await
    else {
        eprintln!("skip: database not available");
        return;
    };
    let mine = project(&pool).await;
    let api_key = key(&pool, mine, &["data-plane:connect"]).await;
    let mut ws = connect(&url, &api_key).await;
    send(
        &mut ws,
        serde_json::json!({"type": "register", "project_id": mine, "api_key": api_key}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "registered");

    // A heartbeat keeps the session.
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(
        &mut ws,
        serde_json::json!({"type": "heartbeat", "uptime_secs": 1, "requests_total": 0}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "heartbeat_ack");

    // Then silence ends it.
    let ended = loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Ok(Some(Ok(Message::Close(frame)))) => break frame,
            Ok(Some(Ok(_))) => continue,
            other => panic!("the session was not ended: {other:?}"),
        }
    };
    let frame = ended.expect("a close frame");
    assert!(
        frame.reason.contains("no message from the data plane"),
        "{frame:?}"
    );
}

/// A data plane that registers without the project's deployment is sent it
/// after its confirmation, and is `deploying` until it reports the outcome:
/// a failure is kept as its `deploy_error`, a success clears it.
#[tokio::test]
async fn a_registration_without_the_deployment_is_sent_it() {
    let Some((pool, url)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let project_id = project(&pool).await;
    let deployed = artifact(&pool, project_id).await;
    ProjectsRepository::new(pool.clone())
        .set_deployment(project_id, deployed)
        .await
        .unwrap();
    let api_key = key(&pool, project_id, &["data-plane:connect"]).await;
    let data_planes = DataPlanesRepository::new(pool.clone());

    let mut ws = connect(&url, &api_key).await;
    send(
        &mut ws,
        serde_json::json!({"type": "register", "project_id": project_id, "api_key": api_key}),
    )
    .await;
    let registered = reply(&mut ws).await;
    assert_eq!(registered["type"], "registered", "{registered}");
    let data_plane_id: Uuid = registered["data_plane_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let available = reply(&mut ws).await;
    assert_eq!(available["type"], "artifact_available", "{available}");
    assert_eq!(available["artifact_id"], deployed.to_string());
    assert_eq!(
        available["download_url"],
        format!("/artifacts/{deployed}/download")
    );
    assert_eq!(available["sha256"], "sha");
    assert_eq!(
        data_planes
            .get(data_plane_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "deploying"
    );

    send(
        &mut ws,
        serde_json::json!({"type": "artifact_downloaded", "artifact_id": deployed, "success": false, "error": "load failed: E1045"}),
    )
    .await;
    send(
        &mut ws,
        serde_json::json!({"type": "heartbeat", "uptime_secs": 1, "requests_total": 0}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "heartbeat_ack");
    let record = data_planes.get(data_plane_id).await.unwrap().unwrap();
    assert_eq!(record.status, "online");
    assert_eq!(record.deploy_error.as_deref(), Some("load failed: E1045"));
    assert_eq!(record.artifact_id, None);

    send(
        &mut ws,
        serde_json::json!({"type": "artifact_downloaded", "artifact_id": deployed, "success": true}),
    )
    .await;
    send(
        &mut ws,
        serde_json::json!({"type": "heartbeat", "uptime_secs": 2, "requests_total": 0}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "heartbeat_ack");
    let record = data_planes.get(data_plane_id).await.unwrap().unwrap();
    assert_eq!(record.deploy_error, None);
    assert_eq!(record.artifact_id, Some(deployed));
}

/// A data plane that registers with the deployment it runs is sent nothing.
#[tokio::test]
async fn a_registration_with_the_deployment_is_sent_nothing() {
    let Some((pool, url)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let project_id = project(&pool).await;
    let deployed = artifact(&pool, project_id).await;
    ProjectsRepository::new(pool.clone())
        .set_deployment(project_id, deployed)
        .await
        .unwrap();
    let api_key = key(&pool, project_id, &["data-plane:connect"]).await;

    let mut ws = connect(&url, &api_key).await;
    send(
        &mut ws,
        serde_json::json!({"type": "register", "project_id": project_id, "api_key": api_key, "artifact_id": deployed}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "registered");
    send(
        &mut ws,
        serde_json::json!({"type": "heartbeat", "artifact_id": deployed, "uptime_secs": 1, "requests_total": 0}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "heartbeat_ack");
}

/// Drift is a data plane reporting another artifact than the project's
/// deployment.
#[tokio::test]
async fn drift_is_measured_against_the_deployment() {
    let Some((pool, url)) = serve().await else {
        eprintln!("skip: database not available");
        return;
    };
    let project_id = project(&pool).await;
    let deployed = ArtifactsRepository::new(pool.clone())
        .create(
            Some(project_id),
            serde_json::json!({"artifact_hash": "sha256:deployed"}),
            vec![0x1f, 0x8b],
            "sha",
            "test",
        )
        .await
        .unwrap()
        .id;
    ProjectsRepository::new(pool.clone())
        .set_deployment(project_id, deployed)
        .await
        .unwrap();
    let api_key = key(&pool, project_id, &["data-plane:connect"]).await;

    let mut ws = connect(&url, &api_key).await;
    // A data plane started from the artifact baked into its image: it
    // claims no artifact, and is sent the deployment, which it has not
    // loaded yet.
    send(
        &mut ws,
        serde_json::json!({"type": "register", "project_id": project_id, "api_key": api_key}),
    )
    .await;
    assert_eq!(reply(&mut ws).await["type"], "registered");
    assert_eq!(reply(&mut ws).await["type"], "artifact_available");
    for (hash, drifted) in [("sha256:baked-in", true), ("sha256:deployed", false)] {
        send(
            &mut ws,
            serde_json::json!({"type": "heartbeat", "artifact_hash": hash, "uptime_secs": 1, "requests_total": 0}),
        )
        .await;
        let ack = reply(&mut ws).await;
        assert_eq!(ack["drift_detected"], drifted, "{hash}: {ack}");
    }
}
