//! SQL over `acts`: an act and its send's job in the caller's transaction, the compare and set
//! of a send, the moves of its state, the jobs of its sends and its reads. The acts that need the
//! owner's look.

use super::{Act, BY_HAND, CONFIRM_KIND, HOLDING, NewAct, QUEUE, SEND_ATTEMPTS, SEND_KIND};
use crate::runtime::store::{NewJob, queue_job};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgExecutor};
use std::time::Duration;

/// The columns of an act, in the order of [`Act`].
const COLUMNS: &str = "id, integration, action, idempotency_key, open_key, request_hash, request,
     subject_type, subject_id, state, attempts, result, error, error_class, actor, caused_by,
     created_at, sent_at, settled_at";

/// Write a new act. `None` when its key is taken. `None` too when another act holds its open key.
pub async fn insert(
    conn: &mut PgConnection,
    a: &NewAct,
    hash: &str,
) -> Result<Option<Act>, sqlx::Error> {
    let (subject_type, subject_id) = match &a.subject {
        Some((kind, id)) => (Some(kind.as_str()), Some(id.as_str())),
        None => (None, None),
    };
    sqlx::query_as::<_, Act>(sqlx::AssertSqlSafe(format!(
        "INSERT INTO acts (integration, action, idempotency_key, open_key, request_hash, request,
                           subject_type, subject_id, actor, caused_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         ON CONFLICT DO NOTHING
         RETURNING {COLUMNS}"
    )))
    .bind(&a.integration)
    .bind(&a.action)
    .bind(&a.key)
    .bind(&a.open_key)
    .bind(hash)
    .bind(&a.request)
    .bind(subject_type)
    .bind(subject_id)
    .bind(&a.actor)
    .bind(a.caused_by)
    .fetch_optional(conn)
    .await
}

/// The act of a key.
pub async fn by_key(conn: &mut PgConnection, key: &str) -> Result<Option<Act>, sqlx::Error> {
    sqlx::query_as::<_, Act>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM acts WHERE idempotency_key = $1"
    )))
    .bind(key)
    .fetch_optional(conn)
    .await
}

/// The act that holds an open key ([`HOLDING`]).
pub async fn open_by_key(
    conn: &mut PgConnection,
    open_key: &str,
) -> Result<Option<Act>, sqlx::Error> {
    sqlx::query_as::<_, Act>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM acts WHERE open_key = $1 AND state = ANY($2)"
    )))
    .bind(open_key)
    .bind(HOLDING.to_vec())
    .fetch_optional(conn)
    .await
}

/// One act.
pub async fn get<'c, E: PgExecutor<'c>>(conn: E, id: i64) -> Result<Option<Act>, sqlx::Error> {
    sqlx::query_as::<_, Act>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM acts WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// The compare and set of a send: a pending act becomes `sending`, its send counted and its time
/// noted. `None` when it is not pending: another worker took it or it was cancelled.
pub async fn take<'c, E: PgExecutor<'c>>(conn: E, id: i64) -> Result<Option<Act>, sqlx::Error> {
    sqlx::query_as::<_, Act>(sqlx::AssertSqlSafe(format!(
        "UPDATE acts SET state = 'sending', attempts = attempts + 1, sent_at = now(),
                         updated_at = now()
         WHERE id = $1 AND state = 'pending'
         RETURNING {COLUMNS}"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// What a move of an act's state writes besides the state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Move<'a> {
    /// The act's result after the move. `None` keeps the one it has.
    pub result: Option<&'a Value>,
    pub error: Option<&'a str>,
    pub class: Option<&'a str>,
}

/// Move an act from one of `from` to `to`: its error and class as given, its result when one is
/// given. A confirmed, failed or cancelled act is settled now. `None` when the act is in none of
/// `from`.
pub async fn moved(
    conn: &mut PgConnection,
    id: i64,
    from: &[&str],
    to: &str,
    m: Move<'_>,
) -> Result<Option<Act>, sqlx::Error> {
    sqlx::query_as::<_, Act>(sqlx::AssertSqlSafe(format!(
        "UPDATE acts
         SET state = $3, result = COALESCE($4, result), error = $5, error_class = $6,
             settled_at = CASE WHEN $3 IN ('confirmed', 'failed', 'cancelled') THEN now() END,
             updated_at = now()
         WHERE id = $1 AND state = ANY($2)
         RETURNING {COLUMNS}"
    )))
    .bind(id)
    .bind(from.to_vec())
    .bind(to)
    .bind(m.result)
    .bind(m.error)
    .bind(m.class)
    .fetch_optional(conn)
    .await
}

/// Queue the job of an act's next send: its `n`th.
pub async fn queue_send(conn: &mut PgConnection, act: &Act, n: i32) -> Result<(), sqlx::Error> {
    queue_job(
        conn,
        NewJob {
            kind: SEND_KIND,
            queue: QUEUE,
            integration: Some(&act.integration),
            payload: json!({ "act": act.id }),
            key: Some(format!("act:{}:{n}", act.id)),
            after: Duration::ZERO,
            max_attempts: SEND_ATTEMPTS,
        },
    )
    .await
    .map(|_| ())
}

/// Queue a read of an act: the `read`th after its `send`th send, `after` from now.
pub async fn queue_read(
    conn: &mut PgConnection,
    act: &Act,
    send: i32,
    read: u32,
    after: Duration,
) -> Result<(), sqlx::Error> {
    queue_job(
        conn,
        NewJob {
            kind: CONFIRM_KIND,
            queue: QUEUE,
            integration: Some(&act.integration),
            payload: json!({ "act": act.id, "send": send, "read": read }),
            key: Some(format!("read:{}:{send}:{read}", act.id)),
            after,
            max_attempts: SEND_ATTEMPTS,
        },
    )
    .await
    .map(|_| ())
}

/// The acts that need the owner's look, the newest first, `limit` at most. Every unknown act needs
/// one. So does an act a person asked for (`user:`) that failed after its route answered: later
/// than `answered_within` after it was asked, settled since `since`. An act the owner marked not
/// sent needs none.
pub async fn to_look_at<'c, E: PgExecutor<'c>>(
    conn: E,
    answered_within: Duration,
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Act>, sqlx::Error> {
    sqlx::query_as::<_, Act>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM acts
         WHERE state = 'unknown'
            OR (state = 'failed' AND actor LIKE 'user:%' AND error_class IS DISTINCT FROM $4
                AND settled_at >= $1
                AND settled_at > created_at + make_interval(secs => $2::float8))
         ORDER BY COALESCE(settled_at, sent_at, created_at) DESC, id DESC
         LIMIT $3"
    )))
    .bind(since)
    .bind(answered_within.as_secs_f64())
    .bind(limit)
    .bind(BY_HAND)
    .fetch_all(conn)
    .await
}

/// Cancel the queued send of an act.
pub async fn cancel_sends(conn: &mut PgConnection, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE jobs SET state = 'cancelled', finished_at = now(),
                         last_error = 'its act was cancelled'
         WHERE kind = $1 AND state = 'queued' AND (payload->>'act')::bigint = $2",
    )
    .bind(SEND_KIND)
    .bind(id)
    .execute(conn)
    .await
    .map(|_| ())
}
