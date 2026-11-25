//! SQL over the tables `requests` owns: `requests`, `request_media`, `seerr_users`,
//! `discover_items`, `face_settings`, `request_intents`, `request_sessions` and
//! `request_session_ends`, and the `face_*` views Singularr Requests reads.

use super::diff::{RequestFacts, StoredRequest};
use super::model::{
    FaceAccount, FaceDiscoverItem, FaceMyRequest, FaceRules, FaceSession, RequestCounts,
    RequestIntent,
};
use crate::core::model::Paging;
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

/// A stored request with the title Seerr gave for it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RequestRow {
    pub id: i64,
    #[sqlx(flatten)]
    pub facts: RequestFacts,
    pub first_seen_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

const ROW_SELECT: &str = "SELECT r.id, r.site, r.seerr_id, r.kind, r.tmdb_id, r.tvdb_id, r.is_4k,
        r.request_status, r.media_status, r.status, r.requested_by, r.requested_by_id,
        r.requested_at, r.seerr_updated_at, r.seasons, r.title_id, r.first_seen_at, r.updated_at,
        m.title, m.year, m.poster_path, m.backdrop_path
     FROM requests r LEFT JOIN request_media m ON m.kind = r.kind AND m.tmdb_id = r.tmdb_id";

const STORED_SELECT: &str =
    "SELECT r.id, r.site, r.seerr_id, r.kind, r.tmdb_id, r.tvdb_id, r.is_4k,
        r.request_status, r.media_status, r.status, r.requested_by, r.requested_by_id,
        r.requested_at, r.seerr_updated_at, r.seasons, r.title_id
     FROM requests r";

/// The checked filters of a request list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub statuses: Vec<String>,
    pub site: Option<String>,
    /// Lower case.
    pub person: Option<String>,
    pub kind: Option<String>,
    pub words: Vec<String>,
    pub title_id: Option<i64>,
}

fn like(word: &str) -> String {
    let escaped = word
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

fn push_filters(qb: &mut QueryBuilder<Postgres>, f: &Filter, status: bool, person: bool) {
    qb.push(" WHERE true");
    if status && !f.statuses.is_empty() {
        qb.push(" AND r.status = ANY(")
            .push_bind(f.statuses.clone())
            .push(")");
    }
    if let Some(site) = &f.site {
        qb.push(" AND r.site = ").push_bind(site.clone());
    }
    if person && let Some(p) = &f.person {
        qb.push(" AND lower(r.requested_by) = ")
            .push_bind(p.clone());
    }
    if let Some(kind) = &f.kind {
        qb.push(" AND r.kind = ").push_bind(kind.clone());
    }
    if let Some(title) = f.title_id {
        qb.push(" AND r.title_id = ").push_bind(title);
    }
    for word in &f.words {
        qb.push(" AND m.title ILIKE ").push_bind(like(word));
    }
}

const FROM_JOINED: &str =
    " FROM requests r LEFT JOIN request_media m ON m.kind = r.kind AND m.tmdb_id = r.tmdb_id";

/// One page of requests. `sort` is `created`, `updated` or `title`; `descending` reverses
/// the natural order (newest first for the dates, A to Z for the title).
pub async fn list(
    pool: &PgPool,
    f: &Filter,
    sort: &str,
    reversed: bool,
    paging: Paging,
) -> Result<(Vec<RequestRow>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*)");
    count.push(FROM_JOINED);
    push_filters(&mut count, f, true, true);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut list = QueryBuilder::<Postgres>::new(ROW_SELECT);
    push_filters(&mut list, f, true, true);
    // The column and its natural direction come from this table, never from the request.
    let (column, natural_desc) = match sort {
        "updated" => ("COALESCE(r.seerr_updated_at, r.requested_at)", true),
        "title" => ("lower(m.title)", false),
        _ => ("r.requested_at", true),
    };
    list.push(" ORDER BY ").push(column);
    list.push(if natural_desc != reversed {
        " DESC NULLS LAST"
    } else {
        " ASC NULLS LAST"
    });
    list.push(", r.id DESC LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = list.build_query_as::<RequestRow>().fetch_all(pool).await?;
    Ok((rows, total))
}

/// Requests per status under every filter but the status.
pub async fn counts(pool: &PgPool, f: &Filter) -> Result<RequestCounts, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT count(*) AS \"all\",
                count(*) FILTER (WHERE r.status = 'pending') AS pending,
                count(*) FILTER (WHERE r.status = 'approved') AS approved,
                count(*) FILTER (WHERE r.status = 'processing') AS processing,
                count(*) FILTER (WHERE r.status = 'available') AS available,
                count(*) FILTER (WHERE r.status = 'removed') AS removed,
                count(*) FILTER (WHERE r.status = 'declined') AS declined,
                count(*) FILTER (WHERE r.status = 'failed') AS failed,
                count(*) FILTER (WHERE r.status = 'unknown') AS unknown",
    );
    qb.push(FROM_JOINED);
    push_filters(&mut qb, f, false, true);
    qb.build_query_as::<RequestCounts>().fetch_one(pool).await
}

