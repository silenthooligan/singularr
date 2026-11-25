//! SQL over the tables `homemovies` owns: `home_movies` and `home_movie_scan`.

use super::scan::{Reading, ScannedTape};
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool};

/// One stored tape.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TapeRow {
    pub id: i64,
    pub path: String,
    pub size_bytes: i64,
    pub modified_at: Option<DateTime<Utc>>,
    pub read_title: String,
    pub read_year_from: Option<i32>,
    pub read_year_to: Option<i32>,
    pub read_uncertain: bool,
    pub read_chapters: serde_json::Value,
    pub annotation: serde_json::Value,
    pub annotated_at: Option<DateTime<Utc>>,
    pub missing_since: Option<DateTime<Utc>>,
    pub first_seen_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TapeRow {
    /// The reading stored in the row, to compare with a fresh one.
    pub fn reading(&self) -> Reading {
        Reading {
            title: self.read_title.clone(),
            year_from: self.read_year_from,
            year_to: self.read_year_to,
            uncertain: self.read_uncertain,
            chapters: serde_json::from_value(self.read_chapters.clone()).unwrap_or_default(),
        }
    }
}

const COLUMNS: &str = "SELECT id, path, size_bytes, modified_at, read_title, read_year_from,
        read_year_to, read_uncertain, read_chapters, annotation, annotated_at, missing_since,
        first_seen_at, updated_at
     FROM home_movies";

/// Every tape, there or missing. The collection is small, so the lists filter in memory.
pub async fn all(pool: &PgPool) -> Result<Vec<TapeRow>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(COLUMNS);
    qb.push(" ORDER BY path");
    qb.build_query_as::<TapeRow>().fetch_all(pool).await
}

pub async fn all_in(conn: &mut PgConnection) -> Result<Vec<TapeRow>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(COLUMNS);
    qb.push(" ORDER BY path");
    qb.build_query_as::<TapeRow>().fetch_all(conn).await
}

pub async fn get(pool: &PgPool, id: i64) -> Result<Option<TapeRow>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(COLUMNS);
    qb.push(" WHERE id = ").push_bind(id);
    qb.build_query_as::<TapeRow>().fetch_optional(pool).await
}

pub async fn get_in(conn: &mut PgConnection, id: i64) -> Result<Option<TapeRow>, sqlx::Error> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Postgres>::new(COLUMNS);
    qb.push(" WHERE id = ").push_bind(id).push(" FOR UPDATE");
    qb.build_query_as::<TapeRow>().fetch_optional(conn).await
}

pub async fn insert(
    conn: &mut PgConnection,
    t: &ScannedTape,
    r: &Reading,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO home_movies (path, size_bytes, modified_at, read_title, read_year_from,
                                 read_year_to, read_uncertain, read_chapters)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(&t.path)
    .bind(t.size_bytes)
    .bind(t.modified_at)
    .bind(&r.title)
    .bind(r.year_from)
    .bind(r.year_to)
    .bind(r.uncertain)
    .bind(Json(&r.chapters))
    .fetch_one(conn)
    .await
}

/// Write the file's facts and its reading into a tape that is there (again): the same path,
/// or the new path of a tape that was moved. The annotation stays.
pub async fn refresh(
    conn: &mut PgConnection,
    id: i64,
    t: &ScannedTape,
    r: &Reading,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE home_movies SET path = $2, size_bytes = $3, modified_at = $4, read_title = $5,
                read_year_from = $6, read_year_to = $7, read_uncertain = $8, read_chapters = $9,
                missing_since = NULL, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&t.path)
    .bind(t.size_bytes)
    .bind(t.modified_at)
    .bind(&r.title)
    .bind(r.year_from)
    .bind(r.year_to)
    .bind(r.uncertain)
    .bind(Json(&r.chapters))
    .execute(conn)
    .await
    .map(|_| ())
}

pub async fn mark_missing(conn: &mut PgConnection, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE home_movies SET missing_since = now(), updated_at = now()
         WHERE id = $1 AND missing_since IS NULL",
    )
    .bind(id)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Store the owner's annotation and mark the tape annotated.
pub async fn annotate(
    conn: &mut PgConnection,
    id: i64,
    annotation: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE home_movies SET annotation = $2, annotated_at = now(), updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(annotation)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Tapes that are there and were never annotated.
pub async fn inbox_count(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM home_movies WHERE annotated_at IS NULL AND missing_since IS NULL",
    )
    .fetch_one(pool)
    .await
}

// The scan's state ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ScanRow {
    pub root: String,
    pub ok: bool,
    pub last_scan_at: DateTime<Utc>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub took_ms: Option<i64>,
    pub error: Option<String>,
    pub counts: serde_json::Value,
}

pub async fn record_scan(
    pool: &PgPool,
    root: &str,
    ok: bool,
    took_ms: i64,
    error: Option<&str>,
    counts: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO home_movie_scan (id, root, ok, last_scan_at, last_ok_at, took_ms, error, counts)
         VALUES (1, $1, $2, now(), CASE WHEN $2 THEN now() END, $3, $4, $5)
         ON CONFLICT (id) DO UPDATE SET
             root = EXCLUDED.root, ok = EXCLUDED.ok, last_scan_at = now(),
             last_ok_at = COALESCE(EXCLUDED.last_ok_at, home_movie_scan.last_ok_at),
             took_ms = EXCLUDED.took_ms, error = EXCLUDED.error,
             counts = CASE WHEN EXCLUDED.ok THEN EXCLUDED.counts ELSE home_movie_scan.counts END",
    )
    .bind(root)
    .bind(ok)
    .bind(took_ms)
    .bind(error)
    .bind(counts)
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn scan_state(pool: &PgPool) -> Result<Option<ScanRow>, sqlx::Error> {
    sqlx::query_as::<_, ScanRow>(
        "SELECT root, ok, last_scan_at, last_ok_at, took_ms, error, counts
         FROM home_movie_scan WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
}
