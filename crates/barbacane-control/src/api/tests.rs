//! Integration tests for the control plane REST API.
//!
//! Tests use `tower::ServiceExt::oneshot()` to drive the axum router in-process,
//! with a real PostgreSQL connection. Set `DATABASE_URL` to run these tests:
//!
//! ```text
//! DATABASE_URL=postgres://barbacane:barbacane@localhost:5432/barbacane \
//!   cargo test -p barbacane-control
//! ```
//!
//! Tests skip gracefully when the database is not reachable.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    response::Response,
    Router,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

/// Build the control-plane router connected to the test database.
/// Returns `None` if `DATABASE_URL` is not reachable (test is skipped).
async fn make_app() -> Option<Router> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://barbacane:barbacane@localhost:5432/barbacane".to_string());

    let pool = crate::db::create_pool(&url).await.ok()?;
    crate::db::run_migrations(&pool).await.ok()?;

    let conn_mgr = Arc::new(crate::api::ConnectionManager::new());
    Some(crate::api::create_router(pool, None, conn_mgr))
}

/// Send one request through the router and return the status + body bytes.
async fn send(app: Router, req: Request<Body>) -> (StatusCode, bytes::Bytes) {
    let resp: Response = app.oneshot(req).await.expect("router returned error");
    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("body collect failed")
        .to_bytes();
    (status, body)
}

/// Parse body bytes as JSON.
fn json_body(body: &bytes::Bytes) -> Value {
    serde_json::from_slice(body).expect("response is not valid JSON")
}

