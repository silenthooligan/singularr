//! SQL over `plays` and `play_sources`. The library filter reads the catalog's links of the
//! media server's items (`title_server`) and writes nothing there.

use super::model::{Filter, Play, PlayRecord, SortKey};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

const COLUMNS: &str = "p.id, p.updated_at, p.server, p.rating_key, p.show_key, p.title_id,
        p.kind, p.title, p.show_title, p.season, p.episode, p.year, p.library_key,
        p.account_id, p.account_name, p.started_at, p.stopped_at, p.watched_secs,
        p.paused_secs, p.duration_secs, p.progress, p.player, p.device, p.platform, p.product,
        p.local, p.address, p.decision, p.video_decision, p.audio_decision,
        p.source_resolution, p.source_bitrate, p.stream_resolution, p.stream_bitrate,
        p.session_id, p.history_id, p.tautulli_id, p.viewed_at, p.raw, p.server_kind,
        p.product_version, p.platform_version, p.device_id, p.secure, p.relayed, p.bandwidth,
        p.source_video_codec, p.source_audio_codec, p.source_audio_channels, p.source_container,
        p.stream_video_codec, p.stream_audio_codec, p.stream_audio_channels, p.stream_container,
        p.transcode_reasons";

/// The columns a play is written with, in the order [`bind_record`] binds them.
const WRITTEN: &str = "server, rating_key, show_key, title_id, kind, title, show_title, season,
        episode, year, library_key, account_id, account_name, started_at, stopped_at,
        watched_secs, paused_secs, duration_secs, progress, player, device, platform, product,
        local, address, decision, video_decision, audio_decision, source_resolution,
        source_bitrate, stream_resolution, stream_bitrate, session_id, history_id, tautulli_id,
        viewed_at, raw, server_kind, product_version, platform_version, device_id, secure,
        relayed, bandwidth, source_video_codec, source_audio_codec, source_audio_channels,
        source_container, stream_video_codec, stream_audio_codec, stream_audio_channels,
        stream_container, transcode_reasons";
/// How many columns [`WRITTEN`] names.
const WRITTEN_COUNT: usize = 53;

/// The stored play a source's record names by the source's own id. A session's id names an
/// open play alone: Jellyfin and Emby name a play by the session and the item. The pair comes
/// back when the item is played again after that play ended.
pub async fn by_source(
    conn: &mut PgConnection,
    r: &PlayRecord,
) -> Result<Option<Play>, sqlx::Error> {
    if r.session_id.is_none() && r.history_id.is_none() && r.tautulli_id.is_none() {
        return Ok(None);
    }
    sqlx::query_as::<_, Play>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM plays p
         WHERE p.server = $1
           AND (($2::text IS NOT NULL AND p.session_id = $2 AND p.stopped_at IS NULL)
                OR ($3::bigint IS NOT NULL AND p.history_id = $3)
                OR ($4::bigint IS NOT NULL AND p.tautulli_id = $4))
         ORDER BY p.id LIMIT 1"
    )))
    .bind(&r.server)
    .bind(&r.session_id)
    .bind(r.history_id)
    .bind(r.tautulli_id)
    .fetch_optional(&mut *conn)
    .await
}

/// The stored plays a record may be another reading of: the same server, person and item,
/// started within two days before it to a day after it ended.
pub async fn candidates(conn: &mut PgConnection, r: &PlayRecord) -> Result<Vec<Play>, sqlx::Error> {
    let Some(item) = &r.rating_key else {
        return Ok(Vec::new());
    };
    let end = r.stopped_at.unwrap_or(r.started_at).max(r.started_at);
    sqlx::query_as::<_, Play>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM plays p
         WHERE p.server = $1 AND p.rating_key = $2 AND p.account_id IS NOT DISTINCT FROM $3
           AND p.started_at >= $4 - interval '2 days' AND p.started_at <= $5 + interval '1 day'
         ORDER BY p.started_at DESC LIMIT 50"
    )))
    .bind(&r.server)
    .bind(item)
    .bind(r.account_id)
    .bind(r.started_at)
    .bind(end)
    .fetch_all(&mut *conn)
    .await
}

