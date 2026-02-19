//! Projects repository for CRUD operations.

use sqlx::PgPool;
use uuid::Uuid;

use super::models::{NewProject, Project, UpdateProject};

/// Repository for project operations.
#[derive(Clone)]
pub struct ProjectsRepository {
    pool: PgPool,
}

impl ProjectsRepository {
    /// Create a new projects repository.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new project.
    pub async fn create(&self, project: NewProject) -> Result<Project, sqlx::Error> {
        sqlx::query_as::<_, Project>(
            r#"
            INSERT INTO projects (name, description, production_mode)
            VALUES ($1, $2, $3)
            RETURNING *
            "#,
        )
        .bind(&project.name)
        .bind(&project.description)
        .bind(project.production_mode)
        .fetch_one(&self.pool)
        .await
    }

    /// List all projects.
    pub async fn list(&self) -> Result<Vec<Project>, sqlx::Error> {
        sqlx::query_as::<_, Project>("SELECT * FROM projects ORDER BY name")
            .fetch_all(&self.pool)
            .await
    }

    /// Get a project by ID.
    pub async fn get_by_id(&self, id: Uuid) -> Result<Option<Project>, sqlx::Error> {
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Get a project by name.
    pub async fn get_by_name(&self, name: &str) -> Result<Option<Project>, sqlx::Error> {
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE name = $1")
            .bind(name)
            .fetch_optional(&self.pool)
            .await
    }

    /// Update a project.
    pub async fn update(
        &self,
        id: Uuid,
        update: UpdateProject,
    ) -> Result<Option<Project>, sqlx::Error> {
        // Build dynamic update query
        let mut set_clauses = vec!["updated_at = NOW()".to_string()];
        let mut param_idx = 2; // $1 is the id

        if update.name.is_some() {
            set_clauses.push(format!("name = ${}", param_idx));
            param_idx += 1;
        }
        if update.description.is_some() {
            set_clauses.push(format!("description = ${}", param_idx));
            param_idx += 1;
        }
        if update.production_mode.is_some() {
            set_clauses.push(format!("production_mode = ${}", param_idx));
        }

        let query = format!(
            "UPDATE projects SET {} WHERE id = $1 RETURNING *",
            set_clauses.join(", ")
        );

        let mut q = sqlx::query_as::<_, Project>(&query).bind(id);

        if let Some(name) = &update.name {
            q = q.bind(name);
        }
        if let Some(description) = &update.description {
            q = q.bind(description);
        }
        if let Some(production_mode) = update.production_mode {
            q = q.bind(production_mode);
        }

        q.fetch_optional(&self.pool).await
    }

    /// Delete a project and all associated entities (cascade).
    pub async fn delete(&self, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM projects WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }
}
