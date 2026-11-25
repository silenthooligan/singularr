//! SQL over the catalog's tables: `libraries`, `titles`, `title_engines`, `seasons`,
//! `episodes`, `copies`, `title_editions`, `untracked_files` and `movie_folder_scan`.

use super::ingest::{self, Assessment, EngineRef, FileFacts, TitleFacts};
use super::model::{
    ArrReading, CatalogCounts, CopyEdition, CopyQuery, CopyView, DiffCounts, EngineFacts,
    FolderScanStatus, Library, Season, SingularrReading, TitleAbout, TitleQuery, TitleSummary,
    VerdictView,
};
use crate::core::model::Paging;
use chrono::{DateTime, Utc};
use singularr_core::ParsedRelease;
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

// Reads for the API ------------------------------------------------------------------------------

pub async fn libraries(pool: &PgPool) -> Result<Vec<Library>, sqlx::Error> {
    sqlx::query_as::<_, Library>(
        "SELECT l.id, l.name, l.kind, l.integration, l.section_key, l.managed_by, l.locations,
                l.root_folders,
                COALESCE((SELECT array_agg(DISTINCT te.engine ORDER BY te.engine)
                          FROM title_engines te WHERE te.library_id = l.id), '{}') AS engines,
                (SELECT count(DISTINCT te.title_id) FROM title_engines te
                 WHERE te.library_id = l.id) AS titles,
                (SELECT count(*) FROM copies c WHERE c.library_id = l.id) AS copies,
                COALESCE((SELECT sum(c.size_bytes) FROM copies c
                          WHERE c.library_id = l.id), 0)::bigint AS size_bytes
         FROM libraries l ORDER BY (l.section_key IS NULL), l.kind NULLS LAST, l.name, l.id",
    )
    .fetch_all(pool)
    .await
}

fn title_filters(
    qb: &mut QueryBuilder<Postgres>,
    q: &TitleQuery,
    uhd_lanes: &[String],
    edition_key: Option<&str>,
) {
    qb.push(" WHERE true");
    if let Some(kind) = q.kind.as_deref().and_then(super::model::TitleKind::parse) {
        qb.push(" AND t.kind = ").push_bind(kind.as_str());
    }
    if let Some(library) = q.library.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND EXISTS (SELECT 1 FROM title_engines te WHERE te.title_id = t.id AND te.library_id = ")
            .push_bind(library.to_string())
            .push(")");
    }
    if let Some(lane) = q.lane.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND (EXISTS (SELECT 1 FROM copies c WHERE c.title_id = t.id AND c.lane = ")
            .push_bind(lane.to_string())
            .push(") OR EXISTS (SELECT 1 FROM title_engines te WHERE te.title_id = t.id AND te.lane = ")
            .push_bind(lane.to_string())
            .push("))");
    }
    if let Some(protected) = q.protected {
        qb.push(if protected {
            " AND EXISTS"
        } else {
            " AND NOT EXISTS"
        });
        qb.push(
            " (SELECT 1 FROM copies c WHERE c.title_id = t.id AND cardinality(c.protections) > 0)",
        );
    }
    if let Some(has_uhd) = q.has_uhd {
        qb.push(if has_uhd {
            " AND EXISTS"
        } else {
            " AND NOT EXISTS"
        });
        qb.push(" (SELECT 1 FROM copies c WHERE c.title_id = t.id AND c.lane = ANY(")
            .push_bind(uhd_lanes.to_vec())
            .push("))");
    }
    if let Some(key) = edition_key {
        // An edition the title holds: a copy of it, or a file on disk no engine tracks.
        qb.push(" AND (EXISTS (SELECT 1 FROM copies c WHERE c.title_id = t.id AND c.edition_key = ")
            .push_bind(key.to_string())
            .push(") OR EXISTS (SELECT 1 FROM untracked_files u WHERE u.title_id = t.id AND u.edition_key = ")
            .push_bind(key.to_string())
            .push("))");
    } else if let Some(edition) = q
        .edition
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        // A name the edition list does not know: the edition names of the copy as held.
        qb.push(
            " AND EXISTS (SELECT 1 FROM copies c, jsonb_array_elements_text(c.held -> 'editions') AS e(name)
                          WHERE c.title_id = t.id AND lower(e.name) = lower(",
        )
        .push_bind(edition.to_string())
        .push("))");
    }
    if let Some(source) = q
        .quality
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        // The source part of the quality the copy is scored as: `webdl` of `WEBDL-1080p`.
        qb.push(
            " AND EXISTS (SELECT 1 FROM copies c WHERE c.title_id = t.id
                          AND split_part(lower(COALESCE(c.verdict ->> 'quality', c.arr_quality, '')), '-', 1) = ",
        )
        .push_bind(source.to_ascii_lowercase())
        .push(")");
    }
    if let Some(missing) = q.missing {
        qb.push(if missing { " AND (" } else { " AND NOT (" });
        qb.push(
            "NOT EXISTS (SELECT 1 FROM copies c WHERE c.title_id = t.id)
             OR EXISTS (SELECT 1 FROM title_engines te WHERE te.title_id = t.id
                        AND te.episode_count IS NOT NULL AND te.file_count < te.episode_count))",
        );
    }
    if let Some(status) = q.status.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        qb.push(" AND lower(t.status) = ")
            .push_bind(status.to_ascii_lowercase());
    }
    if let Some(airing) = q.airing {
        qb.push(if airing {
            " AND t.next_airing > now()"
        } else {
            " AND (t.next_airing IS NULL OR t.next_airing <= now())"
        });
    }
    if let Some(text) = q.q.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        let lower = text.to_ascii_lowercase();
        let id_of = |prefix: &str| {
            lower
                .strip_prefix(prefix)
                .and_then(|n| n.parse::<i64>().ok())
        };
        if let Some(id) = id_of("tmdb:") {
            qb.push(" AND t.tmdb_id = ").push_bind(id);
        } else if let Some(id) = id_of("tvdb:") {
            qb.push(" AND t.tvdb_id = ").push_bind(id);
        } else if lower.starts_with("tt")
            && lower[2..].chars().all(|c| c.is_ascii_digit())
            && lower.len() > 2
        {
            qb.push(" AND t.imdb_id = ").push_bind(lower);
        } else {
            // Every word must appear in the title; LIKE wildcards in the input are literal.
            for word in text.split_whitespace() {
                let escaped = word
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                qb.push(" AND t.title ILIKE ")
                    .push_bind(format!("%{escaped}%"));
            }
        }
    }
}

const TITLE_SUMMARY: &str = "SELECT t.id, t.kind, t.title, t.year, t.poster_url, t.status, t.tmdb_id, t.tvdb_id, t.imdb_id,
        t.overview, t.genres, t.runtime_minutes, t.certification, t.studio, t.network, t.collection,
        t.collection_tmdb_id, t.original_title, t.rating, t.imdb_rating, t.fanart_url, t.banner_url, t.first_aired, t.next_airing,
        t.in_cinemas, t.digital_release,
        CASE WHEN t.kind = 'show' THEN (SELECT count(*) FROM seasons se
                  WHERE se.title_id = t.id AND se.season_number > 0)::int END AS season_count,
        (SELECT max(te.episode_count) FROM title_engines te WHERE te.title_id = t.id) AS episode_count,
        CASE WHEN t.kind = 'show' THEN (SELECT max(te.file_count) FROM title_engines te
                  WHERE te.title_id = t.id) END AS episode_file_count,
        COALESCE((SELECT array_agg(DISTINCT te.library_id ORDER BY te.library_id)
                  FROM title_engines te WHERE te.title_id = t.id), '{}') AS libraries,
        COALESCE((SELECT array_agg(DISTINCT te.lane ORDER BY te.lane)
                  FROM title_engines te WHERE te.title_id = t.id AND te.lane IS NOT NULL), '{}') AS lanes,
        s.copies, s.size_bytes, s.protected,
        (SELECT min(te.added_at) FROM title_engines te WHERE te.title_id = t.id) AS added_at,
        s.best_score, s.newest_copy_at
     FROM titles t
     LEFT JOIN LATERAL (
        SELECT count(*) AS copies, COALESCE(sum(c.size_bytes), 0)::bigint AS size_bytes,
               COALESCE(bool_or(cardinality(c.protections) > 0), false) AS protected,
               max(c.mq_score) AS best_score, max(c.date_added) AS newest_copy_at
        FROM copies c WHERE c.title_id = t.id
     ) s ON true";

/// The sorts of the title list: the column, and whether its natural order is descending.
/// The column comes from this table, never from the request text.
pub fn title_sort(name: &str) -> Option<(&'static str, bool)> {
    Some(match name {
        "added" => ("s.newest_copy_at", true),
        "title" => ("t.sort_title", false),
        "year" => ("t.year", true),
        "score" => ("s.best_score", true),
        "size" => ("s.size_bytes", true),
        "copies" => ("s.copies", true),
        "next" => ("t.next_airing", false),
        _ => return None,
    })
}

/// The lead copy of each title: the highest score, then the newest file.
pub async fn lead_copies(
    pool: &PgPool,
    title_ids: &[i64],
) -> Result<Vec<super::model::LeadCopy>, sqlx::Error> {
    if title_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, super::model::LeadCopy>(
        "SELECT DISTINCT ON (c.title_id)
                c.id AS copy_id, c.title_id, c.library_id AS library, c.lane,
                COALESCE(c.verdict ->> 'quality', c.arr_quality) AS quality,
                COALESCE(c.held ->> 'group', c.arr_group) AS \"group\",
                COALESCE(c.held -> 'editions' ->> 0, NULLIF(c.arr_edition, '')) AS edition,
                c.mq_score AS score, c.size_bytes, c.date_added, c.protections
         FROM copies c
         WHERE c.title_id = ANY($1)
         ORDER BY c.title_id, c.mq_score DESC NULLS LAST, c.date_added DESC NULLS LAST, c.id",
    )
    .bind(title_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// Totals over the titles of one kind: how many, how many hold a file, episodes for shows,
/// and the size of their copies.
pub async fn kind_totals(
    pool: &PgPool,
    kind: &str,
) -> Result<super::model::KindTotals, sqlx::Error> {
    sqlx::query_as::<_, super::model::KindTotals>(
        "SELECT count(*)::bigint AS titles,
                (count(*) FILTER (WHERE s.copies > 0))::bigint AS with_file,
                COALESCE(sum(e.episodes), 0)::bigint AS episodes,
                COALESCE(sum(e.files), 0)::bigint AS episode_files,
                COALESCE(sum(s.size), 0)::bigint AS size_bytes
         FROM titles t
         LEFT JOIN LATERAL (
            SELECT count(*) AS copies, COALESCE(sum(c.size_bytes), 0) AS size
            FROM copies c WHERE c.title_id = t.id
         ) s ON true
         LEFT JOIN LATERAL (
            SELECT max(te.episode_count) AS episodes, max(te.file_count) AS files
            FROM title_engines te WHERE te.title_id = t.id
         ) e ON true
         WHERE t.kind = $1",
    )
    .bind(kind)
    .fetch_one(pool)
    .await
}

/// One row per season, quality and group of a title's files.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SeasonFileRow {
    pub season_number: i32,
    pub quality: Option<String>,
    pub release_group: Option<String>,
    pub files: i64,
    pub size_bytes: i64,
    pub last_added: Option<DateTime<Utc>>,
}

pub async fn season_files(pool: &PgPool, title_id: i64) -> Result<Vec<SeasonFileRow>, sqlx::Error> {
    sqlx::query_as::<_, SeasonFileRow>(
        "SELECT c.season_number,
                COALESCE(c.verdict ->> 'quality', c.arr_quality) AS quality,
                COALESCE(c.held ->> 'group', c.arr_group) AS release_group,
                count(*)::bigint AS files,
                COALESCE(sum(c.size_bytes), 0)::bigint AS size_bytes,
                max(c.date_added) AS last_added
         FROM copies c
         WHERE c.title_id = $1 AND c.season_number IS NOT NULL
         GROUP BY 1, 2, 3
         ORDER BY 1, 4 DESC",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

/// The names of a title's episodes, read from the library file names.
pub async fn episode_names(
    pool: &PgPool,
    title_id: i64,
    season: Option<i32>,
) -> Result<Vec<super::model::EpisodeName>, sqlx::Error> {
    sqlx::query_as::<_, super::model::EpisodeName>(
        "SELECT season_number, episode_number, name FROM episodes
         WHERE title_id = $1 AND ($2::int IS NULL OR season_number = $2)
         ORDER BY season_number, episode_number",
    )
    .bind(title_id)
    .bind(season)
    .fetch_all(pool)
    .await
}

/// Copies held per lane.
pub async fn lane_copy_counts(pool: &PgPool) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT lane, count(*)::bigint FROM copies WHERE lane IS NOT NULL GROUP BY lane",
    )
    .fetch_all(pool)
    .await
}

/// Titles with at least one copy guarded by each protection.
pub async fn protection_title_counts(pool: &PgPool) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT p.id, count(DISTINCT c.title_id)::bigint
         FROM copies c, unnest(c.protections) AS p(id)
         GROUP BY p.id",
    )
    .fetch_all(pool)
    .await
}