fn bind_record<'q>(
    q: sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments>,
    r: &'q PlayRecord,
) -> sqlx::query::Query<'q, Postgres, sqlx::postgres::PgArguments> {
    q.bind(&r.server)
        .bind(&r.rating_key)
        .bind(&r.show_key)
        .bind(r.title_id)
        .bind(&r.kind)
        .bind(&r.title)
        .bind(&r.show_title)
        .bind(r.season)
        .bind(r.episode)
        .bind(r.year)
        .bind(&r.library_key)
        .bind(r.account_id)
        .bind(&r.account_name)
        .bind(r.started_at)
        .bind(r.stopped_at)
        .bind(r.watched_secs)
        .bind(r.paused_secs)
        .bind(r.duration_secs)
        .bind(r.progress)
        .bind(&r.player)
        .bind(&r.device)
        .bind(&r.platform)
        .bind(&r.product)
        .bind(r.local)
        .bind(&r.address)
        .bind(&r.decision)
        .bind(&r.video_decision)
        .bind(&r.audio_decision)
        .bind(&r.source_resolution)
        .bind(r.source_bitrate)
        .bind(&r.stream_resolution)
        .bind(r.stream_bitrate)
        .bind(&r.session_id)
        .bind(r.history_id)
        .bind(r.tautulli_id)
        .bind(r.viewed_at)
        .bind(&r.raw)
        .bind(&r.server_kind)
        .bind(&r.product_version)
        .bind(&r.platform_version)
        .bind(&r.device_id)
        .bind(r.secure)
        .bind(r.relayed)
        .bind(r.bandwidth)
        .bind(&r.source_video_codec)
        .bind(&r.source_audio_codec)
        .bind(r.source_audio_channels)
        .bind(&r.source_container)
        .bind(&r.stream_video_codec)
        .bind(&r.stream_audio_codec)
        .bind(r.stream_audio_channels)
        .bind(&r.stream_container)
        .bind(&r.transcode_reasons)
}

/// `$1, $2, ...` for the written columns.
fn placeholders() -> String {
    (1..=WRITTEN_COUNT)
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub async fn insert(conn: &mut PgConnection, r: &PlayRecord) -> Result<i64, sqlx::Error> {
    let sql = format!(
        "INSERT INTO plays ({WRITTEN}) VALUES ({}) RETURNING id",
        placeholders()
    );
    let q = sqlx::query(sqlx::AssertSqlSafe(sql));
    let row = bind_record(q, r).fetch_one(&mut *conn).await?;
    sqlx::Row::try_get(&row, "id")
}

/// Write a play again, as a merge made it.
pub async fn update(conn: &mut PgConnection, id: i64, r: &PlayRecord) -> Result<(), sqlx::Error> {
    let sql = format!(
        "UPDATE plays SET ({WRITTEN}) = ({}), updated_at = now() WHERE id = ${}",
        placeholders(),
        WRITTEN_COUNT + 1
    );
    let q = sqlx::query(sqlx::AssertSqlSafe(sql));
    bind_record(q, r).bind(id).execute(&mut *conn).await?;
    Ok(())
}

/// The plays of followed sessions that never ended, with when each was last written.
pub async fn open_sessions(
    pool: &PgPool,
    server: &str,
) -> Result<Vec<(PlayRecord, DateTime<Utc>)>, sqlx::Error> {
    let rows = sqlx::query_as::<_, Play>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM plays p
         WHERE p.server = $1 AND p.session_id IS NOT NULL AND p.stopped_at IS NULL"
    )))
    .bind(server)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|p| (p.record, p.updated_at)).collect())
}

/// Where the reading of a source stands: the newest id read.
pub async fn cursor(pool: &PgPool, source: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<i64>>("SELECT newest FROM play_sources WHERE source = $1")
        .bind(source)
        .fetch_optional(pool)
        .await
        .map(Option::flatten)
}

pub async fn set_cursor(pool: &PgPool, source: &str, cursor: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO play_sources (source, newest, read_at) VALUES ($1, $2, now())
         ON CONFLICT (source) DO UPDATE SET newest = EXCLUDED.newest, read_at = now()",
    )
    .bind(source)
    .bind(cursor)
    .execute(pool)
    .await?;
    Ok(())
}

/// When the newest play Tautulli told of started.
pub async fn newest_tautulli(pool: &PgPool) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT max(started_at) FROM plays WHERE tautulli_id IS NOT NULL",
    )
    .fetch_one(pool)
    .await
}

/// How many plays Tautulli told of.
pub async fn from_tautulli(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM plays WHERE tautulli_id IS NOT NULL")
        .fetch_one(pool)
        .await
}

