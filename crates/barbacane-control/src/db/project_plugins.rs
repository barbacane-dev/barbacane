//! Project plugin configurations repository for CRUD operations.

use sqlx::PgPool;
use uuid::Uuid;

use super::models::{NewProjectPluginConfig, ProjectPluginConfig, UpdateProjectPluginConfig};

/// Repository for project plugin configuration operations.
#[derive(Clone)]
pub struct ProjectPluginConfigsRepository {
    pool: PgPool,
}

impl ProjectPluginConfigsRepository {
    /// Create a new repository.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Add a plugin to a project.
    pub async fn create(
        &self,
        project_id: Uuid,
        config: NewProjectPluginConfig,
    ) -> Result<ProjectPluginConfig, sqlx::Error> {
        sqlx::query_as::<_, ProjectPluginConfig>(
            r#"
            INSERT INTO project_plugin_configs
                (project_id, plugin_name, plugin_version, enabled, priority, config)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#,
        )
        .bind(project_id)
        .bind(&config.plugin_name)
        .bind(&config.plugin_version)
        .bind(config.enabled)
        .bind(config.priority)
        .bind(&config.config)
        .fetch_one(&self.pool)
        .await
    }

    /// List all plugin configurations for a project.
    pub async fn list_for_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<ProjectPluginConfig>, sqlx::Error> {
        sqlx::query_as::<_, ProjectPluginConfig>(
            r#"
            SELECT * FROM project_plugin_configs
            WHERE project_id = $1
            ORDER BY priority, plugin_name
            "#,
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Get a specific plugin configuration by project and plugin name.
    pub async fn get(
        &self,
        project_id: Uuid,
        plugin_name: &str,
    ) -> Result<Option<ProjectPluginConfig>, sqlx::Error> {
        sqlx::query_as::<_, ProjectPluginConfig>(
            r#"
            SELECT * FROM project_plugin_configs
            WHERE project_id = $1 AND plugin_name = $2
            "#,
        )
        .bind(project_id)
        .bind(plugin_name)
        .fetch_optional(&self.pool)
        .await
    }

    /// Update a plugin configuration.
    pub async fn update(
        &self,
        project_id: Uuid,
        plugin_name: &str,
        update: UpdateProjectPluginConfig,
    ) -> Result<Option<ProjectPluginConfig>, sqlx::Error> {
        // Build dynamic update query
        let mut set_clauses = vec!["updated_at = NOW()".to_string()];
        let mut param_idx = 3; // $1 is project_id, $2 is plugin_name

        if update.plugin_version.is_some() {
            set_clauses.push(format!("plugin_version = ${}", param_idx));
            param_idx += 1;
        }
        if update.enabled.is_some() {
            set_clauses.push(format!("enabled = ${}", param_idx));
            param_idx += 1;
        }
        if update.priority.is_some() {
            set_clauses.push(format!("priority = ${}", param_idx));
            param_idx += 1;
        }
        if update.config.is_some() {
            set_clauses.push(format!("config = ${}", param_idx));
        }

        let query = format!(
            "UPDATE project_plugin_configs SET {} WHERE project_id = $1 AND plugin_name = $2 RETURNING *",
            set_clauses.join(", ")
        );

        let mut q = sqlx::query_as::<_, ProjectPluginConfig>(&query)
            .bind(project_id)
            .bind(plugin_name);

        if let Some(version) = &update.plugin_version {
            q = q.bind(version);
        }
        if let Some(enabled) = update.enabled {
            q = q.bind(enabled);
        }
        if let Some(priority) = update.priority {
            q = q.bind(priority);
        }
        if let Some(config) = &update.config {
            q = q.bind(config);
        }

        q.fetch_optional(&self.pool).await
    }

    /// Remove a plugin from a project.
    pub async fn delete(&self, project_id: Uuid, plugin_name: &str) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM project_plugin_configs WHERE project_id = $1 AND plugin_name = $2",
        )
        .bind(project_id)
        .bind(plugin_name)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}
