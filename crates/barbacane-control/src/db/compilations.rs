//! Compilations repository for async job tracking.

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use super::models::Compilation;

/// Repository for compilation job operations.
#[derive(Clone)]
pub struct CompilationsRepository {
    pool: PgPool,
}

impl CompilationsRepository {
    /// Create a new compilations repository.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new pending compilation job for a spec.
    pub async fn create(
        &self,
        spec_id: Uuid,
        project_id: Option<Uuid>,
        production: bool,
        additional_specs: serde_json::Value,
    ) -> Result<Compilation, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            INSERT INTO compilations (spec_id, project_id, production, additional_specs)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(spec_id)
        .bind(project_id)
        .bind(production)
        .bind(&additional_specs)
        .fetch_one(&self.pool)
        .await
    }

    /// Get a compilation by ID.
    pub async fn get(&self, id: Uuid) -> Result<Option<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>("SELECT * FROM compilations WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// List compilations for a spec.
    pub async fn list_for_spec(&self, spec_id: Uuid) -> Result<Vec<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            SELECT * FROM compilations
            WHERE spec_id = $1
            ORDER BY started_at DESC
            "#,
        )
        .bind(spec_id)
        .fetch_all(&self.pool)
        .await
    }

    /// List compilations for a project.
    pub async fn list_for_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            SELECT * FROM compilations
            WHERE project_id = $1
            ORDER BY started_at DESC
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Claim a pending compilation (atomically set to compiling).
    /// Returns None if the compilation was already claimed or doesn't exist.
    pub async fn claim(&self, id: Uuid) -> Result<Option<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            UPDATE compilations
            SET status = 'compiling'
            WHERE id = $1 AND status = 'pending'
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Mark a compilation as succeeded with the resulting artifact.
    pub async fn mark_succeeded(
        &self,
        id: Uuid,
        artifact_id: Uuid,
        warnings: serde_json::Value,
    ) -> Result<Option<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            UPDATE compilations
            SET status = 'succeeded', artifact_id = $2, warnings = $3, completed_at = $4
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(artifact_id)
        .bind(&warnings)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
    }

    /// Mark a compilation as failed with errors.
    pub async fn mark_failed(
        &self,
        id: Uuid,
        errors: serde_json::Value,
    ) -> Result<Option<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            UPDATE compilations
            SET status = 'failed', errors = $2, completed_at = $3
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(&errors)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
    }

    /// Delete a compilation.
    pub async fn delete(&self, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM compilations WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