/// The items plays name that no catalog title holds yet, each as (server, key): an episode's
/// show, else the item.
pub async fn unlinked_items(pool: &PgPool) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT server, COALESCE(show_key, rating_key) FROM plays
         WHERE title_id IS NULL AND COALESCE(show_key, rating_key) IS NOT NULL
         LIMIT 20000",
    )
    .fetch_all(pool)
    .await
}

/// Link the plays of these items, each as (server, key, title), to their catalog titles.
pub async fn link(pool: &PgPool, found: &[(String, String, i64)]) -> Result<u64, sqlx::Error> {
    if found.is_empty() {
        return Ok(0);
    }
    let servers: Vec<String> = found.iter().map(|(s, _, _)| s.clone()).collect();
    let keys: Vec<String> = found.iter().map(|(_, k, _)| k.clone()).collect();
    let ids: Vec<i64> = found.iter().map(|(_, _, id)| *id).collect();
    Ok(sqlx::query(
        "UPDATE plays p SET title_id = f.id, updated_at = now()
         FROM unnest($1::text[], $2::text[], $3::bigint[]) AS f(server, key, id)
         WHERE p.title_id IS NULL AND p.server = f.server
           AND COALESCE(p.show_key, p.rating_key) = f.key",
    )
    .bind(servers)
    .bind(keys)
    .bind(ids)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Give the plays of these accounts the names the media server has for them now.
pub async fn rename(
    pool: &PgPool,
    server: &str,
    names: &[(i64, String)],
) -> Result<u64, sqlx::Error> {
    if names.is_empty() {
        return Ok(0);
    }
    let ids: Vec<i64> = names.iter().map(|(id, _)| *id).collect();
    let words: Vec<String> = names.iter().map(|(_, n)| n.clone()).collect();
    Ok(sqlx::query(
        "UPDATE plays p SET account_name = n.name
         FROM unnest($2::bigint[], $3::text[]) AS n(id, name)
         WHERE p.server = $1 AND p.account_id = n.id
           AND p.account_name IS DISTINCT FROM n.name",
    )
    .bind(server)
    .bind(ids)
    .bind(words)
    .execute(pool)
    .await?
    .rows_affected())
}

/// Forget every play that started before `cutoff`.
pub async fn forget_before(pool: &PgPool, cutoff: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM plays WHERE started_at < $1")
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected())
}

/// Forget every address kept: the plays' own, and those in the sources' records (a Plex
/// session's player, a Jellyfin or Emby session's remote end, Tautulli's row).
pub async fn forget_addresses(pool: &PgPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query(
        "UPDATE plays SET address = NULL,
                raw = raw #- '{tautulli,ip_address}' #- '{live,Player,address}'
                          #- '{live,Player,remotePublicAddress}' #- '{live,RemoteEndPoint}'
         WHERE address IS NOT NULL
            OR raw #> '{tautulli,ip_address}' IS NOT NULL
            OR raw #> '{live,Player,address}' IS NOT NULL
            OR raw #> '{live,Player,remotePublicAddress}' IS NOT NULL
            OR raw #> '{live,RemoteEndPoint}' IS NOT NULL",
    )
    .execute(pool)
    .await?
    .rows_affected())
}

// Reads for the statistics -----------------------------------------------------------------------

fn filters(qb: &mut QueryBuilder<Postgres>, f: &Filter) {
    qb.push(" WHERE TRUE");
    if let Some(since) = f.since {
        qb.push(" AND p.started_at >= ").push_bind(since);
    }
    if let Some(until) = f.until {
        qb.push(" AND p.started_at < ").push_bind(until);
    }
    if !f.kinds.is_empty() {
        qb.push(" AND p.kind = ANY(")
            .push_bind(f.kinds.clone())
            .push(")");
    }
    if let Some(library) = &f.library {
        qb.push(" AND ((p.server || '-' || p.library_key) = ")
            .push_bind(library.clone())
            .push(
                " OR EXISTS (SELECT 1 FROM title_server ts WHERE ts.server = p.server
                   AND ts.rating_key = COALESCE(p.show_key, p.rating_key) AND ts.library_id = ",
            )
            .push_bind(library.clone())
            .push("))");
    }
    if let Some(account) = f.account {
        qb.push(" AND p.account_id = ").push_bind(account);
    }
    if let Some(title) = f.title_id {
        qb.push(" AND p.title_id = ").push_bind(title);
    }
    if let Some(decision) = &f.decision {
        qb.push(" AND p.decision = ").push_bind(decision.clone());
    }
    if let Some(platform) = &f.platform {
        qb.push(" AND p.platform = ").push_bind(platform.clone());
    }
    if let Some(server) = &f.server {
        qb.push(" AND p.server = ").push_bind(server.clone());
    }
    if let Some(player) = &f.player {
        qb.push(" AND COALESCE(p.player, p.device) = ")
            .push_bind(player.clone());
    }
    if let Some(product) = &f.product {
        qb.push(" AND p.product = ").push_bind(product.clone());
    }
    if let Some(address) = &f.address {
        qb.push(" AND p.address = ").push_bind(address.clone());
    }
}

