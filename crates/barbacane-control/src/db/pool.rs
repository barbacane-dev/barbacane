//! Database connection pool and migrations.

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Create a PostgreSQL connection pool. `database_url` takes libpq's TLS
/// parameters: `sslmode`, `sslrootcert`, `sslcert` and `sslkey`.
pub async fn create_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    // rustls is built here with two crypto providers (aws-lc-rs for sqlx, ring
    // for reqwest), so it cannot pick a process default by itself, and sqlx
    // asks for one with `sslmode=verify-ca`. aws-lc-rs, as the data plane; a
    // later call keeps the provider already installed.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
}

/// Run database migrations.
pub async fn run_migrations(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!("./migrations").run(pool).await
}

#[cfg(test)]
mod tests {
    use super::create_pool;

    /// A PostgreSQL that serves TLS with a certificate for `localhost`, and the
    /// CA that signed it: `BARBACANE_TEST_POSTGRES_TLS_URL` (without `sslmode`)
    /// and `BARBACANE_TEST_POSTGRES_TLS_CA`. CI starts one (`ci.yml`,
    /// control-plane-tests); a local run skips without it.
    fn tls_server() -> Option<(String, String)> {
        match (
            std::env::var("BARBACANE_TEST_POSTGRES_TLS_URL"),
            std::env::var("BARBACANE_TEST_POSTGRES_TLS_CA"),
        ) {
            (Ok(url), Ok(ca)) => Some((url, ca)),
            _ => {
                let under_ci =
                    std::env::var("CI").is_ok_and(|value| !value.is_empty() && value != "false");
                assert!(
                    !under_ci,
                    "no PostgreSQL serving TLS (BARBACANE_TEST_POSTGRES_TLS_URL and \
                     BARBACANE_TEST_POSTGRES_TLS_CA); under CI a test does not skip for want of it"
                );
                eprintln!("skip: no PostgreSQL serving TLS (BARBACANE_TEST_POSTGRES_TLS_URL)");
                None
            }
        }
    }

    /// Whether the pool's connection is encrypted, as PostgreSQL reports it.
    async fn encrypted(pool: &sqlx::PgPool) -> bool {
        sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
            .fetch_one(pool)
            .await
            .expect("pg_stat_ssl")
    }

    #[tokio::test]
    async fn sslmode_require_connects_over_tls() {
        let Some((url, _)) = tls_server() else {
            return;
        };
        let pool = create_pool(&format!("{url}?sslmode=require"))
            .await
            .expect("connects with sslmode=require");
        assert!(encrypted(&pool).await, "the connection is not encrypted");
    }

    #[tokio::test]
    async fn verify_trusts_the_given_ca_only() {
        let Some((url, ca)) = tls_server() else {
            return;
        };
        let pool = create_pool(&format!("{url}?sslmode=verify-full&sslrootcert={ca}"))
            .await
            .expect("connects with the server's CA");
        assert!(encrypted(&pool).await, "the connection is not encrypted");

        // verify-ca checks the chain but not the name.
        let pool = create_pool(&format!("{url}?sslmode=verify-ca&sslrootcert={ca}"))
            .await
            .expect("connects with verify-ca");
        assert!(encrypted(&pool).await, "the connection is not encrypted");

        // Without `sslrootcert`, the test CA is not a trusted root.
        assert!(
            create_pool(&format!("{url}?sslmode=verify-full"))
                .await
                .is_err(),
            "a certificate from an unknown CA is accepted"
        );
    }
}