/// Titles per lane (a profile in the interface), as the engines place them.
pub async fn lane_title_counts(pool: &PgPool) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT lane, count(DISTINCT title_id)::bigint FROM title_engines
         WHERE lane IS NOT NULL GROUP BY lane ORDER BY lane",
    )
    .fetch_all(pool)
    .await
}

/// `edition_key`: the `edition` filter as a key of the edition list, when it names one.
pub async fn list_titles(
    pool: &PgPool,
    q: &TitleQuery,
    uhd_lanes: &[String],
    edition_key: Option<&str>,
    paging: Paging,
) -> Result<(Vec<TitleSummary>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*) FROM titles t");
    title_filters(&mut count, q, uhd_lanes, edition_key);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut list = QueryBuilder::<Postgres>::new(TITLE_SUMMARY);
    title_filters(&mut list, q, uhd_lanes, edition_key);
    // `sort=added` is the default. A leading `-` asks for descending; `order` decides when
    // it is given.
    let sort = q.sort.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let (name, minus) = match sort.and_then(|s| s.strip_prefix('-')) {
        Some(rest) => (rest, true),
        None => (sort.unwrap_or("added"), false),
    };
    let (column, natural_desc) = title_sort(name).unwrap_or(("s.newest_copy_at", true));
    let descending = match q.order.as_deref().map(str::to_ascii_lowercase).as_deref() {
        Some("asc") => false,
        Some("desc") => true,
        _ => minus || natural_desc,
    };
    list.push(" ORDER BY ").push(column);
    list.push(if descending {
        " DESC NULLS LAST"
    } else {
        " ASC NULLS LAST"
    });
    list.push(", t.sort_title ASC, t.id ASC LIMIT ")
        .push_bind(paging.limit())
        .push(" OFFSET ")
        .push_bind(paging.offset());
    let rows = list
        .build_query_as::<TitleSummary>()
        .fetch_all(pool)
        .await?;
    Ok((rows, total))
}

/// A catalog title found by an outside id, with the number of files it holds.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleLink {
    pub id: i64,
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub copies: i64,
}

/// The titles with one of these TMDB or TVDB ids. Requests and the discover lists link to
/// the catalog through this.
pub async fn title_links(
    pool: &PgPool,
    tmdb_ids: &[i64],
    tvdb_ids: &[i64],
) -> Result<Vec<TitleLink>, sqlx::Error> {
    if tmdb_ids.is_empty() && tvdb_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, TitleLink>(
        "SELECT t.id, t.kind, t.tmdb_id, t.tvdb_id,
                (SELECT count(*) FROM copies c WHERE c.title_id = t.id) AS copies
         FROM titles t
         WHERE t.tmdb_id = ANY($1) OR t.tvdb_id = ANY($2)
         ORDER BY t.id",
    )
    .bind(tmdb_ids.to_vec())
    .bind(tvdb_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// A title as the pages link it: its kind and TMDB id (`movie-603`), its name and year.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleRef {
    pub id: i64,
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub year: Option<i32>,
}

/// The titles with these catalog ids. The Journal names its subjects through this.
pub async fn title_refs(pool: &PgPool, ids: &[i64]) -> Result<Vec<TitleRef>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, TitleRef>(
        "SELECT id, kind, tmdb_id, title, year FROM titles WHERE id = ANY($1) ORDER BY id",
    )
    .bind(ids.to_vec())
    .fetch_all(pool)
    .await
}

/// The lanes a title holds a copy in, with the library of each.
pub async fn title_lanes(
    pool: &PgPool,
    title_id: i64,
) -> Result<Vec<(String, String)>, sqlx::Error> {
    sqlx::query_as::<_, (String, String)>(
        "SELECT DISTINCT lane, library_id FROM copies WHERE title_id = $1 AND lane IS NOT NULL",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TitleRow {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    pub original_language: Option<String>,
    pub poster_url: Option<String>,
    pub status: Option<String>,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    #[sqlx(flatten)]
    pub about: TitleAbout,
}

pub async fn title(pool: &PgPool, id: i64) -> Result<Option<TitleRow>, sqlx::Error> {
    sqlx::query_as::<_, TitleRow>(
        "SELECT id, kind, title, year, original_language, poster_url, status, tmdb_id, tvdb_id, imdb_id,
                overview, genres, runtime_minutes, certification, studio, network, collection,
                collection_tmdb_id, original_title, rating, imdb_rating, fanart_url, banner_url,
                first_aired, next_airing, in_cinemas, digital_release
         FROM titles WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn title_engines(pool: &PgPool, title_id: i64) -> Result<Vec<EngineFacts>, sqlx::Error> {
    sqlx::query_as::<_, EngineFacts>(
        "SELECT engine, library_id AS library, engine_title_id, path, monitored, profile, lane, tags,
                status, added_at, file_count, size_bytes, episode_count, total_episode_count,
                seasons
         FROM title_engines WHERE title_id = $1 ORDER BY library_id DESC, engine",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

pub async fn title_seasons(pool: &PgPool, title_id: i64) -> Result<Vec<Season>, sqlx::Error> {
    sqlx::query_as::<_, Season>(
        "SELECT s.season_number, s.episode_count, s.total_episode_count, s.episode_file_count,
                (SELECT count(*) FROM copies c
                 WHERE c.title_id = s.title_id AND c.season_number = s.season_number) AS copies
         FROM seasons s WHERE s.title_id = $1 ORDER BY s.season_number",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

/// A stored copy with its title, as the database holds it.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CopyRow {
    pub id: i64,
    pub title_id: i64,
    pub title: String,
    pub kind: String,
    pub year: Option<i32>,
    pub library_id: String,
    pub engine: String,
    pub engine_title_id: i64,
    pub engine_file_id: i64,
    pub lane: Option<String>,
    pub season_number: Option<i32>,
    pub episode_numbers: Vec<i32>,
    pub relative_path: String,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    pub release_name: String,
    pub name_source: String,
    pub size_bytes: i64,
    pub date_added: Option<DateTime<Utc>>,
    pub arr_quality: Option<String>,
    pub arr_group: Option<String>,
    pub arr_edition: Option<String>,
    pub arr_custom_formats: Vec<String>,
    pub arr_score: Option<i32>,
    pub languages: Vec<String>,
    pub media_info: Option<serde_json::Value>,
    pub parsed: serde_json::Value,
    pub held: serde_json::Value,
    pub verdict: Option<serde_json::Value>,
    pub mq_quality: Option<String>,
    pub mq_group: Option<String>,
    pub mq_edition: Option<String>,
    pub mq_score: Option<i32>,
    pub diff_quality: bool,
    pub diff_group: bool,
    pub diff_edition: bool,
    pub diff_score: bool,
    pub protections: Vec<String>,
    pub arr_only_formats: Vec<String>,
    pub mq_only_traits: Vec<String>,
    pub arr_proper: bool,
    pub arr_repack: bool,
    /// The title's original language.
    pub original_language: Option<String>,
    pub assessed_with: String,
    pub first_seen_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub edition_key: String,
    pub edition_label: String,
    pub edition_marked: bool,
    pub edition_rank: i32,
}

impl CopyRow {
    /// The stored parse. A row written by an older build whose JSON no longer reads is
    /// parsed again from its name, which always works.
    pub fn parsed(&self) -> ParsedRelease {
        serde_json::from_value(self.parsed.clone())
            .unwrap_or_else(|_| singularr_core::release::parse(&self.release_name))
    }

    /// The release as it was scored. A row whose JSON no longer reads falls back to the
    /// plain parse until the copy is assessed again.
    pub fn held(&self) -> ParsedRelease {
        serde_json::from_value(self.held.clone()).unwrap_or_else(|_| self.parsed())
    }

    pub fn verdict(&self) -> Option<VerdictView> {
        self.verdict
            .clone()
            .and_then(|v| serde_json::from_value(v).ok())
    }

    /// The file facts the copy was built from, to assess it again.
    pub fn file_facts(&self) -> FileFacts {
        FileFacts {
            engine_file_id: self.engine_file_id,
            season_number: self.season_number,
            episode_numbers: self.episode_numbers.clone(),
            episode_name: None,
            relative_path: self.relative_path.clone(),
            scene_name: self.scene_name.clone(),
            original_path: self.original_path.clone(),
            size_bytes: self.size_bytes,
            date_added: self.date_added,
            arr_quality: self.arr_quality.clone(),
            arr_group: self.arr_group.clone(),
            arr_edition: self.arr_edition.clone(),
            arr_proper: self.arr_proper,
            arr_repack: self.arr_repack,
            arr_custom_formats: self.arr_custom_formats.clone(),
            arr_score: self.arr_score,
            languages: self.languages.clone(),
            media_info: self.media_info.clone(),
        }
    }

    pub fn view(self) -> CopyView {
        let parsed = self.parsed();
        let held = self.held();
        let verdict = self.verdict();
        let differs = [
            (self.diff_quality, "quality"),
            (self.diff_group, "group"),
            (self.diff_edition, "edition"),
            (self.diff_score, "score"),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, name)| name.to_string())
        .collect();
        CopyView {
            id: self.id,
            title_id: self.title_id,
            title: self.title,
            kind: self.kind,
            year: self.year,
            library: self.library_id,
            engine: self.engine,
            lane: self.lane,
            season_number: self.season_number,
            episode_numbers: self.episode_numbers,
            relative_path: self.relative_path,
            scene_name: self.scene_name,
            original_path: self.original_path,
            release_name: self.release_name,
            name_source: self.name_source,
            size_bytes: self.size_bytes,
            date_added: self.date_added,
            languages: self.languages,
            media_info: self.media_info,
            arr: ArrReading {
                quality: self.arr_quality,
                group: self.arr_group,
                edition: self.arr_edition,
                custom_formats: self.arr_custom_formats,
                score: self.arr_score,
            },
            singularr: SingularrReading {
                quality: self.mq_quality,
                group: self.mq_group,
                edition: self.mq_edition,
                score: self.mq_score,
            },
            differs,
            parsed,
            verdict,
            held,
            arr_only_formats: self.arr_only_formats,
            singularr_only_traits: self.mq_only_traits,
            protected: !self.protections.is_empty(),
            protections: self.protections,
            first_seen_at: self.first_seen_at,
            updated_at: self.updated_at,
            edition: CopyEdition {
                key: self.edition_key,
                label: self.edition_label,
                marked: self.edition_marked,
            },
        }
    }
}

const COPY_SELECT: &str =
    "SELECT c.id, c.title_id, t.title, t.kind, t.year, c.library_id, c.engine,
        c.engine_title_id, c.engine_file_id, c.lane, c.season_number, c.episode_numbers,
        c.relative_path, c.scene_name, c.original_path, c.release_name, c.name_source,
        c.size_bytes, c.date_added, c.arr_quality, c.arr_group, c.arr_edition,
        c.arr_custom_formats, c.arr_score, c.languages, c.media_info, c.parsed, c.verdict,
        c.mq_quality, c.mq_group, c.mq_edition, c.mq_score, c.diff_quality, c.diff_group,
        c.diff_edition, c.diff_score, c.protections, c.assessed_with, c.first_seen_at, c.updated_at,
        c.held, c.arr_only_formats, c.mq_only_traits, c.arr_proper, c.arr_repack,
        t.original_language, c.edition_key, c.edition_label, c.edition_marked, c.edition_rank
     FROM copies c JOIN titles t ON t.id = c.title_id";

pub async fn copy(pool: &PgPool, id: i64) -> Result<Option<CopyRow>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(COPY_SELECT);
    qb.push(" WHERE c.id = ").push_bind(id);
    qb.build_query_as::<CopyRow>().fetch_optional(pool).await
}

/// The same row inside a transaction, for the event that records its removal.
pub async fn copy_in(conn: &mut PgConnection, id: i64) -> Result<Option<CopyRow>, sqlx::Error> {
    let mut qb = QueryBuilder::<Postgres>::new(COPY_SELECT);
    qb.push(" WHERE c.id = ").push_bind(id);
    qb.build_query_as::<CopyRow>().fetch_optional(conn).await
}

pub async fn title_copies(
    pool: &PgPool,
    title_id: i64,
    q: &CopyQuery,
    paging: Paging,
) -> Result<(Vec<CopyRow>, i64), sqlx::Error> {
    fn filters(qb: &mut QueryBuilder<Postgres>, title_id: i64, q: &CopyQuery) {
        qb.push(" WHERE c.title_id = ").push_bind(title_id);
        if let Some(library) = q.library.as_deref().filter(|v| !v.is_empty()) {
            qb.push(" AND c.library_id = ")
                .push_bind(library.to_string());
        }
        if let Some(season) = q.season {
            qb.push(" AND c.season_number = ").push_bind(season);
        }
    }
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*) FROM copies c");
    filters(&mut count, title_id, q);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;
    let mut list = QueryBuilder::<Postgres>::new(COPY_SELECT);
    filters(&mut list, title_id, q);
    list.push(
        " ORDER BY c.library_id DESC, c.season_number NULLS FIRST, c.episode_numbers, c.id LIMIT ",
    )
    .push_bind(paging.limit())
    .push(" OFFSET ")
    .push_bind(paging.offset());
    let rows = list.build_query_as::<CopyRow>().fetch_all(pool).await?;
    Ok((rows, total))
}

/// Filters of the list of copies whose reading differs from the arr's.
#[derive(Debug, Clone, Default)]
pub struct DiffFilter {
    pub engine: Option<String>,
    /// `quality`, `group`, `edition`, `score`, or `any`. `None` lists every copy.
    pub differs: Option<String>,
    pub lane: Option<String>,
    pub q: Option<String>,
}

fn diff_filters(qb: &mut QueryBuilder<Postgres>, f: &DiffFilter) {
    qb.push(" WHERE true");
    if let Some(engine) = f.engine.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND c.engine = ").push_bind(engine.to_string());
    }
    if let Some(lane) = f.lane.as_deref().filter(|v| !v.is_empty()) {
        qb.push(" AND c.lane = ").push_bind(lane.to_string());
    }
    qb.push(match f.differs.as_deref() {
        Some("quality") => " AND c.diff_quality",
        Some("group") => " AND c.diff_group",
        Some("edition") => " AND c.diff_edition",
        Some("score") => " AND c.diff_score",
        Some("any") => " AND (c.diff_quality OR c.diff_group OR c.diff_edition OR c.diff_score)",
        _ => "",
    });
    if let Some(text) = f.q.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        let escaped = text
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        qb.push(" AND (t.title ILIKE ")
            .push_bind(format!("%{escaped}%"))
            .push(" OR c.release_name ILIKE ")
            .push_bind(format!("%{escaped}%"))
            .push(")");
    }
}

pub async fn list_differing(
    pool: &PgPool,
    f: &DiffFilter,
    paging: Paging,
) -> Result<(Vec<CopyRow>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new(
        "SELECT count(*) FROM copies c JOIN titles t ON t.id = c.title_id",
    );
    diff_filters(&mut count, f);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;
    let mut list = QueryBuilder::<Postgres>::new(COPY_SELECT);
    diff_filters(&mut list, f);
    list.push(
        " ORDER BY t.sort_title, c.season_number NULLS FIRST, c.episode_numbers, c.id LIMIT ",
    )
    .push_bind(paging.limit())
    .push(" OFFSET ")
    .push_bind(paging.offset());
    let rows = list.build_query_as::<CopyRow>().fetch_all(pool).await?;
    Ok((rows, total))
}

/// Counts per kind of difference, for one engine or for all.
pub async fn diff_counts(pool: &PgPool, engine: Option<&str>) -> Result<DiffCounts, sqlx::Error> {
    sqlx::query_as::<_, DiffCounts>(
        "SELECT count(*) AS copies,
                count(*) FILTER (WHERE diff_quality OR diff_group OR diff_edition OR diff_score) AS \"any\",
                count(*) FILTER (WHERE diff_quality) AS quality,
                count(*) FILTER (WHERE diff_group) AS \"group\",
                count(*) FILTER (WHERE diff_edition) AS edition,
                count(*) FILTER (WHERE diff_score) AS score
         FROM copies WHERE ($1::text IS NULL OR engine = $1)",
    )
    .bind(engine)
    .fetch_one(pool)
    .await
}

pub async fn counts(pool: &PgPool) -> Result<CatalogCounts, sqlx::Error> {
    sqlx::query_as::<_, CatalogCounts>(
        "SELECT (SELECT count(*) FROM titles WHERE kind = 'movie') AS movies,
                (SELECT count(*) FROM titles WHERE kind = 'show') AS shows,
                c.copies, c.protected_copies, c.size_bytes
         FROM (SELECT count(*) AS copies,
                      count(*) FILTER (WHERE cardinality(protections) > 0) AS protected_copies,
                      COALESCE(sum(size_bytes), 0)::bigint AS size_bytes
               FROM copies) c",
    )
    .fetch_one(pool)
    .await
}

/// Every copy with what a simulation needs: its parse, its verdict and its title's tags.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SimRow {
    pub id: i64,
    pub title_id: i64,
    pub title: String,
    pub library_id: String,
    pub lane: Option<String>,
    pub release_name: String,
    pub size_bytes: i64,
    pub held: serde_json::Value,
    pub media_info: Option<serde_json::Value>,
    pub languages: Vec<String>,
    pub original_language: Option<String>,
    pub tags: Vec<String>,
}

pub async fn sim_rows(pool: &PgPool) -> Result<Vec<SimRow>, sqlx::Error> {
    sqlx::query_as::<_, SimRow>(
        "SELECT c.id, c.title_id, t.title, c.library_id, c.lane, c.release_name, c.size_bytes,
                c.held, c.media_info, c.languages, t.original_language,
                COALESCE(te.tags, '{}') AS tags
         FROM copies c
         JOIN titles t ON t.id = c.title_id
         LEFT JOIN title_engines te
                ON te.engine = c.engine AND te.engine_title_id = c.engine_title_id
         ORDER BY c.id",
    )
    .fetch_all(pool)
    .await
}

/// A file of the library with what a processing rule asks of it: its parse, its probe, its
/// title's tags and where it lies.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RuleFileRow {
    pub id: i64,
    pub title_id: i64,
    pub title: String,
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub year: Option<i32>,
    pub library_id: String,
    /// The title's folder as its manager names it.
    pub folder: Option<String>,
    pub relative_path: String,
    pub size_bytes: i64,
    pub held: serde_json::Value,
    pub media_info: Option<serde_json::Value>,
    pub languages: Vec<String>,
    pub original_language: Option<String>,
    pub tags: Vec<String>,
}

