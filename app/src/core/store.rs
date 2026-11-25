//! SQL over the tables `core` owns: `events` and `api_keys`, plus the two health questions.

use super::model::{ApiKeyInfo, Event, EventQuery, Paging, Scope};
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

/// Serializes every transaction that appends to the journal, so ids are committed in order.
const JOURNAL_LOCK: i64 = 0x4d51_4a52_4e4c;

pub async fn ping(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1").execute(pool).await.map(|_| ())
}

/// Versions of the migrations the database has applied, in order.
pub async fn applied_migrations(pool: &PgPool) -> Result<Vec<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT version FROM _sqlx_migrations WHERE success ORDER BY version",
    )
    .fetch_all(pool)
    .await
}

/// An event that is not stored yet.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub kind: String,
    pub subject_type: String,
    pub subject_id: String,
    pub actor: String,
    pub cause: Option<i64>,
    pub body: serde_json::Value,
}

const EVENT_COLUMNS: &str = "id, at, kind, subject_type, subject_id, actor, cause, body";

/// Append events, in order, inside the caller's transaction. Returns them with their ids.
pub async fn append_events(
    conn: &mut PgConnection,
    events: &[NewEvent],
) -> Result<Vec<Event>, sqlx::Error> {
    if events.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(JOURNAL_LOCK)
        .execute(&mut *conn)
        .await?;
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    let subject_types: Vec<&str> = events.iter().map(|e| e.subject_type.as_str()).collect();
    let subject_ids: Vec<&str> = events.iter().map(|e| e.subject_id.as_str()).collect();
    let actors: Vec<&str> = events.iter().map(|e| e.actor.as_str()).collect();
    let causes: Vec<Option<i64>> = events.iter().map(|e| e.cause).collect();
    let bodies: Vec<String> = events.iter().map(|e| e.body.to_string()).collect();
    sqlx::query_as::<_, Event>(
        "INSERT INTO events (kind, subject_type, subject_id, actor, cause, body)
         SELECT t.kind, t.subject_type, t.subject_id, t.actor, t.cause, t.body::jsonb
         FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[], $5::bigint[], $6::text[])
              WITH ORDINALITY AS t(kind, subject_type, subject_id, actor, cause, body, ord)
         ORDER BY t.ord
         RETURNING id, at, kind, subject_type, subject_id, actor, cause, body",
    )
    .bind(kinds)
    .bind(subject_types)
    .bind(subject_ids)
    .bind(actors)
    .bind(causes)
    .bind(bodies)
    .fetch_all(&mut *conn)
    .await
    .map(|mut rows| {
        rows.sort_by_key(|e| e.id);
        rows
    })
}

fn event_filters(qb: &mut QueryBuilder<Postgres>, q: &EventQuery) {
    qb.push(" WHERE true");
    if let Some(kind) = q.kind.as_deref().filter(|k| !k.trim().is_empty()) {
        let kinds: Vec<String> = kind
            .split(',')
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
            .collect();
        qb.push(" AND kind = ANY(").push_bind(kinds).push(")");
    }
    if let Some(v) = q.subject_type.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND subject_type = ").push_bind(v.to_string());
    }
    if let Some(v) = q.subject_id.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND subject_id = ").push_bind(v.to_string());
    }
    if let Some(v) = q.actor.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND actor = ").push_bind(v.to_string());
    }
    let mut title = q.title;
    if let Some((kind, id)) = q.subject.as_deref().and_then(|s| s.split_once(':')) {
        match (kind.trim(), id.trim().parse::<i64>()) {
            // A title's history covers its copies and downloads too.
            ("title", Ok(id)) => title = Some(id),
            (kind, _) => {
                qb.push(" AND subject_type = ")
                    .push_bind(kind.to_string())
                    .push(" AND subject_id = ")
                    .push_bind(id.trim().to_string());
            }
        }
    }
    if let Some(title) = title {
        qb.push(" AND ((subject_type = 'title' AND subject_id = ")
            .push_bind(title.to_string())
            .push(") OR body ->> 'title_id' = ")
            .push_bind(title.to_string())
            .push(")");
    }
    if let Some(after) = q.after {
        qb.push(" AND id > ").push_bind(after);
    }
    if let Some(before) = q.before {
        qb.push(" AND id < ").push_bind(before);
    }
}

pub async fn list_events(
    pool: &PgPool,
    q: &EventQuery,
    paging: Paging,
) -> Result<(Vec<Event>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*) FROM events");
    event_filters(&mut count, q);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut list = QueryBuilder::<Postgres>::new(format!("SELECT {EVENT_COLUMNS} FROM events"));
    event_filters(&mut list, q);
    let ascending = q
        .order
        .as_deref()
        .is_some_and(|o| o.eq_ignore_ascii_case("asc"));
    list.push(if ascending {
        " ORDER BY id ASC"
    } else {
        " ORDER BY id DESC"
    });
    list.push(" LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = list.build_query_as::<Event>().fetch_all(pool).await?;
    Ok((rows, total))
}

