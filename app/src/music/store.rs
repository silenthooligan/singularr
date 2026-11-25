//! SQL over the tables `music` owns: `music_artists`, `music_albums`, `music_tracks` and
//! `music_scan`.

use super::model::{AlbumRef, AlbumSummary, ArtistSummary, FormatCount, MusicCounts, Track};
use super::scan::{ScannedAlbum, ScannedTrack};
use crate::core::model::Paging;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

fn like(word: &str) -> String {
    let escaped = word
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{escaped}%")
}

// Reads for the API ------------------------------------------------------------------------------

const ARTIST_SELECT: &str = "SELECT ar.id, ar.name, ar.folder,
        count(a.id) AS album_count,
        COALESCE(sum(a.track_count), 0)::bigint AS track_count,
        COALESCE(sum(a.size_bytes), 0)::bigint AS size_bytes,
        COALESCE((SELECT array_agg(DISTINCT f ORDER BY f)
                  FROM music_albums a2, unnest(a2.formats) AS f
                  WHERE a2.artist_id = ar.id), '{}') AS formats,
        count(a.id) FILTER (WHERE a.cover_path IS NULL) AS missing_covers,
        (SELECT a3.id FROM music_albums a3
         WHERE a3.artist_id = ar.id AND a3.cover_path IS NOT NULL
         ORDER BY a3.year DESC NULLS LAST, a3.id LIMIT 1) AS cover_album_id
     FROM music_artists ar LEFT JOIN music_albums a ON a.artist_id = ar.id";

/// The checked filters of the artist list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtistFilter {
    pub words: Vec<String>,
    pub format: Option<String>,
}

fn artist_filters(qb: &mut QueryBuilder<Postgres>, f: &ArtistFilter) {
    qb.push(" WHERE true");
    for word in &f.words {
        qb.push(" AND ar.name ILIKE ").push_bind(like(word));
    }
    if let Some(format) = &f.format {
        qb.push(" AND EXISTS (SELECT 1 FROM music_albums x WHERE x.artist_id = ar.id AND ")
            .push_bind(format.clone())
            .push(" = ANY(x.formats))");
    }
}

pub async fn artists(
    pool: &PgPool,
    f: &ArtistFilter,
    paging: Paging,
) -> Result<(Vec<ArtistSummary>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*) FROM music_artists ar");
    artist_filters(&mut count, f);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;
    let mut list = QueryBuilder::<Postgres>::new(ARTIST_SELECT);
    artist_filters(&mut list, f);
    list.push(" GROUP BY ar.id ORDER BY ar.sort_name, ar.id LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = list
        .build_query_as::<ArtistSummary>()
        .fetch_all(pool)
        .await?;
    Ok((rows, total))
}

pub async fn artist(pool: &PgPool, id: i64) -> Result<Option<ArtistSummary>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(ARTIST_SELECT);
    qb.push(" WHERE ar.id = ")
        .push_bind(id)
        .push(" GROUP BY ar.id");
    qb.build_query_as::<ArtistSummary>()
        .fetch_optional(pool)
        .await
}

const ALBUM_SELECT: &str = "SELECT a.id, a.artist_id, ar.name AS artist, a.name, a.folder, a.year,
        a.track_count, a.size_bytes, a.formats, a.cover_path IS NOT NULL AS has_cover, a.cover_path,
        EXISTS (SELECT 1 FROM music_albums d
                WHERE d.artist_id = a.artist_id AND d.name_key = a.name_key AND d.id <> a.id)
            AS duplicate,
        a.other_files, a.first_seen_at, a.updated_at
     FROM music_albums a JOIN music_artists ar ON ar.id = a.artist_id";

const DUPLICATE: &str = " EXISTS (SELECT 1 FROM music_albums d
        WHERE d.artist_id = a.artist_id AND d.name_key = a.name_key AND d.id <> a.id)";

/// The checked filters of the album list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumFilter {
    /// A format, or `mixed`.
    pub format: Option<String>,
    pub missing_cover: Option<bool>,
    pub no_year: Option<bool>,
    pub duplicates: Option<bool>,
    pub artist: Option<i64>,
    pub words: Vec<String>,
}

