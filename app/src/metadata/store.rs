//! SQL over `metadata_cache`: TMDB's answers, by the path they were asked with.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// One stored answer. `body` is `None` when TMDB had no such record.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Cached {
    pub fetched_at: DateTime<Utc>,
    pub body: Option<serde_json::Value>,
}

pub async fn get(pool: &PgPool, key: &str) -> Result<Option<Cached>, sqlx::Error> {
    sqlx::query_as::<_, Cached>("SELECT fetched_at, body FROM metadata_cache WHERE key = $1")
        .bind(key)
        .fetch_optional(pool)
        .await
}

pub async fn put(
    pool: &PgPool,
    key: &str,
    body: Option<&serde_json::Value>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO metadata_cache (key, fetched_at, body) VALUES ($1, now(), $2)
         ON CONFLICT (key) DO UPDATE SET fetched_at = now(), body = EXCLUDED.body",
    )
    .bind(key)
    .bind(body)
    .execute(pool)
    .await
    .map(|_| ())
}

/// Stored answers for these paths, fresh or not.
pub async fn get_many(
    pool: &PgPool,
    keys: &[String],
) -> Result<Vec<(String, Cached)>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, (String, DateTime<Utc>, Option<serde_json::Value>)>(
        "SELECT key, fetched_at, body FROM metadata_cache WHERE key = ANY($1)",
    )
    .bind(keys.to_vec())
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(key, fetched_at, body)| (key, Cached { fetched_at, body }))
        .collect())
}

/// The poster path of each of these stored answers that has one (a collection's, a title's).
pub async fn posters_of(
    pool: &PgPool,
    keys: &[String],
) -> Result<Vec<(String, String)>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, (String, String)>(
        "SELECT key, body ->> 'poster_path' FROM metadata_cache
         WHERE key = ANY($1) AND jsonb_typeof(body -> 'poster_path') = 'string'",
    )
    .bind(keys.to_vec())
    .fetch_all(pool)
    .await
}

/// The paths among these with no stored answer, or one older than `ttl_days`, in the order
/// given.
pub async fn stale_of(
    pool: &PgPool,
    keys: &[String],
    ttl_days: i64,
) -> Result<Vec<String>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let fresh: Vec<String> = sqlx::query_scalar(
        "SELECT key FROM metadata_cache
         WHERE key = ANY($1) AND fetched_at > now() - make_interval(days => $2::int)",
    )
    .bind(keys.to_vec())
    .bind(i32::try_from(ttl_days).unwrap_or(i32::MAX))
    .fetch_all(pool)
    .await?;
    let fresh: std::collections::HashSet<String> = fresh.into_iter().collect();
    Ok(keys
        .iter()
        .filter(|k| !fresh.contains(*k))
        .cloned()
        .collect())
}

/// The studios and networks of the stored title answers, by name in lower case: TMDB's id,
/// its name and its logo. `networks` asks the show answers' networks, anything else the
/// movie answers' production companies.
pub async fn companies_named(
    pool: &PgPool,
    field: &str,
    names: &[String],
) -> Result<Vec<(i64, String, Option<String>)>, sqlx::Error> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    let lower: Vec<String> = names.iter().map(|n| n.to_lowercase()).collect();
    let query = if field == "networks" {
        sqlx::query_as::<_, (i64, String, Option<String>)>(
            "SELECT DISTINCT ON (lower(c ->> 'name')) (c ->> 'id')::bigint, c ->> 'name', c ->> 'logo_path'
             FROM metadata_cache,
                  jsonb_array_elements(CASE WHEN jsonb_typeof(body -> 'networks') = 'array'
                                            THEN body -> 'networks' ELSE '[]'::jsonb END) AS c
             WHERE key LIKE '/tv/%' AND lower(c ->> 'name') = ANY($1)
               AND jsonb_typeof(c -> 'id') = 'number'
             ORDER BY lower(c ->> 'name'), (c ->> 'logo_path') IS NULL, (c ->> 'id')::bigint",
        )
    } else {
        sqlx::query_as::<_, (i64, String, Option<String>)>(
            "SELECT DISTINCT ON (lower(c ->> 'name')) (c ->> 'id')::bigint, c ->> 'name', c ->> 'logo_path'
             FROM metadata_cache,
                  jsonb_array_elements(CASE WHEN jsonb_typeof(body -> 'production_companies') = 'array'
                                            THEN body -> 'production_companies' ELSE '[]'::jsonb END) AS c
             WHERE key LIKE '/movie/%' AND lower(c ->> 'name') = ANY($1)
               AND jsonb_typeof(c -> 'id') = 'number'
             ORDER BY lower(c ->> 'name'), (c ->> 'logo_path') IS NULL, (c ->> 'id')::bigint",
        )
    };
    query.bind(lower).fetch_all(pool).await
}

/// Answers kept, for the metrics and the doctor.
pub async fn count(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM metadata_cache")
        .fetch_one(pool)
        .await
}
