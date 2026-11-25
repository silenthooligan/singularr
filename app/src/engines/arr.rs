//! The Radarr and Sonarr adapters (API v3): read, and act where the owner allows it.
//!
//! Endpoints read, all GET: `/system/status`, `/tag`, `/qualityprofile`, `/movie`,
//! `/moviefile?movieId=` (repeated), `/series`, `/episodefile?seriesId=`, `/queue`,
//! `/history?eventType=1`, `/parse?title=`, for naming `/config/naming` and `/customformat`,
//! and for the pages (asked on demand, kept a short
//! time by the caller) `/calendar`, `/history`, `/wanted/missing` and `/wanted/cutoff`. On a
//! person's click: `/release?movieId=`, `?episodeId=` or `?seriesId=&seasonNumber=` (the
//! interactive search, which asks every indexer), `/episode?seriesId=`, `/manualimport?downloadId=`.
//!
//! The acts ([`ACTIONS`], each in the one form [`calls`] declares, passed by `core::outbound`
//! only while the integration's access is `act`): grab a release (`POST /release`), search
//! (`POST /command` with a search command and its ids, nothing else), import a refused download
//! as it is (`POST /command` with the manual import), remove a download from the queue
//! (`DELETE /queue/{id}`). Never, whatever is asked: shutting down, restarting or restoring the
//! manager, deleting or editing a movie, a series or a file, any setting or profile.
//!
//! The `/movie` and `/series` lists also give what a title is (overview, genres, runtime,
//! certification, studio or network, rating, images, dates, and a show's seasons with their
//! episode counts). Those are read leniently: a surprise in one of them is no value, never a
//! failed listing.
//!
//! Radarr's `/movie` list carries each movie's file but not the file's custom formats or
//! their score. Those come from `/moviefile`, asked in batches and only for files that are
//! new or changed since the last pass, with a full refresh every few hours.

use super::access::Action;
use super::model::ArrParse;
use super::{
    ArrNaming, CalendarEntry, EngineError, EngineInfo, EpisodeRef, FoundRelease, GrabPage,
    HistoryEntry, ImportFile, ImportMode, LibraryEngine, ManagerAct, ManagerActed, ManagerEpisode,
    QueueEntry, QueueItem, ReleaseScope, WantedEntry, WantedPage,
};
use crate::catalog::ingest::{EngineRef, FileFacts, SeasonFacts, TitleFacts};
use crate::catalog::model::{TitleAbout, TitleKind};
use crate::core::config::{ArrConfig, ArrKind};
use crate::core::outbound::{
    Body, BodyRule, Calls, Method, OutRequest, OutResponse, Outbound, QueryValue, encode_component,
    expect_ok,
};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use singularr_core::naming::parse_library_name;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Queue items asked for per page, and the most pages read in one pass.
const QUEUE_PAGE: u32 = 500;
const QUEUE_MAX_PAGES: u32 = 10;
/// Movie ids per `/moviefile` call, and how long a file's formats are kept before every
/// file is asked about again (the arr's custom formats can change under an unchanged file).
const MOVIEFILE_BATCH: usize = 100;
const FORMATS_REFRESH: Duration = Duration::from_secs(6 * 3600);

#[derive(Clone)]
pub struct ArrClient {
    engine: EngineRef,
    base: String,
    api_key: String,
    out: Outbound,
}

impl ArrClient {
    /// `None` when the engine has no URL or no key.
    pub fn new(config: &ArrConfig, out: Outbound) -> Option<Self> {
        Some(ArrClient {
            engine: EngineRef {
                id: config.id.to_string(),
                library: config.library.to_string(),
                kind: match config.kind {
                    ArrKind::Radarr => TitleKind::Movie,
                    ArrKind::Sonarr => TitleKind::Show,
                },
            },
            base: config.url.clone()?,
            api_key: config.api_key.clone()?,
            out,
        })
    }

    async fn get<T: DeserializeOwned>(
        &self,
        what: &'static str,
        path: &str,
    ) -> Result<T, EngineError> {
        let url = format!("{}/api/v3{path}", self.base);
        let res = self
            .out
            .send(
                crate::core::outbound::OutRequest::get(&url)
                    .header("X-Api-Key", self.api_key.clone()),
            )
            .await?;
        crate::core::outbound::expect_ok(&url, &res)?;
        serde_json::from_slice(&res.body).map_err(|e| EngineError::Decode {
            engine: self.engine.id.clone(),
            what,
            message: e.to_string(),
        })
    }

    async fn status(&self) -> Result<EngineInfo, EngineError> {
        let raw: RawStatus = self.get("system status", "/system/status").await?;
        Ok(EngineInfo {
            app: raw.app_name,
            version: raw.version,
        })
    }

    async fn tags(&self) -> Result<HashMap<i64, String>, EngineError> {
        let raw: Vec<RawTag> = self.get("tags", "/tag").await?;
        Ok(raw.into_iter().map(|t| (t.id, t.label)).collect())
    }

    async fn profiles(&self) -> Result<HashMap<i64, String>, EngineError> {
        let raw: Vec<RawProfile> = self.get("quality profiles", "/qualityprofile").await?;
        Ok(raw.into_iter().map(|p| (p.id, p.name)).collect())
    }

    /// Every item of the queue as the manager writes it, page by page.
    async fn queue_raw(&self, unknown_flag: &str) -> Result<Vec<RawQueueItem>, EngineError> {
        let mut items = Vec::new();
        for page in 1..=QUEUE_MAX_PAGES {
            let raw: RawQueuePage = self
                .get(
                    "queue",
                    &format!("/queue?page={page}&pageSize={QUEUE_PAGE}&{unknown_flag}=true"),
                )
                .await?;
            let got = raw.records.len();
            items.extend(raw.records);
            if got == 0 || items.len() as i64 >= raw.total_records.unwrap_or(0) {
                break;
            }
        }
        Ok(items)
    }

    async fn queue(&self, unknown_flag: &str) -> Result<Vec<QueueItem>, EngineError> {
        Ok(self
            .queue_raw(unknown_flag)
            .await?
            .into_iter()
            .map(queue_item)
            .collect())
    }

    /// The queue entries of one download, read now.
    async fn queue_entries(
        &self,
        unknown_flag: &str,
        download_id: &str,
    ) -> Result<Vec<QueueEntry>, EngineError> {
        let wanted = download_id.trim().to_ascii_uppercase();
        Ok(self
            .queue_raw(unknown_flag)
            .await?
            .into_iter()
            .filter_map(queue_entry)
            .filter(|e| e.download_id == wanted)
            .collect())
    }

    /// Which manager this is, by the kind of title it holds.
    fn arr_kind(&self) -> ArrKind {
        match self.engine.kind {
            TitleKind::Movie => ArrKind::Radarr,
            TitleKind::Show => ArrKind::Sonarr,
        }
    }

    /// The interactive search. It asks every indexer, so it has a limit of its own.
    async fn releases(&self, scope: ReleaseScope) -> Result<Vec<FoundRelease>, EngineError> {
        let query = match scope {
            ReleaseScope::Movie { movie } => format!("movieId={movie}"),
            ReleaseScope::Episode { episode } => format!("episodeId={episode}"),
            ReleaseScope::Season { series, season } => {
                format!("seriesId={series}&seasonNumber={season}")
            }
        };
        let url = format!("{}{RELEASE_PATH}?{query}", self.base);
        let res = self
            .out
            .send(
                OutRequest::get(&url)
                    .header("X-Api-Key", self.api_key.clone())
                    .timeout(SEARCH_TIMEOUT),
            )
            .await?;
        expect_ok(&url, &res)?;
        found_releases(&res.body).map_err(|e| EngineError::Decode {
            engine: self.engine.id.clone(),
            what: "releases",
            message: e.to_string(),
        })
    }

    /// A series' episodes with their ids: one season's, or all.
    async fn episodes(
        &self,
        series: i64,
        season: Option<i32>,
    ) -> Result<Vec<ManagerEpisode>, EngineError> {
        let mut path = format!("/episode?seriesId={series}");
        if let Some(season) = season {
            path.push_str(&format!("&seasonNumber={season}"));
        }
        let raw: Vec<RawEpisode> = self.get("episodes", &path).await?;
        Ok(raw
            .into_iter()
            .filter_map(|e| {
                Some(ManagerEpisode {
                    id: e.id?,
                    season: e.season_number?,
                    number: e.episode_number?,
                    has_file: e.has_file,
                    monitored: e.monitored,
                })
            })
            .collect())
    }

    /// What a manual import offers for one download, named by its id as the manager wrote it.
    async fn import_offer(&self, download_id: &str) -> Result<Vec<ImportFile>, EngineError> {
        let raw: Vec<RawImport> = self
            .get(
                "manual import",
                &format!(
                    "/manualimport?downloadId={}",
                    encode_component(download_id.trim())
                ),
            )
            .await?;
        Ok(raw.into_iter().filter_map(import_file).collect())
    }

    /// One act, in the form [`calls`] declares. The manager answers a change it takes with
    /// 2xx, and one it cannot make (a release no longer in its cache, a queue item that is
    /// gone, a body it does not take) with 4xx and its words.
    async fn act(&self, act: &ManagerAct) -> Result<ManagerActed, EngineError> {
        let Some((method, path, body)) = act_call(self.arr_kind(), act) else {
            return Ok(ManagerActed::NotOffered);
        };
        let url = format!("{}{path}", self.base);
        let req = OutRequest {
            method,
            url: url.clone(),
            headers: vec![("X-Api-Key".to_string(), self.api_key.clone())],
            body: body.map_or(Body::None, |b| Body::Json(b.to_string())),
            timeout: None,
        };
        let res = self.out.send(req).await?;
        if matches!(res.status, 400 | 404 | 409 | 422) {
            return Ok(ManagerActed::Refused(refusal_words(&res)));
        }
        expect_ok(&url, &res)?;
        // A command answers with itself; a grab with the release; a removal with nothing.
        let command = matches!(
            act,
            ManagerAct::SearchMovies { .. }
                | ManagerAct::SearchEpisodes { .. }
                | ManagerAct::SearchSeason { .. }
                | ManagerAct::SearchSeries { .. }
                | ManagerAct::Import { .. }
        )
        .then(|| serde_json::from_slice::<Value>(&res.body).ok())
        .flatten()
        .and_then(|v| v.get("id").and_then(Value::as_i64));
        Ok(ManagerActed::Done { command })
    }

    async fn grabs(&self, page: u32, page_size: u32) -> Result<GrabPage, EngineError> {
        let raw: RawHistoryPage = self
            .get(
                "grab history",
                &format!(
                    "/history?page={page}&pageSize={page_size}&eventType=1&sortKey=date&sortDirection=descending"
                ),
            )
            .await?;
        Ok(GrabPage {
            records: raw.records.len(),
            total_records: raw.total_records.unwrap_or(0),
            download_ids: raw
                .records
                .into_iter()
                .filter_map(|r| r.download_id)
                .map(|id| id.trim().to_ascii_uppercase())
                .filter(|id| !id.is_empty())
                .collect(),
        })
    }

    async fn parse(&self, name: &str) -> Result<ArrParse, EngineError> {
        let raw: RawParse = self
            .get("parse", &format!("/parse?title={}", encode_component(name)))
            .await?;
        Ok(arr_parse(raw))
    }

    /// How the manager names files, and the custom formats it writes into names. A manager
    /// that cannot list its custom formats still says how it names files.
    async fn naming(&self) -> Result<ArrNaming, EngineError> {
        let raw: RawNaming = self.get("naming", "/config/naming").await?;
        let formats: Vec<RawFormat> = self
            .get("custom formats", "/customformat")
            .await
            .unwrap_or_default();
        Ok(ArrNaming {
            rename: raw.rename_movies.or(raw.rename_episodes).unwrap_or(false),
            movie_folder: text(raw.movie_folder_format),
            movie_file: text(raw.standard_movie_format),
            series_folder: text(raw.series_folder_format),
            season_folder: text(raw.season_folder_format),
            episode_file: text(raw.standard_episode_format),
            colon: raw.colon_replacement_format.map(|v| match v {
                serde_json::Value::String(s) => s,
                other => other.to_string(),
            }),
            named_formats: formats
                .into_iter()
                .filter(|f| f.include_custom_format_when_renaming == Some(true))
                .map(|f| f.name)
                .filter(|n| !n.trim().is_empty())
                .collect(),
        })
    }

    /// The file records of these movies, by file id.
    async fn movie_files(&self, movie_ids: &[i64]) -> Result<HashMap<i64, RawFile>, EngineError> {
        let mut files = HashMap::new();
        for batch in movie_ids.chunks(MOVIEFILE_BATCH) {
            let query: Vec<String> = batch.iter().map(|id| format!("movieId={id}")).collect();
            let found: Vec<RawFile> = self
                .get("movie files", &format!("/moviefile?{}", query.join("&")))
                .await?;
            files.extend(found.into_iter().map(|f| (f.id, f)));
        }
        Ok(files)
    }

    /// The newest records of the history, every kind of event. `extra` adds the arr's own
    /// switches (`&includeEpisode=true` for Sonarr).
    async fn history_records(
        &self,
        limit: u32,
        extra: &str,
    ) -> Result<Vec<HistoryEntry>, EngineError> {
        let raw: RawHistoryList = self
            .get(
                "history",
                &format!(
                    "/history?page=1&pageSize={}&sortKey=date&sortDirection=descending{extra}",
                    limit.clamp(1, PAGES_MAX)
                ),
            )
            .await?;
        Ok(raw.records.into_iter().filter_map(history_entry).collect())
    }
}

/// The most entries a list for the pages asks for in one call.
const PAGES_MAX: u32 = 1000;

/// The calendar's days as the arrs take them: the first day, and the day after the last.
fn calendar_query(start: NaiveDate, end: NaiveDate, extra: &str) -> String {
    let after = end.succ_opt().unwrap_or(end);
    format!(
        "/calendar?start={}&end={}&unmonitored=false{extra}",
        start.format("%Y-%m-%d"),
        after.format("%Y-%m-%d")
    )
}

