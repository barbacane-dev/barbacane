//! Integration tests for the control plane REST API.
//!
//! Tests use `tower::ServiceExt::oneshot()` to drive the axum router in-process,
//! with a real PostgreSQL connection ([`crate::api::test_pool`]: a local run
//! skips without one, a CI run fails). Set `DATABASE_URL` to run them:
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

/// Build the control-plane router connected to the test database
/// ([`crate::api::test_pool`]: skipped locally without one, failed under CI).
async fn make_app() -> Option<Router> {
    let pool = crate::api::test_pool().await?;

    let conn_mgr = Arc::new(crate::api::ConnectionManager::new());
    Some(crate::api::create_router(
        pool,
        None,
        conn_mgr,
        crate::api::AdminAuth::Disabled,
    ))
}

/// The router and its pool, for tests that also query the database.
async fn make_app_with_pool() -> Option<(Router, sqlx::PgPool)> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://barbacane:barbacane@localhost:5432/barbacane".to_string());
    let pool = crate::db::create_pool(&url).await.ok()?;
    crate::db::run_migrations(&pool).await.ok()?;
    let conn_mgr = Arc::new(crate::api::ConnectionManager::new());
    let app = crate::api::create_router(
        pool.clone(),
        None,
        conn_mgr,
        crate::api::AdminAuth::Disabled,
    );
    Some((app, pool))
}

/// Build the control-plane router with a pool that never connects: for the
/// routes that answer before any query.
fn make_app_without_db() -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect_lazy("postgres://nobody@127.0.0.1:1/unreachable")
        .expect("a lazy pool needs no server");
    let conn_mgr = Arc::new(crate::api::ConnectionManager::new());
    crate::api::create_router(pool, None, conn_mgr, crate::api::AdminAuth::Disabled)
}

/// Send one request through the router and return the status + body bytes.
/// The response is checked against what `openapi.yaml` says the operation
/// answers with that status.
async fn send(app: Router, req: Request<Body>) -> (StatusCode, bytes::Bytes) {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let resp: Response = app.oneshot(req).await.expect("router returned error");
    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("body collect failed")
        .to_bytes();
    assert_matches_spec(&method, &path, status, &body);
    (status, body)
}

/// The API's contract.
static SPEC: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
    barbacane_compiler::yaml::from_str(include_str!("../../openapi.yaml"))
        .expect("openapi.yaml parses")
});

/// The spec's operation for `method` and a concrete `path`, with its path
/// template.
fn spec_operation(method: &Method, path: &str) -> Option<(&'static str, &'static Value)> {
    let segments: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let paths = SPEC["paths"].as_object()?;
    paths.iter().find_map(|(template, item)| {
        let parts: Vec<&str> = template.split('/').collect();
        let matches = parts.len() == segments.len()
            && parts
                .iter()
                .zip(&segments)
                .all(|(t, s)| t.starts_with('{') || t == s);
        let operation = item.get(method.as_str().to_ascii_lowercase())?;
        matches.then_some((template.as_str(), operation))
    })
}

