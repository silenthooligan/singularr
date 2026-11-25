//! The database pool and the embedded migrations.

use super::config::Config;
use super::store;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// The migrations in `app/migrations`, embedded at build time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(config: &Config) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(config.db_max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&config.database_url)
        .await
}

/// A pool over another login: Singularr Requests' role (`deploy/requests-role.sql`).
pub async fn connect_url(url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

/// Descriptions of the embedded migrations the database has not applied.
pub async fn pending_migrations(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    let applied = match store::applied_migrations(pool).await {
        Ok(v) => v,
        // No migrations table yet: nothing has been applied.
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P01") => Vec::new(),
        Err(e) => return Err(e),
    };
    Ok(MIGRATOR
        .iter()
        .filter(|m| !applied.contains(&m.version))
        .map(|m| format!("{} {}", m.version, m.description))
        .collect())
}

/// Ready means the database answers and every embedded migration is applied.
pub fn ready_check(pool: PgPool) -> super::http::ReadyCheck {
    std::sync::Arc::new(move || {
        let pool = pool.clone();
        Box::pin(async move {
            store::ping(&pool)
                .await
                .map_err(|e| format!("database: {e}"))?;
            let pending = pending_migrations(&pool)
                .await
                .map_err(|e| format!("database: {e}"))?;
            if pending.is_empty() {
                Ok(())
            } else {
                Err(format!("{} migration(s) not applied", pending.len()))
            }
        })
    })
}