/// Requesters by display name and their number of requests under every filter but the
/// person, most first, with the accounts behind each name as `site:user id`.
pub async fn people(
    pool: &PgPool,
    f: &Filter,
) -> Result<Vec<(String, i64, Vec<String>)>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT r.requested_by, count(*), \
         coalesce(array_agg(DISTINCT r.site || ':' || r.requested_by_id::text) \
         FILTER (WHERE r.requested_by_id IS NOT NULL), '{}'::text[])",
    );
    qb.push(FROM_JOINED);
    push_filters(&mut qb, f, true, false);
    qb.push(" GROUP BY r.requested_by ORDER BY count(*) DESC, r.requested_by");
    qb.build_query_as::<(String, i64, Vec<String>)>()
        .fetch_all(pool)
        .await
}

pub async fn get(pool: &PgPool, id: i64) -> Result<Option<RequestRow>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(ROW_SELECT);
    qb.push(" WHERE r.id = ").push_bind(id);
    qb.build_query_as::<RequestRow>().fetch_optional(pool).await
}

/// Every show request for one of these TMDB ids, on either site: what the season gate counts.
pub async fn show_requests(
    pool: &PgPool,
    tmdb_ids: &[i64],
) -> Result<Vec<StoredRequest>, sqlx::Error> {
    if tmdb_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::<Postgres>::new(STORED_SELECT);
    qb.push(" WHERE r.kind = 'show' AND r.tmdb_id = ANY(")
        .push_bind(tmdb_ids.to_vec())
        .push(")");
    qb.build_query_as::<StoredRequest>().fetch_all(pool).await
}

/// The other requests for one title, newest first.
pub async fn related(
    pool: &PgPool,
    kind: &str,
    tmdb_id: i64,
    except: i64,
) -> Result<Vec<StoredRequest>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(STORED_SELECT);
    qb.push(" WHERE r.kind = ")
        .push_bind(kind.to_string())
        .push(" AND r.tmdb_id = ")
        .push_bind(tmdb_id)
        .push(" AND r.id <> ")
        .push_bind(except)
        .push(" ORDER BY r.requested_at DESC, r.id DESC LIMIT 50");
    qb.build_query_as::<StoredRequest>().fetch_all(pool).await
}

/// Open requests (pending, approved, processing) for these TMDB ids: kind, TMDB id and the
/// request's id, newest first.
pub async fn open_requests(
    pool: &PgPool,
    tmdb_ids: &[i64],
) -> Result<Vec<(String, i64, i64)>, sqlx::Error> {
    if tmdb_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT kind, tmdb_id, id FROM requests
         WHERE tmdb_id = ANY($1) AND status IN ('pending', 'approved', 'processing')
         ORDER BY requested_at DESC, id DESC",
    )
    .bind(tmdb_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// One request of a title, as its page lists it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TitleRequestRow {
    pub id: i64,
    pub requested_by: String,
    pub requested_by_id: Option<i64>,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub status: String,
    pub is_4k: bool,
    pub seasons: Vec<i32>,
}

