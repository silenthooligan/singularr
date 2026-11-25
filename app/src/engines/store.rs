//! SQL over `engine_state` (how each engine's last sync went) and `integration_access` (what
//! the owner saved that Singularr may do with each integration).

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EngineStateRow {
    pub engine: String,
    pub kind: String,
    pub library_id: Option<String>,
    pub ok: bool,
    pub version: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub latency_ms: Option<i32>,
    pub latency_history: Vec<i32>,
    pub counts: serde_json::Value,
    pub failures: i32,
}

pub async fn list(pool: &PgPool) -> Result<Vec<EngineStateRow>, sqlx::Error> {
    sqlx::query_as::<_, EngineStateRow>(
        "SELECT engine, kind, library_id, ok, version, last_sync_at, last_ok_at, last_error,
                latency_ms, latency_history, counts, failures
         FROM engine_state ORDER BY engine",
    )
    .fetch_all(pool)
    .await
}

/// Whether the engine's last call succeeded. `None`: it was never recorded.
pub async fn was_ok(conn: &mut PgConnection, engine: &str) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar::<_, bool>("SELECT ok FROM engine_state WHERE engine = $1")
        .bind(engine)
        .fetch_optional(conn)
        .await
}

/// Record a call that succeeded. `counts` is merged into what earlier passes counted.
pub async fn record_ok(
    conn: &mut PgConnection,
    engine: &str,
    kind: &str,
    library: Option<&str>,
    version: Option<&str>,
    latency_ms: i32,
    counts: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO engine_state (engine, kind, library_id, ok, version, last_sync_at, last_ok_at,
                                   last_error, latency_ms, latency_history, counts, failures,
                                   updated_at)
         VALUES ($1, $2, $3, true, $4, now(), now(), NULL, $5, ARRAY[$5]::integer[], $6, 0, now())
         ON CONFLICT (engine) DO UPDATE SET
             kind = EXCLUDED.kind, library_id = EXCLUDED.library_id, ok = true,
             version = COALESCE(EXCLUDED.version, engine_state.version),
             last_sync_at = now(), last_ok_at = now(), last_error = NULL,
             latency_ms = EXCLUDED.latency_ms,
             latency_history = (engine_state.latency_history || EXCLUDED.latency_ms)[
                 GREATEST(1, cardinality(engine_state.latency_history) - 18):],
             counts = engine_state.counts || EXCLUDED.counts, failures = 0, updated_at = now()",
    )
    .bind(engine)
    .bind(kind)
    .bind(library)
    .bind(version)
    .bind(latency_ms)
    .bind(counts)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Record a call that failed.
pub async fn record_error(
    conn: &mut PgConnection,
    engine: &str,
    kind: &str,
    library: Option<&str>,
    error: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO engine_state (engine, kind, library_id, ok, last_sync_at, last_error, failures,
                                   updated_at)
         VALUES ($1, $2, $3, false, now(), $4, 1, now())
         ON CONFLICT (engine) DO UPDATE SET
             kind = EXCLUDED.kind, library_id = EXCLUDED.library_id, ok = false,
             last_sync_at = now(), last_error = EXCLUDED.last_error,
             failures = engine_state.failures + 1, updated_at = now()",
    )
    .bind(engine)
    .bind(kind)
    .bind(library)
    .bind(error)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Remove the state of engines outside `keep`.
pub async fn prune(conn: &mut PgConnection, keep: &[String]) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM engine_state WHERE NOT (engine = ANY($1))")
        .bind(keep)
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}

/// What the owner saved that Singularr may do with each integration: its id, and `read` or
/// `act`.
pub async fn saved_access(pool: &PgPool) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT integration, access FROM integration_access ORDER BY integration",
    )
    .fetch_all(pool)
    .await
}

/// Save what Singularr may do with an integration, replacing what was saved before.
pub async fn save_access(
    conn: &mut PgConnection,
    integration: &str,
    access: &str,
    by: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO integration_access (integration, access, saved_by, saved_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (integration) DO UPDATE SET
             access = EXCLUDED.access, saved_by = EXCLUDED.saved_by, saved_at = now()",
    )
    .bind(integration)
    .bind(access)
    .bind(by)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Remove the saved access of integrations outside `keep`: a permission does not outlive the
/// listing it was given to.
pub async fn prune_access(conn: &mut PgConnection, keep: &[String]) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM integration_access WHERE NOT (integration = ANY($1))")
        .bind(keep)
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}
