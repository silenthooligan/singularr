//! SQL over `downloads`.

use super::model::{Download, DownloadCounts, DownloadQuery};
use crate::core::model::Paging;
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

const SELECT: &str =
    "SELECT d.hash, d.name, d.category, d.state, d.progress, d.size_bytes, d.added_at,
        d.completed_at, d.seeding_seconds, d.save_path, d.owner, d.owner_source, d.title_id,
        t.title, d.queue_status, d.queue_state, d.queue_health, d.queue_messages, d.stuck,
        d.stuck_reason, d.orphan, d.orphan_reason, d.first_seen_at, d.updated_at,
        d.speed_bytes, d.eta_seconds, d.seeds, d.peers, d.indexer
     FROM downloads d LEFT JOIN titles t ON t.id = d.title_id";

/// Every stored download. The sync compares the client's list against this.
pub async fn all(pool: &PgPool) -> Result<Vec<Download>, sqlx::Error> {
    sqlx::query_as::<_, Download>(SELECT).fetch_all(pool).await
}

/// One download by its hash, in any case.
pub async fn one(pool: &PgPool, hash: &str) -> Result<Option<Download>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(SELECT);
    qb.push(" WHERE d.hash = ")
        .push_bind(hash.trim().to_ascii_uppercase());
    qb.build_query_as::<Download>().fetch_optional(pool).await
}

fn filters(qb: &mut QueryBuilder<Postgres>, q: &DownloadQuery) {
    qb.push(" WHERE true");
    if let Some(owner) = q.owner.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        if owner.eq_ignore_ascii_case("none") {
            qb.push(" AND d.owner IS NULL");
        } else {
            qb.push(" AND d.owner = ").push_bind(owner.to_string());
        }
    }
    if let Some(state) = q.state.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND d.state = ").push_bind(state.to_string());
    }
    if let Some(category) = q.category.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND d.category = ")
            .push_bind(category.to_string());
    }
    if let Some(stuck) = q.stuck {
        qb.push(" AND d.stuck = ").push_bind(stuck);
    }
    if let Some(orphan) = q.orphan {
        qb.push(" AND d.orphan = ").push_bind(orphan);
    }
    if let Some(active) = q.active {
        qb.push(if active {
            " AND d.progress < 1"
        } else {
            " AND d.progress >= 1"
        });
    }
    if let Some(text) = q.q.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        for word in text.split_whitespace() {
            let escaped = word
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            qb.push(" AND d.name ILIKE ")
                .push_bind(format!("%{escaped}%"));
        }
    }
}

pub async fn list(
    pool: &PgPool,
    q: &DownloadQuery,
    paging: Paging,
) -> Result<(Vec<Download>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*) FROM downloads d");
    filters(&mut count, q);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut list = QueryBuilder::<Postgres>::new(SELECT);
    filters(&mut list, q);
    let sort = q.sort.as_deref().unwrap_or("added");
    let (field, reversed) = match sort.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (sort, false),
    };
    // The column and its natural direction come from this table, never from the request.
    let (column, natural_desc) = match field {
        "name" => ("d.name", false),
        "progress" => ("d.progress", false),
        "size" => ("d.size_bytes", true),
        "state" => ("d.state", false),
        _ => ("d.added_at", true),
    };
    list.push(" ORDER BY ").push(column);
    list.push(if natural_desc != reversed {
        " DESC NULLS LAST"
    } else {
        " ASC NULLS LAST"
    });
    list.push(", d.hash LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = list.build_query_as::<Download>().fetch_all(pool).await?;
    Ok((rows, total))
}

pub async fn counts(pool: &PgPool) -> Result<DownloadCounts, sqlx::Error> {
    sqlx::query_as::<_, DownloadCounts>(
        "SELECT count(*) AS total,
                count(*) FILTER (WHERE progress < 1) AS active,
                count(*) FILTER (WHERE progress >= 1) AS complete,
                count(*) FILTER (WHERE stuck) AS stuck,
                count(*) FILTER (WHERE orphan) AS orphans,
                count(*) FILTER (WHERE owner IS NULL) AS unowned
         FROM downloads",
    )
    .fetch_one(pool)
    .await
}

