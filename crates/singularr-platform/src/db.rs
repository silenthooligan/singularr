//! The database pool and the migration runner. The migrations are the app's: it hands its
//! migrator over (`app/src/core/db.rs`).

use super::http::ReadyCheck;
use super::store;
use sqlx::PgPool;
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;

/// A pool over a login: the owner's (`DATABASE_URL`), or Singularr Requests' role
/// (`deploy/requests-role.sql`).
pub async fn connect(url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(10))
        .connect(url)
        .await
}

pub async fn migrate(pool: &PgPool, migrator: &Migrator) -> Result<(), MigrateError> {
    migrator.run(pool).await
}

/// Descriptions of the migrations of `migrator` the database has not applied.
pub async fn pending_migrations(
    pool: &PgPool,
    migrator: &Migrator,
) -> Result<Vec<String>, sqlx::Error> {
    let applied = match store::applied_migrations(pool).await {
        Ok(v) => v,
        // No migrations table yet: nothing has been applied.
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("42P01") => Vec::new(),
        Err(e) => return Err(e),
    };
    Ok(migrator
        .iter()
        .filter(|m| !applied.contains(&m.version))
        .map(|m| format!("{} {}", m.version, m.description))
        .collect())
}

/// Ready means the database answers and every migration of `migrator` is applied.
pub fn ready_check(pool: PgPool, migrator: &'static Migrator) -> ReadyCheck {
    std::sync::Arc::new(move || {
        let pool = pool.clone();
        Box::pin(async move {
            store::ping(&pool)
                .await
                .map_err(|e| format!("database: {e}"))?;
            let pending = pending_migrations(&pool, migrator)
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