/// Plays, hours, people and titles, and the shares of delivery and network.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct Totals {
    pub plays: i64,
    /// Seconds.
    pub watched: i64,
    pub people: i64,
    pub titles: i64,
    pub direct_play: i64,
    pub direct_stream: i64,
    pub transcode: i64,
    pub home: i64,
    pub outside: i64,
    pub first: Option<DateTime<Utc>>,
}

const SHARES: &str = "count(*) FILTER (WHERE p.decision = 'direct play') AS direct_play,
        count(*) FILTER (WHERE p.decision = 'direct stream') AS direct_stream,
        count(*) FILTER (WHERE p.decision = 'transcode') AS transcode";

pub async fn totals(pool: &PgPool, f: &Filter) -> Result<Totals, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT count(*) AS plays, COALESCE(sum(p.watched_secs), 0)::bigint AS watched,
                count(DISTINCT p.account_id) AS people,
                count(DISTINCT COALESCE('t' || p.title_id::text,
                      'k' || COALESCE(p.show_key, p.rating_key), 'n' || p.title)) AS titles, ",
    );
    qb.push(SHARES).push(
        ", count(*) FILTER (WHERE p.local) AS home,
           count(*) FILTER (WHERE p.local = false) AS outside,
           min(p.started_at) AS first
         FROM plays p",
    );
    filters(&mut qb, f);
    qb.build_query_as::<Totals>().fetch_one(pool).await
}

/// One day: its plays, seconds watched, and plays by delivery.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Day {
    pub day: NaiveDate,
    pub plays: i64,
    pub watched: i64,
    pub direct_play: i64,
    pub direct_stream: i64,
    pub transcode: i64,
}

/// Plays per day, by the days of a time zone.
pub async fn days(pool: &PgPool, f: &Filter, zone: &str) -> Result<Vec<Day>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new("SELECT (p.started_at AT TIME ZONE ");
    qb.push_bind(zone.to_string())
        .push(
            ")::date AS day, count(*) AS plays,
             COALESCE(sum(p.watched_secs), 0)::bigint AS watched, ",
        )
        .push(SHARES)
        .push(" FROM plays p");
    filters(&mut qb, f);
    qb.push(" GROUP BY 1 ORDER BY 1");
    qb.build_query_as::<Day>().fetch_all(pool).await
}

/// Plays and seconds watched per weekday (1 Monday to 7 Sunday) and hour, by the clock of a
/// time zone.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Cell {
    pub weekday: i32,
    pub hour: i32,
    pub plays: i64,
    pub watched: i64,
}

pub async fn grid(pool: &PgPool, f: &Filter, zone: &str) -> Result<Vec<Cell>, sqlx::Error> {
    let mut qb =
        QueryBuilder::<Postgres>::new("SELECT EXTRACT(ISODOW FROM p.started_at AT TIME ZONE ");
    qb.push_bind(zone.to_string())
        .push(")::int AS weekday, EXTRACT(HOUR FROM p.started_at AT TIME ZONE ")
        .push_bind(zone.to_string())
        .push(
            ")::int AS hour, count(*) AS plays,
             COALESCE(sum(p.watched_secs), 0)::bigint AS watched FROM plays p",
        );
    filters(&mut qb, f);
    qb.push(" GROUP BY 1, 2");
    qb.build_query_as::<Cell>().fetch_all(pool).await
}

/// A title by its plays: the catalog title, or the media server's item when the catalog does
/// not hold it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleCount {
    pub title_id: Option<i64>,
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    pub plays: i64,
    pub watched: i64,
    pub people: i64,
    pub last: DateTime<Utc>,
}