fn album_filters(qb: &mut QueryBuilder<Postgres>, f: &AlbumFilter) {
    qb.push(" WHERE true");
    match f.format.as_deref() {
        None => {}
        Some("mixed") => {
            qb.push(" AND cardinality(a.formats) > 1");
        }
        Some(format) => {
            qb.push(" AND ")
                .push_bind(format.to_string())
                .push(" = ANY(a.formats)");
        }
    }
    if let Some(missing) = f.missing_cover {
        qb.push(if missing {
            " AND a.cover_path IS NULL"
        } else {
            " AND a.cover_path IS NOT NULL"
        });
    }
    if let Some(no_year) = f.no_year {
        qb.push(if no_year {
            " AND a.year IS NULL"
        } else {
            " AND a.year IS NOT NULL"
        });
    }
    if let Some(duplicates) = f.duplicates {
        qb.push(if duplicates { " AND" } else { " AND NOT" });
        qb.push(DUPLICATE);
    }
    if let Some(artist) = f.artist {
        qb.push(" AND a.artist_id = ").push_bind(artist);
    }
    for word in &f.words {
        let pattern = like(word);
        qb.push(" AND (a.name ILIKE ")
            .push_bind(pattern.clone())
            .push(" OR ar.name ILIKE ")
            .push_bind(pattern)
            .push(")");
    }
}

/// The `ORDER BY` of an album sort. Every text comes from this table, never from the request.
pub fn album_order(name: &str, descending: bool) -> Option<&'static str> {
    Some(match (name, descending) {
        ("artist", false) => "ar.sort_name ASC, a.year ASC NULLS LAST, a.name_key ASC, a.id ASC",
        ("artist", true) => "ar.sort_name DESC, a.year DESC NULLS LAST, a.name_key DESC, a.id DESC",
        ("name", false) => "a.name_key ASC, a.id ASC",
        ("name", true) => "a.name_key DESC, a.id DESC",
        ("year", false) => "a.year ASC NULLS LAST, a.name_key ASC, a.id ASC",
        ("year", true) => "a.year DESC NULLS LAST, a.name_key ASC, a.id ASC",
        ("size", false) => "a.size_bytes ASC, a.id ASC",
        ("size", true) => "a.size_bytes DESC, a.id ASC",
        ("tracks", false) => "a.track_count ASC, a.id ASC",
        ("tracks", true) => "a.track_count DESC, a.id ASC",
        ("added", false) => "a.first_seen_at ASC, a.id ASC",
        ("added", true) => "a.first_seen_at DESC, a.id DESC",
        _ => return None,
    })
}

pub async fn albums(
    pool: &PgPool,
    f: &AlbumFilter,
    order: &'static str,
    paging: Paging,
) -> Result<(Vec<AlbumSummary>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new(
        "SELECT count(*) FROM music_albums a JOIN music_artists ar ON ar.id = a.artist_id",
    );
    album_filters(&mut count, f);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;
    let mut list = QueryBuilder::<Postgres>::new(ALBUM_SELECT);
    album_filters(&mut list, f);
    list.push(" ORDER BY ")
        .push(order)
        .push(" LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = list
        .build_query_as::<AlbumSummary>()
        .fetch_all(pool)
        .await?;
    Ok((rows, total))
}

pub async fn album(pool: &PgPool, id: i64) -> Result<Option<AlbumSummary>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(ALBUM_SELECT);
    qb.push(" WHERE a.id = ").push_bind(id);
    qb.build_query_as::<AlbumSummary>()
        .fetch_optional(pool)
        .await
}

/// Every album of one artist, oldest first.
pub async fn artist_albums(
    pool: &PgPool,
    artist_id: i64,
) -> Result<Vec<AlbumSummary>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(ALBUM_SELECT);
    qb.push(" WHERE a.artist_id = ")
        .push_bind(artist_id)
        .push(" ORDER BY a.year ASC NULLS LAST, a.name_key, a.id");
    qb.build_query_as::<AlbumSummary>().fetch_all(pool).await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TrackRow {
    pub album_id: i64,
    #[sqlx(flatten)]
    pub track: Track,
}

