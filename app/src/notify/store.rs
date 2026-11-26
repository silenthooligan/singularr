//! SQL over `notification_channels` and `notification_deliveries`.
//!
//! Times that decide when a delivery is due are the caller's (`now`), never the database's,
//! so a test can say when it is.

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool};

/// A channel as stored. Its secrets are in it: it has no `Debug` that would print them.
#[derive(Clone, PartialEq, sqlx::FromRow)]
pub struct ChannelRow {
    pub id: i64,
    #[sqlx(rename = "type")]
    pub kind: String,
    pub name: String,
    pub enabled: bool,
    #[sqlx(json)]
    pub settings: Map<String, Value>,
    #[sqlx(json)]
    pub secrets: Map<String, Value>,
    pub events: Vec<String>,
    pub kinds: Vec<String>,
    /// The journal event the dispatcher has gone through for this channel.
    pub last_event: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

/// A channel shows its secrets' names, never their values.
impl std::fmt::Debug for ChannelRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelRow")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("enabled", &self.enabled)
            .field("settings", &self.settings)
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("events", &self.events)
            .field("kinds", &self.kinds)
            .field("last_event", &self.last_event)
            .finish_non_exhaustive()
    }
}

const CHANNEL: &str = "id, type, name, enabled, settings, secrets, events, kinds, last_event,
    created_at, created_by, updated_at, updated_by";

/// What a channel is written with.
pub struct ChannelWrite<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub enabled: bool,
    pub settings: &'a Map<String, Value>,
    pub secrets: &'a Map<String, Value>,
    pub events: &'a [String],
    pub kinds: &'a [String],
    /// Who writes it, as the journal names them.
    pub by: &'a str,
}

pub async fn insert_channel(
    conn: &mut PgConnection,
    c: &ChannelWrite<'_>,
) -> Result<ChannelRow, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(sqlx::AssertSqlSafe(format!(
        "INSERT INTO notification_channels
             (type, name, enabled, settings, secrets, events, kinds, created_by, updated_by)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)
         RETURNING {CHANNEL}"
    )))
    .bind(c.kind)
    .bind(c.name)
    .bind(c.enabled)
    .bind(Json(c.settings))
    .bind(Json(c.secrets))
    .bind(c.events)
    .bind(c.kinds)
    .bind(c.by)
    .fetch_one(conn)
    .await
}

/// Write a channel again. `None` when there is no such channel.
pub async fn update_channel(
    conn: &mut PgConnection,
    id: i64,
    c: &ChannelWrite<'_>,
) -> Result<Option<ChannelRow>, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(sqlx::AssertSqlSafe(format!(
        "UPDATE notification_channels
         SET type = $2, name = $3, enabled = $4, settings = $5, secrets = $6, events = $7,
             kinds = $8, updated_by = $9, updated_at = now()
         WHERE id = $1
         RETURNING {CHANNEL}"
    )))
    .bind(id)
    .bind(c.kind)
    .bind(c.name)
    .bind(c.enabled)
    .bind(Json(c.settings))
    .bind(Json(c.secrets))
    .bind(c.events)
    .bind(c.kinds)
    .bind(c.by)
    .fetch_optional(conn)
    .await
}

/// Nothing at or before `event` is sent to this channel.
pub async fn start_at(conn: &mut PgConnection, id: i64, event: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE notification_channels SET last_event = $2 WHERE id = $1")
        .bind(id)
        .bind(event)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Remove a channel with its deliveries. `None` when there is no such channel.
pub async fn delete_channel(
    conn: &mut PgConnection,
    id: i64,
) -> Result<Option<ChannelRow>, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(sqlx::AssertSqlSafe(format!(
        "DELETE FROM notification_channels WHERE id = $1 RETURNING {CHANNEL}"
    )))
    .bind(id)
    .fetch_optional(conn)
    .await
}

pub async fn channels(pool: &PgPool) -> Result<Vec<ChannelRow>, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {CHANNEL} FROM notification_channels ORDER BY id"
    )))
    .fetch_all(pool)
    .await
}