/// Every request for a title by kind and TMDB id, newest first, at most 50.
pub async fn title_requests(
    pool: &PgPool,
    kind: &str,
    tmdb_id: i64,
) -> Result<Vec<TitleRequestRow>, sqlx::Error> {
    sqlx::query_as::<_, TitleRequestRow>(
        "SELECT id, requested_by, requested_by_id, requested_at, status, is_4k, seasons
         FROM requests WHERE kind = $1 AND tmdb_id = $2
         ORDER BY requested_at DESC, id DESC LIMIT 50",
    )
    .bind(kind)
    .bind(tmdb_id)
    .fetch_all(pool)
    .await
}

/// Every request, newest first, at most `limit`: the Requests page.
pub async fn newest(pool: &PgPool, limit: i64) -> Result<Vec<RequestRow>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(ROW_SELECT);
    qb.push(" ORDER BY r.requested_at DESC, r.id DESC LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<RequestRow>().fetch_all(pool).await
}

/// An account of a request site, by its display name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct SiteAccount {
    pub seerr_user_id: i64,
    pub display_name: String,
    pub is_owner: bool,
}

/// The accounts of a request site, by name.
pub async fn site_accounts(pool: &PgPool, site: &str) -> Result<Vec<SiteAccount>, sqlx::Error> {
    sqlx::query_as::<_, SiteAccount>(
        "SELECT seerr_user_id, display_name, is_owner FROM seerr_users
         WHERE site = $1 ORDER BY lower(display_name), seerr_user_id",
    )
    .bind(site)
    .fetch_all(pool)
    .await
}

/// A title asked for, with what the site said of it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RecentTitle {
    pub kind: String,
    pub tmdb_id: i64,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub poster_path: Option<String>,
    pub requested_at: DateTime<Utc>,
}

/// The titles asked for most recently, each once, the newest request first.
pub async fn recent_titles(pool: &PgPool, limit: i64) -> Result<Vec<RecentTitle>, sqlx::Error> {
    sqlx::query_as::<_, RecentTitle>(
        "SELECT kind, tmdb_id, title, year, poster_path, requested_at FROM (
            SELECT DISTINCT ON (r.kind, r.tmdb_id) r.kind, r.tmdb_id, m.title, m.year,
                   m.poster_path, r.requested_at
            FROM requests r
            LEFT JOIN request_media m ON m.kind = r.kind AND m.tmdb_id = r.tmdb_id
            WHERE r.tmdb_id IS NOT NULL
            ORDER BY r.kind, r.tmdb_id, r.requested_at DESC
         ) x
         ORDER BY requested_at DESC, tmdb_id LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn pending_count(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM requests WHERE status = 'pending'")
        .fetch_one(pool)
        .await
}

// The sync's writes ---------------------------------------------------------------------------

/// Every stored request of one site.
pub async fn stored(
    conn: &mut PgConnection,
    site: &str,
) -> Result<Vec<StoredRequest>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(STORED_SELECT);
    qb.push(" WHERE r.site = ").push_bind(site.to_string());
    qb.build_query_as::<StoredRequest>().fetch_all(conn).await
}

pub async fn insert(conn: &mut PgConnection, f: &RequestFacts) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO requests (site, seerr_id, kind, tmdb_id, tvdb_id, is_4k, request_status,
                               media_status, status, requested_by, requested_by_id, requested_at,
                               seerr_updated_at, seasons, title_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
         RETURNING id",
    )
    .bind(&f.site)
    .bind(f.seerr_id)
    .bind(&f.kind)
    .bind(f.tmdb_id)
    .bind(f.tvdb_id)
    .bind(f.is_4k)
    .bind(&f.request_status)
    .bind(&f.media_status)
    .bind(&f.status)
    .bind(&f.requested_by)
    .bind(f.requested_by_id)
    .bind(f.requested_at)
    .bind(f.seerr_updated_at)
    .bind(f.seasons.as_slice())
    .bind(f.title_id)
    .fetch_one(conn)
    .await
}

