//! SQL over `download_claims` and the markers of the manager passes.
//!
//! The queue job of an arr replaces its queue rows with each read ([`put_queue`]). The history
//! job adds the download ids of each page it reads ([`add_history`]). The downloads job of a
//! client reads the claims of the ids it lists ([`of_ids`]). A claim of an arr the configuration
//! no longer runs goes at start, with its markers ([`keep_managers`]).

use super::downloads::Claims;
use crate::core::markers;
use crate::engines::QueueItem;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use std::collections::HashSet;
use std::time::Duration;

/// The marker of a loaded grab history: when the last read that reached grabs it had (or the end
/// of the history) ended, by the database's clock. Written in the transaction that ends that
/// read. A grab stored at or before it belongs to a complete read: a later read stops at the
/// first page that holds one ([`known_before`]).
pub const HISTORY_LOADED: &str = "history.loaded";

/// The marker of the last full file pass of an arr: when it started.
pub const FILES_FULL_AT: &str = "files.full_at";

/// Replace an arr's queue rows with one read of its queue, in the caller's transaction. The first
/// item that names a download id stands for it. An item with no download id claims nothing.
pub async fn put_queue(
    conn: &mut PgConnection,
    manager: &str,
    items: &[QueueItem],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM download_claims WHERE manager = $1 AND source = 'queue'")
        .bind(manager)
        .execute(&mut *conn)
        .await?;
    let mut named = HashSet::new();
    let items: Vec<&QueueItem> = items
        .iter()
        .filter(|i| i.download_id.as_deref().is_some_and(|id| named.insert(id)))
        .collect();
    if items.is_empty() {
        return Ok(());
    }
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i.download_id.as_deref())
        .collect();
    let queue_ids: Vec<i64> = items.iter().map(|i| i.queue_id).collect();
    let title_ids: Vec<Option<i64>> = items.iter().map(|i| i.engine_title_id).collect();
    let titles: Vec<&str> = items.iter().map(|i| i.title.as_str()).collect();
    let status: Vec<Option<&str>> = items.iter().map(|i| i.status.as_deref()).collect();
    let states: Vec<Option<&str>> = items.iter().map(|i| i.tracked_state.as_deref()).collect();
    let health: Vec<Option<&str>> = items.iter().map(|i| i.tracked_status.as_deref()).collect();
    let messages: Vec<Value> = items.iter().map(|i| json!(i.messages)).collect();
    let indexers: Vec<Option<&str>> = items.iter().map(|i| i.indexer.as_deref()).collect();
    sqlx::query(
        "INSERT INTO download_claims
             (manager, source, download_id, queue_id, engine_title_id, title, status,
              tracked_state, tracked_status, messages, indexer)
         SELECT $1, 'queue', q.id, q.queue_id, q.title_id, q.title, q.status, q.state, q.health,
                ARRAY(SELECT jsonb_array_elements_text(q.messages)), q.indexer
         FROM unnest($2::text[], $3::bigint[], $4::bigint[], $5::text[], $6::text[], $7::text[],
                     $8::text[], $9::jsonb[], $10::text[])
              AS q(id, queue_id, title_id, title, status, state, health, messages, indexer)",
    )
    .bind(manager)
    .bind(&ids)
    .bind(&queue_ids)
    .bind(&title_ids)
    .bind(&titles)
    .bind(&status)
    .bind(&states)
    .bind(&health)
    .bind(&messages)
    .bind(&indexers)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Add the download ids of one page of an arr's grab history, in the caller's transaction. An id
/// known already keeps its row as it was: `seen_at` is when it was first stored.
pub async fn add_history(
    conn: &mut PgConnection,
    manager: &str,
    ids: &[String],
) -> Result<(), sqlx::Error> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO download_claims (manager, source, download_id)
         SELECT $1, 'history', id FROM unnest($2::text[]) AS id
         ON CONFLICT (manager, source, download_id) DO NOTHING",
    )
    .bind(manager)
    .bind(ids)
    .execute(conn)
    .await
    .map(|_| ())
}

/// The download ids an arr's grab history holds.
pub async fn history_count(pool: &PgPool, manager: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM download_claims WHERE manager = $1 AND source = 'history'",
    )
    .bind(manager)
    .fetch_one(pool)
    .await
}

#[derive(sqlx::FromRow)]
struct Row {
    manager: String,
    source: String,
    download_id: String,
    queue_id: Option<i64>,
    engine_title_id: Option<i64>,
    title: Option<String>,
    status: Option<String>,
    tracked_state: Option<String>,
    tracked_status: Option<String>,
    messages: Vec<String>,
    indexer: Option<String>,
}