pub async fn channel(pool: &PgPool, id: i64) -> Result<Option<ChannelRow>, sqlx::Error> {
    sqlx::query_as::<_, ChannelRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {CHANNEL} FROM notification_channels WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// The dispatcher went through the journal up to `event` for these channels.
pub async fn advance(conn: &mut PgConnection, ids: &[i64], event: i64) -> Result<(), sqlx::Error> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE notification_channels SET last_event = $2
         WHERE id = ANY($1) AND last_event < $2",
    )
    .bind(ids)
    .bind(event)
    .execute(conn)
    .await
    .map(|_| ())
}

// Deliveries -------------------------------------------------------------------------------------

/// Waiting for its next attempt.
pub const PENDING: &str = "pending";
/// An attempt is under way.
pub const SENDING: &str = "sending";
pub const SENT: &str = "sent";
pub const FAILED: &str = "failed";

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct DeliveryRow {
    pub id: i64,
    pub channel_id: i64,
    /// The journal event; `None` for a test.
    pub event_id: Option<i64>,
    /// The notification event, or `test`.
    pub event: String,
    /// What the message is made from.
    pub notice: Value,
    pub state: String,
    pub attempts: i32,
    pub next_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// The destination's status, when it answered.
    pub status: Option<i32>,
    /// The destination's own words, or why nothing reached it.
    pub detail: Option<String>,
    pub test: bool,
}

const DELIVERY: &str = "id, channel_id, event_id, event, notice, state, attempts, next_at,
    created_at, finished_at, status, detail, test";

/// A message to send.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDelivery {
    pub channel_id: i64,
    pub event_id: Option<i64>,
    pub event: String,
    pub notice: Value,
}

/// Messages to send, due at `at`. One event is sent to one channel once: a delivery that is
/// there already stays as it is. Returns how many were added.
pub async fn insert_deliveries(
    conn: &mut PgConnection,
    rows: &[NewDelivery],
    at: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    if rows.is_empty() {
        return Ok(0);
    }
    let channels: Vec<i64> = rows.iter().map(|r| r.channel_id).collect();
    let events: Vec<Option<i64>> = rows.iter().map(|r| r.event_id).collect();
    let kinds: Vec<&str> = rows.iter().map(|r| r.event.as_str()).collect();
    let notices: Vec<String> = rows.iter().map(|r| r.notice.to_string()).collect();
    sqlx::query(
        "INSERT INTO notification_deliveries
             (channel_id, event_id, event, notice, state, next_at, created_at)
         SELECT t.channel_id, t.event_id, t.event, t.notice::jsonb, 'pending', $5, $5
         FROM UNNEST($1::bigint[], $2::bigint[], $3::text[], $4::text[])
              AS t(channel_id, event_id, event, notice)
         ON CONFLICT (channel_id, event_id) DO NOTHING",
    )
    .bind(channels)
    .bind(events)
    .bind(kinds)
    .bind(notices)
    .bind(at)
    .execute(conn)
    .await
    .map(|r| r.rows_affected())
}

/// A delivery that is sent now, outside the queue (a test): it starts as an attempt under way.
pub async fn insert_attempt(
    pool: &PgPool,
    channel_id: i64,
    event: &str,
    notice: &Value,
    at: DateTime<Utc>,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO notification_deliveries
             (channel_id, event_id, event, notice, state, attempts, next_at, created_at, test)
         VALUES ($1, NULL, $2, $3, 'sending', 1, $4, $4, true)
         RETURNING id",
    )
    .bind(channel_id)
    .bind(event)
    .bind(notice)
    .bind(at)
    .fetch_one(pool)
    .await
}

/// The deliveries due at `now`, the longest waiting first.
pub async fn due(
    pool: &PgPool,
    now: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<DeliveryRow>, sqlx::Error> {
    sqlx::query_as::<_, DeliveryRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {DELIVERY} FROM notification_deliveries
         WHERE state = 'pending' AND next_at <= $1
         ORDER BY next_at, id LIMIT $2"
    )))
    .bind(now)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// When the next waiting delivery is due.