pub async fn update(conn: &mut PgConnection, id: i64, f: &RequestFacts) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE requests SET kind = $2, tmdb_id = $3, tvdb_id = $4, is_4k = $5,
                request_status = $6, media_status = $7, status = $8, requested_by = $9,
                requested_by_id = $10, requested_at = $11, seerr_updated_at = $12, seasons = $13,
                title_id = $14, updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&f.kind)
    .bind(f.tmdb_id)
    .bind(f.tvdb_id)
    .bind(f.is_4k)
    .bind(&f.request_status)
    .bind(&f.media_status)
    .bind(&f.status)
    .bind(&f.requested_by)
    .bind(f.requested_by_id)
    .bind(f.requested_at)
    .bind(f.seerr_updated_at)
    .bind(f.seasons.as_slice())
    .bind(f.title_id)
    .execute(conn)
    .await
    .map(|_| ())
}

pub async fn delete(conn: &mut PgConnection, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM requests WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await
        .map(|_| ())
}

// The title cache ----------------------------------------------------------------------------

/// The titles that need no lookup: looked up, or failed less than a day ago. Kind and TMDB id.
pub async fn known_media(pool: &PgPool) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT kind, tmdb_id FROM request_media
         WHERE lookup_error IS NULL OR looked_up_at > now() - interval '1 day'",
    )
    .fetch_all(pool)
    .await
}

/// What a lookup found, or why it found nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MediaWrite<'a> {
    pub title: Option<&'a str>,
    pub year: Option<i32>,
    pub poster_path: Option<&'a str>,
    pub backdrop_path: Option<&'a str>,
    pub tvdb_id: Option<i64>,
    pub error: Option<&'a str>,
}

pub async fn upsert_media(
    pool: &PgPool,
    kind: &str,
    tmdb_id: i64,
    m: &MediaWrite<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO request_media (kind, tmdb_id, title, year, poster_path, backdrop_path, tvdb_id,
                                   lookup_error, looked_up_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
         ON CONFLICT (kind, tmdb_id) DO UPDATE SET
             title = COALESCE(EXCLUDED.title, request_media.title),
             year = COALESCE(EXCLUDED.year, request_media.year),
             poster_path = COALESCE(EXCLUDED.poster_path, request_media.poster_path),
             backdrop_path = COALESCE(EXCLUDED.backdrop_path, request_media.backdrop_path),
             tvdb_id = COALESCE(EXCLUDED.tvdb_id, request_media.tvdb_id),
             lookup_error = EXCLUDED.lookup_error, looked_up_at = now()",
    )
    .bind(kind)
    .bind(tmdb_id)
    .bind(m.title)
    .bind(m.year)
    .bind(m.poster_path)
    .bind(m.backdrop_path)
    .bind(m.tvdb_id)
    .bind(m.error)
    .execute(pool)
    .await
    .map(|_| ())
}

/// The looked-up titles for these TMDB ids: kind, TMDB id, title.
pub async fn media_titles(
    pool: &PgPool,
    tmdb_ids: &[i64],
) -> Result<Vec<(String, i64, Option<String>)>, sqlx::Error> {
    if tmdb_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, (String, i64, Option<String>)>(
        "SELECT kind, tmdb_id, title FROM request_media WHERE tmdb_id = ANY($1)",
    )
    .bind(tmdb_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// Titles still waiting for a lookup.
pub async fn unknown_titles(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT count(DISTINCT (r.kind, r.tmdb_id)) FROM requests r
         LEFT JOIN request_media m ON m.kind = r.kind AND m.tmdb_id = r.tmdb_id
         WHERE r.tmdb_id IS NOT NULL AND m.title IS NULL",
    )
    .fetch_one(pool)
    .await
}

// The full process's side of Singularr Requests ---------------------------------------------------

/// One Seerr account as it is stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeerrUserWrite {
    pub seerr_user_id: i64,
    pub display_name: String,
    pub plex_id: Option<i64>,
    pub plex_username: Option<String>,
    pub user_type: Option<i32>,
    pub is_owner: bool,
}

