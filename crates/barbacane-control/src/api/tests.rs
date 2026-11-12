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
    Some(crate::api::create_router(
        pool,
        None,
        conn_mgr,
        crate::api::AdminAuth::Disabled,
    ))
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
