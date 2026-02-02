//! Database models for the control plane.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A project record - the core organizing entity.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub production_mode: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for creating a new project.
#[derive(Debug, Clone, Deserialize)]
pub struct NewProject {
    pub name: String,
    pub description: Option<String>,
    #[serde(default = "default_production_mode")]
    pub production_mode: bool,
}

fn default_production_mode() -> bool {
    true
}

/// Input for updating a project.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProject {
    pub name: Option<String>,
    pub description: Option<String>,
    pub production_mode: Option<bool>,
}

/// Project plugin configuration - per-project plugin settings.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ProjectPluginConfig {
    pub id: Uuid,
    pub project_id: Uuid,
    pub plugin_name: String,
    pub plugin_version: String,
    pub enabled: bool,
    pub priority: i32,
    pub config: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Input for adding a plugin to a project.
#[derive(Debug, Clone, Deserialize)]
pub struct NewProjectPluginConfig {
    pub plugin_name: String,
    pub plugin_version: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    #[serde(default = "default_config")]
    pub config: serde_json::Value,
}

fn default_enabled() -> bool {
    true
}

fn default_config() -> serde_json::Value {
    serde_json::json!({})
}

/// Input for updating a project plugin configuration.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProjectPluginConfig {
    pub plugin_version: Option<String>,
    pub enabled: Option<bool>,
    pub priority: Option<i32>,
    pub config: Option<serde_json::Value>,
}

/// A spec metadata record.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Spec {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub current_sha256: String,
    pub spec_type: String,
    pub spec_version: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A spec revision record with content.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)]
pub struct SpecRevision {
    pub id: Uuid,
    pub spec_id: Uuid,
    pub revision: i32,
    pub sha256: String,
    pub content: Vec<u8>,
    pub filename: String,
    pub created_at: DateTime<Utc>,
}

/// Revision summary without content (for history listing).
#[derive(Debug, Clone, Serialize)]
pub struct SpecRevisionSummary {
    pub revision: i32,
    pub sha256: String,
    pub filename: String,
    pub created_at: DateTime<Utc>,
}

/// A plugin registry entry.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Plugin {
    pub name: String,
    pub version: String,
    pub plugin_type: String,
    pub description: Option<String>,
    pub capabilities: serde_json::Value,
    pub config_schema: serde_json::Value,
    pub sha256: String,
    pub registered_at: DateTime<Utc>,
}

/// Plugin with binary data for downloads.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)]
pub struct PluginWithBinary {
    pub name: String,
    pub version: String,
    pub plugin_type: String,
    pub description: Option<String>,
    pub capabilities: serde_json::Value,
    pub config_schema: serde_json::Value,
    pub wasm_binary: Vec<u8>,
    pub sha256: String,
    pub registered_at: DateTime<Utc>,
}

/// A compiled artifact record.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Artifact {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub manifest: serde_json::Value,
    pub sha256: String,
    pub size_bytes: i64,
    pub compiler_version: String,
    pub compiled_at: DateTime<Utc>,
}

/// Artifact with binary data for downloads.
#[derive(Debug, Clone, FromRow)]
#[allow(dead_code)]
pub struct ArtifactWithData {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub manifest: serde_json::Value,
    pub data: Vec<u8>,
    pub sha256: String,
    pub size_bytes: i64,
    pub compiler_version: String,
    pub compiled_at: DateTime<Utc>,
}

/// A compilation job record.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Compilation {
    pub id: Uuid,
    pub spec_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub status: String,
    pub production: bool,
    pub additional_specs: serde_json::Value,
    pub artifact_id: Option<Uuid>,
    pub errors: serde_json::Value,
    pub warnings: serde_json::Value,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// New spec input for creation.
#[derive(Debug, Clone, Deserialize)]
pub struct NewSpec {
    pub project_id: Uuid,
    pub name: String,
    pub spec_type: String,
    pub spec_version: String,
    pub sha256: String,
    pub content: Vec<u8>,
    pub filename: String,
}

/// New plugin input for registration.
#[derive(Debug, Clone, Deserialize)]
pub struct NewPlugin {
    pub name: String,
    pub version: String,
    pub plugin_type: String,
    pub description: Option<String>,
    pub capabilities: serde_json::Value,
    pub config_schema: serde_json::Value,
    pub wasm_binary: Vec<u8>,
    pub sha256: String,
}

// ============================================================================
// Data Plane Connection Models (M12)
// ============================================================================

/// A connected data plane record.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct DataPlane {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: Option<String>,
    pub artifact_id: Option<Uuid>,
    pub status: String,
    pub last_seen: Option<DateTime<Utc>>,
    pub connected_at: Option<DateTime<Utc>>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

/// Input for registering a data plane.
#[derive(Debug, Clone, Deserialize)]
pub struct NewDataPlane {
    pub project_id: Uuid,
    pub name: Option<String>,
    #[serde(default = "default_metadata")]
    pub metadata: serde_json::Value,
}

fn default_metadata() -> serde_json::Value {
    serde_json::json!({})
}

/// An API key record.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct ApiKey {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    #[allow(dead_code)]
    pub key_hash: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Input for creating an API key.
#[derive(Debug, Clone, Deserialize)]
pub struct NewApiKey {
    pub name: String,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

fn default_scopes() -> Vec<String> {
    vec!["data-plane:connect".to_string()]
}

/// Response when creating an API key (includes the full key once).
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyCreated {
    pub id: Uuid,
    pub name: String,
    pub key: String,
    pub key_prefix: String,
    pub scopes: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}
