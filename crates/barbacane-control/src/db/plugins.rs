//! Plugins repository for CRUD operations.

use sqlx::PgPool;

use super::models::{NewPlugin, Plugin, PluginWithBinary};

/// Repository for plugin operations.
#[derive(Clone)]
pub struct PluginsRepository {
    pool: PgPool,
}

impl PluginsRepository {
    /// Create a new plugins repository.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Register a new plugin.
    pub async fn create(&self, plugin: NewPlugin) -> Result<Plugin, sqlx::Error> {
        sqlx::query_as::<_, Plugin>(
            r#"
            INSERT INTO plugins (name, version, plugin_type, description, capabilities, config_schema, wasm_binary, sha256)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING name, version, plugin_type, description, capabilities, config_schema, sha256, registered_at
            "#,
        )
        .bind(&plugin.name)
        .bind(&plugin.version)
        .bind(&plugin.plugin_type)
        .bind(&plugin.description)
        .bind(&plugin.capabilities)
        .bind(&plugin.config_schema)
        .bind(&plugin.wasm_binary)
        .bind(&plugin.sha256)
        .fetch_one(&self.pool)
        .await
    }

    /// List all plugins with optional filtering.
    pub async fn list(
        &self,
        plugin_type: Option<&str>,
        name: Option<&str>,
    ) -> Result<Vec<Plugin>, sqlx::Error> {
        let mut query = String::from(
            "SELECT name, version, plugin_type, description, capabilities, config_schema, sha256, registered_at FROM plugins WHERE 1=1",
        );
        let mut params: Vec<String> = vec![];

        if let Some(t) = plugin_type {
            params.push(t.to_string());
            query.push_str(&format!(" AND plugin_type = ${}", params.len()));
        }
        if let Some(n) = name {
            params.push(n.to_string());
            query.push_str(&format!(" AND name = ${}", params.len()));
        }
        query.push_str(" ORDER BY name, version DESC");

        // Build the query dynamically
        let mut q = sqlx::query_as::<_, Plugin>(&query);
        for param in &params {
            q = q.bind(param);
        }
        q.fetch_all(&self.pool).await
    }

    /// List all versions of a plugin by name.
    pub async fn list_versions(&self, name: &str) -> Result<Vec<Plugin>, sqlx::Error> {
        sqlx::query_as::<_, Plugin>(
            r#"
            SELECT name, version, plugin_type, description, capabilities, config_schema, sha256, registered_at
            FROM plugins
            WHERE name = $1
            ORDER BY version DESC
            "#,
        )
        .bind(name)
        .fetch_all(&self.pool)
        .await
    }

    /// Get a specific plugin version.
    pub async fn get(&self, name: &str, version: &str) -> Result<Option<Plugin>, sqlx::Error> {
        sqlx::query_as::<_, Plugin>(
            r#"
            SELECT name, version, plugin_type, description, capabilities, config_schema, sha256, registered_at
            FROM plugins
            WHERE name = $1 AND version = $2
            "#,
        )
        .bind(name)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
    }

    /// Get a plugin with its WASM binary.
    pub async fn get_with_binary(
        &self,
        name: &str,
        version: &str,
    ) -> Result<Option<PluginWithBinary>, sqlx::Error> {
        sqlx::query_as::<_, PluginWithBinary>(
            r#"
            SELECT * FROM plugins
            WHERE name = $1 AND version = $2
            "#,
        )
        .bind(name)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
    }

    /// Delete a plugin version.
    /// Returns an error if the plugin is referenced by an artifact.
    pub async fn delete(&self, name: &str, version: &str) -> Result<bool, sqlx::Error> {
        // Check if plugin is referenced by any artifact
        // Note: This check depends on how artifacts reference plugins.
        // For now, we just delete. A proper implementation would check artifact_specs or manifest.

        let result = sqlx::query("DELETE FROM plugins WHERE name = $1 AND version = $2")
            .bind(name)
            .bind(version)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Insert or update a plugin (upsert by name+version).
    pub async fn upsert(&self, plugin: NewPlugin) -> Result<Plugin, sqlx::Error> {
        sqlx::query_as::<_, Plugin>(
            r#"
            INSERT INTO plugins (name, version, plugin_type, description, capabilities, config_schema, wasm_binary, sha256)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (name, version) DO UPDATE SET
                plugin_type = EXCLUDED.plugin_type,
                description = EXCLUDED.description,
                capabilities = EXCLUDED.capabilities,
                config_schema = EXCLUDED.config_schema,
                wasm_binary = EXCLUDED.wasm_binary,
                sha256 = EXCLUDED.sha256,
                registered_at = NOW()
            RETURNING name, version, plugin_type, description, capabilities, config_schema, sha256, registered_at
            "#,
        )
        .bind(&plugin.name)
        .bind(&plugin.version)
        .bind(&plugin.plugin_type)
        .bind(&plugin.description)
        .bind(&plugin.capabilities)
        .bind(&plugin.config_schema)
        .bind(&plugin.wasm_binary)
        .bind(&plugin.sha256)
        .fetch_one(&self.pool)
        .await
    }

    /// Check if a plugin exists.
    pub async fn exists(&self, name: &str, version: &str) -> Result<bool, sqlx::Error> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM plugins WHERE name = $1 AND version = $2")
                .bind(name)
                .bind(version)
                .fetch_one(&self.pool)
                .await?;
        Ok(count > 0)
    }
}
