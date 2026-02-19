//! API keys repository for managing project API keys.

use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::models::{ApiKey, ApiKeyCreated, NewApiKey};

/// Repository for API key CRUD operations.
#[derive(Clone)]
pub struct ApiKeysRepository {
    pool: PgPool,
}

impl ApiKeysRepository {
    /// Create a new repository instance.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Generate a new API key with prefix "bbk_".
    fn generate_key() -> (String, String, String) {
        let mut rng = rand::rng();
        let random_bytes: [u8; 24] = rng.random();
        let key_suffix = base64_simd::URL_SAFE_NO_PAD.encode_to_string(random_bytes);
        let full_key = format!("bbk_{}", key_suffix);
        let prefix = full_key.chars().take(12).collect::<String>();

        let mut hasher = Sha256::new();
        hasher.update(full_key.as_bytes());
        let hash = hex::encode(hasher.finalize());

        (full_key, prefix, hash)
    }

    /// Hash an API key for comparison.
    pub fn hash_key(key: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(key.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Create a new API key. Returns the full key once (not stored).
    pub async fn create(
        &self,
        project_id: Uuid,
        new_key: NewApiKey,
    ) -> Result<ApiKeyCreated, sqlx::Error> {
        let (full_key, prefix, hash) = Self::generate_key();

        let key = sqlx::query_as::<_, ApiKey>(
            r#"
            INSERT INTO api_keys (project_id, name, key_hash, key_prefix, scopes, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING *
            "#,
        )
        .bind(project_id)
        .bind(&new_key.name)
        .bind(&hash)
        .bind(&prefix)
        .bind(&new_key.scopes)
        .bind(new_key.expires_at)
        .fetch_one(&self.pool)
        .await?;

        Ok(ApiKeyCreated {
            id: key.id,
            name: key.name,
            key: full_key,
            key_prefix: key.key_prefix,
            scopes: key.scopes,
            expires_at: key.expires_at,
            created_at: key.created_at,
        })
    }

    /// Get an API key by ID.
    pub async fn get(&self, id: Uuid) -> Result<Option<ApiKey>, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>("SELECT * FROM api_keys WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Validate an API key and update last_used_at.
    pub async fn validate_and_touch(&self, key: &str) -> Result<Option<ApiKey>, sqlx::Error> {
        let hash = Self::hash_key(key);
        sqlx::query_as::<_, ApiKey>(
            r#"
            UPDATE api_keys
            SET last_used_at = NOW()
            WHERE key_hash = $1
              AND revoked_at IS NULL
              AND (expires_at IS NULL OR expires_at > NOW())
            RETURNING *
            "#,
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await
    }

    /// List all API keys for a project (without the full key).
    pub async fn list_for_project(&self, project_id: Uuid) -> Result<Vec<ApiKey>, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>(
            "SELECT * FROM api_keys WHERE project_id = $1 ORDER BY created_at DESC",
        )
        .bind(project_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Revoke an API key.
    pub async fn revoke(&self, id: Uuid) -> Result<Option<ApiKey>, sqlx::Error> {
        sqlx::query_as::<_, ApiKey>(
            r#"
            UPDATE api_keys
            SET revoked_at = NOW()
            WHERE id = $1 AND revoked_at IS NULL
            RETURNING *
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
    }
}
