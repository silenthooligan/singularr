//! SQL over `settings`: the values saved in the interface, by scope and key. Also the facts the
//! System page shows about the database itself, which belong to no module's table.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SavedRow {
    /// `instance`, or `user:<who>`.
    pub scope: String,
    pub key: String,
    pub value: serde_json::Value,
    pub saved_by: String,
    pub saved_at: DateTime<Utc>,
}

/// Every saved value.
pub async fn all(pool: &PgPool) -> Result<Vec<SavedRow>, sqlx::Error> {
    sqlx::query_as::<_, SavedRow>(
        "SELECT scope, key, value, saved_by, saved_at FROM settings ORDER BY scope, key",
    )
    .fetch_all(pool)
    .await
}

/// Save a value, replacing the one saved before.
pub async fn put(
    conn: &mut PgConnection,
    scope: &str,
    key: &str,
    value: &serde_json::Value,
    by: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO settings (scope, key, value, saved_by, saved_at)
         VALUES ($1, $2, $3, $4, now())
         ON CONFLICT (scope, key) DO UPDATE SET
             value = EXCLUDED.value, saved_by = EXCLUDED.saved_by, saved_at = now()",
    )
    .bind(scope)
    .bind(key)
    .bind(value)
    .bind(by)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Take a saved value away.
pub async fn remove(conn: &mut PgConnection, scope: &str, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM settings WHERE scope = $1 AND key = $2")
        .bind(scope)
        .bind(key)
        .execute(conn)
        .await
        .map(|_| ())
}

/// A table's rows (PostgreSQL's estimate, 0 before it is first analyzed) and its size on disk
/// with its indexes, in bytes. `None` when there is no such table.
pub async fn table_facts(pool: &PgPool, table: &str) -> Result<Option<(i64, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT GREATEST(c.reltuples, 0)::bigint, pg_total_relation_size(c.oid)
         FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE c.relname = $1 AND c.relkind = 'r' AND n.nspname = current_schema()",
    )
    .bind(table)
    .fetch_optional(pool)
    .await
}

/// The database server's version and the size of Singularr's database, in bytes.
pub async fn database_facts(pool: &PgPool) -> Result<(String, i64), sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT current_setting('server_version'), pg_database_size(current_database())",
    )
    .fetch_one(pool)
    .await
}