impl RuleFileRow {
    /// The whole path of the file. `None` when its manager names no folder.
    pub fn full_path(&self) -> Option<String> {
        TitleFile {
            copy_id: Some(self.id),
            folder: self.folder.clone(),
            path: self.relative_path.clone(),
            size_bytes: self.size_bytes,
            resolution: None,
            tags: Vec::new(),
        }
        .full_path()
    }
}

/// The files of the library a processing rule is asked about: these copies, or every copy.
pub async fn rule_files(
    pool: &PgPool,
    copies: Option<&[i64]>,
) -> Result<Vec<RuleFileRow>, sqlx::Error> {
    sqlx::query_as::<_, RuleFileRow>(
        "SELECT c.id, c.title_id, t.title, t.kind, t.tmdb_id, t.year, c.library_id,
                te.path AS folder, c.relative_path, c.size_bytes, c.held, c.media_info,
                c.languages, t.original_language, COALESCE(te.tags, '{}') AS tags
         FROM copies c
         JOIN titles t ON t.id = c.title_id
         LEFT JOIN title_engines te
                ON te.engine = c.engine AND te.engine_title_id = c.engine_title_id
         WHERE $1::bigint[] IS NULL OR c.id = ANY($1)
         ORDER BY c.id",
    )
    .bind(copies.map(<[i64]>::to_vec))
    .fetch_all(pool)
    .await
}

// Reads for the sync -----------------------------------------------------------------------------

/// One title of one engine, as stored.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LinkRow {
    pub engine_title_id: i64,
    pub title_id: i64,
    pub title: String,
    pub facts_hash: String,
    pub lane: Option<String>,
    pub tags: Vec<String>,
    pub original_language: Option<String>,
    pub file_count: i32,
    pub files_synced_count: Option<i32>,
    pub files_synced_at: Option<DateTime<Utc>>,
}

pub async fn engine_links(pool: &PgPool, engine: &str) -> Result<Vec<LinkRow>, sqlx::Error> {
    sqlx::query_as::<_, LinkRow>(
        "SELECT te.engine_title_id, te.title_id, t.title, te.facts_hash, te.lane, te.tags,
                t.original_language, te.file_count, te.files_synced_count, te.files_synced_at
         FROM title_engines te JOIN titles t ON t.id = te.title_id
         WHERE te.engine = $1",
    )
    .bind(engine)
    .fetch_all(pool)
    .await
}

/// The catalog's id for an engine's title.
pub async fn title_id_of(
    pool: &PgPool,
    engine: &str,
    engine_title_id: i64,
) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "SELECT title_id FROM title_engines WHERE engine = $1 AND engine_title_id = $2",
    )
    .bind(engine)
    .bind(engine_title_id)
    .fetch_optional(pool)
    .await
}

/// The light form of a stored copy the sync compares against.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CopyKey {
    pub id: i64,
    pub title_id: i64,
    pub engine_title_id: i64,
    pub engine_file_id: i64,
    pub facts_hash: String,
    pub season_number: Option<i32>,
    pub episode_numbers: Vec<i32>,
}

pub async fn copy_keys(
    pool: &PgPool,
    engine: &str,
    engine_title_id: Option<i64>,
) -> Result<Vec<CopyKey>, sqlx::Error> {
    sqlx::query_as::<_, CopyKey>(
        "SELECT id, title_id, engine_title_id, engine_file_id, facts_hash, season_number, episode_numbers
         FROM copies WHERE engine = $1 AND ($2::bigint IS NULL OR engine_title_id = $2)",
    )
    .bind(engine)
    .bind(engine_title_id)
    .fetch_all(pool)
    .await
}

/// Copies assessed under another stamp, oldest id first.
pub async fn stale_copies(
    pool: &PgPool,
    stamp: &str,
    after_id: i64,
    limit: i64,
) -> Result<Vec<(CopyRow, Vec<String>)>, sqlx::Error> {
    #[derive(sqlx::FromRow)]
    struct Stale {
        #[sqlx(flatten)]
        copy: CopyRow,
        tags: Vec<String>,
    }
    let mut qb = QueryBuilder::<Postgres>::new(COPY_SELECT.replacen(
        " FROM copies c",
        ", COALESCE(te.tags, '{}') AS tags FROM copies c",
        1,
    ));
    qb.push(
        " LEFT JOIN title_engines te ON te.engine = c.engine AND te.engine_title_id = c.engine_title_id
          WHERE c.assessed_with <> ",
    )
    .push_bind(stamp.to_string())
    .push(" AND c.id > ")
    .push_bind(after_id)
    .push(" ORDER BY c.id LIMIT ")
    .push_bind(limit);
    let rows = qb.build_query_as::<Stale>().fetch_all(pool).await?;
    Ok(rows.into_iter().map(|s| (s.copy, s.tags)).collect())
}

// Writes, always inside the caller's transaction ------------------------------------------------

/// Insert the title or refresh its facts. Returns its id and whether it is new.
///
/// A fact the engine does not give keeps its stored value: two engines can hold one title,
/// and one of them may know less. The next airing is the exception, because "none" is an
/// answer there: it takes the engine's date, and without one keeps the stored date only
/// while that date is still ahead.
pub async fn upsert_title(
    conn: &mut PgConnection,
    engine: &str,
    t: &TitleFacts,
) -> Result<(i64, bool), sqlx::Error> {
    sqlx::query_as::<_, (i64, bool)>(
        "INSERT INTO titles (ext_key, kind, tmdb_id, tvdb_id, imdb_id, title, sort_title, year,
                             original_language, poster_url, status, overview, genres,
                             runtime_minutes, certification, studio, network, collection,
                             rating, imdb_rating, fanart_url, banner_url, first_aired,
                             next_airing, in_cinemas, digital_release, collection_tmdb_id,
                             original_title)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                 $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28)
         ON CONFLICT (ext_key) DO UPDATE SET
             tmdb_id = COALESCE(EXCLUDED.tmdb_id, titles.tmdb_id),
             tvdb_id = COALESCE(EXCLUDED.tvdb_id, titles.tvdb_id),
             imdb_id = COALESCE(EXCLUDED.imdb_id, titles.imdb_id),
             title = EXCLUDED.title,
             sort_title = EXCLUDED.sort_title,
             year = COALESCE(EXCLUDED.year, titles.year),
             original_language = COALESCE(EXCLUDED.original_language, titles.original_language),
             poster_url = COALESCE(EXCLUDED.poster_url, titles.poster_url),
             status = COALESCE(EXCLUDED.status, titles.status),
             overview = COALESCE(EXCLUDED.overview, titles.overview),
             genres = CASE WHEN cardinality(EXCLUDED.genres) > 0 THEN EXCLUDED.genres
                           ELSE titles.genres END,
             runtime_minutes = COALESCE(EXCLUDED.runtime_minutes, titles.runtime_minutes),
             certification = COALESCE(EXCLUDED.certification, titles.certification),
             studio = COALESCE(EXCLUDED.studio, titles.studio),
             network = COALESCE(EXCLUDED.network, titles.network),
             collection = COALESCE(EXCLUDED.collection, titles.collection),
             rating = COALESCE(EXCLUDED.rating, titles.rating),
             imdb_rating = COALESCE(EXCLUDED.imdb_rating, titles.imdb_rating),
             fanart_url = COALESCE(EXCLUDED.fanart_url, titles.fanart_url),
             banner_url = COALESCE(EXCLUDED.banner_url, titles.banner_url),
             first_aired = COALESCE(EXCLUDED.first_aired, titles.first_aired),
             next_airing = CASE WHEN EXCLUDED.next_airing IS NOT NULL THEN EXCLUDED.next_airing
                                WHEN titles.next_airing > now() THEN titles.next_airing END,
             in_cinemas = COALESCE(EXCLUDED.in_cinemas, titles.in_cinemas),
             digital_release = COALESCE(EXCLUDED.digital_release, titles.digital_release),
             collection_tmdb_id = COALESCE(EXCLUDED.collection_tmdb_id, titles.collection_tmdb_id),
             original_title = COALESCE(EXCLUDED.original_title, titles.original_title),
             updated_at = now()
         RETURNING id, (xmax = 0) AS inserted",
    )
    .bind(ingest::ext_key(t, engine))
    .bind(t.kind.as_str())
    .bind(t.tmdb_id.filter(|n| *n > 0))
    .bind(t.tvdb_id.filter(|n| *n > 0))
    .bind(t.imdb_id.as_deref().filter(|s| !s.is_empty()))
    .bind(&t.title)
    .bind(ingest::sort_key(t))
    .bind(t.year.filter(|y| *y > 0))
    .bind(&t.original_language)
    .bind(&t.poster_url)
    .bind(&t.status)
    .bind(&t.about.overview)
    .bind(&t.about.genres)
    .bind(t.about.runtime_minutes)
    .bind(&t.about.certification)
    .bind(&t.about.studio)
    .bind(&t.about.network)
    .bind(&t.about.collection)
    .bind(t.about.rating)
    .bind(t.about.imdb_rating)
    .bind(&t.about.fanart_url)
    .bind(&t.about.banner_url)
    .bind(t.about.first_aired)
    .bind(t.about.next_airing)
    .bind(t.about.in_cinemas)
    .bind(t.about.digital_release)
    .bind(t.about.collection_tmdb_id)
    .bind(&t.about.original_title)
    .fetch_one(conn)
    .await
}

