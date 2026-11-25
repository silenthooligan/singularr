//! SQL over `downloads`.
//!
//! A row is set aside while the configuration does not list its client ([`set_aside`]). Every
//! read here leaves such a row out. The reconcile alone reads them, to hand a renamed client
//! its rows back ([`set_aside_of`]).

use super::model::{Download, DownloadCounts, DownloadQuery};
use crate::core::model::Paging;
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

/// A download in motion, in SQL over `d`: a torrent not all there yet, a usenet download its
/// client still works on (downloading, verifying, repairing, unpacking). The rule of
/// Activity's stages (`api::pages::labels::queue_stage`).
macro_rules! in_motion {
    () => {
        "(CASE WHEN d.protocol = 'usenet' \
         THEN coalesce(d.status NOT IN ('completed', 'failed'), true) \
         ELSE d.progress < 1 END)"
    };
}

/// A download that is complete, in SQL over `d`: a torrent all there, a usenet download its
/// client completed (`lifecycle::downloads::complete`).
macro_rules! complete {
    () => {
        "(CASE WHEN d.protocol = 'usenet' THEN coalesce(d.status = 'completed', false) \
         ELSE d.progress >= 1 END)"
    };
}

/// Days the rows of a client the configuration stopped listing are kept set aside.
pub const SET_ASIDE_DAYS: i32 = 30;

const SELECT: &str =
    "SELECT d.hash, d.client, d.protocol, d.item_id, d.name, d.category, d.state, d.status,
        d.progress, d.size_bytes, d.added_at,
        d.completed_at, d.seeding_seconds, d.save_path, d.owner, d.owner_source, d.title_id,
        t.title, d.queue_status, d.queue_state, d.queue_health, d.queue_messages, d.stuck,
        d.stuck_reason, d.orphan, d.orphan_reason, d.first_seen_at, d.updated_at,
        d.speed_bytes, d.eta_seconds, d.seeds, d.peers, d.indexer
     FROM downloads d LEFT JOIN titles t ON t.id = d.title_id";

/// Every stored download, of every client.
pub async fn all(pool: &PgPool) -> Result<Vec<Download>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(SELECT);
    qb.push(" WHERE d.absent_since IS NULL");
    qb.build_query_as::<Download>().fetch_all(pool).await
}

/// Every stored download of one client. Its pass compares the client's list against this.
pub async fn of_client(pool: &PgPool, client: &str) -> Result<Vec<Download>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(SELECT);
    qb.push(" WHERE d.absent_since IS NULL AND d.client = ")
        .push_bind(client.to_string());
    qb.build_query_as::<Download>().fetch_all(pool).await
}

/// The rows set aside under these download ids, one per id: the one a manager owns first, then
/// the one written last.
pub async fn set_aside_of(pool: &PgPool, hashes: &[String]) -> Result<Vec<Download>, sqlx::Error> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::<Postgres>::new(SELECT);
    qb.push(" WHERE d.absent_since IS NOT NULL AND d.hash = ANY(")
        .push_bind(hashes.to_vec())
        .push(") ORDER BY d.hash, d.owner IS NULL, d.updated_at DESC, d.client");
    let mut rows = qb.build_query_as::<Download>().fetch_all(pool).await?;
    rows.dedup_by(|later, first| later.hash == first.hash);
    Ok(rows)
}

/// One download by the id the managers know it by, in any case. Two clients can hold the same
/// torrent: the one a manager owns is taken first.
pub async fn one(pool: &PgPool, hash: &str) -> Result<Option<Download>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(SELECT);
    qb.push(" WHERE d.absent_since IS NULL AND d.hash = ")
        .push_bind(hash.trim().to_ascii_uppercase())
        .push(" ORDER BY d.owner IS NULL, d.updated_at DESC, d.client LIMIT 1");
    qb.build_query_as::<Download>().fetch_optional(pool).await
}