/// Fail unless the response is one the spec describes for this operation and
/// status, with a body its schema accepts.
fn assert_matches_spec(method: &Method, path: &str, status: StatusCode, body: &bytes::Bytes) {
    let (template, operation) = spec_operation(method, path)
        .unwrap_or_else(|| panic!("{method} {path} is not in openapi.yaml"));
    let responses = &operation["responses"];
    let mut response = responses
        .get(status.as_str())
        .or_else(|| responses.get("default"))
        .unwrap_or_else(|| panic!("openapi.yaml gives {method} {template} no {status} response"));
    if let Some(reference) = response["$ref"].as_str() {
        let name = reference.trim_start_matches("#/components/responses/");
        response = &SPEC["components"]["responses"][name];
    }
    let Some(schema) = ["application/json", "application/problem+json"]
        .iter()
        .find_map(|media| response["content"][media].get("schema"))
    else {
        // No JSON body described: a 204, or a download.
        return;
    };

    let instance: Value = serde_json::from_slice(body)
        .unwrap_or_else(|e| panic!("{method} {path} {status}: the body is not JSON: {e}"));
    let mut root = schema.clone();
    if let Value::Object(map) = &mut root {
        map.insert("components".to_string(), SPEC["components"].clone());
    }
    let validator = jsonschema::validator_for(&root).unwrap_or_else(|e| {
        panic!("the schema of {method} {template} {status} does not compile: {e}")
    });
    let errors: Vec<String> = validator
        .iter_errors(&instance)
        .map(|e| format!("{} at {}", e, e.instance_path))
        .collect();
    assert!(
        errors.is_empty(),
        "{method} {path} {status} does not match openapi.yaml: {errors:?}\n{instance}"
    );
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
async fn health_is_liveness_and_needs_no_database() {
    let (status, body) = send(make_app_without_db(), empty_req(Method::GET, "/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["status"], "healthy");
}

#[tokio::test]
async fn ready_is_503_without_a_database() {
    let (status, body) = send(make_app_without_db(), empty_req(Method::GET, "/ready")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_body(&body)["status"], "unavailable");
}

#[tokio::test]
async fn ready_is_200_with_a_database() {
    let app = match make_app().await {
        Some(a) => a,
        None => {
            eprintln!("skip: database not available");
            return;
        }
    };
    let (status, body) = send(app, empty_req(Method::GET, "/ready")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["status"], "ready");
}

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

// ---------------------------------------------------------------------------
// Project specs, operations, compilations, artifacts
// ---------------------------------------------------------------------------

const PROJECT_SPEC: &str = r#"openapi: "3.1.0"
info:
  title: Contract Test API
  version: "1.0.0"
x-barbacane-middlewares:
  - name: cors
    config:
      allowed_origins: ["https://app.example.com"]
paths:
  /items:
    get:
      operationId: listItems
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
      responses:
        "200":
          description: ok
"#;

/// A `multipart/form-data` request carrying `content` as the `file` field.
fn multipart_req(uri: &str, filename: &str, content: &str) -> Request<Body> {
    let boundary = "barbacane-test-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{filename}\"\r\nContent-Type: application/yaml\r\n\r\n{content}\r\n--{boundary}--\r\n"
    );
    Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn a_project_spec_is_listed_with_its_operations_which_can_be_patched() {
    let Some(app) = make_app().await else {
        eprintln!("skip: database not available");
        return;
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;

    let (status, body) = send(
        app.clone(),
        multipart_req(
            &format!("/projects/{project_id}/specs"),
            "api.yaml",
            PROJECT_SPEC,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", json_body(&body));
    let spec_id = json_body(&body)["id"]
        .as_str()
        .expect("spec id")
        .to_string();

    let (status, body) = send(
        app.clone(),
        empty_req(Method::GET, &format!("/projects/{project_id}/specs")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body).as_array().map(Vec::len), Some(1));

    let (status, body) = send(
        app.clone(),
        empty_req(Method::GET, &format!("/projects/{project_id}/operations")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let operations = json_body(&body);
    assert_eq!(
        operations["specs"][0]["global_middlewares"][0]["name"],
        "cors"
    );
    assert_eq!(operations["specs"][0]["operations"][0]["path"], "/items");

    let (status, body) = send(
        app.clone(),
        json_req(
            Method::PATCH,
            &format!("/specs/{spec_id}/operations"),
            json!({
                "global_middlewares": [],
                "operations": [{ "path": "/items", "method": "GET", "middlewares": [] }]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", json_body(&body));
    assert_eq!(json_body(&body)["global_middlewares"], json!([]));

    let (status, body) = send(
        app.clone(),
        json_req(
            Method::PATCH,
            &format!("/specs/{spec_id}/operations"),
            json!({ "operations": [{ "path": "/missing", "method": "GET" }] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{}", json_body(&body));

    for list in ["compilations", "artifacts"] {
        let (status, body) = send(
            app.clone(),
            empty_req(Method::GET, &format!("/projects/{project_id}/{list}")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{list}");
        assert!(json_body(&body).is_array(), "{list}");
    }

    let missing = Uuid::new_v4();
    for list in [
        "specs",
        "compilations",
        "artifacts",
        "operations",
        "plugins",
    ] {
        let (status, _) = send(
            app.clone(),
            empty_req(Method::GET, &format!("/projects/{missing}/{list}")),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{list}");
    }
}

#[tokio::test]
async fn a_plugin_is_added_to_updated_in_and_removed_from_a_project() {
    let Some(app) = make_app().await else {
        eprintln!("skip: database not available");
        return;
    };
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://barbacane:barbacane@localhost:5432/barbacane".to_string());
    let pool = crate::db::create_pool(&url).await.expect("pool");
    let plugin_name = format!("contract-{}", Uuid::new_v4().simple());
    crate::db::PluginsRepository::new(pool)
        .create(crate::db::NewPlugin {
            name: plugin_name.clone(),
            version: "1.0.0".to_string(),
            plugin_type: "middleware".to_string(),
            description: None,
            capabilities: json!([]),
            config_schema: json!({}),
            wasm_binary: b"\0asm\x01\0\0\0".to_vec(),
            sha256: "0".repeat(64),
        })
        .await
        .expect("register plugin");
    let project_id = create_project(app.clone(), &unique_project_name()).await;
    let plugins = format!("/projects/{project_id}/plugins");
    let added =
        json!({ "plugin_name": plugin_name, "plugin_version": "1.0.0", "config": { "a": 1 } });

    let (status, body) = send(app.clone(), json_req(Method::POST, &plugins, added.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "{}", json_body(&body));
    assert_eq!(json_body(&body)["enabled"], true);

    let (status, _) = send(app.clone(), json_req(Method::POST, &plugins, added)).await;
    assert_eq!(status, StatusCode::CONFLICT);

    let (status, body) = send(app.clone(), empty_req(Method::GET, &plugins)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body).as_array().map(Vec::len), Some(1));

    let one = format!("{plugins}/{plugin_name}");
    let (status, body) = send(
        app.clone(),
        json_req(
            Method::PUT,
            &one,
            json!({ "enabled": false, "priority": 3 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["enabled"], false);
    assert_eq!(json_body(&body)["config"], json!({ "a": 1 }));

    let (status, _) = send(app.clone(), empty_req(Method::DELETE, &one)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(app.clone(), empty_req(Method::DELETE, &one)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_projects_limits_for_a_plugin_are_set_replaced_removed_and_checked() {
    let Some(app) = make_app().await else {
        eprintln!("skip: database not available");
        return;
    };
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://barbacane:barbacane@localhost:5432/barbacane".to_string());
    let pool = crate::db::create_pool(&url).await.expect("pool");
    let plugin_name = format!("limits-{}", Uuid::new_v4().simple());
    crate::db::PluginsRepository::new(pool)
        .create(crate::db::NewPlugin {
            name: plugin_name.clone(),
            version: "1.0.0".to_string(),
            plugin_type: "middleware".to_string(),
            description: None,
            capabilities: json!([]),
            config_schema: json!({}),
            wasm_binary: b"\0asm\x01\0\0\0".to_vec(),
            sha256: "0".repeat(64),
        })
        .await
        .expect("register plugin");
    let project_id = create_project(app.clone(), &unique_project_name()).await;
    let plugins = format!("/projects/{project_id}/plugins");
    let one = format!("{plugins}/{plugin_name}");

    let invalid = json!({ "plugin_name": plugin_name, "plugin_version": "1.0.0", "limits": { "memory_mib": 4096 } });
    let (status, body) = send(app.clone(), json_req(Method::POST, &plugins, invalid)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(json_body(&body)["detail"]
        .as_str()
        .unwrap_or_default()
        .starts_with("E1044"));

    let added = json!({ "plugin_name": plugin_name, "plugin_version": "1.0.0", "limits": { "memory_mib": 64 } });
    let (status, body) = send(app.clone(), json_req(Method::POST, &plugins, added)).await;
    assert_eq!(status, StatusCode::CREATED, "{}", json_body(&body));
    assert_eq!(json_body(&body)["limits"], json!({ "memory_mib": 64 }));

    let put = |body: Value| json_req(Method::PUT, &one, body);
    let (status, body) = send(app.clone(), put(json!({ "limits": { "fuel": 1000 } }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json_body(&body)["limits"],
        json!({ "fuel": 1000 }),
        "replaced, not merged"
    );

    let (status, body) = send(app.clone(), put(json!({ "enabled": false }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json_body(&body)["limits"],
        json!({ "fuel": 1000 }),
        "kept when left out"
    );

    for invalid in [json!({ "memory": 1 }), json!({ "timeout_ms": 0 }), json!(5)] {
        let (status, body) = send(app.clone(), put(json!({ "limits": invalid }))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{invalid}");
        assert!(json_body(&body)["detail"]
            .as_str()
            .unwrap_or_default()
            .starts_with("E1044"));
    }

    let (status, body) = send(app.clone(), put(json!({ "limits": {} }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        json_body(&body)["limits"],
        Value::Null,
        "an empty object removes them"
    );
}

// ---------------------------------------------------------------------------
// Operability: public docs, registration shapes, key validation
// ---------------------------------------------------------------------------

/// The router with a real admin token and a pool that never connects.
fn make_app_with_token() -> Router {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect_lazy("postgres://nobody@127.0.0.1:1/unreachable")
        .expect("a lazy pool needs no server");
    let conn_mgr = Arc::new(crate::api::ConnectionManager::new());
    crate::api::create_router(
        pool,
        None,
        conn_mgr,
        crate::api::AdminAuth::from_tokens("an-admin-token-of-at-least-32-bytes!!", None),
    )
}

/// A request through the router, outside the spec contract: for the routes
/// that are not operations of the API.
async fn raw(app: Router, req: Request<Body>) -> (StatusCode, bytes::Bytes) {
    let resp: Response = app.oneshot(req).await.expect("router returned error");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    (status, body)
}

#[tokio::test]
async fn the_api_description_and_its_page_need_no_token() {
    let (status, body) = raw(make_app_with_token(), empty_req(Method::GET, "/openapi")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("openapi: 3.1.0"));
    let (status, body) = raw(make_app_with_token(), empty_req(Method::GET, "/docs")).await;
    assert_eq!(status, StatusCode::OK);
    let page = String::from_utf8_lossy(&body);
    assert!(
        page.contains("\"/openapi\""),
        "the page loads the description from /openapi"
    );
    assert!(!page.contains("/api/openapi"));
    let (status, _) = raw(make_app_with_token(), empty_req(Method::GET, "/projects")).await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "everything else keeps its token"
    );
}

/// A `multipart/form-data` plugin registration.
fn plugin_req(name: &str, version: &str, file: &[u8]) -> Request<Body> {
    let boundary = "barbacane-plugin-boundary";
    let mut body = Vec::new();
    for (field, value) in [("name", name), ("version", version), ("type", "middleware")] {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"\r\n\r\n{value}\r\n")
                .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"p.wasm\"\r\nContent-Type: application/wasm\r\n\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(file);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Request::builder()
        .method(Method::POST)
        .uri("/plugins")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn a_plugin_registration_with_a_bad_name_version_or_file_is_refused() {
    for (name, version, file, why) in [
        ("Bad/Name", "1.0.0", &b"\0asm\x01\0\0\0"[..], "name"),
        ("ok-name", "latest", &b"\0asm\x01\0\0\0"[..], "version"),
        ("ok-name", "1.0.0", &b"not wasm"[..], "file"),
    ] {
        let (status, body) = send(make_app_without_db(), plugin_req(name, version, file)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{why}: {}",
            String::from_utf8_lossy(&body)
        );
    }
}

#[tokio::test]
async fn an_api_key_with_an_unknown_scope_or_a_past_expiry_is_refused() {
    let project = Uuid::new_v4();
    for (payload, why) in [
        (json!({"name": "k", "scopes": ["admin:all"]}), "scope"),
        (
            json!({"name": "k", "expires_at": "2001-01-01T00:00:00Z"}),
            "expiry",
        ),
    ] {
        let (status, body) = send(
            make_app_without_db(),
            json_req(
                Method::POST,
                &format!("/projects/{project}/api-keys"),
                payload,
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{why}: {}",
            String::from_utf8_lossy(&body)
        );
    }
}

/// `GET /metrics` exposes requests by route template, compilations and
/// connected data planes.
#[tokio::test]
async fn metrics_count_requests_by_route_template() {
    let app = make_app_without_db();
    let (status, _) = send(app.clone(), empty_req(Method::GET, "/health")).await;
    assert_eq!(status, StatusCode::OK);
    // A templated route; without a database it answers an error, which is
    // all this needs, so it skips the contract check `send` makes.
    let _ = app
        .clone()
        .oneshot(empty_req(
            Method::GET,
            "/projects/00000000-0000-0000-0000-000000000001",
        ))
        .await
        .expect("a response");

    let (status, body) = send(app, empty_req(Method::GET, "/metrics")).await;
    assert_eq!(status, StatusCode::OK);
    let text = String::from_utf8_lossy(&body);
    assert!(
        text.contains(
            r#"barbacane_control_requests_total{method="GET",route="/health",status="200"}"#
        ),
        "{text}"
    );
    assert!(
        text.contains(r#"route="/projects/{id}""#),
        "the route template, not the path: {text}"
    );
    assert!(!text.contains("00000000-0000"), "{text}");
    // No compilation finished in this process yet: the family, no sample.
    assert!(
        text.contains("# TYPE barbacane_control_compilations counter"),
        "{text}"
    );
    assert!(
        text.contains("barbacane_control_data_planes_connected 0"),
        "{text}"
    );
}

#[tokio::test]
async fn the_default_project_cannot_be_deleted() {
    let (status, body) = send(
        make_app_without_db(),
        empty_req(
            Method::DELETE,
            "/projects/00000000-0000-0000-0000-000000000001",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        json_body(&body)["detail"]
            .as_str()
            .is_some_and(|d| d.contains("POST /specs")),
        "{body:?}"
    );
}

#[tokio::test]
async fn the_default_project_exists_after_migrations() {
    let Some((_, pool)) = make_app_with_pool().await else {
        eprintln!("skip: database not available");
        return;
    };
    let default = crate::db::ProjectsRepository::new(pool)
        .get_by_id(crate::api::specs::DEFAULT_PROJECT_ID)
        .await
        .expect("query");
    assert!(default.is_some());
}

#[tokio::test]
async fn deleting_a_project_deletes_its_artifacts() {
    let Some((app, pool)) = make_app_with_pool().await else {
        eprintln!("skip: database not available");
        return;
    };
    let (_, created) = send(
        app.clone(),
        json_req(
            Method::POST,
            "/projects",
            json!({"name": unique_project_name()}),
        ),
    )
    .await;
    let project: Uuid = json_body(&created)["id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .expect("project id");
    let artifacts = crate::db::ArtifactsRepository::new(pool);
    let artifact = artifacts
        .create(
            Some(project),
            json!({}),
            b"artifact".to_vec(),
            &"0".repeat(64),
            "test",
        )
        .await
        .expect("artifact");

    let (status, _) = send(
        app,
        empty_req(Method::DELETE, &format!("/projects/{project}")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        artifacts.get(artifact.id).await.expect("query").is_none(),
        "the artifact went with its project"
    );
}

/// `?name=` matches part of a name, case-insensitively; `%` and `_` in it
/// match themselves.
#[tokio::test]
async fn the_spec_name_filter_is_a_partial_match_of_the_text_given() {
    let Some(app) = make_app().await else {
        eprintln!("skip: database not available");
        return;
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;
    let tag = Uuid::new_v4().simple().to_string();
    let spec = PROJECT_SPEC.replace(
        "title: Contract Test API",
        &format!("title: PartialA{tag} Store"),
    );
    let (status, body) = send(
        app.clone(),
        multipart_req(&format!("/projects/{project_id}/specs"), "api.yaml", &spec),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{}", json_body(&body));

    let names = |body: &bytes::Bytes| -> Vec<String> {
        json_body(body)
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|s| s["name"].as_str().map(str::to_string))
            .collect()
    };
    let (status, body) = send(
        app.clone(),
        empty_req(Method::GET, &format!("/specs?name={}", tag.to_uppercase())),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&body), vec![format!("PartialA{tag} Store")]);

    // `_` is not a wildcard: "Partial_" does not match "PartialA".
    let (_, body) = send(
        app.clone(),
        empty_req(Method::GET, &format!("/specs?name=Partial_{tag}")),
    )
    .await;
    assert!(names(&body).is_empty(), "{:?}", names(&body));
    // Nor is `%`.
    let (_, body) = send(
        app,
        empty_req(Method::GET, &format!("/specs?name=Partial%25{tag}")),
    )
    .await;
    assert!(names(&body).is_empty(), "{:?}", names(&body));
}

#[tokio::test]
async fn deploying_another_projects_artifact_is_404() {
    let Some((app, pool)) = make_app_with_pool().await else {
        eprintln!("skip: database not available");
        return;
    };
    let ours = create_project(app.clone(), &unique_project_name()).await;
    let theirs: Uuid = create_project(app.clone(), &unique_project_name())
        .await
        .parse()
        .expect("uuid");
    let artifact = crate::db::ArtifactsRepository::new(pool)
        .create(
            Some(theirs),
            json!({}),
            b"artifact".to_vec(),
            &"0".repeat(64),
            "test",
        )
        .await
        .expect("artifact");
    let (status, body) = send(
        app,
        json_req(
            Method::POST,
            &format!("/projects/{ours}/deploy"),
            json!({"artifact_id": artifact.id}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{}", json_body(&body));
}

#[tokio::test]
async fn the_api_keys_of_an_unknown_project_are_404() {
    let Some(app) = make_app().await else {
        eprintln!("skip: database not available");
        return;
    };
    let (status, _) = send(
        app,
        empty_req(
            Method::GET,
            &format!("/projects/{}/api-keys", Uuid::new_v4()),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_limit_or_offset_out_of_range_is_400() {
    for uri in [
        "/projects?limit=0",
        "/projects?limit=1001",
        "/artifacts?limit=ten",
        "/specs?offset=-1",
        "/plugins?limit=-5",
    ] {
        let (status, body) = send(make_app_without_db(), empty_req(Method::GET, uri)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        assert_eq!(json_body(&body)["status"], 400, "{uri}");
    }
}

#[tokio::test]
async fn a_list_is_read_page_by_page() {
    let Some(app) = make_app().await else {
        eprintln!("skip: database not available");
        return;
    };
    let project_id = create_project(app.clone(), &unique_project_name()).await;
    for title in ["Paged A", "Paged B", "Paged C"] {
        let spec = PROJECT_SPEC.replace("title: Contract Test API", &format!("title: {title}"));
        let (status, body) = send(
            app.clone(),
            multipart_req(&format!("/projects/{project_id}/specs"), "api.yaml", &spec),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{}", json_body(&body));
    }
    let names = |body: &bytes::Bytes| -> Vec<String> {
        json_body(body)
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|s| s["name"].as_str().map(str::to_string))
            .collect()
    };
    let page = |query: &'static str| {
        let app = app.clone();
        let uri = format!("/projects/{project_id}/specs{query}");
        async move {
            let (status, body) = send(app, empty_req(Method::GET, &uri)).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            names(&body)
        }
    };
    assert_eq!(page("?limit=2").await, vec!["Paged A", "Paged B"]);
    assert_eq!(page("?limit=2&offset=2").await, vec!["Paged C"]);
    assert!(page("?offset=3").await.is_empty());
    assert_eq!(page("").await, vec!["Paged A", "Paged B", "Paged C"]);
}

// ---------------------------------------------------------------------------
// Deployments
// ---------------------------------------------------------------------------

/// A request with `Authorization: Bearer <token>`.
fn bearer_req(method: Method, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {token}"));
    match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("request"),
        None => builder.body(Body::empty()).expect("request"),
    }
}

/// A deploy makes the artifact the project's deployment. The admin token
/// and a data-plane key of the project read it; another project's key finds
/// none, and no credential is refused.
#[tokio::test]
async fn a_deploy_becomes_the_projects_deployment() {
    const TOKEN: &str = "an-admin-token-of-at-least-32-bytes!!";
    let Some(pool) = crate::api::test_pool().await else {
        eprintln!("skip: database not available");
        return;
    };
    let app = crate::api::create_router(
        pool.clone(),
        None,
        Arc::new(crate::api::ConnectionManager::new()),
        crate::api::AdminAuth::from_tokens(TOKEN, None),
    );
    let projects = crate::db::ProjectsRepository::new(pool.clone());
    let new_project = |name: String| crate::db::NewProject {
        name,
        description: None,
        production_mode: false,
    };
    let project = projects
        .create(new_project(unique_project_name()))
        .await
        .unwrap()
        .id;
    let other = projects
        .create(new_project(unique_project_name()))
        .await
        .unwrap()
        .id;
    let artifact = crate::db::ArtifactsRepository::new(pool.clone())
        .create(
            Some(project),
            json!({}),
            b"artifact".to_vec(),
            &"a".repeat(64),
            "test",
        )
        .await
        .unwrap();
    let key_of = |project_id: Uuid| {
        let pool = pool.clone();
        async move {
            crate::db::ApiKeysRepository::new(pool)
                .create(
                    project_id,
                    crate::db::NewApiKey {
                        name: format!("dp-{}", Uuid::new_v4().simple()),
                        scopes: vec!["data-plane:connect".to_string()],
                        expires_at: None,
                    },
                )
                .await
                .unwrap()
                .key
        }
    };
    let deployment = format!("/projects/{project}/deployment");

    let (status, _) = send(
        app.clone(),
        bearer_req(Method::GET, &deployment, TOKEN, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) = send(
        app.clone(),
        bearer_req(
            Method::POST,
            &format!("/projects/{project}/deploy"),
            TOKEN,
            Some(json!({"artifact_id": artifact.id})),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", json_body(&body));

    let (status, body) = send(
        app.clone(),
        bearer_req(Method::GET, &deployment, TOKEN, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", json_body(&body));
    let read = json_body(&body);
    assert_eq!(read["artifact_id"], artifact.id.to_string());
    assert_eq!(read["sha256"], "a".repeat(64));
    assert_eq!(
        read["download_url"],
        format!("/artifacts/{}/download", artifact.id)
    );

    let own_key = key_of(project).await;
    let (status, _) = send(
        app.clone(),
        bearer_req(Method::GET, &deployment, &own_key, None),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let other_key = key_of(other).await;
    let (status, _) = send(
        app.clone(),
        bearer_req(Method::GET, &deployment, &other_key, None),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(app, empty_req(Method::GET, &deployment)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let stored = projects.get_by_id(project).await.unwrap().unwrap();
    assert_eq!(stored.deployed_artifact_id, Some(artifact.id));
}