/// The most played titles: movies, shows (their episodes together), tracks, or all of them.
pub async fn top_titles(
    pool: &PgPool,
    f: &Filter,
    limit: i64,
) -> Result<Vec<TitleCount>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT max(p.title_id) AS title_id, max(p.kind) AS kind,
                max(COALESCE(p.show_title, p.title)) AS title, max(p.year) AS year,
                count(*) AS plays, COALESCE(sum(p.watched_secs), 0)::bigint AS watched,
                count(DISTINCT p.account_id) AS people, max(p.started_at) AS last
         FROM plays p",
    );
    filters(&mut qb, f);
    qb.push(
        " GROUP BY COALESCE('t' || p.title_id::text, 'k' || COALESCE(p.show_key, p.rating_key),
                    'n' || COALESCE(p.show_title, p.title))
          ORDER BY plays DESC, watched DESC, last DESC LIMIT ",
    )
    .push_bind(limit);
    qb.build_query_as::<TitleCount>().fetch_all(pool).await
}

/// A person by their plays, with their newest play's address, player, app, platform and title.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PersonCount {
    pub account_id: Option<i64>,
    pub name: Option<String>,
    pub plays: i64,
    pub watched: i64,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub devices: i64,
    /// The addresses they played from.
    pub addresses: i64,
    pub last_address: Option<String>,
    pub last_player: Option<String>,
    pub last_product: Option<String>,
    pub last_platform: Option<String>,
    pub last_server: Option<String>,
    pub last_title: Option<String>,
    pub last_title_id: Option<i64>,
}

pub async fn people(
    pool: &PgPool,
    f: &Filter,
    limit: i64,
) -> Result<Vec<PersonCount>, sqlx::Error> {
    let newest = |e: &str| {
        format!("(array_agg({e} ORDER BY p.started_at DESC) FILTER (WHERE {e} IS NOT NULL))[1]")
    };
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT p.account_id, {} AS name,
                count(*) AS plays, COALESCE(sum(p.watched_secs), 0)::bigint AS watched,
                min(p.started_at) AS first, max(COALESCE(p.stopped_at, p.started_at)) AS last,
                count(DISTINCT COALESCE(p.player, p.device, p.platform)) AS devices,
                count(DISTINCT p.address) AS addresses,
                {} AS last_address, {} AS last_player, {} AS last_product, {} AS last_platform,
                {} AS last_server, {} AS last_title, {} AS last_title_id
         FROM plays p",
        newest("p.account_name"),
        newest("p.address"),
        newest("COALESCE(p.player, p.device)"),
        newest("p.product"),
        newest("p.platform"),
        newest("p.server"),
        newest("COALESCE(p.show_title, p.title)"),
        newest("p.title_id"),
    ));
    filters(&mut qb, f);
    qb.push(" GROUP BY p.account_id ORDER BY plays DESC, watched DESC LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<PersonCount>().fetch_all(pool).await
}

/// A platform, a player, an app, a media server or a library by its plays.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct NameCount {
    pub name: String,
    pub platform: Option<String>,
    pub product: Option<String>,
    pub plays: i64,
    pub watched: i64,
    pub people: i64,
    pub last: DateTime<Utc>,
}

/// Plays grouped by one expression: its value as the name, with the plays, seconds, people and
/// the last play. `extra` names the platform and the app where they go with the name.
async fn counts_by(
    pool: &PgPool,
    f: &Filter,
    name: &str,
    extra: (&str, &str),
    limit: i64,
) -> Result<Vec<NameCount>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT {name} AS name, {} AS platform, {} AS product, count(*) AS plays,
                COALESCE(sum(p.watched_secs), 0)::bigint AS watched,
                count(DISTINCT p.account_id) AS people, max(p.started_at) AS last
         FROM plays p",
        extra.0, extra.1
    ));
    filters(&mut qb, f);
    qb.push(format!(
        " AND {name} IS NOT NULL GROUP BY 1 ORDER BY plays DESC, watched DESC LIMIT "
    ))
    .push_bind(limit);
    qb.build_query_as::<NameCount>().fetch_all(pool).await
}

pub async fn platforms(
    pool: &PgPool,
    f: &Filter,
    limit: i64,
) -> Result<Vec<NameCount>, sqlx::Error> {
    counts_by(pool, f, "p.platform", ("NULL::text", "NULL::text"), limit).await
}

/// The players (a device by the name its person gave it), with their platform and app.
pub async fn players(pool: &PgPool, f: &Filter, limit: i64) -> Result<Vec<NameCount>, sqlx::Error> {
    counts_by(
        pool,
        f,
        "COALESCE(p.player, p.device)",
        ("max(p.platform)", "max(p.product)"),
        limit,
    )
    .await
}

