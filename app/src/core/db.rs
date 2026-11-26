//! The database: the platform's pool and migration runner, with the migrations in
//! `app/migrations`.

use super::config::Config;
use singularr_platform::db as platform;
use singularr_platform::http::ReadyCheck;
use sqlx::PgPool;

/// A pool over another login: Singularr Requests' role (`deploy/requests-role.sql`).
pub use singularr_platform::db::connect as connect_url;

/// The migrations in `app/migrations`, embedded at build time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(config: &Config) -> Result<PgPool, sqlx::Error> {
    platform::connect(
        &config.bootstrap.database_url,
        config.bootstrap.db_max_connections,
    )
    .await
}

pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    platform::migrate(pool, &MIGRATOR).await
}

/// Descriptions of the embedded migrations the database has not applied.
pub async fn pending_migrations(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    platform::pending_migrations(pool, &MIGRATOR).await
}

/// Ready means the database answers and every embedded migration is applied.
pub fn ready_check(pool: PgPool) -> ReadyCheck {
    platform::ready_check(pool, &MIGRATOR)
}
