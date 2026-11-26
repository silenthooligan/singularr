//! Markers: what a pass knows across starts, one value per scope and name (`history.loaded`,
//! `ledger.whole_at`). A pass writes its marker in the transaction that writes its data, and
//! reads it at its start.

use serde_json::Value;
use sqlx::{PgConnection, PgPool};

/// A marker's value. `None` while it was never written.
pub async fn get(pool: &PgPool, scope: &str, name: &str) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar::<_, Value>("SELECT value FROM markers WHERE scope = $1 AND name = $2")
        .bind(scope)
        .bind(name)
        .fetch_optional(pool)
        .await
}

/// Write a marker in the caller's transaction, over its old value.
pub async fn put(
    conn: &mut PgConnection,
    scope: &str,
    name: &str,
    value: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO markers (scope, name, value) VALUES ($1, $2, $3)
         ON CONFLICT (scope, name) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(scope)
    .bind(name)
    .bind(value)
    .execute(conn)
    .await
    .map(|_| ())
}