/// The player apps (`Plex for Android (TV)`, `Jellyfin Web`), with their newest version.
pub async fn products(
    pool: &PgPool,
    f: &Filter,
    limit: i64,
) -> Result<Vec<NameCount>, sqlx::Error> {
    counts_by(
        pool,
        f,
        "p.product",
        (
            "max(p.platform)",
            "(array_agg(p.product_version ORDER BY p.started_at DESC) FILTER (WHERE p.product_version IS NOT NULL))[1]",
        ),
        limit,
    )
    .await
}

/// The media servers, by their integration ids, with their kind as the platform.
pub async fn servers(pool: &PgPool, f: &Filter) -> Result<Vec<NameCount>, sqlx::Error> {
    counts_by(
        pool,
        f,
        "p.server",
        ("max(p.server_kind)", "NULL::text"),
        100,
    )
    .await
}

/// A library by its plays, with the title played last.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LibraryCount {
    /// `<media server>-<section>`.
    pub library: String,
    pub plays: i64,
    pub watched: i64,
    pub people: i64,
    pub last: DateTime<Utc>,
    pub last_title: Option<String>,
}

/// The libraries by their ids: the section a play names, else the section of its item.
pub async fn libraries(pool: &PgPool, f: &Filter) -> Result<Vec<LibraryCount>, sqlx::Error> {
    const LIBRARY: &str = "COALESCE(p.server || '-' || p.library_key,
        (SELECT ts.library_id FROM title_server ts WHERE ts.server = p.server
           AND ts.rating_key = COALESCE(p.show_key, p.rating_key) LIMIT 1))";
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT {LIBRARY} AS library, count(*) AS plays,
                COALESCE(sum(p.watched_secs), 0)::bigint AS watched,
                count(DISTINCT p.account_id) AS people, max(p.started_at) AS last,
                (array_agg(COALESCE(p.show_title, p.title) ORDER BY p.started_at DESC))[1]
                    AS last_title
         FROM plays p"
    ));
    filters(&mut qb, f);
    qb.push(format!(
        " AND {LIBRARY} IS NOT NULL GROUP BY 1 ORDER BY plays DESC"
    ));
    qb.build_query_as::<LibraryCount>().fetch_all(pool).await
}

/// A label and its plays: a resolution, a reason.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LabelCount {
    pub label: String,
    pub plays: i64,
    pub watched: i64,
}

async fn labels_of(
    pool: &PgPool,
    f: &Filter,
    from: &str,
    label: &str,
) -> Result<Vec<LabelCount>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(format!(
        "SELECT {label} AS label, count(*) AS plays,
                COALESCE(sum(p.watched_secs), 0)::bigint AS watched FROM {from}"
    ));
    filters(&mut qb, f);
    qb.push(format!(
        " AND {label} IS NOT NULL GROUP BY 1 ORDER BY plays DESC, label LIMIT 50"
    ));
    qb.build_query_as::<LabelCount>().fetch_all(pool).await
}

/// The plays by the file's resolution, or by the resolution sent.
pub async fn resolutions(
    pool: &PgPool,
    f: &Filter,
    stream: bool,
) -> Result<Vec<LabelCount>, sqlx::Error> {
    let column = if stream {
        "p.stream_resolution"
    } else {
        "p.source_resolution"
    };
    labels_of(pool, f, "plays p", column).await
}

/// Why the servers transcoded, by the plays each reason was given for.
pub async fn reasons(pool: &PgPool, f: &Filter) -> Result<Vec<LabelCount>, sqlx::Error> {
    labels_of(
        pool,
        f,
        "plays p CROSS JOIN LATERAL unnest(p.transcode_reasons) AS reason",
        "reason",
    )
    .await
}

/// One address a person played from: how often, from which player, first and last.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct AddressCount {
    pub address: String,
    pub plays: i64,
    pub player: Option<String>,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
}

/// The addresses plays came from, the newest first.
pub async fn addresses(
    pool: &PgPool,
    f: &Filter,
    limit: i64,
) -> Result<Vec<AddressCount>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT p.address AS address, count(*) AS plays,
                (array_agg(COALESCE(p.player, p.device) ORDER BY p.started_at DESC)
                    FILTER (WHERE COALESCE(p.player, p.device) IS NOT NULL))[1] AS player,
                min(p.started_at) AS first, max(p.started_at) AS last
         FROM plays p",
    );
    filters(&mut qb, f);
    qb.push(" AND p.address IS NOT NULL GROUP BY p.address ORDER BY last DESC LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<AddressCount>().fetch_all(pool).await
}