pub async fn next_due(pool: &PgPool) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT min(next_at) FROM notification_deliveries WHERE state = 'pending'",
    )
    .fetch_one(pool)
    .await
}

/// Take a waiting delivery for an attempt, before anything is sent. Returns the number of the
/// attempt; `None` when it is not waiting (another round took it). While it is sending,
/// `next_at` holds when the attempt began.
pub async fn claim(pool: &PgPool, id: i64) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar::<_, i32>(
        "UPDATE notification_deliveries
         SET state = 'sending', attempts = attempts + 1, next_at = now()
         WHERE id = $1 AND state = 'pending'
         RETURNING attempts",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

/// How an attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    /// `sent`, `pending` (another attempt at `next_at`) or `failed`.
    pub state: &'static str,
    pub next_at: Option<DateTime<Utc>>,
    pub at: DateTime<Utc>,
    pub status: Option<i32>,
    pub detail: Option<String>,
}

pub async fn finish(pool: &PgPool, id: i64, ended: &Ended) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE notification_deliveries
         SET state = $2,
             next_at = COALESCE($3, next_at),
             finished_at = CASE WHEN $2 = 'pending' THEN NULL ELSE $4 END,
             status = $5,
             detail = $6
         WHERE id = $1",
    )
    .bind(id)
    .bind(ended.state)
    .bind(ended.next_at)
    .bind(ended.at)
    .bind(ended.status)
    .bind(&ended.detail)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Attempts a stop cut short: each waits again, or has failed when it had its last attempt.
pub async fn reset_interrupted(
    pool: &PgPool,
    attempts: i32,
    now: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "UPDATE notification_deliveries
         SET state = CASE WHEN attempts >= $1 OR test THEN 'failed' ELSE 'pending' END,
             finished_at = CASE WHEN attempts >= $1 OR test THEN $2 ELSE NULL END,
             next_at = $2,
             detail = COALESCE(detail, 'Interrupted')
         WHERE state = 'sending'",
    )
    .bind(attempts)
    .bind(now)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
}

/// Attempts a dropped round left sending: they began more than `older` ago. One with attempts left
/// waits again. One at its last attempt has failed. A test is failed. Returns how many.
pub async fn reset_stranded(
    pool: &PgPool,
    attempts: i32,
    older: std::time::Duration,
) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "UPDATE notification_deliveries
         SET state = CASE WHEN attempts >= $1 OR test THEN 'failed' ELSE 'pending' END,
             finished_at = CASE WHEN attempts >= $1 OR test THEN now() END,
             next_at = now(),
             detail = COALESCE(detail, 'Interrupted')
         WHERE state = 'sending' AND next_at < now() - make_interval(secs => $2::float8)",
    )
    .bind(attempts)
    .bind(older.as_secs_f64())
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
}

/// Drop what waits for a channel: it was disabled.
pub async fn drop_waiting(conn: &mut PgConnection, channel_id: i64) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM notification_deliveries WHERE channel_id = $1 AND state = 'pending'")
        .bind(channel_id)
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}

/// Deliveries, newest first: of one channel or of all, older than `before`.
pub async fn deliveries(
    pool: &PgPool,
    channel: Option<i64>,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<DeliveryRow>, sqlx::Error> {
    sqlx::query_as::<_, DeliveryRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {DELIVERY} FROM notification_deliveries
         WHERE ($1::bigint IS NULL OR channel_id = $1) AND ($2::bigint IS NULL OR id < $2)
         ORDER BY id DESC LIMIT $3"
    )))
    .bind(channel)
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Each channel's newest delivery that ended (sent or failed).
pub async fn last_ended(pool: &PgPool) -> Result<Vec<DeliveryRow>, sqlx::Error> {
    sqlx::query_as::<_, DeliveryRow>(sqlx::AssertSqlSafe(format!(
        "SELECT DISTINCT ON (channel_id) {DELIVERY} FROM notification_deliveries
         WHERE state IN ('sent', 'failed')
         ORDER BY channel_id, id DESC"
    )))
    .fetch_all(pool)
    .await
}