fn filters(qb: &mut QueryBuilder<Postgres>, q: &DownloadQuery) {
    qb.push(" WHERE d.absent_since IS NULL");
    if let Some(owner) = q.owner.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        if owner.eq_ignore_ascii_case("none") {
            qb.push(" AND d.owner IS NULL");
        } else {
            qb.push(" AND d.owner = ").push_bind(owner.to_string());
        }
    }
    if let Some(client) = q.client.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        qb.push(" AND d.client = ").push_bind(client.to_string());
    }
    if let Some(protocol) = q
        .protocol
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        qb.push(" AND d.protocol = ")
            .push_bind(protocol.to_ascii_lowercase());
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
            concat!(" AND ", in_motion!())
        } else {
            concat!(" AND NOT ", in_motion!())
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
    list.push(", d.hash, d.client LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = list.build_query_as::<Download>().fetch_all(pool).await?;
    Ok((rows, total))
}

pub async fn counts(pool: &PgPool) -> Result<DownloadCounts, sqlx::Error> {
    sqlx::query_as::<_, DownloadCounts>(concat!(
        "SELECT count(*) AS total,
                count(*) FILTER (WHERE ",
        in_motion!(),
        ") AS active,
                count(*) FILTER (WHERE NOT ",
        in_motion!(),
        ") AS complete,
                count(*) FILTER (WHERE d.stuck) AS stuck,
                count(*) FILTER (WHERE d.orphan) AS orphans,
                count(*) FILTER (WHERE d.owner IS NULL) AS unowned
         FROM downloads d WHERE d.absent_since IS NULL",
    ))
    .fetch_one(pool)
    .await
}

/// Of these titles, the ones with a download in progress (an arr owns it and it is not
/// complete).
pub async fn downloading_titles(pool: &PgPool, title_ids: &[i64]) -> Result<Vec<i64>, sqlx::Error> {
    if title_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar::<_, i64>(concat!(
        "SELECT DISTINCT d.title_id FROM downloads d
         WHERE d.absent_since IS NULL AND d.title_id = ANY($1) AND ",
        in_motion!(),
        " ORDER BY d.title_id",
    ))
    .bind(title_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// A title's downloads that are not done: still downloading, or still in a manager's queue.
pub async fn title_queue(pool: &PgPool, title_id: i64) -> Result<Vec<Download>, sqlx::Error> {
    sqlx::query_as::<_, Download>(concat!(
        "SELECT d.hash, d.client, d.protocol, d.item_id, d.name, d.category, d.state, d.status,
            d.progress, d.size_bytes, d.added_at,
            d.completed_at, d.seeding_seconds, d.save_path, d.owner, d.owner_source, d.title_id,
            t.title, d.queue_status, d.queue_state, d.queue_health, d.queue_messages, d.stuck,
            d.stuck_reason, d.orphan, d.orphan_reason, d.first_seen_at, d.updated_at
         FROM downloads d LEFT JOIN titles t ON t.id = d.title_id
         WHERE d.absent_since IS NULL AND d.title_id = $1 AND (",
        in_motion!(),
        " OR d.owner_source = 'queue')
         ORDER BY d.added_at DESC NULLS LAST, d.hash, d.client",
    ))
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

/// The titles that have a download in motion or stuck. A usenet download that failed is
/// neither downloading nor importing.
pub async fn title_stages(pool: &PgPool) -> Result<Vec<TitleStage>, sqlx::Error> {
    sqlx::query_as::<_, TitleStage>(concat!(
        "SELECT title_id,
                bool_or(stuck) AS stuck,
                bool_or(moving AND NOT stuck) AS downloading,
                bool_or(done AND queue_state IS NOT NULL AND NOT stuck) AS importing
         FROM (SELECT d.title_id, d.stuck, d.queue_state, ",
        in_motion!(),
        " AS moving, ",
        complete!(),
        " AS done
               FROM downloads d
               WHERE d.absent_since IS NULL AND d.title_id IS NOT NULL) m
         WHERE stuck OR moving OR queue_state IS NOT NULL
         GROUP BY title_id ORDER BY title_id",
    ))
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
                                eta_seconds, seeds, peers, indexer, client, protocol, item_id,
                                status, first_seen_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                 $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31)
         ON CONFLICT (client, hash) DO UPDATE SET
             absent_since = NULL,
             protocol = EXCLUDED.protocol, item_id = EXCLUDED.item_id, status = EXCLUDED.status,
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
    .bind(&d.client)
    .bind(&d.protocol)
    .bind(&d.item_id)
    .bind(&d.status)
    .bind(d.first_seen_at)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Remove the downloads an engine outside `keep` owned. The next pass of their download client
/// lists each again with the owner the configured engines give it.
pub async fn prune_owners(conn: &mut PgConnection, keep: &[String]) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM downloads WHERE owner IS NOT NULL AND NOT (owner = ANY($1))")
        .bind(keep)
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}

/// What [`set_aside`] did: the rows it set aside now, and the rows set aside longer than
/// [`SET_ASIDE_DAYS`] that it removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SetAside {
    pub set_aside: u64,
    pub expired: u64,
}

/// Set aside the rows of every download client outside `keep`. Bring back the rows of a client
/// it lists again. A client the configuration stops listing (a variable lost, an id
/// renamed) keeps its rows for [`SET_ASIDE_DAYS`] days, left out of every read. Its return
/// finds them as they were. A client under a new id takes over the rows of the downloads it
/// lists ([`super::downloads::reconcile`]). Rows set aside longer are removed.
pub async fn set_aside(conn: &mut PgConnection, keep: &[String]) -> Result<SetAside, sqlx::Error> {
    sqlx::query(
        "UPDATE downloads SET absent_since = NULL
         WHERE absent_since IS NOT NULL AND client = ANY($1)",
    )
    .bind(keep)
    .execute(&mut *conn)
    .await?;
    let set_aside = sqlx::query(
        "UPDATE downloads SET absent_since = now()
         WHERE absent_since IS NULL AND NOT (client = ANY($1))",
    )
    .bind(keep)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let expired =
        sqlx::query("DELETE FROM downloads WHERE absent_since < now() - make_interval(days => $1)")
            .bind(SET_ASIDE_DAYS)
            .execute(&mut *conn)
            .await?
            .rows_affected();
    Ok(SetAside { set_aside, expired })
}

pub async fn delete(conn: &mut PgConnection, client: &str, hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM downloads WHERE client = $1 AND hash = $2")
        .bind(client)
        .bind(hash)
        .execute(conn)
        .await
        .map(|_| ())
}

/// The names of the downloads the client holds, newest first: what the managers grabbed.
pub async fn download_names(pool: &PgPool, limit: i64) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT name FROM downloads WHERE absent_since IS NULL
         ORDER BY added_at DESC NULLS LAST LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}
