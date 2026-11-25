//! Compilations repository for async job tracking.

use super::Page;
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
    pub async fn list_for_spec(
        &self,
        spec_id: Uuid,
        page: Page,
    ) -> Result<Vec<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            SELECT * FROM compilations
            WHERE spec_id = $1
            ORDER BY started_at DESC, id
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(spec_id)
        .bind(page.limit)
        .bind(page.offset)
        .fetch_all(&self.pool)
        .await
    }

    /// List compilations for a project.
    pub async fn list_for_project(
        &self,
        project_id: Uuid,
        page: Page,
    ) -> Result<Vec<Compilation>, sqlx::Error> {
        sqlx::query_as::<_, Compilation>(
            r#"
            SELECT * FROM compilations
            WHERE project_id = $1
            ORDER BY started_at DESC, id
            LIMIT $2 OFFSET $3
            "#,
        )
        .bind(project_id)
        .bind(page.limit)
        .bind(page.offset)
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

    /// Put back to `pending` the compilations a stopped run left `compiling`.
    /// Returns how many.
    pub async fn requeue_interrupted(&self) -> Result<u64, sqlx::Error> {
        sqlx::query("UPDATE compilations SET status = 'pending' WHERE status = 'compiling'")
            .execute(&self.pool)
            .await
            .map(|done| done.rows_affected())
    }

    /// The pending compilations, oldest first.
    pub async fn pending(&self) -> Result<Vec<Uuid>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT id FROM compilations WHERE status = 'pending' ORDER BY started_at, id",
        )
        .fetch_all(&self.pool)
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
        .inspect(|_| crate::metrics::metrics().compilation(true))
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
        .inspect(|_| crate::metrics::metrics().compilation(false))
    }

    /// Mark a compilation failed if it is still `compiling`; one that ended
    /// keeps its outcome. Returns whether it was marked.
    pub async fn fail_if_compiling(
        &self,
        id: Uuid,
        errors: serde_json::Value,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE compilations
            SET status = 'failed', errors = $2, completed_at = $3
            WHERE id = $1 AND status = 'compiling'
            "#,
        )
        .bind(id)
        .bind(&errors)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        let failed = result.rows_affected() > 0;
        if failed {
            crate::metrics::metrics().compilation(false);
        }
        Ok(failed)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{NewProject, NewSpec, ProjectsRepository, SpecsRepository};

    /// The test database, migrated; `None` when `DATABASE_URL` is unreachable.
    async fn pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://barbacane:barbacane@localhost:5432/barbacane".to_string()
        });
        let pool = crate::db::create_pool(&url).await.ok()?;
        crate::db::run_migrations(&pool).await.ok()?;
        Some(pool)
    }

    #[tokio::test]
    async fn an_interrupted_compilation_is_requeued_and_pending_ones_come_oldest_first() {
        let Some(pool) = pool().await else {
            eprintln!("skip: no database");
            return;
        };
        let project = ProjectsRepository::new(pool.clone())
            .create(NewProject {
                name: format!("requeue-{}", Uuid::new_v4().simple()),
                description: None,
            })
            .await
            .expect("project");
        let spec = SpecsRepository::new(pool.clone())
            .create(NewSpec {
                project_id: project.id,
                name: format!("requeue-{}", Uuid::new_v4().simple()),
                spec_type: "openapi".into(),
                spec_version: "3.0.3".into(),
                sha256: "0".repeat(64),
                content: b"openapi: 3.0.3".to_vec(),
                filename: "api.yaml".into(),
            })
            .await
            .expect("spec");
        let repo = CompilationsRepository::new(pool.clone());
        let first = repo
            .create(spec.id, Some(project.id), false, serde_json::json!([]))
            .await
            .expect("first");
        let second = repo
            .create(spec.id, Some(project.id), false, serde_json::json!([]))
            .await
            .expect("second");
        // The first was being compiled when the process stopped.
        assert!(repo.claim(first.id).await.expect("claim").is_some());

        assert!(repo.requeue_interrupted().await.expect("requeue") >= 1);
        let status = repo
            .get(first.id)
            .await
            .expect("get")
            .expect("exists")
            .status;
        assert_eq!(status, "pending");

        let pending = repo.pending().await.expect("pending");
        let at = |id: Uuid| pending.iter().position(|p| *p == id);
        let (first_at, second_at) = (at(first.id), at(second.id));
        assert!(first_at.is_some() && second_at.is_some(), "{pending:?}");
        assert!(first_at < second_at, "oldest first: {pending:?}");

        ProjectsRepository::new(pool)
            .delete(project.id)
            .await
            .expect("clean up");
    }
}