/// The most streams at one time, and when that was first reached: over the plays whose start
/// and stop are known.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Peak {
    pub streams: i64,
    pub at: DateTime<Utc>,
}

/// The running count of streams over the plays a filter takes: `WITH ... s(t, streams)`.
fn streams_over(qb: &mut QueryBuilder<Postgres>, f: &Filter) {
    qb.push("WITH e AS (SELECT p.started_at AS t, 1 AS d FROM plays p");
    filters(qb, f);
    qb.push(
        " AND p.stopped_at > p.started_at
          UNION ALL SELECT p.stopped_at AS t, -1 AS d FROM plays p",
    );
    filters(qb, f);
    qb.push(
        " AND p.stopped_at > p.started_at),
          s AS (SELECT t, sum(d) OVER (ORDER BY t, d ROWS UNBOUNDED PRECEDING) AS streams FROM e)",
    );
}

pub async fn peak(pool: &PgPool, f: &Filter) -> Result<Option<Peak>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new("");
    streams_over(&mut qb, f);
    qb.push(" SELECT streams::bigint AS streams, t AS at FROM s ORDER BY streams DESC, t LIMIT 1");
    qb.build_query_as::<Peak>().fetch_optional(pool).await
}

/// The most streams at one time on each day, by the days of a time zone.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DayPeak {
    pub day: NaiveDate,
    pub streams: i64,
}

pub async fn peaks(pool: &PgPool, f: &Filter, zone: &str) -> Result<Vec<DayPeak>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new("");
    streams_over(&mut qb, f);
    qb.push(" SELECT (t AT TIME ZONE ")
        .push_bind(zone.to_string())
        .push(")::date AS day, max(streams)::bigint AS streams FROM s GROUP BY 1 ORDER BY 1");
    qb.build_query_as::<DayPeak>().fetch_all(pool).await
}

/// One page of plays, newest first, after the play `before` when one is named; with whether
/// more follow, and how many match in all.
pub async fn page(
    pool: &PgPool,
    f: &Filter,
    before: Option<i64>,
    limit: i64,
) -> Result<(Vec<Play>, bool, i64), sqlx::Error> {
    let total = count(pool, f).await?;
    let mut qb = QueryBuilder::<Postgres>::new("SELECT ");
    qb.push(COLUMNS).push(" FROM plays p");
    filters(&mut qb, f);
    if let Some(before) = before {
        qb.push(
            " AND (p.started_at, p.id) < (SELECT b.started_at, b.id FROM plays b WHERE b.id = ",
        )
        .push_bind(before)
        .push(")");
    }
    qb.push(" ORDER BY p.started_at DESC, p.id DESC LIMIT ")
        .push_bind(limit + 1);
    let mut rows = qb.build_query_as::<Play>().fetch_all(pool).await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(0));
    Ok((rows, more, total))
}

/// How many plays a filter takes.
pub async fn count(pool: &PgPool, f: &Filter) -> Result<i64, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new("SELECT count(*) FROM plays p");
    filters(&mut qb, f);
    qb.build_query_scalar().fetch_one(pool).await
}

/// One page of plays in the order of a column, `offset` plays in: for the history table, whose
/// every column sorts. Plays without the column's value come last; ties go newest first.
pub async fn sorted(
    pool: &PgPool,
    f: &Filter,
    sort: SortKey,
    descending: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<Play>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new("SELECT ");
    qb.push(COLUMNS).push(" FROM plays p");
    filters(&mut qb, f);
    let dir = if descending { "DESC" } else { "ASC" };
    // The start is never empty: in its order the index on (started_at, id) serves.
    let order = match sort {
        SortKey::Date => format!(" ORDER BY p.started_at {dir}, p.id {dir}"),
        other => format!(
            " ORDER BY {} {dir} NULLS LAST, p.started_at DESC, p.id DESC",
            other.expression()
        ),
    };
    qb.push(order)
        .push(" OFFSET ")
        .push_bind(offset.max(0))
        .push(" LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<Play>().fetch_all(pool).await
}

/// Every play a filter takes, as (id, address, start): for an order made outside the database.
pub async fn addresses_of(
    pool: &PgPool,
    f: &Filter,
) -> Result<Vec<(i64, Option<String>, DateTime<Utc>)>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new("SELECT p.id, p.address, p.started_at FROM plays p");
    filters(&mut qb, f);
    qb.build_query_as::<(i64, Option<String>, DateTime<Utc>)>()
        .fetch_all(pool)
        .await
}