/// What these arrs claim of these download ids. `complete` is false: the caller knows whether
/// every arr was heard.
pub async fn of_ids(
    pool: &PgPool,
    managers: &[String],
    ids: &[String],
) -> Result<Claims, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT manager, source, download_id, queue_id, engine_title_id, title, status,
                tracked_state, tracked_status, messages, indexer
         FROM download_claims
         WHERE manager = ANY($1) AND download_id = ANY($2)
         ORDER BY manager, source, download_id",
    )
    .bind(managers)
    .bind(ids)
    .fetch_all(pool)
    .await?;
    let mut claims = Claims::default();
    for r in rows {
        if r.source == "queue" {
            claims.queues.entry(r.manager).or_default().push(QueueItem {
                queue_id: r.queue_id.unwrap_or_default(),
                engine_title_id: r.engine_title_id,
                title: r.title.unwrap_or_default(),
                download_id: Some(r.download_id),
                status: r.status,
                tracked_state: r.tracked_state,
                tracked_status: r.tracked_status,
                messages: r.messages,
                indexer: r.indexer,
            });
        } else {
            claims
                .history
                .entry(r.manager)
                .or_default()
                .insert(r.download_id);
        }
    }
    Ok(claims)
}

/// Remove the claims, the format rows and the markers of every arr but these: at start, before
/// the jobs run. An arr that comes back reads its whole history again.
pub async fn keep_managers(pool: &PgPool, managers: &[String]) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let claims = sqlx::query("DELETE FROM download_claims WHERE NOT (manager = ANY($1))")
        .bind(managers)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    super::formats::keep_managers(&mut tx, managers).await?;
    sqlx::query("DELETE FROM markers WHERE name = ANY($1) AND NOT (scope = ANY($2))")
        .bind(&[HISTORY_LOADED, FILES_FULL_AT, super::formats::REFRESHED_AT][..])
        .bind(managers)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(claims)
}

/// The time a marker of an arr holds: `{"at": ...}`. `None` while it was never written.
pub async fn marked_at(
    pool: &PgPool,
    manager: &str,
    name: &str,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    Ok(markers::get(pool, manager, name)
        .await?
        .as_ref()
        .and_then(at_of))
}

fn at_of(value: &Value) -> Option<DateTime<Utc>> {
    value
        .get("at")
        .and_then(Value::as_str)
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc))
}

/// Write a marker of an arr with a time, in the caller's transaction.
pub async fn mark(
    conn: &mut PgConnection,
    manager: &str,
    name: &str,
    at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    markers::put(conn, manager, name, &json!({ "at": at.to_rfc3339() })).await
}

/// One of these ids was stored for an arr's grab history at or before `loaded`: by a read that
/// reached grabs it had or the end of the history. A read stops at the first page that holds one.
pub async fn known_before(
    pool: &PgPool,
    manager: &str,
    ids: &[String],
    loaded: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    if ids.is_empty() {
        return Ok(false);
    }
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM download_claims
                        WHERE manager = $1 AND source = 'history' AND download_id = ANY($2)
                          AND seen_at <= $3)",
    )
    .bind(manager)
    .bind(ids)
    .bind(loaded)
    .fetch_one(pool)
    .await
}

/// Write a marker of an arr with the database's time now, in the caller's transaction.
pub async fn mark_now(
    conn: &mut PgConnection,
    manager: &str,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO markers (scope, name, value) VALUES ($1, $2, jsonb_build_object('at', now()))
         ON CONFLICT (scope, name) DO UPDATE SET value = EXCLUDED.value, updated_at = now()",
    )
    .bind(manager)
    .bind(name)
    .execute(conn)
    .await
    .map(|_| ())
}

/// A full file pass is due when none ended yet. It is due again once the last one is
/// `files_full` old.
pub fn files_full_due(
    full_at: Option<DateTime<Utc>>,
    files_full: Duration,
    now: DateTime<Utc>,
) -> bool {
    full_at.is_none_or(|at| {
        chrono::Duration::from_std(files_full).is_ok_and(|every| now - at >= every)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full file pass is due when none ended or the last one is as old as its interval.
    #[test]
    fn markers_say_what_a_pass_reads() {
        let now = Utc::now();
        let ago = |s: i64| Some(now - chrono::Duration::seconds(s));
        let full = Duration::from_secs(21_600);
        assert!(files_full_due(None, full, now));
        assert!(!files_full_due(ago(600), full, now));
        assert!(files_full_due(ago(21_600), full, now));
        assert_eq!(
            at_of(&json!({ "at": "2026-11-26T06:00:00+00:00" })).map(|t| t.to_rfc3339()),
            Some("2026-11-26T06:00:00+00:00".to_string())
        );
        assert_eq!(at_of(&json!(true)), None);
    }
}
