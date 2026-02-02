//! Project initialization API handlers.

use axum::{http::StatusCode, Json};
use serde::{Deserialize, Serialize};

use crate::error::ProblemDetails;

/// Request body for project initialization.
#[derive(Debug, Deserialize)]
pub struct InitRequest {
    /// Project/API name (used in spec title).
    pub name: String,
    /// Template to use: "basic" or "minimal".
    #[serde(default = "default_template")]
    pub template: String,
    /// Optional description for the API.
    pub description: Option<String>,
    /// Optional API version.
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_template() -> String {
    "basic".to_string()
}

fn default_version() -> String {
    "1.0.0".to_string()
}

/// A file in the generated project.
#[derive(Debug, Serialize)]
pub struct ProjectFile {
    /// File path relative to project root.
    pub path: String,
    /// File content.
    pub content: String,
}

/// Response containing generated project files.
#[derive(Debug, Serialize)]
pub struct InitResponse {
    /// Generated files.
    pub files: Vec<ProjectFile>,
    /// Next steps for the user.
    pub next_steps: Vec<String>,
}

/// POST /init - Generate project files from template.
pub async fn init_project(
    Json(req): Json<InitRequest>,
) -> Result<(StatusCode, Json<InitResponse>), ProblemDetails> {
    // Validate template
    if req.template != "basic" && req.template != "minimal" {
        return Err(ProblemDetails::bad_request(format!(
            "Unknown template '{}'. Use 'basic' or 'minimal'.",
            req.template
        )));
    }

    // Validate name
    if req.name.trim().is_empty() {
        return Err(ProblemDetails::bad_request("Project name cannot be empty"));
    }

    let description = req
        .description
        .unwrap_or_else(|| format!("A Barbacane-powered API for {}", req.name));

    // Generate files (no .gitignore - that's only for CLI usage)
    let files = vec![
        ProjectFile {
            path: "barbacane.yaml".to_string(),
            content: generate_manifest(),
        },
        ProjectFile {
            path: "api.yaml".to_string(),
            content: generate_spec(&req.name, &req.version, &description, &req.template),
        },
    ];

    let next_steps = vec![
        "Download and extract the project files".to_string(),
        "Add plugins to the plugins/ directory".to_string(),
        "Edit api.yaml to define your API routes".to_string(),
        format!(
            "Compile: barbacane compile --spec api.yaml --manifest barbacane.yaml --output {}.bca",
            slug(&req.name)
        ),
        format!(
            "Run: barbacane serve --artifact {}.bca --listen 0.0.0.0:8080",
            slug(&req.name)
        ),
    ];

    Ok((StatusCode::OK, Json(InitResponse { files, next_steps })))
}

fn slug(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn generate_manifest() -> String {
    r#"# Barbacane project manifest
plugins: {}
"#
    .to_string()
}

fn generate_spec(name: &str, version: &str, description: &str, template: &str) -> String {
    if template == "minimal" {
        format!(
            r#"openapi: "3.1.0"
info:
  title: {name}
  version: "{version}"
  description: {description}

servers:
  - url: http://localhost:8080
    description: Local development

paths:
  /health:
    get:
      summary: Health check
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"status": "ok"}}'
          headers:
            Content-Type: application/json
      responses:
        "200":
          description: Service is healthy
"#,
            name = name,
            version = version,
            description = description,
        )
    } else {
        // Basic template with more examples
        format!(
            r#"openapi: "3.1.0"
info:
  title: {name}
  version: "{version}"
  description: {description}

servers:
  - url: http://localhost:8080
    description: Local development

paths:
  /health:
    get:
      summary: Health check
      operationId: healthCheck
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"status": "ok"}}'
          headers:
            Content-Type: application/json
      responses:
        "200":
          description: Service is healthy
          content:
            application/json:
              schema:
                type: object
                properties:
                  status:
                    type: string
                    example: ok

  /users:
    get:
      summary: List users
      operationId: listUsers
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"users": []}}'
          headers:
            Content-Type: application/json
      parameters:
        - name: limit
          in: query
          schema:
            type: integer
            minimum: 1
            maximum: 100
            default: 10
      responses:
        "200":
          description: List of users
          content:
            application/json:
              schema:
                type: object
                properties:
                  users:
                    type: array
                    items:
                      $ref: '#/components/schemas/User'

    post:
      summary: Create user
      operationId: createUser
      x-barbacane-dispatch:
        name: mock
        config:
          status: 201
          body: '{{"id": "user-123", "message": "Created"}}'
          headers:
            Content-Type: application/json
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateUserRequest'
      responses:
        "201":
          description: User created
        "400":
          description: Invalid request

  /users/{{id}}:
    get:
      summary: Get user by ID
      operationId: getUser
      x-barbacane-dispatch:
        name: mock
        config:
          status: 200
          body: '{{"id": "user-123", "name": "John Doe", "email": "john@example.com"}}'
          headers:
            Content-Type: application/json
      parameters:
        - name: id
          in: path
          required: true
          schema:
            type: string
      responses:
        "200":
          description: User details
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/User'
        "404":
          description: User not found

components:
  schemas:
    User:
      type: object
      properties:
        id:
          type: string
        name:
          type: string
        email:
          type: string
          format: email

    CreateUserRequest:
      type: object
      required:
        - name
        - email
      properties:
        name:
          type: string
          minLength: 1
        email:
          type: string
          format: email
"#,
            name = name,
            version = version,
            description = description,
        )
    }
}
