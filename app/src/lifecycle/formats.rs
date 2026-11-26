//! SQL over `manager_file_formats`: what a manager said of its files' custom formats (Radarr's
//! `/moviefile`), kept between titles passes, with the marker `formats.refreshed_at`. The titles
//! job reads the book at its start ([`book`]). It writes what its read changed in the transaction
//! that ends it ([`write`]).

use super::claims;
use crate::engines::{FormatBook, FormatChanges, KnownFormats};
use serde_json::Value;
use sqlx::{PgConnection, PgPool};

/// The marker of a manager's format book: when every file was last asked about.
pub const REFRESHED_AT: &str = "formats.refreshed_at";

#[derive(sqlx::FromRow)]
struct Row {
    engine_file_id: i64,
    key: String,
    formats: Vec<String>,
    score: Option<i32>,
}

/// A manager's format book: its rows and its marker.
pub async fn book(pool: &PgPool, manager: &str) -> Result<FormatBook, sqlx::Error> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT engine_file_id, key, formats, score FROM manager_file_formats WHERE manager = $1",
    )
    .bind(manager)
    .fetch_all(pool)
    .await?;
    Ok(FormatBook {
        files: rows
            .into_iter()
            .map(|r| {
                (
                    r.engine_file_id,
                    KnownFormats {
                        key: r.key,
                        formats: r.formats,
                        score: r.score,
                    },
                )
            })
            .collect(),
        refreshed_at: claims::marked_at(pool, manager, REFRESHED_AT).await?,
    })
}

/// Write what one read changed in a manager's format book, in the caller's transaction: the
/// entries put, the entries of the files gone, the marker of a read that asked about every file.
pub async fn write(
    conn: &mut PgConnection,
    manager: &str,
    changes: &FormatChanges,
) -> Result<(), sqlx::Error> {
    if !changes.gone.is_empty() {
        sqlx::query(
            "DELETE FROM manager_file_formats WHERE manager = $1 AND engine_file_id = ANY($2)",
        )
        .bind(manager)
        .bind(&changes.gone)
        .execute(&mut *conn)
        .await?;
    }
    if !changes.put.is_empty() {
        let ids: Vec<i64> = changes.put.keys().copied().collect();
        let keys: Vec<&str> = changes.put.values().map(|k| k.key.as_str()).collect();
        // Each file's formats as a JSON array: an array of arrays cannot be unnested row by row.
        let formats: Vec<Value> = changes
            .put
            .values()
            .map(|k| Value::from(k.formats.clone()))
            .collect();
        let scores: Vec<Option<i32>> = changes.put.values().map(|k| k.score).collect();
        sqlx::query(
            "INSERT INTO manager_file_formats (manager, engine_file_id, key, formats, score)
             SELECT $1, f.id, f.key, ARRAY(SELECT jsonb_array_elements_text(f.formats)), f.score
             FROM unnest($2::bigint[], $3::text[], $4::jsonb[], $5::integer[])
                  AS f(id, key, formats, score)
             ON CONFLICT (manager, engine_file_id) DO UPDATE SET
                 key = EXCLUDED.key, formats = EXCLUDED.formats, score = EXCLUDED.score,
                 updated_at = now()",
        )
        .bind(manager)
        .bind(&ids)
        .bind(&keys)
        .bind(&formats)
        .bind(&scores)
        .execute(&mut *conn)
        .await?;
    }
    if let Some(at) = changes.refreshed_at {
        claims::mark(conn, manager, REFRESHED_AT, at).await?;
    }
    Ok(())
}

/// Remove the format rows of every manager but these: at start, with the claims
/// ([`claims::keep_managers`]).
pub async fn keep_managers(
    conn: &mut PgConnection,
    managers: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM manager_file_formats WHERE NOT (manager = ANY($1))")
        .bind(managers)
        .execute(conn)
        .await
        .map(|_| ())
}
