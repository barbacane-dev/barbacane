//! Artifacts repository for CRUD operations.

use sqlx::PgPool;
use uuid::Uuid;

use super::models::{Artifact, ArtifactWithData};

/// Repository for artifact operations.
#[derive(Clone)]
pub struct ArtifactsRepository {
    pool: PgPool,
}

impl ArtifactsRepository {
    /// Create a new artifacts repository.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Store a new artifact.
    pub async fn create(
        &self,
        manifest: serde_json::Value,
        data: Vec<u8>,
        sha256: &str,
        compiler_version: &str,
    ) -> Result<Artifact, sqlx::Error> {
        let size_bytes = data.len() as i64;
        sqlx::query_as::<_, Artifact>(
            r#"
            INSERT INTO artifacts (manifest, data, sha256, size_bytes, compiler_version)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, manifest, sha256, size_bytes, compiler_version, compiled_at
            "#,
        )
        .bind(&manifest)
        .bind(&data)
        .bind(sha256)
        .bind(size_bytes)
        .bind(compiler_version)
        .fetch_one(&self.pool)
        .await
    }

    /// List all artifacts.
    pub async fn list(&self) -> Result<Vec<Artifact>, sqlx::Error> {
        sqlx::query_as::<_, Artifact>(
            r#"
            SELECT id, manifest, sha256, size_bytes, compiler_version, compiled_at
            FROM artifacts
            ORDER BY compiled_at DESC
            "#,
        )
        .fetch_all(&self.pool)
        .await
    }

    /// Get an artifact by ID (metadata only).
    pub async fn get(&self, id: Uuid) -> Result<Option<Artifact>, sqlx::Error> {
        sqlx::query_as::<_, Artifact>(
            r#"
            SELECT id, manifest, sha256, size_bytes, compiler_version, compiled_at
            FROM artifacts
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Get an artifact with its binary data for download.
    pub async fn get_with_data(&self, id: Uuid) -> Result<Option<ArtifactWithData>, sqlx::Error> {
        sqlx::query_as::<_, ArtifactWithData>(
            r#"
            SELECT id, manifest, data, sha256, size_bytes, compiler_version, compiled_at
            FROM artifacts
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Get an artifact by SHA256 hash.
    #[allow(dead_code)]
    pub async fn get_by_sha256(&self, sha256: &str) -> Result<Option<Artifact>, sqlx::Error> {
        sqlx::query_as::<_, Artifact>(
            r#"
            SELECT id, manifest, sha256, size_bytes, compiler_version, compiled_at
            FROM artifacts
            WHERE sha256 = $1
            "#,
        )
        .bind(sha256)
        .fetch_optional(&self.pool)
        .await
    }

    /// Delete an artifact.
    pub async fn delete(&self, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM artifacts WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Get artifacts for a specific spec (via artifact_specs junction).
    #[allow(dead_code)]
    pub async fn list_for_spec(&self, spec_id: Uuid) -> Result<Vec<Artifact>, sqlx::Error> {
        sqlx::query_as::<_, Artifact>(
            r#"
            SELECT a.id, a.manifest, a.sha256, a.size_bytes, a.compiler_version, a.compiled_at
            FROM artifacts a
            INNER JOIN artifact_specs aps ON a.id = aps.artifact_id
            WHERE aps.spec_id = $1
            ORDER BY a.compiled_at DESC
            "#,
        )
        .bind(spec_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Link an artifact to a spec revision.
    pub async fn link_to_spec(
        &self,
        artifact_id: Uuid,
        spec_id: Uuid,
        spec_revision: i32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            INSERT INTO artifact_specs (artifact_id, spec_id, spec_revision)
            VALUES ($1, $2, $3)
            ON CONFLICT (artifact_id, spec_id) DO NOTHING
            "#,
        )
        .bind(artifact_id)
        .bind(spec_id)
        .bind(spec_revision)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