/// The tracks of these albums, in the order the scan found them.
pub async fn tracks(pool: &PgPool, album_ids: &[i64]) -> Result<Vec<TrackRow>, sqlx::Error> {
    if album_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, TrackRow>(
        "SELECT album_id, id, file_name, track_number, title, format, size_bytes
         FROM music_tracks WHERE album_id = ANY($1) ORDER BY album_id, id",
    )
    .bind(album_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// The other folders of the same artist with the same album name.
pub async fn duplicates_of(pool: &PgPool, album_id: i64) -> Result<Vec<AlbumRef>, sqlx::Error> {
    sqlx::query_as::<_, AlbumRef>(
        "SELECT d.id, d.folder, d.track_count, d.size_bytes
         FROM music_albums a JOIN music_albums d
              ON d.artist_id = a.artist_id AND d.name_key = a.name_key AND d.id <> a.id
         WHERE a.id = $1 ORDER BY d.folder",
    )
    .bind(album_id)
    .fetch_all(pool)
    .await
}

pub async fn counts(pool: &PgPool) -> Result<MusicCounts, sqlx::Error> {
    let mut counts = sqlx::query_as::<_, MusicCounts>(
        "SELECT (SELECT count(*) FROM music_artists) AS artists,
                count(*) AS albums,
                COALESCE(sum(a.track_count), 0)::bigint AS tracks,
                COALESCE(sum(a.size_bytes), 0)::bigint AS size_bytes,
                count(*) FILTER (WHERE a.cover_path IS NULL) AS missing_cover,
                count(*) FILTER (WHERE a.year IS NULL) AS no_year,
                count(*) FILTER (WHERE EXISTS (SELECT 1 FROM music_albums d
                    WHERE d.artist_id = a.artist_id AND d.name_key = a.name_key
                      AND d.id <> a.id)) AS duplicates,
                count(*) FILTER (WHERE a.track_count = 0) AS empty,
                count(*) FILTER (WHERE cardinality(a.formats) > 1) AS mixed_formats
         FROM music_albums a",
    )
    .fetch_one(pool)
    .await?;
    counts.formats = sqlx::query_as::<_, FormatCount>(
        "SELECT f AS format, count(*) AS albums
         FROM music_albums, unnest(formats) AS f GROUP BY f ORDER BY count(*) DESC, f",
    )
    .fetch_all(pool)
    .await?;
    Ok(counts)
}

/// `None`: no such album. `Some(None)`: the album has no cover.
pub async fn cover_path(pool: &PgPool, id: i64) -> Result<Option<Option<String>>, sqlx::Error> {
    sqlx::query_scalar::<_, Option<String>>("SELECT cover_path FROM music_albums WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

// The scan's writes ---------------------------------------------------------------------------

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct StoredAlbum {
    pub id: i64,
    pub artist_folder: String,
    pub folder: String,
    pub name: String,
    pub facts_hash: String,
}

pub async fn stored_albums(conn: &mut PgConnection) -> Result<Vec<StoredAlbum>, sqlx::Error> {
    sqlx::query_as::<_, StoredAlbum>(
        "SELECT a.id, ar.folder AS artist_folder, a.folder, a.name, a.facts_hash
         FROM music_albums a JOIN music_artists ar ON ar.id = a.artist_id",
    )
    .fetch_all(conn)
    .await
}

pub async fn upsert_artist(
    conn: &mut PgConnection,
    folder: &str,
    sort_name: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO music_artists (folder, name, sort_name) VALUES ($1, $2, $3)
         ON CONFLICT (folder) DO UPDATE SET name = EXCLUDED.name, sort_name = EXCLUDED.sort_name
         RETURNING id",
    )
    .bind(folder)
    .bind(folder.trim())
    .bind(sort_name)
    .fetch_one(conn)
    .await
}

pub async fn insert_album(
    conn: &mut PgConnection,
    artist_id: i64,
    a: &ScannedAlbum,
    hash: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO music_albums (artist_id, folder, name, name_key, year, track_count, size_bytes,
                                  formats, cover_path, other_files, facts_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(artist_id)
    .bind(&a.folder)
    .bind(&a.name)
    .bind(&a.name_key)
    .bind(a.year)
    .bind(i32::try_from(a.tracks.len()).unwrap_or(i32::MAX))
    .bind(a.size_bytes())
    .bind(a.formats())
    .bind(&a.cover_path)
    .bind(a.other_files)
    .bind(hash)
    .fetch_one(conn)
    .await
}

pub async fn update_album(
    conn: &mut PgConnection,
    id: i64,
    a: &ScannedAlbum,
    hash: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE music_albums SET name = $2, name_key = $3, year = $4, track_count = $5,
                size_bytes = $6, formats = $7, cover_path = $8, other_files = $9, facts_hash = $10,
                updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&a.name)
    .bind(&a.name_key)
    .bind(a.year)
    .bind(i32::try_from(a.tracks.len()).unwrap_or(i32::MAX))
    .bind(a.size_bytes())
    .bind(a.formats())
    .bind(&a.cover_path)
    .bind(a.other_files)
    .bind(hash)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Replace an album's tracks with these, in order.
pub async fn replace_tracks(
    conn: &mut PgConnection,
    album_id: i64,
    tracks: &[ScannedTrack],
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM music_tracks WHERE album_id = $1")
        .bind(album_id)
        .execute(&mut *conn)
        .await?;
    if tracks.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = tracks.iter().map(|t| t.file_name.as_str()).collect();
    let numbers: Vec<Option<i32>> = tracks.iter().map(|t| t.track_number).collect();
    let titles: Vec<Option<&str>> = tracks.iter().map(|t| t.title.as_deref()).collect();
    let formats: Vec<&str> = tracks.iter().map(|t| t.format.as_str()).collect();
    let sizes: Vec<i64> = tracks.iter().map(|t| t.size_bytes).collect();
    sqlx::query(
        "INSERT INTO music_tracks (album_id, file_name, track_number, title, format, size_bytes)
         SELECT $1, t.file_name, t.track_number, t.title, t.format, t.size_bytes
         FROM UNNEST($2::text[], $3::integer[], $4::text[], $5::text[], $6::bigint[])
              WITH ORDINALITY AS t(file_name, track_number, title, format, size_bytes, ord)
         ORDER BY t.ord",
    )
    .bind(album_id)
    .bind(names)
    .bind(numbers)
    .bind(titles)
    .bind(formats)
    .bind(sizes)
    .execute(&mut *conn)
    .await
    .map(|_| ())
}

pub async fn delete_album(conn: &mut PgConnection, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM music_albums WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Artists left without an album.
pub async fn delete_empty_artists(conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "DELETE FROM music_artists ar
         WHERE NOT EXISTS (SELECT 1 FROM music_albums a WHERE a.artist_id = ar.id)",
    )
    .execute(conn)
    .await
    .map(|r| r.rows_affected())
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
        "INSERT INTO music_scan (id, root, ok, last_scan_at, last_ok_at, took_ms, error, counts)
         VALUES (1, $1, $2, now(), CASE WHEN $2 THEN now() END, $3, $4, $5)
         ON CONFLICT (id) DO UPDATE SET
             root = EXCLUDED.root, ok = EXCLUDED.ok, last_scan_at = now(),
             last_ok_at = COALESCE(EXCLUDED.last_ok_at, music_scan.last_ok_at),
             took_ms = EXCLUDED.took_ms, error = EXCLUDED.error,
             counts = CASE WHEN EXCLUDED.ok THEN EXCLUDED.counts ELSE music_scan.counts END",
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
        "SELECT root, ok, last_scan_at, last_ok_at, took_ms, error, counts FROM music_scan WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
}

// The media server's music ----------------------------------------------------------------------

/// A list joined for the database: `string_to_array` splits it again.
fn joined(list: &[String]) -> String {
    list.join("\u{1f}")
}

/// Replace the server's music with what this pass read.
pub async fn replace_server_music(
    conn: &mut PgConnection,
    server: &str,
    artists: &[crate::engines::ServerArtist],
    albums: &[crate::engines::ServerAlbum],
    tracks: &[crate::engines::ServerTrack],
) -> Result<(), sqlx::Error> {
    for sql in [
        "DELETE FROM server_tracks WHERE server = $1",
        "DELETE FROM server_albums WHERE server = $1",
        "DELETE FROM server_artists WHERE server = $1",
    ] {
        sqlx::query(sql).bind(server).execute(&mut *conn).await?;
    }
    sqlx::query(
        "INSERT INTO server_artists (server, rating_key, name, genres, countries, summary, thumb,
                                     added_at)
         SELECT $1, k, n, string_to_array(g, chr(31)), string_to_array(c, chr(31)), s, t, a
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::text[], $7::text[],
                     $8::timestamptz[]) AS x(k, n, g, c, s, t, a)
         ON CONFLICT DO NOTHING",
    )
    .bind(server)
    .bind(
        artists
            .iter()
            .map(|a| a.rating_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind(artists.iter().map(|a| a.name.clone()).collect::<Vec<_>>())
    .bind(
        artists
            .iter()
            .map(|a| joined(&a.genres))
            .collect::<Vec<_>>(),
    )
    .bind(
        artists
            .iter()
            .map(|a| joined(&a.countries))
            .collect::<Vec<_>>(),
    )
    .bind(
        artists
            .iter()
            .map(|a| a.summary.clone())
            .collect::<Vec<_>>(),
    )
    .bind(artists.iter().map(|a| a.thumb.clone()).collect::<Vec<_>>())
    .bind(artists.iter().map(|a| a.added).collect::<Vec<_>>())
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO server_albums (server, rating_key, artist_key, artist, title, year, released,
                                    genres, label, thumb, added_at, plays)
         SELECT $1, k, ak, ar, ti, y, r, string_to_array(g, chr(31)), l, th, a, p
         FROM UNNEST($2::text[], $3::text[], $4::text[], $5::text[], $6::int4[], $7::text[],
                     $8::text[], $9::text[], $10::text[], $11::timestamptz[], $12::int8[])
              AS x(k, ak, ar, ti, y, r, g, l, th, a, p)
         ON CONFLICT DO NOTHING",
    )
    .bind(server)
    .bind(
        albums
            .iter()
            .map(|a| a.rating_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind(
        albums
            .iter()
            .map(|a| a.artist_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind(albums.iter().map(|a| a.artist.clone()).collect::<Vec<_>>())
    .bind(albums.iter().map(|a| a.title.clone()).collect::<Vec<_>>())
    .bind(albums.iter().map(|a| a.year).collect::<Vec<_>>())
    .bind(
        albums
            .iter()
            .map(|a| a.released.clone())
            .collect::<Vec<_>>(),
    )
    .bind(albums.iter().map(|a| joined(&a.genres)).collect::<Vec<_>>())
    .bind(albums.iter().map(|a| a.label.clone()).collect::<Vec<_>>())
    .bind(albums.iter().map(|a| a.thumb.clone()).collect::<Vec<_>>())
    .bind(albums.iter().map(|a| a.added).collect::<Vec<_>>())
    .bind(albums.iter().map(|a| a.plays).collect::<Vec<_>>())
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "INSERT INTO server_tracks (server, rating_key, album_key, number, disc, title, artist,
                                    duration_ms, codec, bitrate, channels, container, size_bytes,
                                    file)
         SELECT $1, k, ak, n, d, ti, ar, ms, co, br, ch, ct, sz, f
         FROM UNNEST($2::text[], $3::text[], $4::int4[], $5::int4[], $6::text[], $7::text[],
                     $8::int8[], $9::text[], $10::int8[], $11::int8[], $12::text[], $13::int8[],
                     $14::text[])
              AS x(k, ak, n, d, ti, ar, ms, co, br, ch, ct, sz, f)
         ON CONFLICT DO NOTHING",
    )
    .bind(server)
    .bind(
        tracks
            .iter()
            .map(|t| t.rating_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind(
        tracks
            .iter()
            .map(|t| t.album_key.clone())
            .collect::<Vec<_>>(),
    )
    .bind(tracks.iter().map(|t| t.number).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.disc).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.title.clone()).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.artist.clone()).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.duration_ms).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.codec.clone()).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.bitrate).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.channels).collect::<Vec<_>>())
    .bind(
        tracks
            .iter()
            .map(|t| t.container.clone())
            .collect::<Vec<_>>(),
    )
    .bind(tracks.iter().map(|t| t.size).collect::<Vec<_>>())
    .bind(tracks.iter().map(|t| t.file.clone()).collect::<Vec<_>>())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One album of the server with what its tracks add up to.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ServerAlbumRow {
    pub rating_key: String,
    pub artist_key: Option<String>,
    pub artist: Option<String>,
    pub title: String,
    pub year: Option<i32>,
    /// `YYYY-MM-DD`, as the server has it.
    pub released: Option<String>,
    pub genres: Vec<String>,
    pub label: Option<String>,
    pub thumb: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
    pub plays: i64,
    pub tracks: i64,
    pub duration_ms: i64,
    pub size_bytes: i64,
    pub codecs: Vec<String>,
    /// The tracks' average, lowest and highest bitrate, kbit/s.
    pub bitrate: Option<i64>,
    pub min_bitrate: Option<i64>,
    pub max_bitrate: Option<i64>,
    /// Each track's disc and number, in one order.
    pub discs: Vec<Option<i32>>,
    pub numbers: Vec<Option<i32>>,
    /// One track's file, for the album's folder.
    pub file: Option<String>,
}

/// The albums of the server, or the one with this rating key.
pub async fn server_albums_where(
    pool: &PgPool,
    rating_key: Option<&str>,
) -> Result<Vec<ServerAlbumRow>, sqlx::Error> {
    sqlx::query_as::<_, ServerAlbumRow>(
        "SELECT a.rating_key, a.artist_key, a.artist, a.title, a.year, a.released, a.genres,
                a.label, a.thumb, a.added_at, a.plays, count(t.rating_key) AS tracks,
                COALESCE(sum(t.duration_ms), 0)::bigint AS duration_ms,
                COALESCE(sum(t.size_bytes), 0)::bigint AS size_bytes,
                COALESCE(array_agg(DISTINCT t.codec) FILTER (WHERE t.codec IS NOT NULL), '{}')
                    AS codecs,
                avg(t.bitrate)::bigint AS bitrate,
                min(t.bitrate) AS min_bitrate,
                max(t.bitrate) AS max_bitrate,
                COALESCE(array_agg(t.disc ORDER BY t.rating_key)
                         FILTER (WHERE t.rating_key IS NOT NULL), '{}') AS discs,
                COALESCE(array_agg(t.number ORDER BY t.rating_key)
                         FILTER (WHERE t.rating_key IS NOT NULL), '{}') AS numbers,
                min(t.file) AS file
         FROM server_albums a
         LEFT JOIN server_tracks t ON t.server = a.server AND t.album_key = a.rating_key
         WHERE ($1::text IS NULL OR a.rating_key = $1)
         GROUP BY a.server, a.rating_key
         ORDER BY lower(coalesce(a.artist, '')), a.year NULLS LAST, lower(a.title)",
    )
    .bind(rating_key)
    .fetch_all(pool)
    .await
}

pub async fn server_albums(pool: &PgPool) -> Result<Vec<ServerAlbumRow>, sqlx::Error> {
    server_albums_where(pool, None).await
}

/// One artist of the server with their albums, tracks and size.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ServerArtistRow {
    pub rating_key: String,
    pub name: String,
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub summary: Option<String>,
    pub thumb: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
    pub albums: i64,
    pub tracks: i64,
    pub size_bytes: i64,
}