/// The lenient readers: the SDK's, at their old paths.
pub(super) use singularr_sdk::read::{lenient, null_as_default};

// What the arrs send. Every field is optional or defaulted: a field an arr version does not
// send must not fail the whole listing.

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStatus {
    app_name: Option<String>,
    version: Option<String>,
}

#[derive(Deserialize)]
struct RawTag {
    id: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    label: String,
}

#[derive(Deserialize)]
struct RawProfile {
    id: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
}

/// `/config/naming`: Radarr's fields or Sonarr's, every one optional.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawNaming {
    rename_movies: Option<bool>,
    rename_episodes: Option<bool>,
    /// Radarr writes a word (`smart`), Sonarr a number (`4`).
    colon_replacement_format: Option<serde_json::Value>,
    standard_movie_format: Option<String>,
    movie_folder_format: Option<String>,
    standard_episode_format: Option<String>,
    series_folder_format: Option<String>,
    season_folder_format: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFormat {
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
    include_custom_format_when_renaming: Option<bool>,
}

#[derive(Deserialize, Default)]
struct Named {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImage {
    cover_type: Option<String>,
    remote_url: Option<String>,
}

/// One source's rating. The arrs write 0 for "not rated".
#[derive(Deserialize, Default)]
struct RawRating {
    value: Option<f64>,
}

/// Radarr keeps one rating per source. TMDB's and IMDb's are read; Metacritic and Rotten
/// Tomatoes count to 100 and are left out.
#[derive(Deserialize, Default)]
struct RawMovieRatings {
    imdb: Option<RawRating>,
    tmdb: Option<RawRating>,
}

/// The collection a movie belongs to. Newer Radarr versions call its name `title`, older
/// ones `name`.
#[derive(Deserialize, Default)]
struct RawCollection {
    title: Option<String>,
    name: Option<String>,
    #[serde(default, rename = "tmdbId")]
    tmdb_id: Option<i64>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawRevision {
    version: Option<i32>,
    is_repack: Option<bool>,
}

#[derive(Deserialize, Default)]
struct RawQuality {
    quality: Option<Named>,
    revision: Option<RawRevision>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawFile {
    id: i64,
    season_number: Option<i32>,
    relative_path: Option<String>,
    path: Option<String>,
    size: Option<i64>,
    date_added: Option<String>,
    scene_name: Option<String>,
    release_group: Option<String>,
    /// Radarr sends a string, empty for no edition. Sonarr does not send the field.
    edition: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    languages: Vec<Named>,
    quality: Option<RawQuality>,
    #[serde(default, deserialize_with = "null_as_default")]
    custom_formats: Vec<Named>,
    custom_format_score: Option<i32>,
    media_info: Option<serde_json::Value>,
    original_file_path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMovie {
    id: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    sort_title: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    original_title: Option<String>,
    year: Option<i32>,
    tmdb_id: Option<i64>,
    imdb_id: Option<String>,
    original_language: Option<Named>,
    status: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    monitored: bool,
    quality_profile_id: Option<i64>,
    #[serde(default, deserialize_with = "null_as_default")]
    tags: Vec<i64>,
    path: Option<String>,
    added: Option<String>,
    size_on_disk: Option<i64>,
    #[serde(default, deserialize_with = "null_as_default")]
    images: Vec<RawImage>,
    movie_file: Option<RawFile>,
    #[serde(default, deserialize_with = "lenient")]
    overview: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    genres: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lenient")]
    runtime: Option<i32>,
    #[serde(default, deserialize_with = "lenient")]
    certification: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    studio: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    collection: Option<RawCollection>,
    #[serde(default, deserialize_with = "lenient")]
    ratings: Option<RawMovieRatings>,
    #[serde(default, deserialize_with = "lenient")]
    in_cinemas: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    digital_release: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawStats {
    episode_file_count: Option<i32>,
    /// Monitored and aired, or held: what the arr counts toward completion.
    episode_count: Option<i32>,
    total_episode_count: Option<i32>,
    size_on_disk: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSeason {
    season_number: i32,
    #[serde(default, deserialize_with = "null_as_default")]
    monitored: bool,
    statistics: Option<RawStats>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSeries {
    id: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    sort_title: Option<String>,
    year: Option<i32>,
    tvdb_id: Option<i64>,
    tmdb_id: Option<i64>,
    imdb_id: Option<String>,
    original_language: Option<Named>,
    status: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    monitored: bool,
    quality_profile_id: Option<i64>,
    #[serde(default, deserialize_with = "null_as_default")]
    tags: Vec<i64>,
    path: Option<String>,
    added: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    images: Vec<RawImage>,
    #[serde(default, deserialize_with = "null_as_default")]
    seasons: Vec<RawSeason>,
    statistics: Option<RawStats>,
    #[serde(default, deserialize_with = "lenient")]
    overview: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    genres: Option<Vec<String>>,
    #[serde(default, deserialize_with = "lenient")]
    runtime: Option<i32>,
    #[serde(default, deserialize_with = "lenient")]
    certification: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    network: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    ratings: Option<RawRating>,
    #[serde(default, deserialize_with = "lenient")]
    first_aired: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    next_airing: Option<String>,
}

#[derive(Deserialize)]
struct RawStatusMessage {
    title: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    messages: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawQueueItem {
    id: i64,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    title: Option<String>,
    status: Option<String>,
    tracked_download_status: Option<String>,
    tracked_download_state: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    status_messages: Vec<RawStatusMessage>,
    error_message: Option<String>,
    download_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    indexer: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    protocol: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    download_client: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    download_client_has_post_import_category: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawQueuePage {
    #[serde(default, deserialize_with = "null_as_default")]
    records: Vec<RawQueueItem>,
    total_records: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryRecord {
    download_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryPage {
    #[serde(default, deserialize_with = "null_as_default")]
    records: Vec<RawHistoryRecord>,
    total_records: Option<i64>,
}

/// An episode as the calendar, the history and the wanted lists send it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEpisode {
    id: Option<i64>,
    series_id: Option<i64>,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    title: Option<String>,
    air_date: Option<String>,
    air_date_utc: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    has_file: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    monitored: bool,
    #[serde(default, deserialize_with = "lenient")]
    last_search_time: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    episode_file: Option<RawFileQuality>,
}

/// A file record, read for its quality alone.
#[derive(Deserialize)]
struct RawFileQuality {
    #[serde(default, deserialize_with = "lenient")]
    quality: Option<RawQuality>,
}

/// A movie as the calendar and the wanted lists send it, read for its dates and file.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMovieDates {
    id: i64,
    #[serde(default, deserialize_with = "null_as_default")]
    monitored: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    has_file: bool,
    #[serde(default, deserialize_with = "lenient")]
    in_cinemas: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    digital_release: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    physical_release: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    release_date: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    last_search_time: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    movie_file: Option<RawFileQuality>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMoviePage {
    #[serde(default, deserialize_with = "null_as_default")]
    records: Vec<RawMovieDates>,
    total_records: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEpisodePage {
    #[serde(default, deserialize_with = "null_as_default")]
    records: Vec<RawEpisode>,
    total_records: Option<i64>,
}

/// One record of the full history. `data` holds the event's details, all as text.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryItem {
    id: i64,
    movie_id: Option<i64>,
    series_id: Option<i64>,
    source_title: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    quality: Option<RawQuality>,
    date: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    event_type: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    data: Option<HashMap<String, serde_json::Value>>,
    #[serde(default, deserialize_with = "lenient")]
    episode: Option<RawEpisode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawHistoryList {
    #[serde(default, deserialize_with = "null_as_default")]
    records: Vec<RawHistoryItem>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawParsedInfo {
    #[serde(default, deserialize_with = "null_as_default")]
    movie_titles: Vec<String>,
    series_title: Option<String>,
    year: Option<i32>,
    season_number: Option<i32>,
    #[serde(default, deserialize_with = "null_as_default")]
    episode_numbers: Vec<i32>,
    quality: Option<RawQuality>,
    release_group: Option<String>,
    edition: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    languages: Vec<Named>,
}

#[derive(Deserialize, Default)]
struct RawTitled {
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawParse {
    parsed_movie_info: Option<RawParsedInfo>,
    parsed_episode_info: Option<RawParsedInfo>,
    movie: Option<RawTitled>,
    series: Option<RawTitled>,
    #[serde(default, deserialize_with = "null_as_default")]
    custom_formats: Vec<Named>,
    custom_format_score: Option<i32>,
}

// Conversions ------------------------------------------------------------------------------------

fn text(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// An arr timestamp. The arrs write `0001-01-01T00:00:00Z` for "never", which is no date.
fn date(v: &Option<String>) -> Option<DateTime<Utc>> {
    let parsed = DateTime::parse_from_rfc3339(v.as_deref()?.trim()).ok()?;
    (parsed.year() > 1900).then(|| parsed.with_timezone(&Utc))
}

fn names(list: Vec<Named>) -> Vec<String> {
    list.into_iter().filter_map(|n| text(n.name)).collect()
}

/// The remote address of a title's image of one kind: `poster` or `fanart`.
fn image(images: &[RawImage], cover: &str) -> Option<String> {
    images
        .iter()
        .find(|i| {
            i.cover_type
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case(cover))
        })
        .and_then(|i| text(i.remote_url.clone()))
}

fn genres(list: Option<Vec<String>>) -> Vec<String> {
    list.unwrap_or_default()
        .into_iter()
        .filter_map(|g| text(Some(g)))
        .collect()
}

/// A rating on the 0 to 10 scale, to one decimal. 0 is "not rated".
fn rating_value(rating: Option<RawRating>) -> Option<f64> {
    let value = rating?.value?;
    (value > 0.0 && value <= 10.0).then(|| (value * 10.0).round() / 10.0)
}

fn tag_names(ids: &[i64], tags: &HashMap<i64, String>) -> Vec<String> {
    let mut names: Vec<String> = ids.iter().filter_map(|id| tags.get(id).cloned()).collect();
    names.sort();
    names
}

/// The episode title of a library file name: the part after `SxxEyy - ` and before the
/// first bracket.
fn episode_name(file_name: &str) -> Option<String> {
    let mut parts = file_name.splitn(3, " - ");
    let (_, _, rest) = (parts.next()?, parts.next()?, parts.next()?);
    let name = rest.split('[').next()?.trim();
    let name = name
        .rsplit_once('.')
        .filter(|(_, ext)| ext.len() <= 4 && !ext.contains(' '))
        .map_or(name, |(n, _)| n);
    (!name.is_empty()).then(|| name.to_string())
}

fn file_facts(raw: RawFile, has_edition: bool) -> FileFacts {
    let relative_path = text(raw.relative_path)
        .or_else(|| text(raw.path))
        .unwrap_or_default();
    let file_name = relative_path.rsplit(['/', '\\']).next().unwrap_or_default();
    let lib = parse_library_name(file_name);
    let scene_name = text(raw.scene_name);
    // The library name states season and episodes; a file that was never renamed falls
    // back to what the release parser reads from its scene name.
    let (season, episodes) = if lib.episodes.is_empty() {
        let parsed = singularr_core::release::parse(scene_name.as_deref().unwrap_or(file_name));
        (parsed.season, parsed.episodes)
    } else {
        (lib.season, lib.episodes)
    };
    let (quality, revision) = match raw.quality {
        Some(q) => (q.quality, q.revision),
        None => (None, None),
    };
    FileFacts {
        engine_file_id: raw.id,
        season_number: raw.season_number.or(season.map(i32::from)),
        episode_numbers: episodes.into_iter().map(i32::from).collect(),
        episode_name: raw.season_number.and_then(|_| episode_name(file_name)),
        relative_path,
        scene_name,
        original_path: text(raw.original_file_path),
        size_bytes: raw.size.unwrap_or(0),
        date_added: date(&raw.date_added),
        // A revision above 1 is what the arrs call a proper.
        arr_proper: revision.as_ref().and_then(|r| r.version).unwrap_or(1) > 1,
        arr_repack: revision.as_ref().and_then(|r| r.is_repack).unwrap_or(false),
        arr_quality: quality.and_then(|q| text(q.name)),
        arr_group: text(raw.release_group),
        arr_edition: if has_edition {
            Some(raw.edition.unwrap_or_default().trim().to_string())
        } else {
            None
        },
        arr_custom_formats: names(raw.custom_formats),
        arr_score: raw.custom_format_score,
        languages: names(raw.languages),
        media_info: raw.media_info,
    }
}

/// What a queue item says of itself: each line of its status messages once (a message without
/// lines is its title), then its error.
fn queue_messages(raw: &RawQueueItem) -> Vec<String> {
    let mut messages = Vec::new();
    for m in &raw.status_messages {
        let title = text(m.title.clone());
        if m.messages.is_empty() {
            messages.extend(title);
        } else {
            for line in &m.messages {
                let line = line.trim().to_string();
                if !line.is_empty() && !messages.contains(&line) {
                    messages.push(line);
                }
            }
        }
    }
    if let Some(e) = text(raw.error_message.clone())
        && !messages.contains(&e)
    {
        messages.push(e);
    }
    messages
}

fn queue_item(raw: RawQueueItem) -> QueueItem {
    let messages = queue_messages(&raw);
    QueueItem {
        queue_id: raw.id,
        engine_title_id: raw.movie_id.or(raw.series_id).filter(|id| *id > 0),
        title: raw.title.unwrap_or_default(),
        download_id: text(raw.download_id).map(|id| id.to_ascii_uppercase()),
        status: text(raw.status),
        tracked_state: text(raw.tracked_download_state),
        tracked_status: text(raw.tracked_download_status),
        messages,
        indexer: text(raw.indexer),
    }
}

/// A day the arrs write as `2026-11-24` or as a timestamp.
fn day(v: &Option<String>) -> Option<NaiveDate> {
    let t = v.as_deref()?.trim();
    NaiveDate::parse_from_str(t.get(..10)?, "%Y-%m-%d")
        .ok()
        .filter(|d| d.year() > 1900)
}

fn quality_name(q: Option<RawQuality>) -> Option<String> {
    q.and_then(|q| q.quality).and_then(|q| text(q.name))
}

fn episode_ref(e: &RawEpisode) -> Option<EpisodeRef> {
    Some(EpisodeRef {
        season: e.season_number?,
        number: e.episode_number?,
        title: text(e.title.clone()),
    })
}

fn history_entry(raw: RawHistoryItem) -> Option<HistoryEntry> {
    let at = date(&raw.date)?;
    let data = raw.data.unwrap_or_default();
    // The details are text; a number is read as its digits.
    let field = |key: &str| match data.get(key) {
        Some(serde_json::Value::String(s)) => text(Some(s.clone())),
        Some(serde_json::Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    Some(HistoryEntry {
        id: raw.id,
        engine_title_id: raw.movie_id.or(raw.series_id).filter(|id| *id > 0),
        episode: raw.episode.as_ref().and_then(episode_ref),
        event: raw.event_type.unwrap_or_default(),
        at,
        source_title: text(raw.source_title),
        quality: quality_name(raw.quality),
        group: field("releaseGroup"),
        size: field("size")
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|n| *n > 0),
        indexer: field("indexer"),
        reason: field("reason").or_else(|| field("message")),
        imported_path: field("importedPath"),
    })
}

/// A movie's releases in the calendar's days: one entry per release that falls in them.
fn movie_releases(m: &RawMovieDates, start: NaiveDate, end: NaiveDate) -> Vec<CalendarEntry> {
    [
        ("theaters", &m.in_cinemas),
        ("digital", &m.digital_release),
        ("physical", &m.physical_release),
    ]
    .into_iter()
    .filter_map(|(release, at)| {
        let d = day(at)?;
        (d >= start && d <= end).then_some(CalendarEntry {
            engine_title_id: m.id,
            episode: None,
            release: Some(release),
            date: d,
            airs_at: None,
            has_file: m.has_file,
            monitored: m.monitored,
        })
    })
    .collect()
}

/// When a movie came out for a home library: its release date, else the earlier of the
/// digital and the physical release, else the theaters.
fn movie_released(m: &RawMovieDates) -> Option<DateTime<Utc>> {
    date(&m.release_date)
        .or_else(
            || match (date(&m.digital_release), date(&m.physical_release)) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
        )
        .or_else(|| date(&m.in_cinemas))
}

fn wanted_movie(m: RawMovieDates) -> WantedEntry {
    WantedEntry {
        id: m.id,
        engine_title_id: m.id,
        episode: None,
        released: movie_released(&m),
        monitored: m.monitored,
        have: m.movie_file.and_then(|f| quality_name(f.quality)),
        last_search: date(&m.last_search_time),
    }
}

fn wanted_episode(e: RawEpisode) -> Option<WantedEntry> {
    Some(WantedEntry {
        id: e.id?,
        engine_title_id: e.series_id.filter(|id| *id > 0)?,
        episode: episode_ref(&e),
        released: date(&e.air_date_utc),
        monitored: e.monitored,
        have: e.episode_file.and_then(|f| quality_name(f.quality)),
        last_search: date(&e.last_search_time),
    })
}

fn arr_parse(raw: RawParse) -> ArrParse {
    let matched_title = raw
        .movie
        .and_then(|m| text(m.title))
        .or(raw.series.and_then(|s| text(s.title)));
    let is_movie = raw.parsed_movie_info.is_some();
    let info = raw
        .parsed_movie_info
        .or(raw.parsed_episode_info)
        .unwrap_or_default();
    ArrParse {
        title: info
            .movie_titles
            .into_iter()
            .next()
            .or(text(info.series_title)),
        year: info.year.filter(|y| *y > 0),
        season: if is_movie { None } else { info.season_number },
        episodes: info.episode_numbers,
        quality: info
            .quality
            .and_then(|q| q.quality)
            .and_then(|q| text(q.name)),
        group: text(info.release_group),
        edition: if is_movie {
            Some(info.edition.unwrap_or_default())
        } else {
            None
        },
        languages: names(info.languages),
        custom_formats: names(raw.custom_formats),
        score: raw.custom_format_score,
        matched_title,
    }
}

// Searching, importing and acting ----------------------------------------------------------------

/// The paths below the address for the calls beyond GET, each as `calls` declares it.
const RELEASE_PATH: &str = "/api/v3/release";
const COMMAND_PATH: &str = "/api/v3/command";
/// One item of the queue, by the manager's number of it.
const QUEUE_ITEM_PATH: &str = "/api/v3/queue/{id}";
/// The command that imports files as they are, whatever the manager's rules said of them.
const MANUAL_IMPORT: &str = "ManualImport";
/// The managers' import modes, in their words ([`ImportMode`]).
const MODES: [&str; 3] = ["auto", "move", "copy"];
const FLAGS: &[&str] = &["true", "false"];
/// The most movies or episodes one search command names.
pub const SEARCH_MAX: usize = 50;
/// The most files one import takes, and the most episodes one file holds.
const IMPORT_MAX: usize = 100;
const FILE_EPISODES_MAX: usize = 100;
/// How long an interactive search may take: it asks every indexer.
pub const SEARCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Paths refused whatever the method, GET included, on both managers: shutting down and
/// restarting, restoring and deleting backups, and any write to a setting
/// (`/config/<part>/<id>`), a quality profile or definition, a custom format, a release or
/// delay profile. Making a backup and updating the manager are commands, and no command passes
/// but the searches and the manual import.
const NEVER: [&str; 12] = [
    "/api/v3/system/shutdown",
    "/api/v3/system/restart",
    "/api/v3/system/backup/",
    "/api/v3/config/{any}/{id}",
    "/api/v3/qualityprofile/{id}",
    "/api/v3/qualitydefinition/update",
    "/api/v3/qualitydefinition/{id}",
    "/api/v3/customformat/{id}",
    "/api/v3/customformat/bulk",
    "/api/v3/releaseprofile/{id}",
    "/api/v3/delayprofile/{id}",
    "/api/v3/delayprofile/reorder/{id}",
];
/// Radarr's paths that delete or edit a movie or a movie's file.
const NEVER_RADARR: [&str; 5] = [
    "/api/v3/movie/{id}",
    "/api/v3/movie/editor",
    "/api/v3/moviefile/{id}",
    "/api/v3/moviefile/bulk",
    "/api/v3/moviefile/editor",
];
/// Sonarr's paths that delete or edit a series or an episode's file, and its language
/// profiles.
const NEVER_SONARR: [&str; 6] = [
    "/api/v3/series/{id}",
    "/api/v3/series/editor",
    "/api/v3/episodefile/{id}",
    "/api/v3/episodefile/bulk",
    "/api/v3/episodefile/editor",
    "/api/v3/languageprofile/{id}",
];

/// What Singularr can do to a manager beyond reading it, when the owner allows it.
pub const ACTIONS: &[Action] = &[
    Action {
        id: "search",
        label: "Search for releases",
    },
    Action {
        id: "grab",
        label: "Grab a release",
    },
    Action {
        id: "import",
        label: "Import a refused download",
    },
    Action {
        id: "remove",
        label: "Remove a download",
    },
];

/// A search command by its name, with only these keys besides the name.
fn search_command(name: &str, keys: &[&str]) -> BodyRule {
    let mut members = vec!["name"];
    members.extend_from_slice(keys);
    BodyRule::new().only("", &members).one_of("/name", &[name])
}

/// One file of a manual import, in the fields the manager's import takes for it.
fn import_file_rule(kind: ArrKind) -> BodyRule {
    let rule = match kind {
        ArrKind::Radarr => BodyRule::new()
            .only(
                "",
                &[
                    "path",
                    "movieId",
                    "quality",
                    "languages",
                    "releaseGroup",
                    "downloadId",
                    "indexerFlags",
                ],
            )
            .id("/movieId"),
        ArrKind::Sonarr => BodyRule::new()
            .only(
                "",
                &[
                    "path",
                    "seriesId",
                    "episodeIds",
                    "quality",
                    "languages",
                    "releaseGroup",
                    "downloadId",
                    "indexerFlags",
                    "releaseType",
                ],
            )
            .id("/seriesId")
            .ids("/episodeIds", FILE_EPISODES_MAX),
    };
    rule.text("/path")
        .only("/quality", &["quality", "revision"])
        .text("/downloadId")
        .whole("/indexerFlags")
}

/// The calls beyond GET the adapter makes, all of them acts ([`ACTIONS`]), each in its one
/// form: a grab names a release by its guid and indexer and nothing else; a search is one of
/// the manager's search commands with its ids and nothing else (Radarr `MoviesSearch`; Sonarr
/// `EpisodeSearch`, `SeasonSearch`, `SeriesSearch`); an import is the manual import command
/// with its files and an import mode; a removal is one queue item, with its two choices in the
/// query. Any other command name (a rename, a backup, an update, a refresh) is no listed call.
/// And the paths refused whatever the method ([`NEVER`]).
pub fn calls(kind: ArrKind) -> Calls {
    let grab = BodyRule::new()
        .only("", &["guid", "indexerId"])
        .text("/guid")
        .id("/indexerId");
    let searches = match kind {
        ArrKind::Radarr => {
            vec![search_command("MoviesSearch", &["movieIds"]).ids("/movieIds", SEARCH_MAX)]
        }
        ArrKind::Sonarr => vec![
            search_command("EpisodeSearch", &["episodeIds"]).ids("/episodeIds", SEARCH_MAX),
            search_command("SeasonSearch", &["seriesId", "seasonNumber"])
                .id("/seriesId")
                .whole("/seasonNumber"),
            search_command("SeriesSearch", &["seriesId"]).id("/seriesId"),
        ],
    };
    let import = BodyRule::new()
        .only("", &["name", "files", "importMode"])
        .one_of("/name", &[MANUAL_IMPORT])
        .one_of("/importMode", &MODES)
        .each("/files", IMPORT_MAX, import_file_rule(kind));
    let calls = searches
        .into_iter()
        .fold(
            Calls::default().act_with("grab", Method::Post, RELEASE_PATH, grab),
            |calls, rule| calls.act_with("search", Method::Post, COMMAND_PATH, rule),
        )
        .act_with("import", Method::Post, COMMAND_PATH, import)
        .act_query(
            "remove",
            Method::Delete,
            QUEUE_ITEM_PATH,
            &[
                ("removeFromClient", QueryValue::OneOf(FLAGS)),
                ("blocklist", QueryValue::OneOf(FLAGS)),
            ],
        );
    let titles: &[&str] = match kind {
        ArrKind::Radarr => &NEVER_RADARR,
        ArrKind::Sonarr => &NEVER_SONARR,
    };
    NEVER
        .iter()
        .chain(titles)
        .fold(calls, |calls, path| calls.never(path))
}

/// A quality as the managers take it back: its quality and its revision, nothing else.
fn quality_only(q: &Value) -> Value {
    json!({"quality": q.get("quality"), "revision": q.get("revision")})
}

/// One file of a manual import as the command takes it.
fn import_file_body(kind: ArrKind, f: &ImportFile) -> Value {
    let mut body = json!({
        "path": f.path,
        "quality": f.quality.as_ref().map(quality_only),
        "languages": f.languages,
        "downloadId": f.download_id,
        "indexerFlags": f.indexer_flags,
    });
    match kind {
        ArrKind::Radarr => body["movieId"] = json!(f.title_id),
        ArrKind::Sonarr => {
            body["seriesId"] = json!(f.title_id);
            body["episodeIds"] = json!(f.episode_ids);
            if let Some(t) = &f.release_type {
                body["releaseType"] = json!(t);
            }
        }
    }
    if let Some(group) = &f.release_group {
        body["releaseGroup"] = json!(group);
    }
    body
}

/// The name of the manager's command an act sends, for a search or an import.
pub fn command_name(act: &ManagerAct) -> Option<&'static str> {
    match act {
        ManagerAct::SearchMovies { .. } => Some("MoviesSearch"),
        ManagerAct::SearchEpisodes { .. } => Some("EpisodeSearch"),
        ManagerAct::SearchSeason { .. } => Some("SeasonSearch"),
        ManagerAct::SearchSeries { .. } => Some("SeriesSearch"),
        ManagerAct::Import { .. } => Some(MANUAL_IMPORT),
        ManagerAct::Grab { .. } | ManagerAct::Remove { .. } => None,
    }
}

/// The call that makes an act, as [`calls`] declares it: the method, the path (with its query)
/// below the address, and the body. `None` for an act this manager does not take (a movie
/// search on Sonarr).
fn act_call(kind: ArrKind, act: &ManagerAct) -> Option<(Method, String, Option<Value>)> {
    let command = |body: Value| Some((Method::Post, COMMAND_PATH.to_string(), Some(body)));
    match (act, kind) {
        (ManagerAct::Grab { guid, indexer_id }, _) => Some((
            Method::Post,
            RELEASE_PATH.to_string(),
            Some(json!({"guid": guid, "indexerId": indexer_id})),
        )),
        (ManagerAct::SearchMovies { movies }, ArrKind::Radarr) => {
            command(json!({"name": "MoviesSearch", "movieIds": movies}))
        }
        (ManagerAct::SearchEpisodes { episodes }, ArrKind::Sonarr) => {
            command(json!({"name": "EpisodeSearch", "episodeIds": episodes}))
        }
        (ManagerAct::SearchSeason { series, season }, ArrKind::Sonarr) => {
            command(json!({"name": "SeasonSearch", "seriesId": series, "seasonNumber": season}))
        }
        (ManagerAct::SearchSeries { series }, ArrKind::Sonarr) => {
            command(json!({"name": "SeriesSearch", "seriesId": series}))
        }
        (ManagerAct::Import { files, mode }, kind) => command(json!({
            "name": MANUAL_IMPORT,
            "files": files.iter().map(|f| import_file_body(kind, f)).collect::<Vec<_>>(),
            "importMode": mode.as_str(),
        })),
        (
            ManagerAct::Remove {
                queue_id,
                blocklist,
            },
            _,
        ) => Some((
            Method::Delete,
            format!("/api/v3/queue/{queue_id}?removeFromClient=true&blocklist={blocklist}"),
            None,
        )),
        _ => None,
    }
}

/// What a manager said when it did not take an act: its message, from the shapes it answers
/// with (a text, an object with a message, a list of validation errors).
fn refusal_words(res: &OutResponse) -> String {
    let said = serde_json::from_slice::<Value>(&res.body).ok();
    let words = said
        .as_ref()
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            Value::Array(items) => items.iter().find_map(|i| {
                i.get("errorMessage")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
            Value::Object(_) => ["message", "errorMessage", "description"]
                .iter()
                .find_map(|k| v.get(*k).and_then(Value::as_str).map(str::to_string)),
            _ => None,
        })
        .unwrap_or_else(|| res.text());
    words.trim().chars().take(300).collect()
}

/// How a refused download is imported, from what the queue says of its download client: a
/// torrent goes on seeding from its files, and a client that moves an imported download to a
/// category of its own keeps them too, so both are copied (a hardlink where the file system
/// allows one); a usenet download is moved; anything else is left to the manager.
pub fn import_mode(entry: &QueueEntry) -> ImportMode {
    let protocol = entry.protocol.as_deref().unwrap_or_default();
    if entry.post_import_category || protocol.eq_ignore_ascii_case("torrent") {
        ImportMode::Copy
    } else if protocol.eq_ignore_ascii_case("usenet") {
        ImportMode::Move
    } else {
        ImportMode::Auto
    }
}

/// A release as the interactive search answers it. Read leniently: a field of an unexpected
/// shape is no value.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRelease {
    #[serde(default, deserialize_with = "lenient")]
    guid: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    indexer_id: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    indexer: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    title: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    protocol: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    quality: Option<RawQuality>,
    #[serde(default, deserialize_with = "lenient")]
    size: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    age: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    publish_date: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    seeders: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    leechers: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    languages: Option<Vec<Named>>,
    #[serde(default, deserialize_with = "lenient")]
    release_group: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    edition: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    season_number: Option<i32>,
    #[serde(default, deserialize_with = "lenient")]
    mapped_season_number: Option<i32>,
    #[serde(default, deserialize_with = "lenient")]
    episode_numbers: Option<Vec<i32>>,
    #[serde(default, deserialize_with = "lenient")]
    mapped_episode_numbers: Option<Vec<i32>>,
    #[serde(default, deserialize_with = "lenient")]
    full_season: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    custom_formats: Option<Vec<Named>>,
    #[serde(default, deserialize_with = "lenient")]
    custom_format_score: Option<i32>,
    #[serde(default, deserialize_with = "lenient")]
    approved: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    rejections: Option<Vec<Value>>,
    #[serde(default, deserialize_with = "lenient")]
    temporarily_rejected: Option<bool>,
}

/// The words of a manager's rejections: each a text, or an object with its `reason`.
fn rejection_words(list: Option<Vec<Value>>) -> Vec<String> {
    list.unwrap_or_default()
        .into_iter()
        .filter_map(|r| match r {
            Value::String(s) => text(Some(s)),
            other => text(
                other
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ),
        })
        .collect()
}

/// The releases of an interactive search's answer, each with the manager's word. A release
/// without a guid, an indexer or a name cannot be grabbed and is left out.
pub fn found_releases(answer: &[u8]) -> Result<Vec<FoundRelease>, serde_json::Error> {
    let raw: Vec<RawRelease> = serde_json::from_slice(answer)?;
    Ok(raw.into_iter().filter_map(found_release).collect())
}

/// A release with a guid, an indexer and a name; one without any of them cannot be grabbed and
/// is left out.
fn found_release(raw: RawRelease) -> Option<FoundRelease> {
    let guid = text(raw.guid)?;
    let indexer_id = raw.indexer_id.filter(|n| *n > 0)?;
    let name = text(raw.title)?;
    let episodes = raw
        .mapped_episode_numbers
        .filter(|e| !e.is_empty())
        .or(raw.episode_numbers)
        .unwrap_or_default();
    Some(FoundRelease {
        guid,
        indexer_id,
        indexer: text(raw.indexer),
        name,
        protocol: text(raw.protocol),
        quality: quality_name(raw.quality),
        size: raw.size.filter(|n| *n > 0),
        age_days: raw.age.filter(|n| *n >= 0),
        published: date(&raw.publish_date),
        seeders: raw.seeders,
        leechers: raw.leechers,
        languages: names(raw.languages.unwrap_or_default()),
        group: text(raw.release_group),
        edition: text(raw.edition),
        season: raw.mapped_season_number.or(raw.season_number),
        episodes,
        full_season: raw.full_season.unwrap_or(false),
        formats: names(raw.custom_formats.unwrap_or_default()),
        score: raw.custom_format_score,
        approved: raw.approved.unwrap_or(false),
        rejections: rejection_words(raw.rejections),
        temporarily_rejected: raw.temporarily_rejected.unwrap_or(false),
    })
}

/// A record that is named by its id alone, here.
#[derive(Deserialize)]
struct RawId {
    id: i64,
}

/// One file a manual import offers, as the manager writes it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawImport {
    #[serde(default, deserialize_with = "lenient")]
    path: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    size: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    movie: Option<RawId>,
    #[serde(default, deserialize_with = "lenient")]
    series: Option<RawId>,
    #[serde(default, deserialize_with = "lenient")]
    season_number: Option<i32>,
    #[serde(default, deserialize_with = "lenient")]
    episodes: Option<Vec<RawId>>,
    #[serde(default)]
    quality: Option<Value>,
    #[serde(default)]
    languages: Option<Value>,
    #[serde(default, deserialize_with = "lenient")]
    release_group: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    download_id: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    indexer_flags: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    release_type: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    rejections: Option<Vec<Value>>,
}

fn import_file(raw: RawImport) -> Option<ImportFile> {
    Some(ImportFile {
        path: text(raw.path)?,
        size: raw.size.filter(|n| *n > 0),
        title_id: raw.movie.or(raw.series).map(|t| t.id).filter(|id| *id > 0),
        episode_ids: raw
            .episodes
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.id)
            .filter(|id| *id > 0)
            .collect(),
        season: raw.season_number,
        quality: raw.quality.filter(Value::is_object),
        languages: raw
            .languages
            .filter(Value::is_array)
            .unwrap_or_else(|| json!([])),
        release_group: text(raw.release_group),
        download_id: text(raw.download_id),
        indexer_flags: raw.indexer_flags.filter(|n| *n >= 0).unwrap_or(0),
        release_type: text(raw.release_type),
        rejections: rejection_words(raw.rejections),
    })
}

/// A queue item as one download's entry: one without a download id names no download.
fn queue_entry(raw: RawQueueItem) -> Option<QueueEntry> {
    let messages = queue_messages(&raw);
    let written = text(raw.download_id)?;
    Some(QueueEntry {
        queue_id: raw.id,
        engine_title_id: raw.movie_id.or(raw.series_id).filter(|id| *id > 0),
        download_id: written.to_ascii_uppercase(),
        download_id_as_written: written,
        title: raw.title.unwrap_or_default(),
        protocol: text(raw.protocol),
        download_client: text(raw.download_client),
        post_import_category: raw.download_client_has_post_import_category == Some(true),
        tracked_state: text(raw.tracked_download_state),
        messages,
    })
}

/// What `/moviefile` said about one file, and the facts of the file it was said about.
struct FileFormats {
    key: String,
    formats: Vec<String>,
    score: Option<i32>,
}

#[derive(Default)]
struct FormatCache {
    by_file: HashMap<i64, FileFormats>,
    refreshed: Option<Instant>,
}

pub struct Radarr {
    client: ArrClient,
    formats: Mutex<FormatCache>,
}

impl Radarr {
    pub fn new(client: ArrClient) -> Self {
        Radarr {
            client,
            formats: Mutex::new(FormatCache::default()),
        }
    }

    /// Fill in each file's custom formats and score. `titles` pairs a movie id with its file.
    async fn fill_formats(&self, titles: &mut [TitleFacts]) -> Result<(), EngineError> {
        let key =
            |f: &FileFacts| format!("{}|{}|{:?}", f.relative_path, f.size_bytes, f.date_added);
        // Which movies to ask about: every one when the cache is old, else those whose file
        // is not known or has changed.
        let wanted: Vec<i64> = {
            let mut cache = self.formats.lock().unwrap();
            if cache
                .refreshed
                .is_none_or(|at| at.elapsed() >= FORMATS_REFRESH)
            {
                cache.by_file.clear();
                cache.refreshed = Some(Instant::now());
            }
            titles
                .iter()
                .filter(|t| {
                    t.files.iter().flatten().any(|f| {
                        cache
                            .by_file
                            .get(&f.engine_file_id)
                            .is_none_or(|known| known.key != key(f))
                    })
                })
                .map(|t| t.engine_title_id)
                .collect()
        };
        let fetched = self.client.movie_files(&wanted).await?;
        let mut cache = self.formats.lock().unwrap();
        for file in titles.iter_mut().flat_map(|t| t.files.iter_mut().flatten()) {
            if let Some(raw) = fetched.get(&file.engine_file_id) {
                cache.by_file.insert(
                    file.engine_file_id,
                    FileFormats {
                        key: key(file),
                        formats: raw
                            .custom_formats
                            .iter()
                            .filter_map(|n| text(n.name.clone()))
                            .collect(),
                        score: raw.custom_format_score,
                    },
                );
            }
            if let Some(known) = cache.by_file.get(&file.engine_file_id) {
                file.arr_custom_formats = known.formats.clone();
                file.arr_score = known.score;
            }
        }
        // Files that are gone need no entry.
        let listed: std::collections::HashSet<i64> = titles
            .iter()
            .flat_map(|t| t.files.iter().flatten())
            .map(|f| f.engine_file_id)
            .collect();
        cache.by_file.retain(|id, _| listed.contains(id));
        Ok(())
    }
}

#[async_trait]
impl LibraryEngine for Radarr {
    fn engine(&self) -> &EngineRef {
        &self.client.engine
    }

    async fn status(&self) -> Result<EngineInfo, EngineError> {
        self.client.status().await
    }

    async fn titles(&self) -> Result<Vec<TitleFacts>, EngineError> {
        let tags = self.client.tags().await?;
        let profiles = self.client.profiles().await?;
        let movies: Vec<RawMovie> = self.client.get("movies", "/movie").await?;
        let mut titles: Vec<TitleFacts> = movies
            .into_iter()
            .map(|m| {
                let profile = m
                    .quality_profile_id
                    .and_then(|id| profiles.get(&id).cloned());
                let files: Vec<FileFacts> = m
                    .movie_file
                    .map(|f| file_facts(f, true))
                    .into_iter()
                    .collect();
                let ratings = m.ratings.unwrap_or_default();
                let collection_tmdb_id = m
                    .collection
                    .as_ref()
                    .and_then(|c| c.tmdb_id)
                    .filter(|n| *n > 0);
                let original_title = text(m.original_title).filter(|o| *o != m.title);
                let about = TitleAbout {
                    overview: text(m.overview),
                    genres: genres(m.genres),
                    runtime_minutes: m.runtime.filter(|n| *n > 0),
                    certification: text(m.certification),
                    studio: text(m.studio),
                    network: None,
                    collection: m
                        .collection
                        .and_then(|c| text(c.title).or_else(|| text(c.name))),
                    collection_tmdb_id,
                    original_title,
                    rating: rating_value(ratings.tmdb),
                    imdb_rating: rating_value(ratings.imdb),
                    fanart_url: image(&m.images, "fanart"),
                    banner_url: image(&m.images, "banner"),
                    first_aired: None,
                    next_airing: None,
                    in_cinemas: date(&m.in_cinemas),
                    digital_release: date(&m.digital_release),
                };
                TitleFacts {
                    engine_title_id: m.id,
                    kind: TitleKind::Movie,
                    title: m.title,
                    sort_title: text(m.sort_title),
                    year: m.year.filter(|y| *y > 0),
                    tmdb_id: m.tmdb_id.filter(|n| *n > 0),
                    tvdb_id: None,
                    imdb_id: text(m.imdb_id),
                    original_language: m.original_language.and_then(|l| text(l.name)),
                    poster_url: image(&m.images, "poster"),
                    status: text(m.status),
                    path: text(m.path),
                    monitored: m.monitored,
                    // The lane is the policy's to give (`Policy::lane_for_arr`); the
                    // catalog fills it in.
                    lane: None,
                    profile,
                    tags: tag_names(&m.tags, &tags),
                    added_at: date(&m.added),
                    file_count: files.len() as i32,
                    size_bytes: m.size_on_disk.unwrap_or(0),
                    seasons: Vec::new(),
                    episode_count: None,
                    total_episode_count: None,
                    about,
                    files: Some(files),
                }
            })
            .collect();
        self.fill_formats(&mut titles).await?;
        Ok(titles)
    }

    async fn files(&self, _engine_title_id: i64) -> Result<Vec<FileFacts>, EngineError> {
        // A movie's file comes with the movie.
        Ok(Vec::new())
    }

    async fn queue(&self) -> Result<Vec<QueueItem>, EngineError> {
        self.client.queue("includeUnknownMovieItems").await
    }

    async fn grabs(&self, page: u32, page_size: u32) -> Result<GrabPage, EngineError> {
        self.client.grabs(page, page_size).await
    }

    async fn parse(&self, name: &str) -> Result<ArrParse, EngineError> {
        self.client.parse(name).await
    }

    async fn naming(&self) -> Result<Option<ArrNaming>, EngineError> {
        self.client.naming().await.map(Some)
    }

    async fn calendar(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<CalendarEntry>, EngineError> {
        let movies: Vec<RawMovieDates> = self
            .client
            .get("calendar", &calendar_query(start, end, ""))
            .await?;
        Ok(movies
            .iter()
            .flat_map(|m| movie_releases(m, start, end))
            .collect())
    }

    async fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, EngineError> {
        self.client.history_records(limit, "").await
    }

    async fn wanted(&self, cutoff: bool, limit: u32) -> Result<WantedPage, EngineError> {
        let list = if cutoff { "cutoff" } else { "missing" };
        let raw: RawMoviePage = self
            .client
            .get(
                "wanted list",
                &format!(
                    "/wanted/{list}?page=1&pageSize={}&monitored=true",
                    limit.clamp(1, PAGES_MAX)
                ),
            )
            .await?;
        Ok(WantedPage {
            total: raw.total_records.unwrap_or(raw.records.len() as i64),
            entries: raw.records.into_iter().map(wanted_movie).collect(),
        })
    }

    /// A movie's releases. Radarr searches for movies alone.
    async fn releases(
        &self,
        scope: ReleaseScope,
    ) -> Result<Option<Vec<FoundRelease>>, EngineError> {
        match scope {
            ReleaseScope::Movie { .. } => self.client.releases(scope).await.map(Some),
            _ => Ok(None),
        }
    }

    async fn queue_entries(&self, download_id: &str) -> Result<Vec<QueueEntry>, EngineError> {
        self.client
            .queue_entries("includeUnknownMovieItems", download_id)
            .await
    }

    async fn import_offer(&self, download_id: &str) -> Result<Vec<ImportFile>, EngineError> {
        self.client.import_offer(download_id).await
    }

    async fn act(&self, act: &ManagerAct) -> Result<ManagerActed, EngineError> {
        self.client.act(act).await
    }
}

pub struct Sonarr(pub ArrClient);

#[async_trait]
impl LibraryEngine for Sonarr {
    fn engine(&self) -> &EngineRef {
        &self.0.engine
    }

    async fn status(&self) -> Result<EngineInfo, EngineError> {
        self.0.status().await
    }

    async fn titles(&self) -> Result<Vec<TitleFacts>, EngineError> {
        let tags = self.0.tags().await?;
        let profiles = self.0.profiles().await?;
        let series: Vec<RawSeries> = self.0.get("series", "/series").await?;
        Ok(series
            .into_iter()
            .map(|s| {
                let profile = s
                    .quality_profile_id
                    .and_then(|id| profiles.get(&id).cloned());
                let stats = s.statistics.unwrap_or_default();
                let about = TitleAbout {
                    overview: text(s.overview),
                    genres: genres(s.genres),
                    runtime_minutes: s.runtime.filter(|n| *n > 0),
                    certification: text(s.certification),
                    studio: None,
                    network: text(s.network),
                    collection: None,
                    collection_tmdb_id: None,
                    original_title: None,
                    rating: rating_value(s.ratings),
                    imdb_rating: None,
                    fanart_url: image(&s.images, "fanart"),
                    banner_url: image(&s.images, "banner"),
                    first_aired: date(&s.first_aired),
                    next_airing: date(&s.next_airing),
                    in_cinemas: None,
                    digital_release: None,
                };
                TitleFacts {
                    engine_title_id: s.id,
                    kind: TitleKind::Show,
                    title: s.title,
                    sort_title: text(s.sort_title),
                    year: s.year.filter(|y| *y > 0),
                    tmdb_id: s.tmdb_id.filter(|n| *n > 0),
                    tvdb_id: s.tvdb_id.filter(|n| *n > 0),
                    imdb_id: text(s.imdb_id),
                    original_language: s.original_language.and_then(|l| text(l.name)),
                    poster_url: image(&s.images, "poster"),
                    status: text(s.status),
                    path: text(s.path),
                    monitored: s.monitored,
                    lane: None,
                    profile,
                    tags: tag_names(&s.tags, &tags),
                    added_at: date(&s.added),
                    file_count: stats.episode_file_count.unwrap_or(0),
                    size_bytes: stats.size_on_disk.unwrap_or(0),
                    seasons: s
                        .seasons
                        .into_iter()
                        .map(|season| {
                            let st = season.statistics.unwrap_or_default();
                            SeasonFacts {
                                season_number: season.season_number,
                                monitored: season.monitored,
                                episode_count: st.episode_count.unwrap_or(0),
                                total_episode_count: st.total_episode_count.unwrap_or(0),
                                episode_file_count: st.episode_file_count.unwrap_or(0),
                                size_bytes: st.size_on_disk.unwrap_or(0),
                            }
                        })
                        .collect(),
                    episode_count: stats.episode_count,
                    total_episode_count: stats.total_episode_count,
                    about,
                    files: None,
                }
            })
            .collect())
    }

    async fn files(&self, engine_title_id: i64) -> Result<Vec<FileFacts>, EngineError> {
        let files: Vec<RawFile> = self
            .0
            .get(
                "episode files",
                &format!("/episodefile?seriesId={engine_title_id}"),
            )
            .await?;
        Ok(files.into_iter().map(|f| file_facts(f, false)).collect())
    }

    async fn queue(&self) -> Result<Vec<QueueItem>, EngineError> {
        self.0.queue("includeUnknownSeriesItems").await
    }

    async fn grabs(&self, page: u32, page_size: u32) -> Result<GrabPage, EngineError> {
        self.0.grabs(page, page_size).await
    }

    async fn parse(&self, name: &str) -> Result<ArrParse, EngineError> {
        self.0.parse(name).await
    }

    async fn naming(&self) -> Result<Option<ArrNaming>, EngineError> {
        self.0.naming().await.map(Some)
    }

    async fn calendar(
        &self,
        start: NaiveDate,
        end: NaiveDate,
    ) -> Result<Vec<CalendarEntry>, EngineError> {
        let episodes: Vec<RawEpisode> = self
            .0
            .get(
                "calendar",
                &calendar_query(start, end, "&includeSeries=false&includeEpisodeFile=false"),
            )
            .await?;
        Ok(episodes
            .iter()
            .filter_map(|e| {
                let airs_at = date(&e.air_date_utc);
                // The day the network gives, else the day of the air time.
                let d = day(&e.air_date).or_else(|| airs_at.map(|a| a.date_naive()))?;
                (d >= start && d <= end).then(|| CalendarEntry {
                    engine_title_id: e.series_id.filter(|id| *id > 0).unwrap_or_default(),
                    episode: episode_ref(e),
                    release: None,
                    date: d,
                    airs_at,
                    has_file: e.has_file,
                    monitored: e.monitored,
                })
            })
            .filter(|c| c.engine_title_id > 0)
            .collect())
    }

    async fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, EngineError> {
        self.0.history_records(limit, "&includeEpisode=true").await
    }

    async fn wanted(&self, cutoff: bool, limit: u32) -> Result<WantedPage, EngineError> {
        let path = if cutoff {
            "/wanted/cutoff?page=1&pageSize={n}&monitored=true&includeSeries=false&includeEpisodeFile=true"
        } else {
            "/wanted/missing?page=1&pageSize={n}&monitored=true&includeSeries=false"
        };
        let raw: RawEpisodePage = self
            .0
            .get(
                "wanted list",
                &path.replace("{n}", &limit.clamp(1, PAGES_MAX).to_string()),
            )
            .await?;
        Ok(WantedPage {
            total: raw.total_records.unwrap_or(raw.records.len() as i64),
            entries: raw.records.into_iter().filter_map(wanted_episode).collect(),
        })
    }

    /// An episode's or a season's releases. Sonarr searches for no movie.
    async fn releases(
        &self,
        scope: ReleaseScope,
    ) -> Result<Option<Vec<FoundRelease>>, EngineError> {
        match scope {
            ReleaseScope::Movie { .. } => Ok(None),
            _ => self.0.releases(scope).await.map(Some),
        }
    }

    async fn episodes(
        &self,
        series: i64,
        season: Option<i32>,
    ) -> Result<Vec<ManagerEpisode>, EngineError> {
        self.0.episodes(series, season).await
    }

    async fn queue_entries(&self, download_id: &str) -> Result<Vec<QueueEntry>, EngineError> {
        self.0
            .queue_entries("includeUnknownSeriesItems", download_id)
            .await
    }

    async fn import_offer(&self, download_id: &str) -> Result<Vec<ImportFile>, EngineError> {
        self.0.import_offer(download_id).await
    }

    async fn act(&self, act: &ManagerAct) -> Result<ManagerActed, EngineError> {
        self.0.act(act).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method};
    use std::sync::Arc;

    const BASE_RADARR: &str = "http://192.0.2.61:7979";
    const BASE_SONARR: &str = "http://192.0.2.61:8989";

    fn client(
        id: &'static str,
        kind: ArrKind,
        library: &'static str,
        base: &str,
        recorded: Arc<Recorded>,
    ) -> ArrClient {
        let mut allow = Allowlist::with_metadata_providers();
        allow.allow_engine(id, base, &[]).unwrap();
        let out = Outbound::with_transport(allow, recorded);
        let config = ArrConfig {
            id: id.to_string(),
            kind,
            library: library.to_string(),
            url: Some(base.to_string()),
            api_key: Some("test-key".to_string()),
        };
        ArrClient::new(&config, out).unwrap()
    }

    fn radarr() -> (Radarr, Arc<Recorded>) {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v3/tag",
            200,
            include_str!("../../tests/fixtures/radarr_tag.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/qualityprofile",
            200,
            include_str!("../../tests/fixtures/radarr_qualityprofile.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/movie",
            200,
            include_str!("../../tests/fixtures/radarr_movie.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/moviefile",
            200,
            include_str!("../../tests/fixtures/radarr_moviefile.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/queue",
            200,
            include_str!("../../tests/fixtures/radarr_queue.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/history",
            200,
            include_str!("../../tests/fixtures/radarr_history.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/parse",
            200,
            include_str!("../../tests/fixtures/radarr_parse.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/system/status",
            200,
            include_str!("../../tests/fixtures/radarr_system_status.json"),
        );
        (
            Radarr::new(client(
                "radarr",
                ArrKind::Radarr,
                "radarr",
                BASE_RADARR,
                recorded.clone(),
            )),
            recorded,
        )
    }

    fn sonarr() -> (Sonarr, Arc<Recorded>) {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/api/v3/tag", 200, "[]");
        recorded.answer(
            Method::Get,
            "/api/v3/qualityprofile",
            200,
            include_str!("../../tests/fixtures/radarr_qualityprofile.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/series",
            200,
            include_str!("../../tests/fixtures/sonarr_series.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/episodefile?seriesId=1",
            200,
            include_str!("../../tests/fixtures/sonarr_episodefile.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/queue",
            200,
            include_str!("../../tests/fixtures/sonarr_queue.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/parse",
            200,
            include_str!("../../tests/fixtures/sonarr_parse.json"),
        );
        (
            Sonarr(client(
                "sonarr",
                ArrKind::Sonarr,
                "sonarr",
                BASE_SONARR,
                recorded.clone(),
            )),
            recorded,
        )
    }

    #[tokio::test]
    async fn radarr_movies_become_titles_with_their_file() {
        let (engine, recorded) = radarr();
        let titles = engine.titles().await.unwrap();
        assert_eq!(titles.len(), 3);
        let lane = &titles[0];
        assert_eq!(lane.title, "10 Cloverfield Lane");
        assert_eq!((lane.tmdb_id, lane.year), (Some(333371), Some(2016)));
        assert_eq!(lane.profile.as_deref(), Some("HD-1080p"));
        assert_eq!(
            lane.lane, None,
            "the policy places a title, not the adapter"
        );
        assert_eq!(
            lane.poster_url.as_deref(),
            Some("https://image.tmdb.org/t/p/original/poster1.jpg")
        );
        assert_eq!(lane.original_language.as_deref(), Some("English"));
        assert!(lane.added_at.is_some());
        // What the movie is, from the same list call.
        let about = &lane.about;
        assert!(
            about
                .overview
                .as_deref()
                .unwrap()
                .starts_with("After a car accident")
        );
        assert_eq!(about.genres, vec!["Thriller", "Science Fiction"]);
        assert_eq!(
            (about.runtime_minutes, about.certification.as_deref()),
            (Some(104), Some("PG-13"))
        );
        assert_eq!(
            (about.studio.as_deref(), about.network.as_deref()),
            (Some("Bad Robot"), None)
        );
        assert_eq!(
            (about.rating, about.imdb_rating),
            (Some(7.0), Some(7.2)),
            "TMDB's 6.99 to one decimal, and IMDb's"
        );
        assert_eq!(about.collection.as_deref(), Some("Cloverfield Collection"));
        assert_eq!(about.collection_tmdb_id, Some(529892));
        assert_eq!(
            about.fanart_url.as_deref(),
            Some("https://image.tmdb.org/t/p/original/fanart1.jpg")
        );
        assert_eq!(
            about.in_cinemas.map(|d| d.to_rfc3339()).as_deref(),
            Some("2016-03-10T00:00:00+00:00")
        );
        assert!(about.digital_release.is_some());
        assert_eq!((lane.episode_count, about.next_airing), (None, None));
        let file = &lane.files.as_ref().unwrap()[0];
        assert_eq!(file.engine_file_id, 9001);
        assert_eq!(file.arr_quality.as_deref(), Some("Bluray-1080p"));
        assert_eq!(file.arr_group.as_deref(), Some("SM737"));
        assert_eq!(file.arr_edition.as_deref(), Some(""));
        assert!(
            file.arr_proper && !file.arr_repack,
            "revision 2 is a proper"
        );
        // The movie list carries no formats; they come from /moviefile.
        assert_eq!(file.arr_custom_formats, vec!["DD+ ATMOS", "x265"]);
        assert_eq!(file.arr_score, Some(1250));
        assert_eq!(file.size_bytes, 5269043108);
        assert!(file.scene_name.as_deref().unwrap().contains("PROPER"));
        assert!(file.media_info.is_some());

        let open_matte = &titles[1];
        assert_eq!(open_matte.tags, vec!["open-matte"]);
        assert_eq!(open_matte.profile.as_deref(), Some("UHD-4K"));
        assert_eq!(open_matte.files.as_ref().unwrap()[0].arr_score, Some(300));
        assert!(!open_matte.files.as_ref().unwrap()[0].arr_proper);
        assert_eq!(
            open_matte.files.as_ref().unwrap()[0].arr_edition.as_deref(),
            Some("Open Matte")
        );

        // A runtime that is not a number is no runtime, a collection that is not an object
        // is none, IMDb's 0 is "not rated" and an empty overview is none. The listing does
        // not fail.
        let odd = &open_matte.about;
        assert_eq!((odd.runtime_minutes, odd.overview.as_deref()), (None, None));
        assert_eq!((odd.rating, odd.imdb_rating), (Some(8.0), None));
        assert_eq!(odd.collection, None);
        assert!(odd.genres.is_empty() && odd.fanart_url.is_none());

        let missing = &titles[2];
        assert_eq!(missing.about, TitleAbout::default());
        assert!(
            missing.files.as_ref().unwrap().is_empty(),
            "a movie without a file has no copy"
        );
        assert_eq!(missing.file_count, 0);

        // One /moviefile call for the two movies that have a file, by repeated movieId (Radarr 7 refuses movieIds).
        let asked: Vec<String> = recorded
            .urls_seen()
            .into_iter()
            .filter(|u| u.contains("/moviefile"))
            .collect();
        assert_eq!(asked.len(), 1);
        assert!(asked[0].ends_with("/api/v3/moviefile?movieId=2877&movieId=3001"));

        // The next pass finds the same files and does not ask about them again.
        let again = engine.titles().await.unwrap();
        assert_eq!(again[0].files.as_ref().unwrap()[0].arr_score, Some(1250));
        assert_eq!(
            recorded
                .urls_seen()
                .iter()
                .filter(|u| u.contains("/moviefile"))
                .count(),
            1
        );

        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
        assert!(
            recorded
                .urls_seen()
                .iter()
                .all(|u| u.starts_with(BASE_RADARR))
        );
    }

    #[tokio::test]
    async fn sonarr_series_and_episode_files_are_read() {
        let (engine, recorded) = sonarr();
        let titles = engine.titles().await.unwrap();
        assert_eq!(titles.len(), 2);
        let show = &titles[0];
        assert_eq!((show.title.as_str(), show.tvdb_id), ("1883", Some(396390)));
        assert_eq!(show.kind, TitleKind::Show);
        assert_eq!(show.profile.as_deref(), Some("HD-1080p"));
        assert_eq!(show.file_count, 2);
        assert_eq!(
            show.seasons,
            vec![SeasonFacts {
                season_number: 1,
                monitored: true,
                episode_count: 10,
                total_episode_count: 10,
                episode_file_count: 2,
                size_bytes: 6284799824,
            }]
        );
        assert_eq!(
            (show.episode_count, show.total_episode_count),
            (Some(10), Some(10))
        );
        assert!(show.files.is_none());
        // What the show is, from the same list call.
        let about = &show.about;
        assert_eq!(
            (about.network.as_deref(), about.studio.as_deref()),
            (Some("Paramount+"), None)
        );
        assert_eq!(about.genres, vec!["Drama", "Western"]);
        assert_eq!(
            (about.runtime_minutes, about.certification.as_deref()),
            (Some(60), Some("TV-MA"))
        );
        assert_eq!((about.rating, about.imdb_rating), (Some(8.7), None));
        assert_eq!(
            about.banner_url.as_deref(),
            Some("https://artworks.thetvdb.com/banners/b1.jpg")
        );
        assert_eq!(
            about.fanart_url.as_deref(),
            Some("https://artworks.thetvdb.com/banners/fanart/f1.jpg")
        );
        assert!(about.first_aired.is_some());
        assert_eq!(
            about.next_airing, None,
            "a show that ended has no next episode"
        );
        let airing = &titles[1];
        assert!(airing.about.next_airing.is_some());
        assert_eq!(
            (airing.episode_count, airing.total_episode_count),
            (Some(6), Some(7))
        );
        let specials = &airing.seasons[0];
        assert_eq!(
            (
                specials.season_number,
                specials.monitored,
                specials.total_episode_count
            ),
            (0, false, 1)
        );

        let files = engine.files(1).await.unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(
            (files[0].season_number, files[0].episode_numbers.clone()),
            (Some(1), vec![1])
        );
        assert_eq!(files[0].episode_name.as_deref(), Some("1883"));
        assert_eq!(files[1].episode_name.as_deref(), Some("Behind Us a Cliff"));
        assert_eq!(files[0].arr_edition, None, "Sonarr records no edition");
        assert_eq!(
            files[0].arr_custom_formats,
            vec!["5.1 Surround", "DD+", "QxR"]
        );
        assert_eq!(files[0].arr_score, Some(1550));
        assert!(
            recorded
                .urls_seen()
                .iter()
                .any(|u| u.ends_with("/api/v3/episodefile?seriesId=1"))
        );
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
    }

    #[tokio::test]
    async fn the_queue_carries_state_messages_and_unknown_items() {
        let (engine, recorded) = radarr();
        let queue = engine.queue().await.unwrap();
        assert_eq!(queue.len(), 3);
        let blocked = &queue[0];
        assert_eq!(blocked.engine_title_id, Some(2877));
        assert_eq!(
            blocked.download_id.as_deref(),
            Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1")
        );
        assert_eq!(blocked.tracked_state.as_deref(), Some("importBlocked"));
        assert_eq!(blocked.tracked_status.as_deref(), Some("warning"));
        assert_eq!(
            blocked.messages,
            vec!["Not an upgrade for existing movie file(s)"]
        );
        assert_eq!(queue[1].messages, Vec::<String>::new());
        assert_eq!(
            queue[2].engine_title_id, None,
            "an unknown item has no title behind it"
        );
        assert!(recorded.urls_seen()[0].contains("includeUnknownMovieItems=true"));

        let (sonarr, recorded) = sonarr();
        let queue = sonarr.queue().await.unwrap();
        assert_eq!(queue[0].engine_title_id, Some(1));
        assert!(recorded.urls_seen()[0].contains("includeUnknownSeriesItems=true"));
    }

    #[tokio::test]
    async fn grab_history_gives_upper_case_download_ids() {
        let (engine, recorded) = radarr();
        let page = engine.grabs(1, 250).await.unwrap();
        assert_eq!(page.total_records, 3);
        assert_eq!(page.records, 3);
        assert_eq!(
            page.download_ids,
            vec![
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1".to_string(),
                "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB2".to_string()
            ]
        );
        let url = &recorded.urls_seen()[0];
        assert!(
            url.contains("eventType=1") && url.contains("page=1") && url.contains("pageSize=250")
        );
    }

    #[tokio::test]
    async fn the_arr_parser_is_asked_with_a_get() {
        let (engine, recorded) = radarr();
        let parsed = engine
            .parse("10 Cloverfield Lane 2016 1080p BluRay x265-SM737 & more")
            .await
            .unwrap();
        assert_eq!(parsed.title.as_deref(), Some("10 Cloverfield Lane"));
        assert_eq!(parsed.quality.as_deref(), Some("Bluray-1080p"));
        assert_eq!(parsed.group.as_deref(), Some("SM737"));
        assert_eq!(parsed.edition.as_deref(), Some(""));
        assert_eq!(parsed.score, Some(1250));
        assert_eq!(parsed.matched_title.as_deref(), Some("10 Cloverfield Lane"));
        assert_eq!(recorded.methods_seen(), vec![Method::Get]);
        assert!(recorded.urls_seen()[0].contains("/api/v3/parse?title=10%20Cloverfield%20Lane%202016%201080p%20BluRay%20x265-SM737%20%26%20more"));

        let (sonarr, _) = sonarr();
        let parsed = sonarr
            .parse("Shoresy.S01E03.1080p.WEB.h264-NTb")
            .await
            .unwrap();
        assert_eq!((parsed.season, parsed.episodes.clone()), (Some(1), vec![3]));
        assert_eq!(parsed.edition, None);
        assert_eq!(parsed.title.as_deref(), Some("Shoresy"));
    }

    #[tokio::test]
    async fn how_a_manager_names_files_is_read_with_gets() {
        let (radarr, recorded) = radarr();
        recorded.answer(
            Method::Get,
            "/api/v3/config/naming",
            200,
            include_str!("../../tests/fixtures/radarr_naming.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v3/customformat",
            200,
            include_str!("../../tests/fixtures/radarr_customformat.json"),
        );
        let naming = radarr.naming().await.unwrap().unwrap();
        assert!(naming.rename);
        assert!(
            naming
                .movie_file
                .as_deref()
                .unwrap()
                .starts_with("{Movie CleanTitle} {(Release Year)}")
        );
        assert_eq!(
            naming.movie_folder.as_deref(),
            Some("{Movie CleanTitle} ({Release Year}) {tmdb-{TmdbId}}")
        );
        assert_eq!(naming.colon.as_deref(), Some("smart"));
        assert_eq!(naming.named_formats, ["IMAX Enhanced", "Hybrid"]);
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));

        let (sonarr, recorded) = sonarr();
        recorded.answer(
            Method::Get,
            "/api/v3/config/naming",
            200,
            include_str!("../../tests/fixtures/sonarr_naming.json"),
        );
        // A manager whose custom formats cannot be listed still says how it names files.
        recorded.answer(Method::Get, "/api/v3/customformat", 500, "{}");
        let naming = sonarr.naming().await.unwrap().unwrap();
        assert_eq!(naming.season_folder.as_deref(), Some("Season {season:00}"));
        assert_eq!(
            naming.series_folder.as_deref(),
            Some("{Series TitleYear} {tvdb-{TvdbId}}")
        );
        assert_eq!(naming.colon.as_deref(), Some("4"));
        assert!(naming.named_formats.is_empty() && naming.movie_file.is_none());
    }

    #[tokio::test]
    async fn a_failing_engine_is_an_error_not_an_empty_list() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v3/tag",
            401,
            "{\"error\":\"Unauthorized\"}",
        );
        let engine = Radarr::new(client(
            "radarr",
            ArrKind::Radarr,
            "radarr",
            BASE_RADARR,
            recorded,
        ));
        let err = engine.titles().await.unwrap_err();
        assert!(err.to_string().contains("401"), "{err}");

        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v3/system/status",
            200,
            "<html>login</html>",
        );
        let engine = Radarr::new(client(
            "radarr",
            ArrKind::Radarr,
            "radarr",
            BASE_RADARR,
            recorded,
        ));
        assert!(matches!(
            engine.status().await,
            Err(EngineError::Decode { .. })
        ));
    }

    #[test]
    fn episode_names_and_dates_are_read() {
        assert_eq!(
            episode_name(
                "Shoresy (2022) - S01E03 - Know Your Role [HDTV-1080p][EAC3 5.1][h264]-NTb.mkv"
            )
            .as_deref(),
            Some("Know Your Role")
        );
        assert_eq!(
            episode_name("Show - S01E01 - A - B [WEBDL-1080p]-G.mkv").as_deref(),
            Some("A - B")
        );
        assert_eq!(episode_name("Movie (2000) [Bluray-1080p]-G.mkv"), None);
        assert!(date(&Some("2026-09-21T20:20:00Z".to_string())).is_some());
        assert!(date(&Some("0001-01-01T00:00:00Z".to_string())).is_none());
        assert!(date(&Some("yesterday".to_string())).is_none());
        assert!(date(&None).is_none());
    }

    // Acting -------------------------------------------------------------------------------

    use crate::core::outbound::{Access, AccessSwitch, OutboundError};

    const HASH: &str = "147E6690379BCAC2718AAA6A0D4089BFED6737B2";

    fn base_of(kind: ArrKind) -> (&'static str, &'static str) {
        match kind {
            ArrKind::Radarr => ("radarr", BASE_RADARR),
            ArrKind::Sonarr => ("sonarr", BASE_SONARR),
        }
    }

    /// A manager listed with the calls its adapter declares, and the switch of its access.
    fn acting(kind: ArrKind, recorded: Arc<Recorded>) -> (ArrClient, AccessSwitch) {
        let (id, base) = base_of(kind);
        let mut allow = Allowlist::with_metadata_providers();
        allow.allow_integration(id, base, calls(kind)).unwrap();
        let switch = allow.switch(id).expect("a manager has acts");
        let out = Outbound::with_transport(allow, recorded);
        let config = ArrConfig {
            id: id.to_string(),
            kind,
            library: id.to_string(),
            url: Some(base.to_string()),
            api_key: Some("test-key".to_string()),
        };
        (ArrClient::new(&config, out).unwrap(), switch)
    }

    fn file(kind: ArrKind) -> ImportFile {
        ImportFile {
            path: "/downloads/Some Release/file.mkv".to_string(),
            size: Some(7),
            title_id: Some(4806),
            episode_ids: match kind {
                ArrKind::Radarr => Vec::new(),
                ArrKind::Sonarr => vec![16846, 16847],
            },
            season: None,
            quality: Some(
                json!({"quality": {"id": 7, "name": "Bluray-1080p"}, "revision": {"version": 1}, "extra": 1}),
            ),
            languages: json!([{"id": 1, "name": "English"}]),
            release_group: Some("GRP".to_string()),
            download_id: Some(HASH.to_string()),
            indexer_flags: 0,
            release_type: match kind {
                ArrKind::Radarr => None,
                ArrKind::Sonarr => Some("multiEpisode".to_string()),
            },
            rejections: Vec::new(),
        }
    }

    /// Every act of each manager goes in its one form, and only while the access is `act`:
    /// refused before anything leaves under `read`, sent and answered under `act`, and turned
    /// back again the moment the access is taken back.
    #[tokio::test]
    async fn each_act_goes_in_its_one_form_and_only_with_the_access() {
        for kind in [ArrKind::Radarr, ArrKind::Sonarr] {
            let (_, base) = base_of(kind);
            let recorded = Recorded::new();
            recorded.answer(
                Method::Post,
                &format!("{base}{RELEASE_PATH}"),
                200,
                r#"{"guid": "g", "id": 0}"#,
            );
            recorded.answer(
                Method::Post,
                &format!("{base}{COMMAND_PATH}"),
                201,
                r#"{"id": 4242, "name": "x", "status": "queued"}"#,
            );
            recorded.answer(Method::Delete, &format!("{base}/api/v3/queue/"), 200, "");
            let (client, switch) = acting(kind, recorded.clone());
            let grab = ManagerAct::Grab {
                guid: "https://indexer.example.org/guid/9af2921d9d47".to_string(),
                indexer_id: 17,
            };
            let import = ManagerAct::Import {
                files: vec![file(kind)],
                mode: ImportMode::Copy,
            };
            let remove = ManagerAct::Remove {
                queue_id: 1374604330,
                blocklist: true,
            };
            let every = match kind {
                ArrKind::Radarr => vec![
                    grab,
                    ManagerAct::SearchMovies {
                        movies: vec![4806, 12],
                    },
                    import,
                    remove,
                ],
                ArrKind::Sonarr => vec![
                    grab,
                    ManagerAct::SearchEpisodes {
                        episodes: vec![16846],
                    },
                    ManagerAct::SearchSeason {
                        series: 513,
                        season: 0,
                    },
                    ManagerAct::SearchSeries { series: 513 },
                    import,
                    remove,
                ],
            };
            for act in &every {
                let refused = client.act(act).await.unwrap_err();
                assert!(
                    matches!(
                        refused,
                        EngineError::Outbound(OutboundError::NotAllowed { .. })
                    ),
                    "{act:?}: {refused:?}"
                );
            }
            assert!(recorded.calls_sent().is_empty(), "nothing left under read");

            switch.set(Access::Act);
            for act in &every {
                let acted = client.act(act).await.unwrap();
                let command = (act.word() == "search" || act.word() == "import").then_some(4242);
                assert_eq!(acted, ManagerActed::Done { command }, "{act:?}");
            }
            let sent: Vec<(Method, String, Option<Value>)> = recorded
                .calls_sent()
                .iter()
                .map(|c| {
                    let body = match &c.body {
                        Body::Json(text) => Some(serde_json::from_str(text).unwrap()),
                        Body::None => None,
                        other => panic!("{other:?}"),
                    };
                    (c.method, c.url.trim_start_matches(base).to_string(), body)
                })
                .collect();
            let grab_body =
                json!({"guid": "https://indexer.example.org/guid/9af2921d9d47", "indexerId": 17});
            let quality =
                json!({"quality": {"id": 7, "name": "Bluray-1080p"}, "revision": {"version": 1}});
            let delete = (
                Method::Delete,
                "/api/v3/queue/1374604330?removeFromClient=true&blocklist=true".to_string(),
                None,
            );
            let post = |path: &str, body: Value| (Method::Post, path.to_string(), Some(body));
            let expected = match kind {
                ArrKind::Radarr => vec![
                    post(RELEASE_PATH, grab_body),
                    post(
                        COMMAND_PATH,
                        json!({"name": "MoviesSearch", "movieIds": [4806, 12]}),
                    ),
                    post(
                        COMMAND_PATH,
                        json!({"name": "ManualImport", "importMode": "copy", "files": [{
                            "path": "/downloads/Some Release/file.mkv", "movieId": 4806,
                            "quality": quality, "languages": [{"id": 1, "name": "English"}],
                            "releaseGroup": "GRP", "downloadId": HASH, "indexerFlags": 0,
                        }]}),
                    ),
                    delete,
                ],
                ArrKind::Sonarr => vec![
                    post(RELEASE_PATH, grab_body),
                    post(
                        COMMAND_PATH,
                        json!({"name": "EpisodeSearch", "episodeIds": [16846]}),
                    ),
                    post(
                        COMMAND_PATH,
                        json!({"name": "SeasonSearch", "seriesId": 513, "seasonNumber": 0}),
                    ),
                    post(
                        COMMAND_PATH,
                        json!({"name": "SeriesSearch", "seriesId": 513}),
                    ),
                    post(
                        COMMAND_PATH,
                        json!({"name": "ManualImport", "importMode": "copy", "files": [{
                            "path": "/downloads/Some Release/file.mkv", "seriesId": 4806,
                            "episodeIds": [16846, 16847], "quality": quality,
                            "languages": [{"id": 1, "name": "English"}], "releaseGroup": "GRP",
                            "downloadId": HASH, "indexerFlags": 0, "releaseType": "multiEpisode",
                        }]}),
                    ),
                    delete,
                ],
            };
            assert_eq!(sent, expected, "{kind:?}");
            assert!(
                recorded
                    .calls_sent()
                    .iter()
                    .all(|c| c.headers == [("X-Api-Key".to_string(), "test-key".to_string())])
            );
            // A search the other manager makes is not offered here, and nothing is sent.
            let before = recorded.calls_sent().len();
            let other = match kind {
                ArrKind::Radarr => ManagerAct::SearchSeries { series: 1 },
                ArrKind::Sonarr => ManagerAct::SearchMovies { movies: vec![1] },
            };
            assert_eq!(client.act(&other).await.unwrap(), ManagerActed::NotOffered);
            // The manager says no in its own words; a failure that is not its no is an error.
            recorded.answer(
                Method::Post,
                &format!("{base}{RELEASE_PATH}"),
                404,
                r#"{"message": "Couldn't find requested release in cache, try searching again"}"#,
            );
            assert_eq!(
                client.act(&every[0]).await.unwrap(),
                ManagerActed::Refused(
                    "Couldn't find requested release in cache, try searching again".to_string()
                )
            );
            recorded.answer(Method::Post, &format!("{base}{COMMAND_PATH}"), 500, "");
            assert!(client.act(&every[1]).await.is_err());
            // Taken back: refused again with the next call.
            switch.set(Access::Read);
            let after = recorded.calls_sent().len();
            assert_eq!(after, before + 2);
            assert!(client.act(&every[1]).await.is_err());
            assert_eq!(recorded.calls_sent().len(), after);
        }
    }

    fn json_body(value: &Value) -> Body {
        Body::Json(value.to_string())
    }

    /// Under write access each act path passes in its one form alone: another key, another
    /// command name, another value, the same act on another path, and every write the manager
    /// documents in its API is refused as no listed call.
    #[test]
    fn only_the_declared_forms_pass_whatever_the_access() {
        for (kind, docs) in [
            (
                ArrKind::Radarr,
                include_str!("../../tests/fixtures/radarr_api_docs.json"),
            ),
            (
                ArrKind::Sonarr,
                include_str!("../../tests/fixtures/sonarr_api_docs.json"),
            ),
        ] {
            let (id, base) = base_of(kind);
            let mut allow = Allowlist::default();
            allow.allow_integration(id, base, calls(kind)).unwrap();
            allow.switch(id).unwrap().set(Access::Act);
            let check = |method: Method, path: &str, body: &Body| {
                allow.check_call(method, &format!("{base}{path}"), body)
            };
            let refused = |method: Method, path: &str, body: Value| {
                let err = check(method, path, &json_body(&body)).unwrap_err();
                assert!(
                    matches!(err, OutboundError::NotListed { .. }),
                    "{} {path} {body}: {err:?}",
                    method.as_str()
                );
            };
            // Each act in its form passes.
            let search = match kind {
                ArrKind::Radarr => json!({"name": "MoviesSearch", "movieIds": [1, 2]}),
                ArrKind::Sonarr => json!({"name": "EpisodeSearch", "episodeIds": [1, 2]}),
            };
            let import_file = match kind {
                ArrKind::Radarr => {
                    json!({"path": "/d/a.mkv", "movieId": 1, "quality": {"quality": {}}, "languages": [], "downloadId": "H", "indexerFlags": 0})
                }
                ArrKind::Sonarr => {
                    json!({"path": "/d/a.mkv", "seriesId": 1, "episodeIds": [1], "quality": {"quality": {}}, "languages": [], "downloadId": "H", "indexerFlags": 0})
                }
            };
            let import = json!({"name": "ManualImport", "files": [import_file.clone()], "importMode": "auto"});
            for (method, path, body) in [
                (
                    Method::Post,
                    RELEASE_PATH,
                    json_body(&json!({"guid": "g", "indexerId": 3})),
                ),
                (Method::Post, COMMAND_PATH, json_body(&search)),
                (Method::Post, COMMAND_PATH, json_body(&import)),
                (
                    Method::Delete,
                    "/api/v3/queue/7?removeFromClient=true&blocklist=false",
                    Body::None,
                ),
            ] {
                assert!(
                    check(method, path, &body).is_ok(),
                    "{} {path}",
                    method.as_str()
                );
            }
            // The same paths in any other form.
            let mut other_files = Vec::new();
            for (key, value) in [
                ("movieFileId", json!(9)),
                ("episodeFileId", json!(9)),
                ("customFormats", json!([])),
                ("rejections", json!([])),
            ] {
                let mut f = import_file.clone();
                f[key] = value;
                other_files.push(f);
            }
            for key in ["path", "downloadId", "quality", "indexerFlags"] {
                let mut f = import_file.clone();
                f.as_object_mut().unwrap().remove(key);
                other_files.push(f);
            }
            let mut f = import_file.clone();
            f["path"] = json!(" ");
            other_files.push(f);
            for f in other_files {
                refused(
                    Method::Post,
                    COMMAND_PATH,
                    json!({"name": "ManualImport", "files": [f], "importMode": "auto"}),
                );
            }
            for body in [
                json!({"guid": "g", "indexerId": 3, "downloadClientId": 1}),
                json!({"guid": "g", "indexerId": 3, "shouldOverride": true}),
                json!({"guid": "g", "indexerId": "3"}),
                json!({"guid": "g", "indexerId": 0}),
                json!({"guid": "", "indexerId": 3}),
                json!({"indexerId": 3}),
                json!([{"guid": "g", "indexerId": 3}]),
            ] {
                refused(Method::Post, RELEASE_PATH, body);
            }
            let too_many: Vec<i64> = (1..=(SEARCH_MAX as i64 + 1)).collect();
            for body in [
                json!({"name": "RefreshMovie", "movieIds": [1]}),
                json!({"name": "RenameMovie", "movieIds": [1]}),
                json!({"name": "RenameFiles", "movieId": 1, "files": [1]}),
                json!({"name": "RefreshSeries", "seriesId": 1}),
                json!({"name": "RenameSeries", "seriesIds": [1]}),
                json!({"name": "Backup"}),
                json!({"name": "ApplicationUpdate"}),
                json!({"name": "ApplicationCheckUpdate"}),
                json!({"name": "ResetApiKey"}),
                json!({"name": "DeleteLogFiles"}),
                json!({"name": "RssSync"}),
                json!({"name": "MissingMoviesSearch"}),
                json!({"name": "CutoffUnmetMoviesSearch"}),
                json!({"name": "MissingEpisodeSearch"}),
                json!({"name": "DownloadedMoviesScan", "path": "/d"}),
                json!({"name": "moviessearch", "movieIds": [1]}),
                json!({"name": "MoviesSearch", "movieIds": [1], "sendUpdatesToClient": true}),
                json!({"name": "MoviesSearch", "movieIds": []}),
                json!({"name": "MoviesSearch", "movieIds": [0]}),
                json!({"name": "MoviesSearch", "movieIds": ["1"]}),
                json!({"name": "MoviesSearch", "movieIds": [1.5]}),
                json!({"name": "MoviesSearch", "movieIds": too_many}),
                json!({"name": "EpisodeSearch", "episodeIds": [1], "seriesId": 2}),
                json!({"name": "EpisodeSearch", "episodeIds": too_many}),
                json!({"name": "SeasonSearch", "seriesId": 1}),
                json!({"name": "SeasonSearch", "seriesId": 1, "seasonNumber": -1}),
                json!({"name": "SeriesSearch", "seriesId": 0}),
                json!({"name": "ManualImport", "files": [], "importMode": "auto"}),
                json!({"name": "ManualImport", "files": [import_file], "importMode": "hardlink"}),
                json!({"name": "ManualImport", "files": [import_file]}),
                json!({"name": "ManualImport", "files": [import_file], "importMode": "auto", "sendUpdatesToClient": true}),
                json!({"name": "ManualImport", "files": ["/d/a.mkv"], "importMode": "auto"}),
                json!({}),
            ] {
                refused(Method::Post, COMMAND_PATH, body);
            }
            // The search commands are each manager's own: the other's is refused.
            refused(
                Method::Post,
                COMMAND_PATH,
                match kind {
                    ArrKind::Radarr => json!({"name": "EpisodeSearch", "episodeIds": [1]}),
                    ArrKind::Sonarr => json!({"name": "MoviesSearch", "movieIds": [1]}),
                },
            );
            for path in [
                "/api/v3/queue/7",
                "/api/v3/queue/7?removeFromClient=true",
                "/api/v3/queue/7?removeFromClient=true&blocklist=yes",
                "/api/v3/queue/7?removeFromClient=true&blocklist=false&skipRedownload=true",
                "/api/v3/queue/7?removeFromClient=true&blocklist=false&changeCategory=true",
                "/api/v3/queue/7?removeFromClient=true&removeFromClient=false&blocklist=true",
                "/api/v3/queue/bulk?removeFromClient=true&blocklist=false",
                "/api/v3/queue/x7?removeFromClient=true&blocklist=false",
                "/api/v3/queue/7/8?removeFromClient=true&blocklist=false",
                "/api/v3/queue/12345678901?removeFromClient=true&blocklist=false",
                "/api/v3/queue?removeFromClient=true&blocklist=false",
            ] {
                let err = check(Method::Delete, path, &Body::None).unwrap_err();
                assert!(matches!(err, OutboundError::NotListed { .. }), "{path}");
            }
            // Every write the manager's API documents: none is a listed call with any body but
            // the acts' own forms, which these are not.
            let doc: Value = serde_json::from_str(docs).unwrap();
            let mut writes = 0;
            for (path, ops) in doc["paths"].as_object().unwrap() {
                let concrete = path
                    .replace("{id}", "7")
                    .replace("{name}", "x")
                    .replace("{filename}", "x.txt")
                    .replace("{path}", "x")
                    .replace("{movieId}", "7");
                for (word, method) in [
                    ("post", Method::Post),
                    ("put", Method::Put),
                    ("delete", Method::Delete),
                ] {
                    if ops.get(word).is_none() {
                        continue;
                    }
                    writes += 1;
                    for body in [
                        Body::None,
                        json_body(&json!({})),
                        json_body(&json!({"name": "RefreshMovie"})),
                        json_body(&json!({"ids": [7]})),
                    ] {
                        let err = check(method, &concrete, &body).unwrap_err();
                        assert!(
                            matches!(err, OutboundError::NotListed { .. }),
                            "{word} {concrete}: {err:?}"
                        );
                    }
                }
            }
            assert!(writes > 100, "{writes}");
        }
    }

    /// The paths that shut down, restart, restore, delete a title or a file, or change a setting
    /// or a profile are refused with every method and every body, GET included, under write
    /// access too. A read next to them still passes.
    #[test]
    fn acting_never_reaches_what_shuts_down_deletes_or_configures() {
        for kind in [ArrKind::Radarr, ArrKind::Sonarr] {
            let (id, base) = base_of(kind);
            let mut allow = Allowlist::default();
            allow.allow_integration(id, base, calls(kind)).unwrap();
            allow.switch(id).unwrap().set(Access::Act);
            let never = calls(kind).never;
            assert_eq!(
                never.len(),
                NEVER.len() + if kind == ArrKind::Radarr { 5 } else { 6 }
            );
            for declared in &never {
                let path = declared.replace("{id}", "7").replace("{any}", "naming");
                let paths = match path.strip_suffix('/') {
                    Some(below) => vec![format!("{below}/7"), format!("{below}/restore/7")],
                    None => vec![path],
                };
                for path in paths {
                    for method in [Method::Get, Method::Post, Method::Put, Method::Delete] {
                        for body in [
                            Body::None,
                            json_body(&json!({"guid": "g", "indexerId": 3})),
                            json_body(&json!({"name": "MoviesSearch", "movieIds": [7]})),
                        ] {
                            let err = allow
                                .check_call(method, &format!("{base}{path}"), &body)
                                .unwrap_err();
                            assert!(
                                matches!(err, OutboundError::NotListed { .. }),
                                "{} {path}: {err:?}",
                                method.as_str()
                            );
                        }
                    }
                }
            }
            // The reads the adapter makes pass, also next to a barred path.
            for path in [
                "/api/v3/movie",
                "/api/v3/series",
                "/api/v3/moviefile?movieId=1",
                "/api/v3/episodefile?seriesId=1",
                "/api/v3/config/naming",
                "/api/v3/qualityprofile",
                "/api/v3/customformat",
                "/api/v3/system/status",
                "/api/v3/system/backup",
                "/api/v3/release?movieId=1",
                "/api/v3/manualimport?downloadId=H",
                "/api/v3/queue?page=1",
            ] {
                assert!(
                    allow.check(Method::Get, &format!("{base}{path}")).is_ok(),
                    "{path}"
                );
            }
        }
    }

    /// The interactive search asks with GET and a limit of its own, and every release comes
    /// back with the manager's reading, formats, score and rejections.
    #[tokio::test]
    async fn an_interactive_search_reads_every_release_with_the_manager_s_word() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE_RADARR}/api/v3/release"),
            200,
            include_str!("../../tests/fixtures/radarr_release.json"),
        );
        let (radarr, _) = acting(ArrKind::Radarr, recorded.clone());
        let found = radarr
            .releases(ReleaseScope::Movie { movie: 4806 })
            .await
            .unwrap();
        assert_eq!(found.len(), 205);
        let first = &found[0];
        assert_eq!(
            (
                first.guid.as_str(),
                first.indexer_id,
                first.indexer.as_deref()
            ),
            (
                "https://indexer.example.org/guid/9af2921d9d47",
                17,
                Some("Torrenting (Prowlarr)")
            )
        );
        assert_eq!(first.quality.as_deref(), Some("Remux-1080p"));
        assert_eq!((first.score, first.approved), (Some(1750), false));
        assert_eq!(first.formats.len(), 5);
        assert!(first.rejections[0].contains("open[ ._-]?matte"));
        assert_eq!((first.seeders, first.age_days), (Some(4), Some(144)));
        assert_eq!(first.group.as_deref(), Some("FraMeSToR"));
        assert!(
            found
                .iter()
                .all(|r| !r.approved && !r.rejections.is_empty())
        );
        let sent = recorded.calls_sent();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].method, Method::Get);
        assert!(sent[0].url.ends_with("/api/v3/release?movieId=4806"));
        assert_eq!(sent[0].timeout, Some(SEARCH_TIMEOUT));

        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE_SONARR}/api/v3/release"),
            200,
            include_str!("../../tests/fixtures/sonarr_release.json"),
        );
        let (sonarr, _) = acting(ArrKind::Sonarr, recorded.clone());
        let found = sonarr
            .releases(ReleaseScope::Episode { episode: 16846 })
            .await
            .unwrap();
        assert_eq!(found.len(), 110);
        assert_eq!(found.iter().filter(|r| r.approved).count(), 20);
        let first = &found[0];
        assert_eq!((first.season, first.episodes.clone()), (Some(2), vec![1]));
        assert_eq!(first.quality.as_deref(), Some("WEBDL-1080p"));
        assert!(first.approved && first.rejections.is_empty());
        let season = sonarr
            .releases(ReleaseScope::Season {
                series: 513,
                season: 2,
            })
            .await
            .unwrap();
        assert_eq!(season.len(), 110);
        let urls = recorded.urls_seen();
        assert!(urls[0].ends_with("/api/v3/release?episodeId=16846"));
        assert!(urls[1].ends_with("/api/v3/release?seriesId=513&seasonNumber=2"));
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
    }

    /// A download the manager refused to import: its queue entry, read now, names the client,
    /// and the manual import offers its file with the manager's reading, which the import sends
    /// back unchanged.
    #[tokio::test]
    async fn a_refused_download_is_imported_from_what_the_manual_import_offers() {
        let recorded = Recorded::new();
        let queue = format!(
            r#"{{"page": 1, "pageSize": 500, "totalRecords": 1, "records": {}}}"#,
            include_str!("../../tests/fixtures/radarr_queue_import_refused.json")
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE_RADARR}/api/v3/queue"),
            200,
            &queue,
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE_RADARR}/api/v3/manualimport"),
            200,
            include_str!("../../tests/fixtures/radarr_manualimport.json"),
        );
        recorded.answer(
            Method::Post,
            &format!("{BASE_RADARR}{COMMAND_PATH}"),
            201,
            r#"{"id": 77}"#,
        );
        let (client, switch) = acting(ArrKind::Radarr, recorded.clone());
        let entries = client
            .queue_entries("includeUnknownMovieItems", &HASH.to_ascii_lowercase())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(
            (
                entry.queue_id,
                entry.engine_title_id,
                entry.download_id.as_str()
            ),
            (1374604330, Some(4806), HASH)
        );
        assert_eq!(
            (entry.protocol.as_deref(), entry.download_client.as_deref()),
            (Some("torrent"), Some("qBittorrent"))
        );
        assert!(!entry.post_import_category);
        assert_eq!(entry.tracked_state.as_deref(), Some("importPending"));
        assert!(entry.messages[0].starts_with("Not a Custom Format upgrade"));
        // The client keeps seeding a torrent: it is copied.
        assert_eq!(import_mode(entry), ImportMode::Copy);
        let with = |protocol: &str, category: bool| QueueEntry {
            protocol: Some(protocol.to_string()),
            post_import_category: category,
            ..entry.clone()
        };
        assert_eq!(import_mode(&with("usenet", false)), ImportMode::Move);
        assert_eq!(import_mode(&with("usenet", true)), ImportMode::Copy);
        assert_eq!(import_mode(&with("unknown", false)), ImportMode::Auto);
        assert_eq!(
            ImportMode::ALL.map(ImportMode::as_str),
            MODES,
            "the rule takes the modes the adapter sends"
        );

        let offer = client.import_offer(HASH).await.unwrap();
        assert_eq!(offer.len(), 1);
        let f = &offer[0];
        assert_eq!(f.path, "/downloads/The Devil Wears Prada.mkv");
        assert_eq!((f.title_id, f.size), (Some(4806), Some(7911378658)));
        assert_eq!((f.indexer_flags, f.download_id.as_deref()), (1, Some(HASH)));
        assert!(f.rejections[0].starts_with("Not a Custom Format upgrade"));
        switch.set(Access::Act);
        let acted = client
            .act(&ManagerAct::Import {
                files: offer.clone(),
                mode: import_mode(entry),
            })
            .await
            .unwrap();
        assert_eq!(acted, ManagerActed::Done { command: Some(77) });
        let sent = recorded.calls_sent();
        assert!(
            sent[1].url.ends_with(
                "/api/v3/manualimport?downloadId=147E6690379BCAC2718AAA6A0D4089BFED6737B2"
            )
        );
        let Body::Json(body) = &sent[2].body else {
            panic!("{:?}", sent[2].body)
        };
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"name": "ManualImport", "importMode": "copy", "files": [{
                "path": "/downloads/The Devil Wears Prada.mkv",
                "movieId": 4806,
                "quality": {
                    "quality": {"id": 7, "name": "Bluray-1080p", "source": "bluray", "resolution": 1080, "modifier": "none"},
                    "revision": {"version": 1, "real": 0, "isRepack": false}
                },
                "languages": [{"id": 1, "name": "English"}],
                "downloadId": HASH,
                "indexerFlags": 1
            }]})
        );
    }

    /// A usenet download's id is its client's, in the client's case (SABnzbd's `nzo_id`, the
    /// `drone` value in NZBGet). The queue entry matches it in any case. The manual import and
    /// the import command carry it as the manager wrote it: the manager finds a download by
    /// the exact id.
    #[tokio::test]
    async fn a_usenet_download_is_imported_under_the_id_its_manager_wrote() {
        const NZO: &str = "SABnzbd_nzo_x7f3k2ab";
        let recorded = Recorded::new();
        let mut records: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/radarr_queue_import_refused.json"
        ))
        .unwrap();
        records[0]["downloadId"] = json!(NZO);
        records[0]["protocol"] = json!("usenet");
        records[0]["downloadClient"] = json!("SABnzbd");
        let queue = json!({"page": 1, "pageSize": 500, "totalRecords": 1, "records": records});
        recorded.answer(
            Method::Get,
            &format!("{BASE_RADARR}/api/v3/queue"),
            200,
            &queue.to_string(),
        );
        let mut offer: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/radarr_manualimport.json"
        ))
        .unwrap();
        offer[0]["downloadId"] = json!(NZO);
        recorded.answer(
            Method::Get,
            &format!("{BASE_RADARR}/api/v3/manualimport"),
            200,
            &offer.to_string(),
        );
        recorded.answer(
            Method::Post,
            &format!("{BASE_RADARR}{COMMAND_PATH}"),
            201,
            r#"{"id": 78}"#,
        );
        let (client, switch) = acting(ArrKind::Radarr, recorded.clone());
        let entries = client
            .queue_entries("includeUnknownMovieItems", &NZO.to_ascii_uppercase())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(
            (
                entry.download_id.as_str(),
                entry.download_id_as_written.as_str()
            ),
            ("SABNZBD_NZO_X7F3K2AB", NZO)
        );
        assert_eq!(import_mode(entry), ImportMode::Move);
        let offer = client
            .import_offer(&entry.download_id_as_written)
            .await
            .unwrap();
        assert_eq!(offer[0].download_id.as_deref(), Some(NZO));
        switch.set(Access::Act);
        let acted = client
            .act(&ManagerAct::Import {
                files: offer,
                mode: import_mode(entry),
            })
            .await
            .unwrap();
        assert_eq!(acted, ManagerActed::Done { command: Some(78) });
        let sent = recorded.calls_sent();
        assert!(
            sent[1]
                .url
                .ends_with("/api/v3/manualimport?downloadId=SABnzbd_nzo_x7f3k2ab"),
            "{}",
            sent[1].url
        );
        let Body::Json(body) = &sent[2].body else {
            panic!("{:?}", sent[2].body)
        };
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            (&body["importMode"], &body["files"][0]["downloadId"]),
            (&json!("move"), &json!(NZO))
        );
    }

    /// A series' episodes come with the manager's ids, which its searches name.
    #[tokio::test]
    async fn a_series_episodes_are_read_with_their_ids() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE_SONARR}/api/v3/episode"),
            200,
            r#"[{"id": 16846, "seriesId": 513, "seasonNumber": 2, "episodeNumber": 1, "hasFile": false, "monitored": true},
                {"id": 16847, "seriesId": 513, "seasonNumber": 2, "episodeNumber": 2, "hasFile": true, "monitored": true},
                {"seriesId": 513, "seasonNumber": 2, "episodeNumber": 3}]"#,
        );
        let (sonarr, _) = acting(ArrKind::Sonarr, recorded.clone());
        let episodes = sonarr.episodes(513, Some(2)).await.unwrap();
        assert_eq!(
            episodes,
            vec![
                ManagerEpisode {
                    id: 16846,
                    season: 2,
                    number: 1,
                    has_file: false,
                    monitored: true
                },
                ManagerEpisode {
                    id: 16847,
                    season: 2,
                    number: 2,
                    has_file: true,
                    monitored: true
                },
            ]
        );
        assert!(recorded.urls_seen()[0].ends_with("/api/v3/episode?seriesId=513&seasonNumber=2"));
    }
}
