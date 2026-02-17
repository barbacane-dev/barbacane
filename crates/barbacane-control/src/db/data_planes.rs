//! Data planes repository for managing connected data plane records.

use chrono::Utc;
use sqlx::PgPool;
use uuid::Uuid;

use super::models::{DataPlane, NewDataPlane};

/// Repository for data plane CRUD operations.
#[derive(Clone)]
pub struct DataPlanesRepository {
    pool: PgPool,
}

impl DataPlanesRepository {
    /// Create a new repository instance.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create a new data plane record (used when a data plane connects).
    pub async fn create(&self, data_plane: NewDataPlane) -> Result<DataPlane, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>(
            r#"
            INSERT INTO data_planes (project_id, name, status, connected_at, metadata)
            VALUES ($1, $2, 'online', NOW(), $3)
            RETURNING *
            "#,
        )
        .bind(data_plane.project_id)
        .bind(&data_plane.name)
        .bind(&data_plane.metadata)
        .fetch_one(&self.pool)
        .await
    }

    /// Get a data plane by ID.
    pub async fn get(&self, id: Uuid) -> Result<Option<DataPlane>, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>("SELECT * FROM data_planes WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// List all data planes for a project.
    pub async fn list_for_project(&self, project_id: Uuid) -> Result<Vec<DataPlane>, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>(
            "SELECT * FROM data_planes WHERE project_id = $1 ORDER BY created_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
    }

    /// List online data planes for a project.
    #[allow(dead_code)]
    pub async fn list_online_for_project(
        &self,
        project_id: Uuid,
    ) -> Result<Vec<DataPlane>, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>(
            "SELECT * FROM data_planes WHERE project_id = $1 AND status = 'online' ORDER BY connected_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Update last_seen timestamp (called on heartbeat).
    pub async fn update_last_seen(&self, id: Uuid) -> Result<Option<DataPlane>, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>(
            r#"
            UPDATE data_planes
            SET last_seen = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Update the artifact ID (called when data plane downloads new artifact).
    pub async fn update_artifact(
        &self,
        id: Uuid,
        artifact_id: Uuid,
    ) -> Result<Option<DataPlane>, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>(
            r#"
            UPDATE data_planes
            SET artifact_id = $2, last_seen = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(artifact_id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Set data plane status.
    #[allow(dead_code)]
    pub async fn set_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<Option<DataPlane>, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>(
            r#"
            UPDATE data_planes
            SET status = $2, last_seen = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await
    }

    /// Mark a data plane as offline (called when connection drops).
    pub async fn mark_offline(&self, id: Uuid) -> Result<Option<DataPlane>, sqlx::Error> {
        sqlx::query_as::<_, DataPlane>(
            r#"
            UPDATE data_planes
            SET status = 'offline', last_seen = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }

    /// Delete a data plane record.
    pub async fn delete(&self, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM data_planes WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Mark all online data planes as offline.
    ///
    /// Called on startup since the in-memory connection manager is empty
    /// and no WebSocket connections exist yet.
    pub async fn mark_all_offline(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE data_planes
            SET status = 'offline', last_seen = NOW()
            WHERE status = 'online'
            "#,
        )
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Mark stale data planes as offline (those not seen in the last N minutes).
    pub async fn mark_stale_offline(&self, stale_minutes: i64) -> Result<u64, sqlx::Error> {
        let cutoff = Utc::now() - chrono::Duration::minutes(stale_minutes);
        let result = sqlx::query(
            r#"
            UPDATE data_planes
            SET status = 'offline'
            WHERE status = 'online' AND last_seen < $1
            "#,
        )
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}