pub async fn server_artists(pool: &PgPool) -> Result<Vec<ServerArtistRow>, sqlx::Error> {
    sqlx::query_as::<_, ServerArtistRow>(
        "SELECT ar.rating_key, ar.name, ar.genres, ar.countries, ar.summary, ar.thumb,
                ar.added_at,
                (SELECT count(*) FROM server_albums a
                 WHERE a.server = ar.server AND a.artist_key = ar.rating_key) AS albums,
                (SELECT count(*) FROM server_tracks t JOIN server_albums a
                     ON a.server = t.server AND a.rating_key = t.album_key
                 WHERE a.server = ar.server AND a.artist_key = ar.rating_key) AS tracks,
                COALESCE((SELECT sum(t.size_bytes) FROM server_tracks t JOIN server_albums a
                     ON a.server = t.server AND a.rating_key = t.album_key
                 WHERE a.server = ar.server AND a.artist_key = ar.rating_key), 0)::bigint
                    AS size_bytes
         FROM server_artists ar ORDER BY lower(ar.name)",
    )
    .fetch_all(pool)
    .await
}

/// One track of a server album.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ServerTrackRow {
    pub number: Option<i32>,
    pub disc: Option<i32>,
    pub title: String,
    pub artist: Option<String>,
    pub duration_ms: Option<i64>,
    pub codec: Option<String>,
    pub bitrate: Option<i64>,
    pub size_bytes: Option<i64>,
    pub file: Option<String>,
}

