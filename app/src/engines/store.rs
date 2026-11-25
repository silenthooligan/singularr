//! SQL over `engine_state` (what each engine's passes found: its version, latency and counts),
//! `integration_health` (how each pass of each integration went) and `integration_access` (what
//! the owner saved that Singularr may do with each integration).
//!
//! An integration is up while every pass of it is. Its state (`ok`, `last_error`, `failures`)
//! is its worst pass: a failing one first, the one with the most failures in a row of those.

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

/// Every engine with what its passes found, its state taken from its worst pass. Its last
/// success is the worst pass's while one fails, and the newest of its passes' while none does.
/// An engine whose passes are not recorded yet keeps what its last call left in `engine_state`.
pub async fn list(pool: &PgPool) -> Result<Vec<EngineStateRow>, sqlx::Error> {
    sqlx::query_as::<_, EngineStateRow>(
        "SELECT s.engine, s.kind, s.library_id, COALESCE(w.ok, s.ok) AS ok, s.version,
                s.last_sync_at,
                CASE WHEN w.ok IS NULL THEN s.last_ok_at
                     WHEN w.ok THEN w.newest_ok ELSE w.last_ok_at END AS last_ok_at,
                CASE WHEN w.ok IS NULL THEN s.last_error ELSE w.last_error END AS last_error,
                s.latency_ms, s.latency_history, s.counts,
                COALESCE(w.failures, s.failures) AS failures
         FROM engine_state s
         LEFT JOIN LATERAL (
             SELECT h.ok, h.last_error, h.failures, h.last_ok_at,
                    max(h.last_ok_at) OVER () AS newest_ok
             FROM integration_health h
             WHERE h.integration = s.engine
             ORDER BY h.ok, h.failures DESC, h.last_run_at DESC
             LIMIT 1
         ) w ON true
         ORDER BY s.engine",
    )
    .fetch_all(pool)
    .await
}

/// Keep the pass rows of these passes alone: the passes this process runs. A row of a pass no
/// loop runs any longer would keep its integration down.
pub async fn keep_passes(
    conn: &mut PgConnection,
    passes: &[(String, &str)],
) -> Result<u64, sqlx::Error> {
    let integrations: Vec<&str> = passes.iter().map(|(i, _)| i.as_str()).collect();
    let names: Vec<&str> = passes.iter().map(|(_, p)| *p).collect();
    sqlx::query(
        "DELETE FROM integration_health h
         WHERE NOT EXISTS (
             SELECT 1 FROM unnest($1::text[], $2::text[]) AS k(integration, pass)
             WHERE k.integration = h.integration AND k.pass = h.pass)",
    )
    .bind(&integrations)
    .bind(&names)
    .execute(conn)
    .await
    .map(|r| r.rows_affected())
}

/// Whether the engine is up: every pass of it succeeded at its last run. An engine whose
/// passes are not recorded yet is what its last call left in `engine_state`. `None`: it was
/// never recorded.
pub async fn was_ok(conn: &mut PgConnection, engine: &str) -> Result<Option<bool>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<bool>>(
        "SELECT COALESCE(
             (SELECT bool_and(ok) FROM integration_health WHERE integration = $1),
             (SELECT ok FROM engine_state WHERE engine = $1))",
    )
    .bind(engine)
    .fetch_one(conn)
    .await
}

/// Record a pass of an integration that succeeded, with its round trip.
pub async fn record_pass_ok(
    conn: &mut PgConnection,
    integration: &str,
    pass: &str,
    latency_ms: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO integration_health (integration, pass, ok, last_run_at, last_ok_at,
                                         last_error, failures, latency_ms)
         VALUES ($1, $2, true, now(), now(), NULL, 0, $3)
         ON CONFLICT (integration, pass) DO UPDATE SET
             ok = true, last_run_at = now(), last_ok_at = now(), last_error = NULL,
             failures = 0, latency_ms = EXCLUDED.latency_ms",
    )
    .bind(integration)
    .bind(pass)
    .bind(latency_ms)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Record a pass of an integration that failed.
pub async fn record_pass_error(
    conn: &mut PgConnection,
    integration: &str,
    pass: &str,
    error: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO integration_health (integration, pass, ok, last_run_at, last_error, failures)
         VALUES ($1, $2, false, now(), $3, 1)
         ON CONFLICT (integration, pass) DO UPDATE SET
             ok = false, last_run_at = now(), last_error = EXCLUDED.last_error,
             failures = integration_health.failures + 1",
    )
    .bind(integration)
    .bind(pass)
    .bind(error)
    .execute(conn)
    .await
    .map(|_| ())
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

/// Remove the state of engines outside `keep`, their passes with it. The count is the engines'.
pub async fn prune(conn: &mut PgConnection, keep: &[String]) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM integration_health WHERE NOT (integration = ANY($1))")
        .bind(keep)
        .execute(&mut *conn)
        .await?;
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
