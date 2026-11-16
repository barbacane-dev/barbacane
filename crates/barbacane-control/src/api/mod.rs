//! REST API handlers for the control plane.

mod api_keys;
mod artifacts;
mod auth;
mod compilations;
mod data_planes;
mod health;
mod init;
pub(crate) mod multipart;
mod operations;
mod pagination;
mod plugins;
mod project_plugins;
mod projects;
mod router;
mod specs;
pub mod ws;

pub use auth::AdminAuth;
pub use router::create_router;
pub use ws::ConnectionManager;

#[cfg(test)]
mod tests;

/// The migrated PostgreSQL pool the tests run against: `DATABASE_URL`, or
/// the local default. Without one, a local run skips the test (`None`) and a
/// CI run (`CI` set) fails it, since a job whose tests need a database must
/// have one and a skipped test there passes without having run.
#[cfg(test)]
pub(crate) async fn test_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://barbacane:barbacane@localhost:5432/barbacane".to_string());
    let unavailable = |why: String| {
        let under_ci = std::env::var("CI").is_ok_and(|value| !value.is_empty() && value != "false");
        assert!(
            !under_ci,
            "PostgreSQL at {url}: {why}; under CI a test does not skip for want of it"
        );
        eprintln!("skip: PostgreSQL at {url}: {why}");
    };
    let pool = match crate::db::create_pool(&url).await {
        Ok(pool) => pool,
        Err(e) => {
            unavailable(e.to_string());
            return None;
        }
    };
    if let Err(e) = crate::db::run_migrations(&pool).await {
        unavailable(e.to_string());
        return None;
    }
    Some(pool)
}