/// Of these titles, the ones with a download in progress (an arr owns it and it is not
/// complete).
pub async fn downloading_titles(pool: &PgPool, title_ids: &[i64]) -> Result<Vec<i64>, sqlx::Error> {
    if title_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar::<_, i64>(
        "SELECT DISTINCT title_id FROM downloads
         WHERE title_id = ANY($1) AND progress < 1 ORDER BY title_id",
    )
    .bind(title_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// A title's downloads that are not done: still downloading, or still in a manager's queue.
pub async fn title_queue(pool: &PgPool, title_id: i64) -> Result<Vec<Download>, sqlx::Error> {
    sqlx::query_as::<_, Download>(
        "SELECT d.hash, d.name, d.category, d.state, d.progress, d.size_bytes, d.added_at,
            d.completed_at, d.seeding_seconds, d.save_path, d.owner, d.owner_source, d.title_id,
            t.title, d.queue_status, d.queue_state, d.queue_health, d.queue_messages, d.stuck,
            d.stuck_reason, d.orphan, d.orphan_reason, d.first_seen_at, d.updated_at
         FROM downloads d LEFT JOIN titles t ON t.id = d.title_id
         WHERE d.title_id = $1 AND (d.progress < 1 OR d.owner_source = 'queue')
         ORDER BY d.added_at DESC NULLS LAST, d.hash",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

/// Where a title's downloads are: any stuck, any still downloading, any complete and waiting in
/// an arr's queue (importing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleStage {
    pub title_id: i64,
    pub stuck: bool,
    pub downloading: bool,
    pub importing: bool,
}

/// The titles that have a download in motion or stuck.
pub async fn title_stages(pool: &PgPool) -> Result<Vec<TitleStage>, sqlx::Error> {
    sqlx::query_as::<_, TitleStage>(
        "SELECT title_id,
                bool_or(stuck) AS stuck,
                bool_or(progress < 1 AND NOT stuck) AS downloading,
                bool_or(progress >= 1 AND queue_state IS NOT NULL AND NOT stuck) AS importing
         FROM downloads
         WHERE title_id IS NOT NULL AND (stuck OR progress < 1 OR queue_state IS NOT NULL)
         GROUP BY title_id ORDER BY title_id",
    )
    .fetch_all(pool)
    .await
}

/// Insert or refresh one download.
pub async fn upsert(conn: &mut PgConnection, d: &Download) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO downloads (hash, name, category, state, progress, size_bytes, added_at,
                                completed_at, seeding_seconds, save_path, owner, owner_source,
                                title_id, queue_status, queue_state, queue_health, queue_messages,
                                stuck, stuck_reason, orphan, orphan_reason, speed_bytes,
                                eta_seconds, seeds, peers, indexer)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                 $19, $20, $21, $22, $23, $24, $25, $26)
         ON CONFLICT (hash) DO UPDATE SET
             speed_bytes = EXCLUDED.speed_bytes, eta_seconds = EXCLUDED.eta_seconds,
             seeds = EXCLUDED.seeds, peers = EXCLUDED.peers, indexer = EXCLUDED.indexer,
             name = EXCLUDED.name, category = EXCLUDED.category, state = EXCLUDED.state,
             progress = EXCLUDED.progress, size_bytes = EXCLUDED.size_bytes,
             added_at = EXCLUDED.added_at, completed_at = EXCLUDED.completed_at,
             seeding_seconds = EXCLUDED.seeding_seconds, save_path = EXCLUDED.save_path,
             owner = EXCLUDED.owner, owner_source = EXCLUDED.owner_source,
             title_id = EXCLUDED.title_id, queue_status = EXCLUDED.queue_status,
             queue_state = EXCLUDED.queue_state, queue_health = EXCLUDED.queue_health,
             queue_messages = EXCLUDED.queue_messages, stuck = EXCLUDED.stuck,
             stuck_reason = EXCLUDED.stuck_reason, orphan = EXCLUDED.orphan,
             orphan_reason = EXCLUDED.orphan_reason, updated_at = now()",
    )
    .bind(&d.hash)
    .bind(&d.name)
    .bind(&d.category)
    .bind(&d.state)
    .bind(d.progress)
    .bind(d.size_bytes)
    .bind(d.added_at)
    .bind(d.completed_at)
    .bind(d.seeding_seconds)
    .bind(&d.save_path)
    .bind(&d.owner)
    .bind(&d.owner_source)
    .bind(d.title_id)
    .bind(&d.queue_status)
    .bind(&d.queue_state)
    .bind(&d.queue_health)
    .bind(Json(&d.queue_messages))
    .bind(d.stuck)
    .bind(&d.stuck_reason)
    .bind(d.orphan)
    .bind(&d.orphan_reason)
    .bind(d.speed_bytes)
    .bind(d.eta_seconds)
    .bind(d.seeds)
    .bind(d.peers)
    .bind(&d.indexer)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Remove the downloads an engine outside `keep` owned. The next pass of the download client
/// lists each again with the owner the configured engines give it.
pub async fn prune_owners(conn: &mut PgConnection, keep: &[String]) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM downloads WHERE owner IS NOT NULL AND NOT (owner = ANY($1))")
        .bind(keep)
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}

pub async fn delete(conn: &mut PgConnection, hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM downloads WHERE hash = $1")
        .bind(hash)
        .execute(conn)
        .await
        .map(|_| ())
}

/// The names of the downloads the client holds, newest first: what the managers grabbed.
pub async fn download_names(pool: &PgPool, limit: i64) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT name FROM downloads ORDER BY added_at DESC NULLS LAST LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}