/// Events after an id, oldest first. The stream replays from here.
pub async fn events_after(
    pool: &PgPool,
    after: i64,
    limit: i64,
) -> Result<Vec<Event>, sqlx::Error> {
    sqlx::query_as::<_, Event>(
        "SELECT id, at, kind, subject_type, subject_id, actor, cause, body
         FROM events WHERE id > $1 ORDER BY id ASC LIMIT $2",
    )
    .bind(after)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The Journal page's reading of the journal: newest first, below an id, of these kinds and
/// actors (none: all), with these words in the kind, the subject, the actor or the body.
pub async fn find_events(
    pool: &PgPool,
    kinds: &[String],
    actors: &[String],
    words: Option<&str>,
    before: Option<i64>,
    limit: i64,
) -> Result<Vec<Event>, sqlx::Error> {
    let mut qb =
        QueryBuilder::<Postgres>::new(format!("SELECT {EVENT_COLUMNS} FROM events WHERE true"));
    if !kinds.is_empty() {
        qb.push(" AND kind = ANY(")
            .push_bind(kinds.to_vec())
            .push(")");
    }
    if !actors.is_empty() {
        qb.push(" AND actor = ANY(")
            .push_bind(actors.to_vec())
            .push(")");
    }
    if let Some(words) = words.map(str::trim).filter(|w| !w.is_empty()) {
        let pattern = format!(
            "%{}%",
            words
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        qb.push(" AND (kind ILIKE ")
            .push_bind(pattern.clone())
            .push(" OR subject_id ILIKE ")
            .push_bind(pattern.clone())
            .push(" OR actor ILIKE ")
            .push_bind(pattern.clone())
            .push(" OR body::text ILIKE ")
            .push_bind(pattern)
            .push(")");
    }
    if let Some(before) = before {
        qb.push(" AND id < ").push_bind(before);
    }
    qb.push(" ORDER BY id DESC LIMIT ").push_bind(limit);
    qb.build_query_as::<Event>().fetch_all(pool).await
}

/// The newest events about these subjects of one type, at most `limit`, oldest first.
pub async fn subject_events(
    pool: &PgPool,
    subject_type: &str,
    subject_ids: &[String],
    limit: i64,
) -> Result<Vec<Event>, sqlx::Error> {
    if subject_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, Event>(
        "SELECT * FROM (
             SELECT id, at, kind, subject_type, subject_id, actor, cause, body
             FROM events WHERE subject_type = $1 AND subject_id = ANY($2)
             ORDER BY id DESC LIMIT $3
         ) newest ORDER BY id ASC",
    )
    .bind(subject_type)
    .bind(subject_ids.to_vec())
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn latest_event_id(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT COALESCE(max(id), 0) FROM events")
        .fetch_one(pool)
        .await
}

pub async fn count_events(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM events")
        .fetch_one(pool)
        .await
}

// API keys ---------------------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ApiKeyRow {
    pub id: i64,
    pub name: String,
    pub prefix: String,
    pub sha256: String,
    pub scopes: Vec<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl ApiKeyRow {
    pub fn scopes(&self) -> Vec<Scope> {
        self.scopes.iter().filter_map(|s| Scope::parse(s)).collect()
    }

    pub fn info(&self) -> ApiKeyInfo {
        ApiKeyInfo {
            id: self.id,
            name: self.name.clone(),
            prefix: self.prefix.clone(),
            scopes: self.scopes(),
            created_by: self.created_by.clone(),
            created_at: self.created_at,
            last_used: self.last_used,
            revoked_at: self.revoked_at,
        }
    }
}

const KEY_COLUMNS: &str =
    "id, name, prefix, sha256, scopes, created_by, created_at, last_used, revoked_at";

pub async fn insert_api_key(
    conn: &mut PgConnection,
    name: &str,
    prefix: &str,
    sha256: &str,
    scopes: &[Scope],
    created_by: &str,
) -> Result<ApiKeyRow, sqlx::Error> {
    let scopes: Vec<String> = scopes.iter().map(|s| s.as_str().to_string()).collect();
    sqlx::query_as::<_, ApiKeyRow>(
        "INSERT INTO api_keys (name, prefix, sha256, scopes, created_by)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id, name, prefix, sha256, scopes, created_by, created_at, last_used, revoked_at",
    )
    .bind(name)
    .bind(prefix)
    .bind(sha256)
    .bind(scopes)
    .bind(created_by)
    .fetch_one(conn)
    .await
}

pub async fn list_api_keys(pool: &PgPool) -> Result<Vec<ApiKeyRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {KEY_COLUMNS} FROM api_keys ORDER BY revoked_at IS NOT NULL, created_at DESC"
    )))
    .fetch_all(pool)
    .await
}

/// The key with this prefix, revoked or not. The caller compares the hash.
pub async fn api_key_by_prefix(
    pool: &PgPool,
    prefix: &str,
) -> Result<Option<ApiKeyRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {KEY_COLUMNS} FROM api_keys WHERE prefix = $1"
    )))
    .bind(prefix)
    .fetch_optional(pool)
    .await
}

/// Mark a key revoked. `None` when there is no such key or it was revoked before.
pub async fn revoke_api_key(
    conn: &mut PgConnection,
    id: i64,
) -> Result<Option<ApiKeyRow>, sqlx::Error> {
    sqlx::query_as::<_, ApiKeyRow>(
        "UPDATE api_keys SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL
         RETURNING id, name, prefix, sha256, scopes, created_by, created_at, last_used, revoked_at",
    )
    .bind(id)
    .fetch_optional(conn)
    .await
}

/// Record that a key was used, at most once a minute per key.
pub async fn touch_api_key(pool: &PgPool, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE api_keys SET last_used = now()
         WHERE id = $1 AND (last_used IS NULL OR last_used < now() - interval '1 minute')",
    )
    .bind(id)
    .execute(pool)
    .await
    .map(|_| ())
}
