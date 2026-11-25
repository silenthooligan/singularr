//! SQL over `plays` and `play_sources`. The library filter reads the catalog's links of the
//! media server's items (`title_server`) and writes nothing there.

use super::model::{Filter, Play, PlayRecord};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

const COLUMNS: &str = "p.id, p.updated_at, p.server, p.rating_key, p.show_key, p.title_id,
        p.kind, p.title, p.show_title, p.season, p.episode, p.year, p.library_key,
        p.account_id, p.account_name, p.started_at, p.stopped_at, p.watched_secs,
        p.paused_secs, p.duration_secs, p.progress, p.player, p.device, p.platform, p.product,
        p.local, p.address, p.decision, p.video_decision, p.audio_decision,
        p.source_resolution, p.source_bitrate, p.stream_resolution, p.stream_bitrate,
        p.session_id, p.history_id, p.tautulli_id, p.viewed_at, p.raw";

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
}

pub async fn insert(conn: &mut PgConnection, r: &PlayRecord) -> Result<i64, sqlx::Error> {
    let q = sqlx::query_scalar::<_, i64>(
        "INSERT INTO plays (server, rating_key, show_key, title_id, kind, title, show_title,
                            season, episode, year, library_key, account_id, account_name,
                            started_at, stopped_at, watched_secs, paused_secs, duration_secs,
                            progress, player, device, platform, product, local, address,
                            decision, video_decision, audio_decision, source_resolution,
                            source_bitrate, stream_resolution, stream_bitrate, session_id,
                            history_id, tautulli_id, viewed_at, raw)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                 $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33,
                 $34, $35, $36, $37)
         RETURNING id",
    );
    let q = q
        .bind(&r.server)
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
        .bind(&r.raw);
    q.fetch_one(&mut *conn).await
}

/// Write a play again, as a merge made it.
pub async fn update(conn: &mut PgConnection, id: i64, r: &PlayRecord) -> Result<(), sqlx::Error> {
    let q = sqlx::query(
        "UPDATE plays SET server = $1, rating_key = $2, show_key = $3, title_id = $4, kind = $5,
                title = $6, show_title = $7, season = $8, episode = $9, year = $10,
                library_key = $11, account_id = $12, account_name = $13, started_at = $14,
                stopped_at = $15, watched_secs = $16, paused_secs = $17, duration_secs = $18,
                progress = $19, player = $20, device = $21, platform = $22, product = $23,
                local = $24, address = $25, decision = $26, video_decision = $27,
                audio_decision = $28, source_resolution = $29, source_bitrate = $30,
                stream_resolution = $31, stream_bitrate = $32, session_id = $33,
                history_id = $34, tautulli_id = $35, viewed_at = $36, raw = $37,
                updated_at = now()
         WHERE id = $38",
    );
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

/// Plays per weekday (1 Monday to 7 Sunday) and hour, by the clock of a time zone.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Cell {
    pub weekday: i32,
    pub hour: i32,
    pub plays: i64,
}

pub async fn grid(pool: &PgPool, f: &Filter, zone: &str) -> Result<Vec<Cell>, sqlx::Error> {
    let mut qb =
        QueryBuilder::<Postgres>::new("SELECT EXTRACT(ISODOW FROM p.started_at AT TIME ZONE ");
    qb.push_bind(zone.to_string())
        .push(")::int AS weekday, EXTRACT(HOUR FROM p.started_at AT TIME ZONE ")
        .push_bind(zone.to_string())
        .push(")::int AS hour, count(*) AS plays FROM plays p");
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

/// A person by their plays.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PersonCount {
    pub account_id: Option<i64>,
    pub name: Option<String>,
    pub plays: i64,
    pub watched: i64,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub devices: i64,
}

pub async fn people(
    pool: &PgPool,
    f: &Filter,
    limit: i64,
) -> Result<Vec<PersonCount>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT p.account_id,
                (array_agg(p.account_name ORDER BY p.started_at DESC)
                    FILTER (WHERE p.account_name IS NOT NULL))[1] AS name,
                count(*) AS plays, COALESCE(sum(p.watched_secs), 0)::bigint AS watched,
                min(p.started_at) AS first, max(COALESCE(p.stopped_at, p.started_at)) AS last,
                count(DISTINCT COALESCE(p.player, p.device, p.platform)) AS devices
         FROM plays p",
    );
    filters(&mut qb, f);
    qb.push(" GROUP BY p.account_id ORDER BY plays DESC, watched DESC LIMIT ")
        .push_bind(limit);
    qb.build_query_as::<PersonCount>().fetch_all(pool).await
}

/// A platform or a player by its plays.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct NameCount {
    pub name: String,
    pub platform: Option<String>,
    pub product: Option<String>,
    pub plays: i64,
    pub watched: i64,
    pub last: DateTime<Utc>,
}

pub async fn platforms(
    pool: &PgPool,
    f: &Filter,
    limit: i64,
) -> Result<Vec<NameCount>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT p.platform AS name, NULL::text AS platform, NULL::text AS product,
                count(*) AS plays, COALESCE(sum(p.watched_secs), 0)::bigint AS watched,
                max(p.started_at) AS last
         FROM plays p",
    );
    filters(&mut qb, f);
    qb.push(
        " AND p.platform IS NOT NULL GROUP BY p.platform ORDER BY plays DESC, watched DESC LIMIT ",
    )
    .push_bind(limit);
    qb.build_query_as::<NameCount>().fetch_all(pool).await
}

/// The players (a device by the name its person gave it), with their platform and app.
pub async fn players(pool: &PgPool, f: &Filter, limit: i64) -> Result<Vec<NameCount>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(
        "SELECT COALESCE(p.player, p.device) AS name, max(p.platform) AS platform,
                max(p.product) AS product, count(*) AS plays,
                COALESCE(sum(p.watched_secs), 0)::bigint AS watched, max(p.started_at) AS last
         FROM plays p",
    );
    filters(&mut qb, f);
    qb.push(
        " AND COALESCE(p.player, p.device) IS NOT NULL GROUP BY COALESCE(p.player, p.device)
          ORDER BY plays DESC, watched DESC LIMIT ",
    )
    .push_bind(limit);
    qb.build_query_as::<NameCount>().fetch_all(pool).await
}

/// The most streams at one time, and when that was first reached: over the plays whose start
/// and stop are known.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Peak {
    pub streams: i64,
    pub at: DateTime<Utc>,
}

pub async fn peak(pool: &PgPool, f: &Filter) -> Result<Option<Peak>, sqlx::Error> {
    let mut qb =
        QueryBuilder::<Postgres>::new("WITH e AS (SELECT p.started_at AS t, 1 AS d FROM plays p");
    filters(&mut qb, f);
    qb.push(
        " AND p.stopped_at > p.started_at
          UNION ALL SELECT p.stopped_at AS t, -1 AS d FROM plays p",
    );
    filters(&mut qb, f);
    qb.push(
        " AND p.stopped_at > p.started_at),
          s AS (SELECT t, sum(d) OVER (ORDER BY t, d ROWS UNBOUNDED PRECEDING) AS streams FROM e)
          SELECT streams::bigint AS streams, t AS at FROM s
          ORDER BY streams DESC, t LIMIT 1",
    );
    qb.build_query_as::<Peak>().fetch_optional(pool).await
}

/// One page of plays, newest first, after the play `before` when one is named; with whether
/// more follow, and how many match in all.
pub async fn page(
    pool: &PgPool,
    f: &Filter,
    before: Option<i64>,
    limit: i64,
) -> Result<(Vec<Play>, bool, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*) FROM plays p");
    filters(&mut count, f);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;
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