/// The plays of these ids, in no order.
pub async fn by_ids(pool: &PgPool, ids: &[i64]) -> Result<Vec<Play>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, Play>(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM plays p WHERE p.id = ANY($1)"
    )))
    .bind(ids)
    .fetch_all(pool)
    .await
}

/// The values the history's filters choose from: the people (by account, with their newest
/// name), and the servers, players, apps and platforms plays name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choices {
    pub people: Vec<(i64, Option<String>)>,
    pub servers: Vec<(String, Option<String>)>,
    pub players: Vec<String>,
    pub products: Vec<String>,
    pub platforms: Vec<String>,
}

pub async fn choices(pool: &PgPool) -> Result<Choices, sqlx::Error> {
    let people = sqlx::query_as::<_, (i64, Option<String>)>(
        "SELECT account_id, (array_agg(account_name ORDER BY started_at DESC)
                    FILTER (WHERE account_name IS NOT NULL))[1]
         FROM plays WHERE account_id IS NOT NULL GROUP BY account_id ORDER BY count(*) DESC
         LIMIT 1000",
    )
    .fetch_all(pool)
    .await?;
    let servers = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT server, max(server_kind) FROM plays GROUP BY server ORDER BY count(*) DESC",
    )
    .fetch_all(pool)
    .await?;
    let distinct = |expr: &str| {
        format!(
            "SELECT {expr} FROM plays WHERE {expr} IS NOT NULL GROUP BY 1 ORDER BY count(*) DESC
             LIMIT 500"
        )
    };
    let players =
        sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(distinct("COALESCE(player, device)")))
            .fetch_all(pool)
            .await?;
    let products = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(distinct("product")))
        .fetch_all(pool)
        .await?;
    let platforms = sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(distinct("platform")))
        .fetch_all(pool)
        .await?;
    Ok(Choices {
        people,
        servers,
        players,
        products,
        platforms,
    })
}

/// The start of the first of the last `days` days by the clock of a time zone (`None`: every
/// day), and today there.
pub async fn local_range(
    pool: &PgPool,
    zone: &str,
    days: Option<i64>,
) -> Result<(Option<DateTime<Utc>>, NaiveDate), sqlx::Error> {
    sqlx::query_as::<_, (Option<DateTime<Utc>>, NaiveDate)>(
        "SELECT CASE WHEN $2::bigint IS NULL THEN NULL
                     ELSE (date_trunc('day', now() AT TIME ZONE $1)
                           - make_interval(days => ($2::bigint - 1)::int)) AT TIME ZONE $1
                END,
                (now() AT TIME ZONE $1)::date",
    )
    .bind(zone)
    .bind(days)
    .fetch_one(pool)
    .await
}

/// The start of two days by the clock of a time zone: the first one's, and the end of the
/// second (the start of the day after it). For a range of dates the history asks for.
pub async fn local_days(
    pool: &PgPool,
    zone: &str,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<(Option<DateTime<Utc>>, Option<DateTime<Utc>>), sqlx::Error> {
    sqlx::query_as::<_, (Option<DateTime<Utc>>, Option<DateTime<Utc>>)>(
        "SELECT ($2::date)::timestamp AT TIME ZONE $1,
                (($3::date) + 1)::timestamp AT TIME ZONE $1",
    )
    .bind(zone)
    .bind(from)
    .bind(to)
    .fetch_one(pool)
    .await
}

/// One title's plays: how many, by how many people, seconds watched, and the last.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleTotals {
    pub plays: i64,
    pub people: i64,
    pub watched: i64,
    pub last: Option<DateTime<Utc>>,
}

pub async fn title_totals(pool: &PgPool, title_id: i64) -> Result<TitleTotals, sqlx::Error> {
    sqlx::query_as::<_, TitleTotals>(
        "SELECT count(*) AS plays, count(DISTINCT account_id) AS people,
                COALESCE(sum(watched_secs), 0)::bigint AS watched, max(started_at) AS last
         FROM plays WHERE title_id = $1",
    )
    .bind(title_id)
    .fetch_one(pool)
    .await
}