pub async fn upsert_link(
    conn: &mut PgConnection,
    engine: &EngineRef,
    title_id: i64,
    t: &TitleFacts,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO title_engines (engine, engine_title_id, title_id, library_id, path, monitored,
                                    profile, lane, tags, status, added_at, file_count, size_bytes,
                                    facts_hash, episode_count, total_episode_count, seasons)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
         ON CONFLICT (engine, engine_title_id) DO UPDATE SET
             title_id = EXCLUDED.title_id, library_id = EXCLUDED.library_id, path = EXCLUDED.path,
             monitored = EXCLUDED.monitored, profile = EXCLUDED.profile, lane = EXCLUDED.lane,
             tags = EXCLUDED.tags, status = EXCLUDED.status, added_at = EXCLUDED.added_at,
             file_count = EXCLUDED.file_count, size_bytes = EXCLUDED.size_bytes,
             facts_hash = EXCLUDED.facts_hash, episode_count = EXCLUDED.episode_count,
             total_episode_count = EXCLUDED.total_episode_count, seasons = EXCLUDED.seasons,
             updated_at = now()",
    )
    .bind(&engine.id)
    .bind(t.engine_title_id)
    .bind(title_id)
    .bind(&engine.library)
    .bind(&t.path)
    .bind(t.monitored)
    .bind(&t.profile)
    .bind(&t.lane)
    .bind(&t.tags)
    .bind(&t.status)
    .bind(t.added_at)
    .bind(t.file_count)
    .bind(t.size_bytes)
    .bind(ingest::title_facts_hash(t))
    .bind(t.episode_count)
    .bind(t.total_episode_count)
    .bind(Json(&t.seasons))
    .execute(conn)
    .await
    .map(|_| ())
}

/// Rebuild a show's seasons from what its engines say (`title_engines.seasons`): one row
/// per season any engine lists, each count the largest any engine reports. A season no
/// engine lists any more goes.
pub async fn refresh_seasons(conn: &mut PgConnection, title_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "WITH said AS (
             SELECT (e ->> 'season_number')::int AS season_number,
                    COALESCE(max((e ->> 'episode_count')::int), 0) AS episode_count,
                    COALESCE(max((e ->> 'total_episode_count')::int), 0) AS total_episode_count,
                    COALESCE(max((e ->> 'episode_file_count')::int), 0) AS episode_file_count
             FROM title_engines te, jsonb_array_elements(te.seasons) AS e
             WHERE te.title_id = $1 AND e ->> 'season_number' IS NOT NULL
             GROUP BY 1
         ), gone AS (
             DELETE FROM seasons s
             WHERE s.title_id = $1
               AND NOT EXISTS (SELECT 1 FROM said WHERE said.season_number = s.season_number)
         )
         INSERT INTO seasons (title_id, season_number, episode_count, total_episode_count,
                              episode_file_count)
         SELECT $1, season_number, episode_count, total_episode_count, episode_file_count
         FROM said
         ON CONFLICT (title_id, season_number) DO UPDATE SET
             episode_count = EXCLUDED.episode_count,
             total_episode_count = EXCLUDED.total_episode_count,
             episode_file_count = EXCLUDED.episode_file_count",
    )
    .bind(title_id)
    .execute(conn)
    .await
    .map(|_| ())
}

pub async fn upsert_episodes(
    conn: &mut PgConnection,
    title_id: i64,
    season_number: i32,
    episode_numbers: &[i32],
    name: Option<&str>,
) -> Result<(), sqlx::Error> {
    if episode_numbers.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO episodes (title_id, season_number, episode_number, name)
         SELECT $1, $2, e, $4 FROM UNNEST($3::int[]) AS x(e)
         ON CONFLICT (title_id, season_number, episode_number) DO UPDATE
         SET name = COALESCE(EXCLUDED.name, episodes.name)",
    )
    .bind(title_id)
    .bind(season_number)
    .bind(episode_numbers)
    .bind(name)
    .execute(conn)
    .await
    .map(|_| ())
}

pub async fn delete_link(
    conn: &mut PgConnection,
    engine: &str,
    engine_title_id: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM title_engines WHERE engine = $1 AND engine_title_id = $2")
        .bind(engine)
        .bind(engine_title_id)
        .execute(conn)
        .await
        .map(|_| ())
}

pub async fn links_left(conn: &mut PgConnection, title_id: i64) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM title_engines WHERE title_id = $1")
        .bind(title_id)
        .fetch_one(conn)
        .await
}

pub async fn delete_title(conn: &mut PgConnection, title_id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM titles WHERE id = $1")
        .bind(title_id)
        .execute(conn)
        .await
        .map(|_| ())
}

/// What a copy row is written from.
pub struct CopyWrite<'a> {
    pub engine: &'a EngineRef,
    pub title_id: i64,
    pub engine_title_id: i64,
    pub lane: Option<&'a str>,
    pub file: &'a FileFacts,
    pub assessment: &'a Assessment,
    pub facts_hash: &'a str,
    pub stamp: &'a str,
}

pub async fn insert_copy(conn: &mut PgConnection, w: &CopyWrite<'_>) -> Result<i64, sqlx::Error> {
    let a = w.assessment;
    let f = w.file;
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO copies (title_id, library_id, engine, engine_title_id, engine_file_id, lane,
                             season_number, episode_numbers, relative_path, scene_name,
                             original_path, release_name, name_source, size_bytes, date_added,
                             arr_quality, arr_group, arr_edition, arr_custom_formats, arr_score,
                             languages, media_info, parsed, verdict, mq_quality, mq_group,
                             mq_edition, mq_score, diff_quality, diff_group, diff_edition,
                             diff_score, protections, assessed_with, facts_hash, held,
                             arr_only_formats, mq_only_traits, arr_proper, arr_repack,
                             edition_key, edition_label, edition_marked, edition_rank)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                 $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33, $34, $35,
                 $36, $37, $38, $39, $40, $41, $42, $43, $44)
         ON CONFLICT (engine, engine_file_id) DO UPDATE SET
             title_id = EXCLUDED.title_id, engine_title_id = EXCLUDED.engine_title_id,
             lane = EXCLUDED.lane, season_number = EXCLUDED.season_number,
             episode_numbers = EXCLUDED.episode_numbers, relative_path = EXCLUDED.relative_path,
             scene_name = EXCLUDED.scene_name, original_path = EXCLUDED.original_path,
             release_name = EXCLUDED.release_name, name_source = EXCLUDED.name_source,
             size_bytes = EXCLUDED.size_bytes, date_added = EXCLUDED.date_added,
             arr_quality = EXCLUDED.arr_quality, arr_group = EXCLUDED.arr_group,
             arr_edition = EXCLUDED.arr_edition, arr_custom_formats = EXCLUDED.arr_custom_formats,
             arr_score = EXCLUDED.arr_score, languages = EXCLUDED.languages,
             media_info = EXCLUDED.media_info, parsed = EXCLUDED.parsed, verdict = EXCLUDED.verdict,
             mq_quality = EXCLUDED.mq_quality, mq_group = EXCLUDED.mq_group,
             mq_edition = EXCLUDED.mq_edition, mq_score = EXCLUDED.mq_score,
             diff_quality = EXCLUDED.diff_quality, diff_group = EXCLUDED.diff_group,
             diff_edition = EXCLUDED.diff_edition, diff_score = EXCLUDED.diff_score,
             protections = EXCLUDED.protections, assessed_with = EXCLUDED.assessed_with,
             facts_hash = EXCLUDED.facts_hash, held = EXCLUDED.held,
             arr_only_formats = EXCLUDED.arr_only_formats,
             mq_only_traits = EXCLUDED.mq_only_traits, arr_proper = EXCLUDED.arr_proper,
             arr_repack = EXCLUDED.arr_repack, edition_key = EXCLUDED.edition_key,
             edition_label = EXCLUDED.edition_label, edition_marked = EXCLUDED.edition_marked,
             edition_rank = EXCLUDED.edition_rank, updated_at = now()
         RETURNING id",
    )
    .bind(w.title_id)
    .bind(&w.engine.library)
    .bind(&w.engine.id)
    .bind(w.engine_title_id)
    .bind(f.engine_file_id)
    .bind(w.lane)
    .bind(f.season_number)
    .bind(&f.episode_numbers)
    .bind(&f.relative_path)
    .bind(&f.scene_name)
    .bind(&f.original_path)
    .bind(&a.release_name)
    .bind(a.name_source)
    .bind(f.size_bytes)
    .bind(f.date_added)
    .bind(&f.arr_quality)
    .bind(&f.arr_group)
    .bind(&f.arr_edition)
    .bind(&f.arr_custom_formats)
    .bind(f.arr_score)
    .bind(&f.languages)
    .bind(f.media_info.as_ref().map(Json))
    .bind(Json(&a.parsed))
    .bind(a.verdict_view().map(Json))
    .bind(&a.mq_quality)
    .bind(&a.mq_group)
    .bind(&a.mq_edition)
    .bind(a.mq_score)
    .bind(a.diff_quality)
    .bind(a.diff_group)
    .bind(a.diff_edition)
    .bind(a.diff_score)
    .bind(&a.protections)
    .bind(w.stamp)
    .bind(w.facts_hash)
    .bind(Json(&a.held))
    .bind(&a.arr_only_formats)
    .bind(&a.singularr_only_traits)
    .bind(f.arr_proper)
    .bind(f.arr_repack)
    .bind(&a.edition.key)
    .bind(&a.edition.label)
    .bind(a.edition.marked)
    .bind(a.edition_rank)
    .fetch_one(conn)
    .await
}

pub async fn delete_copy(conn: &mut PgConnection, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM copies WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Store a fresh assessment of a copy whose file did not change.
pub async fn update_assessment(
    conn: &mut PgConnection,
    id: i64,
    a: &Assessment,
    stamp: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE copies SET release_name = $2, name_source = $3, parsed = $4, verdict = $5,
                mq_quality = $6, mq_group = $7, mq_edition = $8, mq_score = $9, diff_quality = $10,
                diff_group = $11, diff_edition = $12, diff_score = $13, protections = $14,
                assessed_with = $15, held = $16, arr_only_formats = $17, mq_only_traits = $18,
                edition_key = $19, edition_label = $20, edition_marked = $21, edition_rank = $22,
                updated_at = now()
         WHERE id = $1",
    )
    .bind(id)
    .bind(&a.release_name)
    .bind(a.name_source)
    .bind(Json(&a.parsed))
    .bind(a.verdict_view().map(Json))
    .bind(&a.mq_quality)
    .bind(&a.mq_group)
    .bind(&a.mq_edition)
    .bind(a.mq_score)
    .bind(a.diff_quality)
    .bind(a.diff_group)
    .bind(a.diff_edition)
    .bind(a.diff_score)
    .bind(&a.protections)
    .bind(stamp)
    .bind(Json(&a.held))
    .bind(&a.arr_only_formats)
    .bind(&a.singularr_only_traits)
    .bind(&a.edition.key)
    .bind(&a.edition.label)
    .bind(a.edition.marked)
    .bind(a.edition_rank)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Stamp copies whose assessment came out the same.
pub async fn stamp_copies(
    conn: &mut PgConnection,
    ids: &[i64],
    stamp: &str,
) -> Result<(), sqlx::Error> {
    if ids.is_empty() {
        return Ok(());
    }
    sqlx::query("UPDATE copies SET assessed_with = $2 WHERE id = ANY($1)")
        .bind(ids)
        .bind(stamp)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Note that a title's files were read, and how many the engine said it has.
pub async fn mark_files_synced(
    conn: &mut PgConnection,
    engine: &str,
    engine_title_id: i64,
    file_count: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE title_engines SET files_synced_at = now(), files_synced_count = $3
         WHERE engine = $1 AND engine_title_id = $2",
    )
    .bind(engine)
    .bind(engine_title_id)
    .bind(file_count)
    .execute(conn)
    .await
    .map(|_| ())
}

/// A library's root folders are the parents of its titles' folders.
/// Copies per library, by library id: the gauges at /metrics.
pub async fn copies_by_library(pool: &PgPool) -> Result<Vec<(String, i64)>, sqlx::Error> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT library_id, count(*) FROM copies GROUP BY library_id ORDER BY library_id",
    )
    .fetch_all(pool)
    .await
}