/// Write a site's accounts. With `complete` (every page was read), accounts the site no longer
/// lists are removed. Returns (written, removed).
pub async fn write_seerr_users(
    conn: &mut PgConnection,
    site: &str,
    users: &[SeerrUserWrite],
    complete: bool,
) -> Result<(usize, usize), sqlx::Error> {
    for u in users {
        sqlx::query(
            "INSERT INTO seerr_users (site, seerr_user_id, display_name, plex_id, plex_username,
                                      user_type, is_owner, synced_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())
             ON CONFLICT (site, seerr_user_id) DO UPDATE SET
                 display_name = EXCLUDED.display_name, plex_id = EXCLUDED.plex_id,
                 plex_username = EXCLUDED.plex_username,
                 user_type = EXCLUDED.user_type,
                 is_owner = EXCLUDED.is_owner, synced_at = now()",
        )
        .bind(site)
        .bind(u.seerr_user_id)
        .bind(&u.display_name)
        .bind(u.plex_id)
        .bind(&u.plex_username)
        .bind(u.user_type)
        .bind(u.is_owner)
        .execute(&mut *conn)
        .await?;
    }
    let removed = if complete {
        let ids: Vec<i64> = users.iter().map(|u| u.seerr_user_id).collect();
        sqlx::query("DELETE FROM seerr_users WHERE site = $1 AND NOT (seerr_user_id = ANY($2))")
            .bind(site)
            .bind(&ids)
            .execute(&mut *conn)
            .await?
            .rows_affected() as usize
    } else {
        0
    };
    Ok((users.len(), removed))
}

/// Remove the requests and accounts of request sites other than `keep` (all of them when no
/// Seerr integration is configured). Returns (requests, accounts) removed.
pub async fn prune_sites(
    conn: &mut PgConnection,
    keep: Option<&str>,
) -> Result<(u64, u64), sqlx::Error> {
    let requests = sqlx::query("DELETE FROM requests WHERE $1::text IS NULL OR site <> $1")
        .bind(keep)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    let accounts = sqlx::query("DELETE FROM seerr_users WHERE $1::text IS NULL OR site <> $1")
        .bind(keep)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok((requests, accounts))
}

/// One account of a site with what it asked for, for the People list. No address is stored.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AccountRow {
    pub seerr_user_id: i64,
    pub display_name: String,
    pub plex_id: Option<i64>,
    pub user_type: Option<i32>,
    pub is_owner: bool,
    pub requests: i64,
    pub last_request: Option<DateTime<Utc>>,
    /// The newest sign-in on Singularr Requests.
    pub last_sign_in: Option<DateTime<Utc>>,
}

/// Every account of a site: the owner's first, then by name.
pub async fn accounts(pool: &PgPool, site: &str) -> Result<Vec<AccountRow>, sqlx::Error> {
    sqlx::query_as::<_, AccountRow>(
        "SELECT u.seerr_user_id, u.display_name, u.plex_id, u.user_type, u.is_owner,
                (SELECT count(*) FROM requests r
                 WHERE r.site = u.site AND r.requested_by_id = u.seerr_user_id)::bigint AS requests,
                (SELECT max(r.requested_at) FROM requests r
                 WHERE r.site = u.site AND r.requested_by_id = u.seerr_user_id) AS last_request,
                (SELECT max(s.created_at) FROM request_sessions s
                 WHERE s.site = u.site AND s.seerr_user_id = u.seerr_user_id) AS last_sign_in
         FROM seerr_users u
         WHERE u.site = $1
         ORDER BY u.is_owner DESC, lower(u.display_name), u.seerr_user_id",
    )
    .bind(site)
    .fetch_all(pool)
    .await
}

/// How many accounts of a site are stored.
pub async fn seerr_user_count(pool: &PgPool, site: &str) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM seerr_users WHERE site = $1")
        .bind(site)
        .fetch_one(pool)
        .await
}

/// One title of a discover list, to store.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoverWrite {
    pub kind: String,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub rating: Option<f64>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
}