/// Build a JSON request.
fn json_req(method: Method, uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

/// Build a request with no body.
fn empty_req(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_check_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let (status, body) = send(app, empty_req(Method::GET, "/health")).await;
    assert_eq!(status, StatusCode::OK);
    let j = json_body(&body);
    assert_eq!(j["status"], "healthy");
    assert!(j["version"].is_string());
}

// ---------------------------------------------------------------------------
// Project CRUD
// ---------------------------------------------------------------------------

fn unique_project_name() -> String {
    format!("test-project-{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn project_create_returns_201() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let name = unique_project_name();
    let (status, body) = send(
        app,
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let j = json_body(&body);
    assert!(j["id"].is_string());
    assert_eq!(j["name"], name);
}

#[tokio::test]
async fn project_get_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let name = unique_project_name();

    // Create
    let (_, create_body) = send(
        app.clone(),
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    let id = json_body(&create_body)["id"].as_str().unwrap().to_string();

    // Get
    let (status, body) = send(app, empty_req(Method::GET, &format!("/projects/{}", id))).await;
    assert_eq!(status, StatusCode::OK);
    let j = json_body(&body);
    assert_eq!(j["id"].as_str().unwrap(), id);
    assert_eq!(j["name"], name);
}

#[tokio::test]
async fn project_list_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let (status, body) = send(app, empty_req(Method::GET, "/projects")).await;
    assert_eq!(status, StatusCode::OK);
    let j = json_body(&body);
    assert!(j.is_array(), "expected array, got: {}", j);
}

#[tokio::test]
async fn project_update_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let name = unique_project_name();

    let (_, create_body) = send(
        app.clone(),
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    let id = json_body(&create_body)["id"].as_str().unwrap().to_string();

    let (status, body) = send(
        app,
        json_req(
            Method::PUT,
            &format!("/projects/{}", id),
            json!({"description": "updated description"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let j = json_body(&body);
    assert_eq!(j["description"], "updated description");
}

#[tokio::test]
async fn project_delete_returns_204() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let name = unique_project_name();

    let (_, create_body) = send(
        app.clone(),
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    let id = json_body(&create_body)["id"].as_str().unwrap().to_string();

    // Delete
    let (status, _) = send(
        app.clone(),
        empty_req(Method::DELETE, &format!("/projects/{}", id)),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Confirm gone
    let (status, _) = send(app, empty_req(Method::GET, &format!("/projects/{}", id))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn project_not_found_returns_404() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let fake_id = Uuid::new_v4();
    let (status, body) = send(
        app,
        empty_req(Method::GET, &format!("/projects/{}", fake_id)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let j = json_body(&body);
    assert_eq!(j["status"], 404);
    assert!(j["title"].is_string());
    assert!(j["type"].is_string());
}

#[tokio::test]
async fn project_duplicate_name_returns_409() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let name = unique_project_name();

    let (s1, _) = send(
        app.clone(),
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED);

    let (s2, body) = send(
        app,
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    assert_eq!(
        s2,
        StatusCode::CONFLICT,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let j = json_body(&body);
    assert_eq!(j["status"], 409);
}

// ---------------------------------------------------------------------------
// API Keys
// ---------------------------------------------------------------------------

/// Create a project and return its ID string.
async fn create_project(app: Router, name: &str) -> String {
    let (status, body) = send(
        app,
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "create_project failed: {}",
        String::from_utf8_lossy(&body)
    );
    json_body(&body)["id"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn api_key_create_returns_201() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;

    let (status, body) = send(
        app,
        json_req(
            Method::POST,
            &format!("/projects/{}/api-keys", project_id),
            json!({"name": "test-key"}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "body: {}",
        String::from_utf8_lossy(&body)
    );
    let j = json_body(&body);
    assert!(j["id"].is_string());
    // Full key is returned once on creation
    let key = j["key"].as_str().expect("key field must be present");
    assert!(
        key.starts_with("bbk_"),
        "expected bbk_ prefix, got: {}",
        key
    );
}

#[tokio::test]
async fn api_key_list_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;

    // Create one key
    send(
        app.clone(),
        json_req(
            Method::POST,
            &format!("/projects/{}/api-keys", project_id),
            json!({"name": "list-test-key"}),
        ),
    )
    .await;

    let (status, body) = send(
        app,
        empty_req(Method::GET, &format!("/projects/{}/api-keys", project_id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let j = json_body(&body);
    assert!(j.is_array());
    assert!(
        !j.as_array().unwrap().is_empty(),
        "expected at least one key"
    );
    // Full key is NOT returned in list — only prefix
    assert!(
        j[0]["key"].is_null() || !j[0].as_object().unwrap().contains_key("key"),
        "full key should not appear in list response"
    );
}

#[tokio::test]
async fn api_key_revoke_returns_204() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;

    let (_, key_body) = send(
        app.clone(),
        json_req(
            Method::POST,
            &format!("/projects/{}/api-keys", project_id),
            json!({"name": "revoke-test-key"}),
        ),
    )
    .await;
    let key_id = json_body(&key_body)["id"].as_str().unwrap().to_string();

    let (status, _) = send(
        app,
        empty_req(
            Method::DELETE,
            &format!("/projects/{}/api-keys/{}", project_id, key_id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// Read-only list endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plugins_list_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let (status, body) = send(app, empty_req(Method::GET, "/plugins")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json_body(&body).is_array());
}

#[tokio::test]
async fn data_planes_list_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;

    let (status, body) = send(
        app,
        empty_req(
            Method::GET,
            &format!("/projects/{}/data-planes", project_id),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let j = json_body(&body);
    assert!(j.is_array());
    assert!(
        j.as_array().unwrap().is_empty(),
        "new project should have no data planes"
    );
}

#[tokio::test]
async fn data_planes_nonexistent_project_returns_404() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let fake_id = Uuid::new_v4();
    let (status, _) = send(
        app,
        empty_req(Method::GET, &format!("/projects/{}/data-planes", fake_id)),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn artifacts_list_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let (status, body) = send(app, empty_req(Method::GET, "/artifacts")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(json_body(&body).is_array());
}

// ---------------------------------------------------------------------------
// Error format (RFC 9457 Problem Details)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn not_found_is_rfc9457_problem_details() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let (status, body) = send(
        app,
        empty_req(Method::GET, &format!("/projects/{}", Uuid::new_v4())),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let j = json_body(&body);
    assert_eq!(j["status"], 404, "missing 'status' field");
    assert!(j["title"].is_string(), "missing 'title' field");
    assert!(
        j["type"].as_str().unwrap_or("").starts_with("urn:"),
        "type must be a URN, got: {}",
        j["type"]
    );
}

#[tokio::test]
async fn conflict_is_rfc9457_problem_details() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let name = unique_project_name();
    send(
        app.clone(),
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    let (status, body) = send(
        app,
        json_req(Method::POST, "/projects", json!({"name": name})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let j = json_body(&body);
    assert_eq!(j["status"], 409, "missing 'status' field");
    assert!(j["title"].is_string(), "missing 'title' field");
    assert!(
        j["type"].as_str().unwrap_or("").starts_with("urn:"),
        "type must be a URN, got: {}",
        j["type"]
    );
}

#[tokio::test]
async fn project_specs_list_returns_200() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;

    let (status, body) = send(
        app,
        empty_req(Method::GET, &format!("/projects/{}/specs", project_id)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json_body(&body).is_array());
}