/// What one library has: titles with a file, and for shows the episodes and those with a file
/// as the managers count them.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct LibraryHoldings {
    pub library_id: String,
    pub with_file: i64,
    pub episodes: i64,
    pub episode_files: i64,
}

/// [`LibraryHoldings`] of every library that holds titles: the Settings page's libraries.
pub async fn library_holdings(pool: &PgPool) -> Result<Vec<LibraryHoldings>, sqlx::Error> {
    sqlx::query_as::<_, LibraryHoldings>(
        "SELECT te.library_id,
                (count(DISTINCT te.title_id) FILTER (WHERE EXISTS (
                    SELECT 1 FROM copies c
                    WHERE c.title_id = te.title_id AND c.library_id = te.library_id)))::bigint
                    AS with_file,
                COALESCE(sum(te.episode_count), 0)::bigint AS episodes,
                COALESCE(sum(te.file_count), 0)::bigint AS episode_files
         FROM title_engines te
         GROUP BY te.library_id
         ORDER BY te.library_id",
    )
    .fetch_all(pool)
    .await
}

/// A library as the configuration names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryWrite {
    pub id: String,
    pub name: String,
    /// `movie` or `show`.
    pub kind: String,
    /// The integration it belongs to.
    pub integration: String,
}

/// Make sure an engine's library exists before rows name it. It is named by its id until the
/// configuration names it ([`upsert_libraries`]).
pub async fn ensure_library(
    conn: &mut PgConnection,
    id: &str,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, integration) VALUES ($1, $1, $2, $1)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(kind)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Write the libraries of the configuration: their names, kinds and integrations.
pub async fn upsert_libraries(
    pool: &PgPool,
    libraries: &[LibraryWrite],
) -> Result<(), sqlx::Error> {
    for l in libraries {
        sqlx::query(
            "INSERT INTO libraries (id, name, kind, integration, updated_at)
             VALUES ($1, $2, $3, $4, now())
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, kind = EXCLUDED.kind,
                 integration = EXCLUDED.integration, updated_at = now()",
        )
        .bind(&l.id)
        .bind(&l.name)
        .bind(&l.kind)
        .bind(&l.integration)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// What [`prune_engines`] removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Pruned {
    /// The engines whose rows went.
    pub engines: Vec<String>,
    pub copies: u64,
    pub untracked_files: u64,
    pub title_links: u64,
    pub titles: u64,
    pub libraries: u64,
}

/// Remove what engines outside `keep` reported: their copies, untracked files and title
/// links, the titles no engine holds any more, the worked-out editions no file holds, and the
/// libraries of integrations outside `keep` that nothing names.
pub async fn prune_engines(
    conn: &mut PgConnection,
    keep: &[String],
) -> Result<Pruned, sqlx::Error> {
    let engines: Vec<String> = sqlx::query_scalar(
        "SELECT engine FROM title_engines WHERE NOT (engine = ANY($1))
         UNION SELECT engine FROM copies WHERE NOT (engine = ANY($1))
         UNION SELECT engine FROM untracked_files WHERE NOT (engine = ANY($1))
         ORDER BY 1",
    )
    .bind(keep)
    .fetch_all(&mut *conn)
    .await?;
    let mut pruned = Pruned {
        engines,
        ..Pruned::default()
    };
    if !pruned.engines.is_empty() {
        let gone = &pruned.engines;
        pruned.copies = sqlx::query("DELETE FROM copies WHERE engine = ANY($1)")
            .bind(gone)
            .execute(&mut *conn)
            .await?
            .rows_affected();
        pruned.untracked_files = sqlx::query("DELETE FROM untracked_files WHERE engine = ANY($1)")
            .bind(gone)
            .execute(&mut *conn)
            .await?
            .rows_affected();
        pruned.title_links = sqlx::query("DELETE FROM title_engines WHERE engine = ANY($1)")
            .bind(gone)
            .execute(&mut *conn)
            .await?
            .rows_affected();
        pruned.titles = sqlx::query(
            "DELETE FROM titles t WHERE NOT EXISTS
                 (SELECT 1 FROM title_engines te WHERE te.title_id = t.id)",
        )
        .execute(&mut *conn)
        .await?
        .rows_affected();
        sqlx::query(
            "DELETE FROM title_editions te
             WHERE te.set_by IS NULL
               AND NOT EXISTS (SELECT 1 FROM copies c
                               WHERE c.title_id = te.title_id AND c.edition_key = te.key)
               AND NOT EXISTS (SELECT 1 FROM untracked_files u
                               WHERE u.title_id = te.title_id AND u.edition_key = te.key)",
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "UPDATE title_editions te SET is_default = true
             FROM (SELECT DISTINCT ON (t.title_id) t.title_id, t.key FROM title_editions t
                   WHERE NOT EXISTS (SELECT 1 FROM title_editions d
                                     WHERE d.title_id = t.title_id AND d.is_default)
                   ORDER BY t.title_id, (t.key <> 'theatrical'), t.rank, t.key) d
             WHERE te.title_id = d.title_id AND te.key = d.key",
        )
        .execute(&mut *conn)
        .await?;
    }
    pruned.libraries = sqlx::query(
        "DELETE FROM libraries l
         WHERE NOT (COALESCE(l.integration, l.id) = ANY($1))
           AND NOT EXISTS (SELECT 1 FROM title_engines te WHERE te.library_id = l.id)
           AND NOT EXISTS (SELECT 1 FROM copies c WHERE c.library_id = l.id)
           AND NOT EXISTS (SELECT 1 FROM untracked_files u WHERE u.library_id = l.id)",
    )
    .bind(keep)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(pruned)
}

pub async fn refresh_library_roots(
    conn: &mut PgConnection,
    library: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE libraries l SET updated_at = now(), root_folders = COALESCE((
             SELECT array_agg(DISTINCT regexp_replace(te.path, '[/\\\\][^/\\\\]+[/\\\\]?$', ''))
             FROM title_engines te WHERE te.library_id = l.id AND te.path IS NOT NULL), '{}')
         WHERE l.id = $1",
    )
    .bind(library)
    .execute(conn)
    .await
    .map(|_| ())
}

// Editions ---------------------------------------------------------------------------------------

/// Work a movie's editions out again from what it holds: every edition a copy or an untracked
/// file belongs to is a wanted row; a worked-out row whose edition is no longer held goes; the
/// default is `default_key` when it is held, else the held edition first in the list's order.
/// Rows the owner set are left as they are. For a show it removes worked-out rows only.
pub async fn refresh_editions(
    conn: &mut PgConnection,
    title_id: i64,
    default_key: &str,
) -> Result<(), sqlx::Error> {
    const HELD: &str = "WITH held AS (
             SELECT edition_key AS key, edition_label AS label, edition_rank AS rank
             FROM copies WHERE title_id = $1
             UNION ALL
             SELECT edition_key, edition_label, edition_rank FROM untracked_files WHERE title_id = $1),
         movie AS (SELECT 1 FROM titles WHERE id = $1 AND kind = 'movie')";
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{HELD}
         DELETE FROM title_editions te
         WHERE te.title_id = $1 AND te.set_by IS NULL
           AND (NOT EXISTS (SELECT 1 FROM movie) OR te.key NOT IN (SELECT key FROM held))"
    )))
    .bind(title_id)
    .execute(&mut *conn)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "{HELD}
         INSERT INTO title_editions (title_id, key, label, wanted, is_default, rank)
         SELECT DISTINCT ON (key) $1, key, label, true, false, rank
         FROM held WHERE EXISTS (SELECT 1 FROM movie)
         ORDER BY key, rank, label
         ON CONFLICT (title_id, key) DO UPDATE SET
             label = EXCLUDED.label, rank = EXCLUDED.rank, updated_at = now()
         WHERE title_editions.set_by IS NULL
           AND (title_editions.label, title_editions.rank)
               IS DISTINCT FROM (EXCLUDED.label, EXCLUDED.rank)"
    )))
    .bind(title_id)
    .execute(&mut *conn)
    .await?;
    let owner_chose: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM title_editions
                        WHERE title_id = $1 AND is_default AND set_by IS NOT NULL)",
    )
    .bind(title_id)
    .fetch_one(&mut *conn)
    .await?;
    if owner_chose {
        return Ok(());
    }
    let chosen: Option<String> = sqlx::query_scalar(
        "SELECT key FROM title_editions WHERE title_id = $1
         ORDER BY (key <> $2), rank, key LIMIT 1",
    )
    .bind(title_id)
    .bind(default_key)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(chosen) = chosen else {
        return Ok(());
    };
    // Two statements: the one-default index is checked row by row.
    sqlx::query(
        "UPDATE title_editions SET is_default = false, updated_at = now()
         WHERE title_id = $1 AND is_default AND key <> $2",
    )
    .bind(title_id)
    .bind(&chosen)
    .execute(&mut *conn)
    .await?;
    sqlx::query(
        "UPDATE title_editions SET is_default = true, updated_at = now()
         WHERE title_id = $1 AND key = $2 AND NOT is_default",
    )
    .bind(title_id)
    .bind(&chosen)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One row of `title_editions`.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleEditionRow {
    pub key: String,
    pub label: String,
    pub wanted: bool,
    pub is_default: bool,
    pub set_by: Option<String>,
    pub rank: i32,
}

/// A title's editions in the list's order.
pub async fn title_editions(
    pool: &PgPool,
    title_id: i64,
) -> Result<Vec<TitleEditionRow>, sqlx::Error> {
    sqlx::query_as::<_, TitleEditionRow>(
        "SELECT key, label, wanted, is_default, set_by, rank FROM title_editions
         WHERE title_id = $1 ORDER BY rank, key",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

/// One file that holds an edition: a copy, or an untracked file.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct HoldingRow {
    pub edition_key: String,
    pub library_id: String,
    pub engine: String,
    pub copy_id: Option<i64>,
    pub lane: Option<String>,
    pub quality: Option<String>,
    pub resolution: Option<String>,
    pub file_name: String,
    pub size_bytes: i64,
}

/// Every file of a title with the edition it belongs to: its copies first, then the files in
/// its folders that no engine tracks.
pub async fn edition_holdings(
    pool: &PgPool,
    title_id: i64,
) -> Result<Vec<HoldingRow>, sqlx::Error> {
    sqlx::query_as::<_, HoldingRow>(
        "SELECT edition_key, library_id, engine, copy_id, lane, quality, resolution, file_name,
                size_bytes
         FROM (
             SELECT c.edition_key, c.library_id, c.engine, c.id AS copy_id, c.lane,
                    COALESCE(c.verdict ->> 'quality', c.arr_quality) AS quality,
                    NULL::text AS resolution, c.relative_path AS file_name, c.size_bytes,
                    0 AS untracked, c.id AS ord
             FROM copies c WHERE c.title_id = $1
             UNION ALL
             SELECT u.edition_key, u.library_id, u.engine, NULL, NULL, NULL, u.resolution,
                    u.file_name, u.size_bytes, 1, u.id
             FROM untracked_files u WHERE u.title_id = $1
         ) f
         ORDER BY untracked, library_id DESC, ord",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

/// A movie folder as an engine names it, and the files the engine tracks in it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MovieFolder {
    pub engine: String,
    pub library_id: String,
    pub title_id: i64,
    pub title: String,
    pub path: String,
    /// The tracked files, as paths below the folder.
    pub tracked: Vec<String>,
}

/// Every movie folder an engine names, with its tracked files.
pub async fn movie_folders(pool: &PgPool) -> Result<Vec<MovieFolder>, sqlx::Error> {
    sqlx::query_as::<_, MovieFolder>(
        "SELECT te.engine, te.library_id, te.title_id, t.title, te.path,
                COALESCE(array_agg(c.relative_path ORDER BY c.id)
                         FILTER (WHERE c.id IS NOT NULL), '{}') AS tracked
         FROM title_engines te
         JOIN titles t ON t.id = te.title_id AND t.kind = 'movie'
         LEFT JOIN copies c ON c.engine = te.engine AND c.engine_title_id = te.engine_title_id
         WHERE te.path IS NOT NULL
         GROUP BY te.engine, te.engine_title_id, te.library_id, te.title_id, t.title, te.path
         ORDER BY te.title_id, te.engine",
    )
    .fetch_all(pool)
    .await
}

/// An untracked file as stored.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UntrackedRow {
    pub id: i64,
    pub engine: String,
    pub title_id: i64,
    pub path: String,
    pub file_name: String,
    pub size_bytes: i64,
    pub edition_key: String,
}

/// Every untracked file known.
pub async fn all_untracked(pool: &PgPool) -> Result<Vec<UntrackedRow>, sqlx::Error> {
    sqlx::query_as::<_, UntrackedRow>(
        "SELECT id, engine, title_id, path, file_name, size_bytes, edition_key
         FROM untracked_files ORDER BY id",
    )
    .fetch_all(pool)
    .await
}

/// What an untracked file row is written from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UntrackedWrite {
    pub engine: String,
    pub library_id: String,
    pub title_id: i64,
    pub path: String,
    pub file_name: String,
    pub size_bytes: i64,
    pub modified_at: Option<DateTime<Utc>>,
    pub edition_key: String,
    pub edition_label: String,
    pub edition_marked: bool,
    pub edition_rank: i32,
    pub resolution: Option<String>,
}

