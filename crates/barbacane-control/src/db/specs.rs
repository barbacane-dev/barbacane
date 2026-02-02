//! Specs repository for CRUD operations.

use sqlx::PgPool;
use uuid::Uuid;

use super::models::{NewSpec, Spec, SpecRevision, SpecRevisionSummary};

/// Repository for spec operations.
#[derive(Clone)]
pub struct SpecsRepository {
    pool: PgPool,
}

impl SpecsRepository {
    /// Create a new specs repository.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new spec with its first revision.
    pub async fn create(&self, spec: NewSpec) -> Result<Spec, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let row = sqlx::query_as::<_, Spec>(
            r#"
            INSERT INTO specs (project_id, name, current_sha256, spec_type, spec_version)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(spec.project_id)
        .bind(&spec.name)
        .bind(&spec.sha256)
        .bind(&spec.spec_type)
        .bind(&spec.spec_version)
        .fetch_one(&mut *tx)
        .await?;

        sqlx::query(
            r#"
            INSERT INTO spec_revisions (spec_id, revision, sha256, content, filename)
            VALUES ($1, 1, $2, $3, $4)
            "#,
        )
        .bind(row.id)
        .bind(&spec.sha256)
        .bind(&spec.content)
        .bind(&spec.filename)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(row)
    }

    /// List all specs with optional filtering (global).
    pub async fn list(
        &self,
        spec_type: Option<&str>,
        name: Option<&str>,
    ) -> Result<Vec<Spec>, sqlx::Error> {
        let mut query = String::from("SELECT * FROM specs WHERE 1=1");
        let mut params: Vec<String> = vec![];

        if let Some(t) = spec_type {
            params.push(t.to_string());
            query.push_str(&format!(" AND spec_type = ${}", params.len()));
        }
        if let Some(n) = name {
            params.push(n.to_string());
            query.push_str(&format!(" AND name ILIKE ${}", params.len()));
        }
        query.push_str(" ORDER BY name");

        let mut q = sqlx::query_as::<_, Spec>(&query);
        for param in &params {
            q = q.bind(param);
        }
        q.fetch_all(&self.pool).await
    }

    /// List specs for a specific project.
    pub async fn list_for_project(&self, project_id: Uuid) -> Result<Vec<Spec>, sqlx::Error> {
        sqlx::query_as::<_, Spec>("SELECT * FROM specs WHERE project_id = $1 ORDER BY name")
            .bind(project_id)
            .fetch_all(&self.pool)
            .await
    }

    /// Get a spec by ID.
    pub async fn get_by_id(&self, id: Uuid) -> Result<Option<Spec>, sqlx::Error> {
        sqlx::query_as::<_, Spec>("SELECT * FROM specs WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Get a spec by name (global - deprecated, use get_by_project_and_name).
    #[deprecated(note = "Use get_by_project_and_name instead")]
    #[allow(dead_code)]
    pub async fn get_by_name(&self, name: &str) -> Result<Option<Spec>, sqlx::Error> {
        sqlx::query_as::<_, Spec>("SELECT * FROM specs WHERE name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
    }

    /// Get a spec by project ID and name.
    pub async fn get_by_project_and_name(
        &self,
        project_id: Uuid,
        name: &str,
    ) -> Result<Option<Spec>, sqlx::Error> {
        sqlx::query_as::<_, Spec>("SELECT * FROM specs WHERE project_id = $1 AND name = $2")
            .bind(project_id)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
    }

    /// Get the latest revision for a spec.
    pub async fn get_latest_revision(
        &self,
        spec_id: Uuid,
    ) -> Result<Option<SpecRevision>, sqlx::Error> {
        sqlx::query_as::<_, SpecRevision>(
            r#"
            SELECT * FROM spec_revisions
            WHERE spec_id = $1
            ORDER BY revision DESC
            LIMIT 1
            "#,
        )
        .bind(spec_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Update a spec by project and name with a new revision.
    /// Returns the updated spec and the new revision number.
    #[allow(clippy::too_many_arguments)]
    pub async fn update(
        &self,
        project_id: Uuid,
        name: &str,
        spec_type: &str,
        spec_version: &str,
        sha256: &str,
        content: Vec<u8>,
        filename: &str,
    ) -> Result<(Spec, i32), sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        // Get spec ID
        let spec: Spec = sqlx::query_as("SELECT * FROM specs WHERE project_id = $1 AND name = $2")
            .bind(project_id)
            .bind(name)
            .fetch_one(&mut *tx)
            .await?;

        // Get next revision number
        let next_revision: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(revision), 0) + 1 FROM spec_revisions WHERE spec_id = $1",
        )
        .bind(spec.id)
        .fetch_one(&mut *tx)
        .await?;

        // Insert new revision
        sqlx::query(
            r#"
            INSERT INTO spec_revisions (spec_id, revision, sha256, content, filename)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(spec.id)
        .bind(next_revision)
        .bind(sha256)
        .bind(&content)
        .bind(filename)
        .execute(&mut *tx)
        .await?;

        // Update spec metadata
        let updated_spec = sqlx::query_as::<_, Spec>(
            r#"
            UPDATE specs
            SET current_sha256 = $2, spec_type = $3, spec_version = $4, updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(spec.id)
        .bind(sha256)
        .bind(spec_type)
        .bind(spec_version)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok((updated_spec, next_revision))
    }

    /// Delete a spec and all its revisions (cascade).
    pub async fn delete(&self, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM specs WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Get revision history for a spec.
    pub async fn get_history(
        &self,
        spec_id: Uuid,
    ) -> Result<Vec<SpecRevisionSummary>, sqlx::Error> {
        let revisions = sqlx::query_as::<_, SpecRevision>(
            r#"
            SELECT * FROM spec_revisions
            WHERE spec_id = $1
            ORDER BY revision DESC
            "#,
        )
        .bind(spec_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(revisions
            .into_iter()
            .map(|r| SpecRevisionSummary {
                revision: r.revision,
                sha256: r.sha256,
                filename: r.filename,
                created_at: r.created_at,
            })
            .collect())
    }

    /// Get a specific revision by spec ID and revision number.
    pub async fn get_revision(
        &self,
        spec_id: Uuid,
        revision: i32,
    ) -> Result<Option<SpecRevision>, sqlx::Error> {
        sqlx::query_as::<_, SpecRevision>(
            r#"
            SELECT * FROM spec_revisions
            WHERE spec_id = $1 AND revision = $2
            "#,
        )
        .bind(spec_id)
        .bind(revision)
        .fetch_optional(&self.pool)
        .await
    }
}
