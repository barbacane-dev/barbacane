//! Database models for the control plane.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

/// A spec metadata record.
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Spec {
    pub id: Uuid,
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
    pub spec_id: Uuid,
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