/// Insert or refresh an untracked file. Returns its id.
pub async fn upsert_untracked(
    conn: &mut PgConnection,
    w: &UntrackedWrite,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO untracked_files (title_id, engine, library_id, path, file_name, size_bytes,
                                      modified_at, edition_key, edition_label, edition_marked,
                                      edition_rank, resolution)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
         ON CONFLICT (engine, path) DO UPDATE SET
             title_id = EXCLUDED.title_id, library_id = EXCLUDED.library_id,
             file_name = EXCLUDED.file_name, size_bytes = EXCLUDED.size_bytes,
             modified_at = EXCLUDED.modified_at, edition_key = EXCLUDED.edition_key,
             edition_label = EXCLUDED.edition_label, edition_marked = EXCLUDED.edition_marked,
             edition_rank = EXCLUDED.edition_rank, resolution = EXCLUDED.resolution,
             seen_at = now()
         RETURNING id",
    )
    .bind(w.title_id)
    .bind(&w.engine)
    .bind(&w.library_id)
    .bind(&w.path)
    .bind(&w.file_name)
    .bind(w.size_bytes)
    .bind(w.modified_at)
    .bind(&w.edition_key)
    .bind(&w.edition_label)
    .bind(w.edition_marked)
    .bind(w.edition_rank)
    .bind(&w.resolution)
    .fetch_one(conn)
    .await
}

pub async fn delete_untracked(conn: &mut PgConnection, id: i64) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM untracked_files WHERE id = $1")
        .bind(id)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Record the outcome of a movie folder scan.
pub async fn record_folder_scan(
    pool: &PgPool,
    roots: &[String],
    ok: bool,
    took_ms: i64,
    error: Option<&str>,
    counts: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO movie_folder_scan (id, roots, ok, last_scan_at, last_ok_at, took_ms, error, counts)
         VALUES (1, $1, $2, now(), CASE WHEN $2 THEN now() END, $3, $4, $5)
         ON CONFLICT (id) DO UPDATE SET
             roots = EXCLUDED.roots, ok = EXCLUDED.ok, last_scan_at = EXCLUDED.last_scan_at,
             last_ok_at = COALESCE(EXCLUDED.last_ok_at, movie_folder_scan.last_ok_at),
             took_ms = EXCLUDED.took_ms, error = EXCLUDED.error, counts = EXCLUDED.counts",
    )
    .bind(roots)
    .bind(ok)
    .bind(took_ms)
    .bind(error)
    .bind(Json(counts))
    .execute(pool)
    .await
    .map(|_| ())
}

pub async fn folder_scan_status(pool: &PgPool) -> Result<Option<FolderScanStatus>, sqlx::Error> {
    sqlx::query_as::<_, FolderScanStatus>(
        "SELECT roots, ok, last_scan_at, last_ok_at, took_ms, error, counts
         FROM movie_folder_scan WHERE id = 1",
    )
    .fetch_optional(pool)
    .await
}

// The media server ------------------------------------------------------------------------------

/// A library section of the media server, as the server pass writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLibraryWrite {
    pub id: String,
    pub name: String,
    /// `movie`, `show`, `music`, `homevideo` or `photo`.
    pub kind: String,
    pub integration: String,
    pub section_key: String,
    pub managed_by: Option<String>,
    pub locations: Vec<String>,
}

/// Write the server's sections as libraries. A section the server no longer lists goes when
/// nothing names it. Returns the libraries removed.
pub async fn write_server_libraries(
    conn: &mut PgConnection,
    server: &str,
    libraries: &[ServerLibraryWrite],
) -> Result<u64, sqlx::Error> {
    for l in libraries {
        sqlx::query(
            "INSERT INTO libraries (id, name, kind, integration, section_key, managed_by, locations,
                                    updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())
             ON CONFLICT (id) DO UPDATE SET name = EXCLUDED.name, kind = EXCLUDED.kind,
                 integration = EXCLUDED.integration, section_key = EXCLUDED.section_key,
                 managed_by = EXCLUDED.managed_by, locations = EXCLUDED.locations,
                 updated_at = now()",
        )
        .bind(&l.id)
        .bind(&l.name)
        .bind(&l.kind)
        .bind(&l.integration)
        .bind(&l.section_key)
        .bind(&l.managed_by)
        .bind(&l.locations)
        .execute(&mut *conn)
        .await?;
    }
    let keep: Vec<String> = libraries.iter().map(|l| l.id.clone()).collect();
    sqlx::query(
        "DELETE FROM libraries l
         WHERE l.integration = $1 AND l.section_key IS NOT NULL AND NOT (l.id = ANY($2))
           AND NOT EXISTS (SELECT 1 FROM title_engines te WHERE te.library_id = l.id)
           AND NOT EXISTS (SELECT 1 FROM copies c WHERE c.library_id = l.id)
           AND NOT EXISTS (SELECT 1 FROM untracked_files u WHERE u.library_id = l.id)",
    )
    .bind(server)
    .bind(&keep)
    .execute(&mut *conn)
    .await
    .map(|r| r.rows_affected())
}

/// What the server says about one of its items.
#[derive(Debug, Clone, PartialEq)]
pub struct TitleServerWrite {
    pub rating_key: String,
    /// `movie` or `show`.
    pub kind: String,
    pub library_id: String,
    pub tmdb: Option<i64>,
    pub tvdb: Option<i64>,
    pub imdb: Option<String>,
    pub critic_rating: Option<f64>,
    pub critic_image: Option<String>,
    pub audience_rating: Option<f64>,
    pub audience_image: Option<String>,
    pub plays: i64,
    pub last_played: Option<DateTime<Utc>>,
    pub edition: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
}

/// How many sections and how many matched items of a media server the read model holds: what
/// a new listing is compared with before it may replace them.
pub async fn server_counts(pool: &PgPool, server: &str) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT (SELECT count(*) FROM libraries
                 WHERE integration = $1 AND section_key IS NOT NULL),
                (SELECT count(*) FROM title_server WHERE server = $1)",
    )
    .bind(server)
    .fetch_one(pool)
    .await
}

/// Write what the server says about the catalog's titles, each item matched by its TMDB, TVDB
/// or IMDb id; an item that matches no title is left out. Rows of this server the pass did not
/// write go. Returns the items matched.
pub async fn write_title_server(
    conn: &mut PgConnection,
    server: &str,
    rows: &[TitleServerWrite],
) -> Result<u64, sqlx::Error> {
    let started: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *conn)
        .await?;
    let mut matched = 0;
    for r in rows {
        matched += sqlx::query(
            "INSERT INTO title_server (server, rating_key, title_id, library_id, critic_rating,
                                       critic_image, audience_rating, audience_image, plays,
                                       last_played, edition, added_at, seen_at)
             SELECT $1, $2, t.id, $3, $4, $5, $6, $7, $8, $9, $10, $11, clock_timestamp()
             FROM titles t
             WHERE t.kind = $12
               AND (($13::bigint IS NOT NULL AND t.tmdb_id = $13)
                    OR ($14::bigint IS NOT NULL AND t.tvdb_id = $14)
                    OR ($15::text IS NOT NULL AND t.imdb_id = $15))
             ORDER BY (t.tmdb_id IS NOT DISTINCT FROM $13) DESC, t.id
             LIMIT 1
             ON CONFLICT (server, rating_key) DO UPDATE SET
                 title_id = EXCLUDED.title_id, library_id = EXCLUDED.library_id,
                 critic_rating = EXCLUDED.critic_rating, critic_image = EXCLUDED.critic_image,
                 audience_rating = EXCLUDED.audience_rating,
                 audience_image = EXCLUDED.audience_image, plays = EXCLUDED.plays,
                 last_played = EXCLUDED.last_played, edition = EXCLUDED.edition,
                 added_at = EXCLUDED.added_at, seen_at = EXCLUDED.seen_at",
        )
        .bind(server)
        .bind(&r.rating_key)
        .bind(&r.library_id)
        .bind(r.critic_rating)
        .bind(&r.critic_image)
        .bind(r.audience_rating)
        .bind(&r.audience_image)
        .bind(r.plays)
        .bind(r.last_played)
        .bind(&r.edition)
        .bind(r.added_at)
        .bind(&r.kind)
        .bind(r.tmdb)
        .bind(r.tvdb)
        .bind(&r.imdb)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    }
    sqlx::query("DELETE FROM title_server WHERE server = $1 AND seen_at < $2")
        .bind(server)
        .bind(started)
        .execute(&mut *conn)
        .await?;
    Ok(matched)
}

// Reads for the pages (`api::pages`) ------------------------------------------------------------

/// What one engine's library holds, by library id.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct LibraryTotals {
    pub library_id: String,
    pub titles: i64,
    /// Titles with at least one file.
    pub with_file: i64,
    /// Shows: episodes counted toward completion, and episodes with a file.
    pub episodes: i64,
    pub episode_files: i64,
    pub size_bytes: i64,
}

pub async fn library_totals(pool: &PgPool) -> Result<Vec<LibraryTotals>, sqlx::Error> {
    sqlx::query_as::<_, LibraryTotals>(
        "WITH t AS (
             SELECT te.library_id,
                    count(DISTINCT te.title_id) AS titles,
                    count(DISTINCT te.title_id) FILTER (WHERE te.file_count > 0) AS with_file,
                    sum(te.episode_count) AS episodes,
                    sum(te.file_count) FILTER (WHERE te.episode_count IS NOT NULL) AS episode_files
             FROM title_engines te GROUP BY te.library_id),
         s AS (SELECT c.library_id, sum(c.size_bytes) AS size FROM copies c GROUP BY c.library_id)
         SELECT t.library_id, t.titles::bigint AS titles, t.with_file::bigint AS with_file,
                COALESCE(t.episodes, 0)::bigint AS episodes,
                COALESCE(t.episode_files, 0)::bigint AS episode_files,
                COALESCE(s.size, 0)::bigint AS size_bytes
         FROM t LEFT JOIN s ON s.library_id = t.library_id
         ORDER BY t.library_id",
    )
    .fetch_all(pool)
    .await
}

/// One title of the library tables, with its lead copy (movies) and what the server says.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct LibraryRow {
    pub id: i64,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub year: Option<i32>,
    pub poster_url: Option<String>,
    pub rating: Option<f64>,
    pub imdb_rating: Option<f64>,
    pub certification: Option<String>,
    pub runtime_minutes: Option<i32>,
    pub genres: Vec<String>,
    pub studio: Option<String>,
    pub network: Option<String>,
    pub collection: Option<String>,
    pub collection_tmdb_id: Option<i64>,
    pub status: Option<String>,
    pub in_cinemas: Option<DateTime<Utc>>,
    pub digital_release: Option<DateTime<Utc>>,
    pub next_airing: Option<DateTime<Utc>>,
    pub monitored: bool,
    pub profile: Option<String>,
    /// The server's section that holds the title, else the section its engine manages, else
    /// the engine's own library.
    pub library: Option<String>,
    pub engine_added: Option<DateTime<Utc>>,
    pub server_added: Option<DateTime<Utc>>,
    pub plays: i64,
    /// Rotten Tomatoes' critics score in percent, as the server reports it.
    pub critics: Option<i32>,
    pub seasons: i32,
    pub episodes: Option<i32>,
    pub have: Option<i32>,
    /// Files held: copies and files no engine tracks.
    pub files: i64,
    pub size_bytes: i64,
    /// Edition labels, in the list's order.
    pub editions: Vec<String>,
    /// The lead copy, movies only: the release as held (without its notes), the probe's
    /// words, the score, the lane and when it came in.
    pub held: Option<serde_json::Value>,
    pub probe_codec: Option<String>,
    pub probe_hdr: Option<String>,
    pub probe_audio: Option<String>,
    pub probe_channels: Option<String>,
    pub score: Option<i32>,
    pub lane: Option<String>,
    pub copy_added: Option<DateTime<Utc>>,
}