/// An album's tracks in disc and track order.
pub async fn server_album_tracks(
    pool: &PgPool,
    album_key: &str,
) -> Result<Vec<ServerTrackRow>, sqlx::Error> {
    sqlx::query_as::<_, ServerTrackRow>(
        "SELECT number, disc, title, artist, duration_ms, codec, bitrate, size_bytes, file
         FROM server_tracks WHERE album_key = $1
         ORDER BY disc NULLS FIRST, number NULLS LAST, title, rating_key",
    )
    .bind(album_key)
    .fetch_all(pool)
    .await
}

/// The server's image path of an album (`album`) or an artist (`artist`).
pub async fn server_thumb(
    pool: &PgPool,
    kind: &str,
    rating_key: &str,
) -> Result<Option<String>, sqlx::Error> {
    let sql = if kind == "artist" {
        "SELECT thumb FROM server_artists WHERE rating_key = $1 ORDER BY server LIMIT 1"
    } else {
        "SELECT thumb FROM server_albums WHERE rating_key = $1 ORDER BY server LIMIT 1"
    };
    Ok(sqlx::query_scalar::<_, Option<String>>(sql)
        .bind(rating_key)
        .fetch_optional(pool)
        .await?
        .flatten())
}

/// Every file the server's music names.
pub async fn server_track_files(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT file FROM server_tracks WHERE file IS NOT NULL")
        .fetch_all(pool)
        .await
}

/// What the media server's music holds, over all its music sections.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct MusicTotals {
    pub albums: i64,
    pub artists: i64,
    pub tracks: i64,
    pub size_bytes: i64,
}

/// The server's music totals; `None` until a server pass has read any music.
pub async fn server_totals(pool: &PgPool) -> Result<Option<MusicTotals>, sqlx::Error> {
    let t = sqlx::query_as::<_, MusicTotals>(
        "SELECT (SELECT count(*) FROM server_albums) AS albums,
                (SELECT count(*) FROM server_artists) AS artists,
                (SELECT count(*) FROM server_tracks) AS tracks,
                (SELECT COALESCE(sum(size_bytes), 0) FROM server_tracks)::bigint AS size_bytes",
    )
    .fetch_one(pool)
    .await?;
    Ok((t.albums + t.artists + t.tracks > 0).then_some(t))
}

/// Every audio file of the music folder, as `album folder/file name` under the root.
pub async fn folder_track_paths(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT ar.folder || '/' || al.folder || '/' || t.file_name
         FROM music_tracks t
         JOIN music_albums al ON al.id = t.album_id
         JOIN music_artists ar ON ar.id = al.artist_id
         ORDER BY 1",
    )
    .fetch_all(pool)
    .await
}