/// Replace one stored discover list.
pub async fn replace_discover(
    conn: &mut PgConnection,
    list: &str,
    items: &[DiscoverWrite],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM discover_items WHERE list = $1")
        .bind(list)
        .execute(&mut *conn)
        .await?;
    for (position, item) in items.iter().enumerate() {
        sqlx::query(
            "INSERT INTO discover_items (list, position, kind, tmdb_id, title, year, overview,
                                         rating, poster_path, backdrop_path, fetched_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, now())",
        )
        .bind(list)
        .bind(i32::try_from(position).unwrap_or(i32::MAX))
        .bind(&item.kind)
        .bind(item.tmdb_id)
        .bind(&item.title)
        .bind(item.year)
        .bind(&item.overview)
        .bind(item.rating)
        .bind(&item.poster_path)
        .bind(&item.backdrop_path)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// The request rules and the owner face's address, as the face reads them.
pub async fn write_face_settings(
    pool: &PgPool,
    max_seasons: i32,
    gated_sites: &[String],
    owner_url: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO face_settings (id, max_seasons, gated_sites, owner_url, updated_at)
         VALUES (1, $1, $2, $3, now())
         ON CONFLICT (id) DO UPDATE SET max_seasons = EXCLUDED.max_seasons,
             gated_sites = EXCLUDED.gated_sites, owner_url = EXCLUDED.owner_url, updated_at = now()",
    )
    .bind(max_seasons)
    .bind(gated_sites)
    .bind(owner_url)
    .execute(pool)
    .await
    .map(|_| ())
}

const INTENT_SELECT: &str = "SELECT id, site, display_name AS requested_by, seerr_user_id, kind,
        tmdb_id, seasons, title, year, created_at AS asked_at, state, processed_at, outcome
     FROM request_intents";

/// Intents not picked up yet, oldest first.
pub async fn new_intents(pool: &PgPool, limit: i64) -> Result<Vec<RequestIntent>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(INTENT_SELECT);
    qb.push(" WHERE state = 'new' ORDER BY id LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<RequestIntent>().fetch_all(pool).await
}

/// Record what was made of an intent. Only an intent still `new` is changed; returns whether
/// it was.
pub async fn finish_intent(
    conn: &mut PgConnection,
    id: i64,
    state: &str,
    outcome: &serde_json::Value,
) -> Result<bool, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE request_intents SET state = $2, outcome = $3, processed_at = now()
         WHERE id = $1 AND state = 'new'",
    )
    .bind(id)
    .bind(state)
    .bind(Json(outcome))
    .execute(conn)
    .await?
    .rows_affected()
        == 1)
}

/// Intents, newest first.
pub async fn list_intents(
    pool: &PgPool,
    state: Option<&str>,
    paging: Paging,
) -> Result<(Vec<RequestIntent>, i64), sqlx::Error> {
    let total: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM request_intents WHERE ($1::text IS NULL OR state = $1)",
    )
    .bind(state)
    .fetch_one(pool)
    .await?;
    let mut qb = QueryBuilder::<Postgres>::new(INTENT_SELECT);
    qb.push(" WHERE (")
        .push_bind(state.map(str::to_string))
        .push("::text IS NULL OR state = ")
        .push_bind(state.map(str::to_string))
        .push(") ORDER BY id DESC LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = qb.build_query_as::<RequestIntent>().fetch_all(pool).await?;
    Ok((rows, total))
}

// The face's side: the `face_*` views and three inserts, as the portal's role ----------------------

/// Begin a transaction that names the session: the views and the intent trigger read the
/// token from `marquee.session_token` and hash it themselves.
async fn as_session(
    pool: &PgPool,
    token: &str,
) -> Result<sqlx::Transaction<'static, Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Named by migration 0005 (its views read it); the name must not change.
    sqlx::query("SELECT set_config('marquee.session_token', $1, true)")
        .bind(token)
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

/// The live session of a token.
pub async fn face_session(pool: &PgPool, token: &str) -> Result<Option<FaceSession>, sqlx::Error> {
    let mut tx = as_session(pool, token).await?;
    let row = sqlx::query_as::<_, FaceSession>(
        "SELECT plex_account_id, display_name, site, seerr_user_id, expires_at, is_owner
         FROM face_session",
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// The Seerr account of a Plex account, when the Seerr integration knows it.
pub async fn face_account(
    pool: &PgPool,
    plex_account_id: i64,
) -> Result<Option<FaceAccount>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    // Named by migration 0005 (the face_account view reads it); the name must not change.
    sqlx::query("SELECT set_config('marquee.plex_account', $1, true)")
        .bind(plex_account_id.to_string())
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query_as::<_, FaceAccount>(
        "SELECT plex_account_id, display_name, seerr_user_id, is_owner FROM face_account",
    )
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row)
}