/// Every title of one kind, in one query: the library tables.
pub async fn library_rows(pool: &PgPool, kind: &str) -> Result<Vec<LibraryRow>, sqlx::Error> {
    sqlx::query_as::<_, LibraryRow>(
        "SELECT t.id, t.tmdb_id, t.title, t.year, t.poster_url, t.rating, t.imdb_rating,
                t.certification, t.runtime_minutes, t.genres, t.studio, t.network, t.collection,
                t.collection_tmdb_id, t.status, t.in_cinemas, t.digital_release, t.next_airing,
                COALESCE(e.monitored, false) AS monitored, e.profile,
                COALESCE(srv.library, e.section, e.library) AS library,
                e.added AS engine_added, srv.added AS server_added,
                COALESCE(srv.plays, 0)::bigint AS plays, srv.critics,
                (SELECT count(*) FROM seasons se
                 WHERE se.title_id = t.id AND se.season_number > 0)::int AS seasons,
                e.episodes, e.have,
                (f.copies + f.untracked)::bigint AS files,
                (f.copy_size + f.untracked_size)::bigint AS size_bytes,
                COALESCE((SELECT array_agg(x.label ORDER BY x.rank, x.key) FROM title_editions x
                          WHERE x.title_id = t.id), '{}') AS editions,
                lead.held, lead.probe_codec, lead.probe_hdr, lead.probe_audio, lead.probe_channels,
                lead.score, lead.lane, lead.copy_added
         FROM titles t
         LEFT JOIN LATERAL (
             SELECT bool_or(te.monitored) AS monitored,
                    (array_agg(te.profile ORDER BY te.engine))[1] AS profile,
                    (array_agg(l.name ORDER BY te.engine))[1] AS library,
                    (array_agg(s.name ORDER BY te.engine) FILTER (WHERE s.name IS NOT NULL))[1]
                        AS section,
                    min(te.added_at) AS added,
                    max(te.episode_count) AS episodes,
                    max(te.file_count) AS have
             FROM title_engines te
             LEFT JOIN libraries l ON l.id = te.library_id
             LEFT JOIN LATERAL (
                 SELECT sl.name FROM libraries sl
                 WHERE sl.managed_by = te.engine AND sl.section_key IS NOT NULL
                 ORDER BY sl.section_key LIMIT 1
             ) s ON true
             WHERE te.title_id = t.id
         ) e ON true
         LEFT JOIN LATERAL (
             SELECT sum(ts.plays)::bigint AS plays, min(ts.added_at) AS added,
                    (array_agg(sl.name ORDER BY ts.plays DESC, ts.rating_key)
                         FILTER (WHERE sl.name IS NOT NULL))[1] AS library,
                    max(CASE WHEN ts.critic_image LIKE 'rottentomatoes://%'
                             THEN round(ts.critic_rating * 10)::int END) AS critics
             FROM title_server ts LEFT JOIN libraries sl ON sl.id = ts.library_id
             WHERE ts.title_id = t.id
         ) srv ON true
         LEFT JOIN LATERAL (
             SELECT (SELECT count(*) FROM copies c WHERE c.title_id = t.id) AS copies,
                    (SELECT COALESCE(sum(c.size_bytes), 0) FROM copies c
                     WHERE c.title_id = t.id) AS copy_size,
                    (SELECT count(*) FROM untracked_files u WHERE u.title_id = t.id) AS untracked,
                    (SELECT COALESCE(sum(u.size_bytes), 0) FROM untracked_files u
                     WHERE u.title_id = t.id) AS untracked_size
         ) f ON true
         LEFT JOIN LATERAL (
             SELECT c.held - 'notes' - 'raw' AS held,
                    c.media_info ->> 'videoCodec' AS probe_codec,
                    c.media_info ->> 'videoDynamicRangeType' AS probe_hdr,
                    c.media_info ->> 'audioCodec' AS probe_audio,
                    c.media_info ->> 'audioChannels' AS probe_channels,
                    c.mq_score AS score, c.lane, c.date_added AS copy_added
             FROM copies c WHERE c.title_id = t.id AND t.kind = 'movie'
             ORDER BY c.mq_score DESC NULLS LAST, c.date_added DESC NULLS LAST, c.id
             LIMIT 1
         ) lead ON true
         WHERE t.kind = $1
         ORDER BY t.sort_title, t.id",
    )
    .bind(kind)
    .fetch_all(pool)
    .await
}

/// One engine's word on a title, as a card needs it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct CardLink {
    pub engine: String,
    pub engine_title_id: i64,
    pub profile: Option<String>,
    pub lane: Option<String>,
    pub monitored: bool,
}

/// A title as a card shows it, with what decides its library state.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct CardRow {
    pub id: i64,
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub year: Option<i32>,
    pub poster_url: Option<String>,
    pub rating: Option<f64>,
    pub status: Option<String>,
    pub copies: i64,
    pub episodes: Option<i32>,
    pub have: Option<i32>,
    #[sqlx(json)]
    pub links: Vec<CardLink>,
}

const CARD_SELECT: &str = "SELECT t.id, t.kind, t.tmdb_id, t.title, t.year, t.poster_url, t.rating,
        t.status,
        (SELECT count(*) FROM copies c WHERE c.title_id = t.id) AS copies,
        (SELECT max(te.episode_count) FROM title_engines te WHERE te.title_id = t.id) AS episodes,
        (SELECT max(te.file_count) FROM title_engines te WHERE te.title_id = t.id) AS have,
        COALESCE((SELECT jsonb_agg(jsonb_build_object(
                      'engine', te.engine, 'engine_title_id', te.engine_title_id,
                      'profile', te.profile, 'lane', te.lane, 'monitored', te.monitored)
                      ORDER BY te.engine)
                  FROM title_engines te WHERE te.title_id = t.id), '[]'::jsonb) AS links
     FROM titles t";

/// The cards of these titles.
pub async fn title_cards(pool: &PgPool, ids: &[i64]) -> Result<Vec<CardRow>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, CardRow>(sqlx::AssertSqlSafe(format!(
        "{CARD_SELECT} WHERE t.id = ANY($1)"
    )))
    .bind(ids.to_vec())
    .fetch_all(pool)
    .await
}

/// The cards of the titles an engine knows by these ids of its own.
pub async fn engine_title_cards(
    pool: &PgPool,
    engine: &str,
    engine_ids: &[i64],
) -> Result<Vec<CardRow>, sqlx::Error> {
    if engine_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, CardRow>(sqlx::AssertSqlSafe(format!(
        "{CARD_SELECT} WHERE t.id IN (SELECT te.title_id FROM title_engines te
                                      WHERE te.engine = $1 AND te.engine_title_id = ANY($2))"
    )))
    .bind(engine)
    .bind(engine_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// The movies whose newest file came in last: title id and when.
pub async fn recent_movies(
    pool: &PgPool,
    limit: i64,
) -> Result<Vec<(i64, DateTime<Utc>)>, sqlx::Error> {
    sqlx::query_as::<_, (i64, DateTime<Utc>)>(
        "SELECT c.title_id, max(c.date_added) AS added
         FROM copies c JOIN titles t ON t.id = c.title_id AND t.kind = 'movie'
         WHERE c.date_added IS NOT NULL
         GROUP BY c.title_id ORDER BY added DESC, c.title_id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// An episode file that came in, with the episode's name when the catalog knows it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RecentEpisode {
    pub title_id: i64,
    pub season_number: i32,
    pub episode_number: i32,
    pub name: Option<String>,
    pub added: DateTime<Utc>,
}

/// The episode files that came in last, newest first; a file of several episodes stands for
/// its first.
pub async fn recent_episodes(pool: &PgPool, limit: i64) -> Result<Vec<RecentEpisode>, sqlx::Error> {
    sqlx::query_as::<_, RecentEpisode>(
        "SELECT c.title_id, c.season_number, c.episode_numbers[1] AS episode_number, e.name,
                c.date_added AS added
         FROM copies c
         JOIN titles t ON t.id = c.title_id AND t.kind = 'show'
         LEFT JOIN episodes e ON e.title_id = c.title_id AND e.season_number = c.season_number
                             AND e.episode_number = c.episode_numbers[1]
         WHERE c.date_added IS NOT NULL AND c.season_number IS NOT NULL
           AND cardinality(c.episode_numbers) > 0
         ORDER BY c.date_added DESC, c.id DESC LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The name of one episode of one title, read from its library file name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleEpisodeName {
    pub title_id: i64,
    pub season_number: i32,
    pub episode_number: i32,
    pub name: Option<String>,
}

/// The names of these episodes, given as (title id, season, episode).
pub async fn episode_names_of(
    pool: &PgPool,
    keys: &[(i64, i32, i32)],
) -> Result<Vec<TitleEpisodeName>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let titles: Vec<i64> = keys.iter().map(|k| k.0).collect();
    let seasons: Vec<i32> = keys.iter().map(|k| k.1).collect();
    let numbers: Vec<i32> = keys.iter().map(|k| k.2).collect();
    sqlx::query_as::<_, TitleEpisodeName>(
        "SELECT e.title_id, e.season_number, e.episode_number, e.name
         FROM episodes e
         JOIN unnest($1::bigint[], $2::int[], $3::int[]) AS k(title_id, season, number)
           ON e.title_id = k.title_id AND e.season_number = k.season
          AND e.episode_number = k.number",
    )
    .bind(titles)
    .bind(seasons)
    .bind(numbers)
    .fetch_all(pool)
    .await
}

/// The catalog titles of these items of the media servers, each item as (server, key): two
/// servers may use one key for two items. Answers (server, key, title). No lookup goes by the
/// key alone.
pub async fn server_items_titles(
    pool: &PgPool,
    items: &[(String, String)],
) -> Result<Vec<(String, String, i64)>, sqlx::Error> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let servers: Vec<String> = items.iter().map(|(s, _)| s.clone()).collect();
    let keys: Vec<String> = items.iter().map(|(_, k)| k.clone()).collect();
    sqlx::query_as::<_, (String, String, i64)>(
        "SELECT ts.server, ts.rating_key, ts.title_id
         FROM title_server ts
         JOIN unnest($1::text[], $2::text[]) AS i(server, key)
           ON ts.server = i.server AND ts.rating_key = i.key",
    )
    .bind(servers)
    .bind(keys)
    .fetch_all(pool)
    .await
}

/// What the server says about a catalog title, the most played item first.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct TitleServerRow {
    pub server: String,
    pub rating_key: String,
    pub library_id: Option<String>,
    pub critic_rating: Option<f64>,
    pub critic_image: Option<String>,
    pub audience_rating: Option<f64>,
    pub audience_image: Option<String>,
    pub plays: i64,
    pub last_played: Option<DateTime<Utc>>,
    pub edition: Option<String>,
}

/// The server's machine identifier on the libraries of its sections, so an item's address in
/// the server's web app can be built.
pub async fn write_server_machine(
    conn: &mut PgConnection,
    server: &str,
    machine_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE libraries SET machine_id = $2
         WHERE integration = $1 AND section_key IS NOT NULL
           AND machine_id IS DISTINCT FROM $2",
    )
    .bind(server)
    .bind(machine_id)
    .execute(conn)
    .await
    .map(|_| ())
}

/// A library by id with what a title record names of it: the name, the kind, the server
/// section and its server's machine identifier, the folders, and who manages it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LibraryName {
    pub id: String,
    pub name: String,
    pub kind: Option<String>,
    pub integration: Option<String>,
    pub section_key: Option<String>,
    pub managed_by: Option<String>,
    pub machine_id: Option<String>,
    pub locations: Vec<String>,
}

/// Every library, without counting what it holds.
pub async fn library_names(pool: &PgPool) -> Result<Vec<LibraryName>, sqlx::Error> {
    sqlx::query_as::<_, LibraryName>(
        "SELECT id, name, kind, integration, section_key, managed_by, machine_id, locations
         FROM libraries ORDER BY (section_key IS NULL), name, id",
    )
    .fetch_all(pool)
    .await
}

/// The episodes of a show with a file, as (season, episode), in order.
pub async fn held_episodes(pool: &PgPool, title_id: i64) -> Result<Vec<(i32, i32)>, sqlx::Error> {
    sqlx::query_as::<_, (i32, i32)>(
        "SELECT DISTINCT c.season_number, e.n
         FROM copies c, unnest(c.episode_numbers) AS e(n)
         WHERE c.title_id = $1 AND c.season_number IS NOT NULL
         ORDER BY 1, 2",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

/// A movie of the library that belongs to a collection TMDB knows.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct CollectionMember {
    pub collection_tmdb_id: i64,
    pub collection: Option<String>,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub poster_url: Option<String>,
    pub rating: Option<f64>,
}

/// A title as the search index takes it.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct SearchTitle {
    pub kind: String,
    pub tmdb_id: i64,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i32>,
    pub collection: Option<String>,
    pub collection_tmdb_id: Option<i64>,
    pub studio: Option<String>,
    pub network: Option<String>,
    pub poster_url: Option<String>,
    pub rating: Option<f64>,
}

/// Every title with a TMDB id (the interface names titles by it), for the search index.
pub async fn search_titles(pool: &PgPool) -> Result<Vec<SearchTitle>, sqlx::Error> {
    sqlx::query_as::<_, SearchTitle>(
        "SELECT kind, tmdb_id, title, original_title, year, collection, collection_tmdb_id,
                studio, network, poster_url, rating
         FROM titles WHERE tmdb_id IS NOT NULL ORDER BY kind, sort_title, id",
    )
    .fetch_all(pool)
    .await
}

/// The studios of the library's movies (`studio`) or the networks of its shows (`network`),
/// with how many titles name each, the most first, at most 40.
pub async fn name_counts(pool: &PgPool, field: &str) -> Result<Vec<(String, i64)>, sqlx::Error> {
    let query = if field == "network" {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT network, count(*)::bigint FROM titles
             WHERE kind = 'show' AND network IS NOT NULL AND network <> ''
             GROUP BY network ORDER BY 2 DESC, 1 LIMIT 40",
        )
    } else {
        sqlx::query_as::<_, (String, i64)>(
            "SELECT studio, count(*)::bigint FROM titles
             WHERE kind = 'movie' AND studio IS NOT NULL AND studio <> ''
             GROUP BY studio ORDER BY 2 DESC, 1 LIMIT 40",
        )
    };
    query.fetch_all(pool).await
}

/// Every movie of the library that belongs to a collection, by collection, then year.
pub async fn collection_members(pool: &PgPool) -> Result<Vec<CollectionMember>, sqlx::Error> {
    sqlx::query_as::<_, CollectionMember>(
        "SELECT collection_tmdb_id, collection, tmdb_id, title, year, poster_url, rating
         FROM titles
         WHERE kind = 'movie' AND collection_tmdb_id IS NOT NULL AND tmdb_id IS NOT NULL
         ORDER BY collection_tmdb_id, year NULLS LAST, sort_title",
    )
    .fetch_all(pool)
    .await
}

pub async fn title_server(
    pool: &PgPool,
    title_id: i64,
) -> Result<Vec<TitleServerRow>, sqlx::Error> {
    sqlx::query_as::<_, TitleServerRow>(
        "SELECT server, rating_key, library_id, critic_rating, critic_image, audience_rating,
                audience_image, plays, last_played, edition
         FROM title_server WHERE title_id = $1 ORDER BY plays DESC, rating_key",
    )
    .bind(title_id)
    .fetch_all(pool)
    .await
}

/// One file of a title as it lies on disk: a copy an engine tracks, or a file in the title's
/// folder that no engine tracks.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleFile {
    /// The copy, when an engine tracks the file.
    pub copy_id: Option<i64>,
    /// The title's folder as the engine that tracks the file names it.
    pub folder: Option<String>,
    /// A tracked file: its path below the folder. A file no engine tracks: its whole path.
    pub path: String,
    pub size_bytes: i64,
    /// What the folder scan read of a file no engine tracks: `2160p`, `1080p`.
    pub resolution: Option<String>,
    /// The tags the title carries in the manager whose file this is.
    pub tags: Vec<String>,
}

impl TitleFile {
    /// The whole path of the file. `None` for a tracked file whose engine names no folder.
    pub fn full_path(&self) -> Option<String> {
        if self.copy_id.is_none() {
            return Some(self.path.clone());
        }
        let folder = self.folder.as_deref()?.trim().trim_end_matches(['/', '\\']);
        if folder.is_empty() {
            return None;
        }
        // The folder's own separator: a manager on Windows names it with backslashes.
        let between = if folder.contains('\\') && !folder.contains('/') {
            '\\'
        } else {
            '/'
        };
        Some(format!("{folder}{between}{}", self.path))
    }
}

/// The files of a title whose own name (the last part of the path) is `name`: the copies its
/// engines track first, then the files in its folders no engine tracks.
pub async fn title_files_named(
    pool: &PgPool,
    title_id: i64,
    name: &str,
) -> Result<Vec<TitleFile>, sqlx::Error> {
    sqlx::query_as::<_, TitleFile>(
        r"SELECT copy_id, folder, path, size_bytes, resolution, tags
          FROM (
              SELECT c.id AS copy_id, te.path AS folder, c.relative_path AS path, c.size_bytes,
                     NULL::text AS resolution, COALESCE(te.tags, '{}'::text[]) AS tags,
                     0 AS untracked, c.id AS ord
              FROM copies c
              LEFT JOIN title_engines te
                     ON te.engine = c.engine AND te.engine_title_id = c.engine_title_id
              WHERE c.title_id = $1
              UNION ALL
              SELECT NULL, NULL, u.path, u.size_bytes, u.resolution,
                     COALESCE((SELECT array_agg(DISTINCT tag)
                               FROM title_engines tu, unnest(tu.tags) AS tag
                               WHERE tu.title_id = u.title_id AND tu.engine = u.engine),
                              '{}'::text[]),
                     1, u.id
              FROM untracked_files u WHERE u.title_id = $1
          ) f
          WHERE regexp_replace(f.path, '^.*[/\\]', '') = $2
          ORDER BY untracked, ord",
    )
    .bind(title_id)
    .bind(name)
    .fetch_all(pool)
    .await
}

/// Every title's folder, as each engine that manages the title names it: (title id, folder).
/// A file outside the catalog's own records (a processing run's source or result) is linked to
/// its title through the folder it lies in.
pub async fn title_folders(pool: &PgPool) -> Result<Vec<(i64, String)>, sqlx::Error> {
    sqlx::query_as::<_, (i64, String)>(
        "SELECT title_id, path FROM title_engines
         WHERE path IS NOT NULL AND path <> '' ORDER BY title_id, engine",
    )
    .fetch_all(pool)
    .await
}

/// A title with the ids other things name it by.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TitleIds {
    pub id: i64,
    /// `movie` or `show`.
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub imdb_id: Option<String>,
}

/// The titles that carry one of these TMDB, TVDB or IMDb ids (the last in lower case). A file
/// that lies in no title's folder (a title that moved since) is linked to its title through
/// the ids its path names.
pub async fn titles_with_ids(
    pool: &PgPool,
    tmdb_ids: &[i64],
    tvdb_ids: &[i64],
    imdb_ids: &[String],
) -> Result<Vec<TitleIds>, sqlx::Error> {
    if tmdb_ids.is_empty() && tvdb_ids.is_empty() && imdb_ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, TitleIds>(
        "SELECT id, kind, tmdb_id, tvdb_id, imdb_id FROM titles
         WHERE tmdb_id = ANY($1) OR tvdb_id = ANY($2) OR imdb_id = ANY($3)
         ORDER BY id",
    )
    .bind(tmdb_ids.to_vec())
    .bind(tvdb_ids.to_vec())
    .bind(imdb_ids.to_vec())
    .fetch_all(pool)
    .await
}

/// How many files of the library carry one release group, collective and service, as each
/// file was scored.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ReleaseTags {
    pub release_group: Option<String>,
    pub collective: Option<String>,
    pub service: Option<String>,
    pub files: i64,
}

/// The release groups, collectives and services of the library's files, with their counts.
pub async fn release_tags(pool: &PgPool) -> Result<Vec<ReleaseTags>, sqlx::Error> {
    sqlx::query_as::<_, ReleaseTags>(
        "SELECT held->>'group' AS release_group, held->>'collective' AS collective,
                held->>'service' AS service, count(*)::bigint AS files
         FROM copies
         GROUP BY 1, 2, 3
         ORDER BY 4 DESC",
    )
    .fetch_all(pool)
    .await
}

/// A file of the library as it was scored: the release, what is known beyond the name, and
/// its title.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct HeldFile {
    pub copy_id: i64,
    pub title_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub season_number: Option<i32>,
    pub episode_numbers: Vec<i32>,
    pub relative_path: String,
    /// The title's folder, as its manager names it.
    pub folder: Option<String>,
    /// The episode's name, for an episode file.
    pub episode_title: Option<String>,
    pub size_bytes: i64,
    pub edition_label: String,
    pub edition_marked: bool,
    pub held: serde_json::Value,
    pub media_info: Option<serde_json::Value>,
    pub languages: Vec<String>,
    pub original_language: Option<String>,
    pub tags: Vec<String>,
    /// What the file's manager reports, which it names the file from: the quality and its
    /// revision, the release group, Radarr's edition and the custom formats.
    pub arr_quality: Option<String>,
    pub arr_proper: bool,
    pub arr_repack: bool,
    pub arr_group: Option<String>,
    pub arr_edition: Option<String>,
    pub arr_custom_formats: Vec<String>,
}

/// The files of one title, episodes in order, at most `limit`.
pub async fn held_files_of_title(
    pool: &PgPool,
    title_id: i64,
    limit: i64,
) -> Result<Vec<HeldFile>, sqlx::Error> {
    sqlx::query_as::<_, HeldFile>(
        "SELECT c.id AS copy_id, c.title_id, t.title, t.year, t.kind, t.tmdb_id, t.tvdb_id,
                t.imdb_id, c.season_number, c.episode_numbers, c.relative_path,
                te.path AS folder, e.name AS episode_title, c.size_bytes,
                c.edition_label, c.edition_marked, c.held, c.media_info, c.languages,
                t.original_language, COALESCE(te.tags, '{}') AS tags, c.arr_quality,
                c.arr_proper, c.arr_repack, c.arr_group, c.arr_edition, c.arr_custom_formats
         FROM copies c
         JOIN titles t ON t.id = c.title_id
         LEFT JOIN title_engines te
                ON te.engine = c.engine AND te.engine_title_id = c.engine_title_id
         LEFT JOIN episodes e
                ON e.title_id = c.title_id AND e.season_number = c.season_number
               AND e.episode_number = c.episode_numbers[1]
         WHERE c.title_id = $1
         ORDER BY c.season_number NULLS FIRST, c.episode_numbers, c.id
         LIMIT $2",
    )
    .bind(title_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Files of one kind of title (`movie` or `show`), those of `lane` first, newest first, at
/// most `limit`.
pub async fn held_files_sample(
    pool: &PgPool,
    kind: &str,
    lane: &str,
    limit: i64,
) -> Result<Vec<HeldFile>, sqlx::Error> {
    sqlx::query_as::<_, HeldFile>(
        "SELECT c.id AS copy_id, c.title_id, t.title, t.year, t.kind, t.tmdb_id, t.tvdb_id,
                t.imdb_id, c.season_number, c.episode_numbers, c.relative_path,
                te.path AS folder, e.name AS episode_title, c.size_bytes,
                c.edition_label, c.edition_marked, c.held, c.media_info, c.languages,
                t.original_language, COALESCE(te.tags, '{}') AS tags, c.arr_quality,
                c.arr_proper, c.arr_repack, c.arr_group, c.arr_edition, c.arr_custom_formats
         FROM copies c
         JOIN titles t ON t.id = c.title_id
         LEFT JOIN title_engines te
                ON te.engine = c.engine AND te.engine_title_id = c.engine_title_id
         LEFT JOIN episodes e
                ON e.title_id = c.title_id AND e.season_number = c.season_number
               AND e.episode_number = c.episode_numbers[1]
         WHERE t.kind = $1
         ORDER BY (c.lane = $2) DESC NULLS LAST, c.date_added DESC NULLS LAST, c.id DESC
         LIMIT $3",
    )
    .bind(kind)
    .bind(lane)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Every file of one kind of title, for a naming convention to be checked against: at most
/// `limit`, by title.
pub async fn held_files_of_kind(
    pool: &PgPool,
    kind: &str,
    limit: i64,
) -> Result<Vec<HeldFile>, sqlx::Error> {
    sqlx::query_as::<_, HeldFile>(
        "SELECT c.id AS copy_id, c.title_id, t.title, t.year, t.kind, t.tmdb_id, t.tvdb_id,
                t.imdb_id, c.season_number, c.episode_numbers, c.relative_path,
                te.path AS folder, e.name AS episode_title, c.size_bytes,
                c.edition_label, c.edition_marked, c.held, c.media_info, c.languages,
                t.original_language, COALESCE(te.tags, '{}') AS tags, c.arr_quality,
                c.arr_proper, c.arr_repack, c.arr_group, c.arr_edition, c.arr_custom_formats
         FROM copies c
         JOIN titles t ON t.id = c.title_id
         LEFT JOIN title_engines te
                ON te.engine = c.engine AND te.engine_title_id = c.engine_title_id
         LEFT JOIN episodes e
                ON e.title_id = c.title_id AND e.season_number = c.season_number
               AND e.episode_number = c.episode_numbers[1]
         WHERE t.kind = $1
         ORDER BY t.sort_title, c.season_number NULLS FIRST, c.episode_numbers, c.id
         LIMIT $2",
    )
    .bind(kind)
    .bind(limit)
    .fetch_all(pool)
    .await
}