/// A new session. The Seerr account and the name come from `seerr_users` (a trigger); an
/// account the Seerr integration does not know is refused there too.
pub async fn insert_session(
    pool: &PgPool,
    token_sha256: &str,
    plex_account_id: i64,
    expires_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO request_sessions (token_sha256, plex_account_id, expires_at)
         VALUES ($1, $2, $3)",
    )
    .bind(token_sha256)
    .bind(plex_account_id)
    .bind(expires_at)
    .execute(pool)
    .await
    .map(|_| ())
}

/// End a session (sign-out).
pub async fn end_session(pool: &PgPool, token_sha256: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO request_session_ends (token_sha256) VALUES ($1) ON CONFLICT DO NOTHING",
    )
    .bind(token_sha256)
    .execute(pool)
    .await
    .map(|_| ())
}

/// The state of each title, by kind and TMDB id: `available`, `processing` or `requested`.
/// A title with no row is not here.
pub async fn face_availability(
    pool: &PgPool,
    tmdb_ids: &[i64],
) -> Result<Vec<(String, i64, String)>, sqlx::Error> {
    if tmdb_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, (String, i64, String)>(
        "SELECT kind, tmdb_id, state FROM face_availability WHERE tmdb_id = ANY($1)",
    )
    .bind(tmdb_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// The state of each season of a show that has one.
pub async fn face_seasons(pool: &PgPool, tmdb_id: i64) -> Result<Vec<(i32, String)>, sqlx::Error> {
    sqlx::query_as::<_, (i32, String)>(
        "SELECT season_number, state FROM face_seasons WHERE tmdb_id = $1 ORDER BY season_number",
    )
    .bind(tmdb_id)
    .fetch_all(pool)
    .await
}

/// Every stored discover list, in order.
pub async fn face_discover(pool: &PgPool) -> Result<Vec<FaceDiscoverItem>, sqlx::Error> {
    sqlx::query_as::<_, FaceDiscoverItem>(
        "SELECT list, position, kind, tmdb_id, title, year, overview, rating, poster_path,
                backdrop_path, fetched_at
         FROM face_discover ORDER BY list, position",
    )
    .fetch_all(pool)
    .await
}

pub async fn face_rules(pool: &PgPool) -> Result<Option<FaceRules>, sqlx::Error> {
    sqlx::query_as::<_, FaceRules>("SELECT max_seasons, gated_sites, owner_url FROM face_rules")
        .fetch_optional(pool)
        .await
}

/// The signed-in person's own requests, newest first.
pub async fn face_my_requests(
    pool: &PgPool,
    token: &str,
) -> Result<Vec<FaceMyRequest>, sqlx::Error> {
    let mut tx = as_session(pool, token).await?;
    let rows = sqlx::query_as::<_, FaceMyRequest>(
        "SELECT source, id, kind, tmdb_id, seasons, title, year, asked_at, status, gate,
                gate_reason, poster_path, is_4k
         FROM face_my_requests ORDER BY asked_at DESC, id DESC LIMIT 200",
    )
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(rows)
}

/// What the face asks for: kind, TMDB id, seasons, title and year. Who asks comes from the
/// session of the token (a trigger), never from here.
pub struct IntentWrite<'a> {
    pub kind: &'a str,
    pub tmdb_id: i64,
    pub seasons: &'a [i32],
    pub title: Option<&'a str>,
    pub year: Option<i32>,
}

pub async fn insert_intent(
    pool: &PgPool,
    token: &str,
    w: &IntentWrite<'_>,
) -> Result<(), sqlx::Error> {
    let mut tx = as_session(pool, token).await?;
    sqlx::query(
        "INSERT INTO request_intents (kind, tmdb_id, seasons, title, year)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(w.kind)
    .bind(w.tmdb_id)
    .bind(w.seasons)
    .bind(w.title)
    .bind(w.year)
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}
