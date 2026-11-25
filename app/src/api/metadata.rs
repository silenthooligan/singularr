//! The `/api/v1` routes of the metadata service: rich title, credits, season, person and
//! collection records, the collections the library has a part of, and the browse lists, in
//! the shapes of the web client's sample set (`web/data`). Search is `api::search`.
//!
//! The records come from TMDB through `metadata::records` (cached in the module's table). What
//! the library has is added here, above every module it reads: each card's `state`; a title's
//! `library` (from the catalog and the media servers' read model), `history` (the journal),
//! `queue` (the downloads), `requests` and `processing` (the runs of the processing services on
//! its files, with each file marked by the run that made it or works on it); a season's episodes
//! with their files from the catalog's copies. Every route needs a caller with `read`.

use super::pages::labels;
use super::processing::{FileMarks, title_processing};
use super::routes::{ApiPath, ApiQuery};
use super::state::AppState;
use crate::catalog::model::{CopyQuery, CopyView, TitleDetail, TitleKind};
use crate::catalog::service as catalog;
use crate::catalog::store::{self as catalog_store, LibraryName, TitleServerRow};
use crate::core::auth::Caller;
use crate::core::config::Config;
use crate::core::error::{AppError, AppResult};
use crate::core::model::{Event, EventQuery};
use crate::core::schema::ProblemDoc;
use crate::engines::MediaServer;
use crate::lifecycle::model::Download;
use crate::lifecycle::store as downloads_store;
use crate::metadata::MetadataProvider;
use crate::metadata::record::{
    BrowseList, BrowseTile, Card, CollectionMap, CollectionRecord, DiscoverPage, DiscoverRow,
    EpisodeFile, EpisodeRef, HistoryItem, LibraryEdition, LibraryFacts, LibraryFile, PersonRecord,
    Plays, PlexLink, QueueItem, RequestWhat, Requester, RottenTomatoes, Score, SeasonRecord,
    Specials, TitleCredits, TitleRecord, TitleRequest, TraceLine,
};
use crate::metadata::records::{ListKey, RECORD_TTL, collection_path, collection_record, title_id};
use crate::metadata::store as metadata_store;
use crate::requests::store as requests_store;
use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::Value;
use singularr_core::Policy;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use utoipa::OpenApi;

type ApiResult<T> = Result<Json<T>, AppError>;

/// Collection records fetched in the background by one call of the collections list.
const WARM_PER_CALL: usize = 20;

/// `movie`, `show` (or `tv`).
fn parse_kind(text: &str) -> AppResult<TitleKind> {
    match text.trim().to_ascii_lowercase().as_str() {
        "movie" => Ok(TitleKind::Movie),
        "show" | "tv" => Ok(TitleKind::Show),
        other => Err(AppError::bad_request(format!(
            "kind is `movie` or `show`, not `{other}`"
        ))),
    }
}

/// An id written `person-6384` or `6384`.
fn parse_id(text: &str, prefix: &str) -> AppResult<i64> {
    let t = text.trim();
    t.strip_prefix(prefix)
        .and_then(|r| r.strip_prefix('-'))
        .unwrap_or(t)
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| {
            AppError::bad_request(format!("`{t}` is not an id like `{prefix}-603` or `603`"))
        })
}

/// What the library holds of titles, by (kind, TMDB id), in the status vocabulary:
/// `available` (a copy is held), `downloading`, `requested` (an open request), `missing` (a
/// manager lists the title and holds no copy). Absent: nothing.
pub async fn card_states(
    pool: &sqlx::PgPool,
    keys: &[(String, i64)],
) -> AppResult<HashMap<(String, i64), &'static str>> {
    let mut tmdb_ids: Vec<i64> = keys.iter().map(|(_, id)| *id).collect();
    tmdb_ids.sort_unstable();
    tmdb_ids.dedup();
    let mut out = HashMap::new();
    if tmdb_ids.is_empty() {
        return Ok(out);
    }
    let links = catalog_store::title_links(pool, &tmdb_ids, &[]).await?;
    let ids: Vec<i64> = links.iter().map(|l| l.id).collect();
    let downloading = downloads_store::downloading_titles(pool, &ids).await?;
    let open = requests_store::open_requests(pool, &tmdb_ids).await?;
    for (kind, tmdb) in keys {
        let link = links
            .iter()
            .find(|l| l.kind == *kind && l.tmdb_id == Some(*tmdb));
        let requested = open.iter().any(|(k, t, _)| k == kind && t == tmdb);
        let state = match link {
            Some(l) if l.copies > 0 => Some("available"),
            Some(l) if downloading.contains(&l.id) => Some("downloading"),
            _ if requested => Some("requested"),
            Some(_) => Some("missing"),
            None => None,
        };
        if let Some(s) = state {
            out.insert((kind.clone(), *tmdb), s);
        }
    }
    Ok(out)
}

/// Mark every card of these lists with its library state.
pub async fn mark(pool: &sqlx::PgPool, lists: &mut [&mut Vec<Card>]) -> AppResult<()> {
    let keys: Vec<(String, i64)> = lists
        .iter()
        .flat_map(|l| l.iter().filter_map(Card::key))
        .collect();
    let states = card_states(pool, &keys).await?;
    for list in lists.iter_mut() {
        for card in list.iter_mut() {
            card.state = card
                .key()
                .and_then(|k| states.get(&k))
                .map(|s| s.to_string());
        }
    }
    Ok(())
}

/// The catalog title of a TMDB title, by its TMDB id (and for a show its TVDB id).
async fn catalog_title(
    pool: &sqlx::PgPool,
    kind: TitleKind,
    tmdb: i64,
    tvdb: Option<i64>,
) -> AppResult<Option<i64>> {
    let tvdb_ids: Vec<i64> = match kind {
        TitleKind::Show => tvdb.into_iter().collect(),
        TitleKind::Movie => Vec::new(),
    };
    let links = catalog_store::title_links(pool, &[tmdb], &tvdb_ids).await?;
    Ok(links
        .iter()
        .filter(|l| l.kind == kind.as_str())
        .find(|l| l.tmdb_id == Some(tmdb))
        .or_else(|| links.iter().find(|l| l.kind == kind.as_str()))
        .map(|l| l.id))
}

/// `1:52:03` or `52:03` in minutes.
fn runtime_minutes(text: &str) -> Option<i64> {
    let parts: Vec<i64> = text
        .split(':')
        .map(|p| p.trim().parse::<i64>().ok())
        .collect::<Option<Vec<_>>>()?;
    match parts.as_slice() {
        [h, m, _s] => Some(h * 60 + m),
        [m, _s] => Some(*m),
        _ => None,
    }
}

fn split_list(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(serde_json::Value::as_str)
        .map(|s| {
            s.split('/')
                .map(str::trim)
                .filter(|x| !x.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// The lines of a resolution as written: `1080p` is 1080, `4K` is 2160.
pub fn lines(label: &str) -> Option<i32> {
    let t = label.trim().to_ascii_lowercase();
    if t == "4k" || t == "uhd" {
        return Some(2160);
    }
    t.trim_end_matches(['p', 'i'])
        .parse::<i32>()
        .ok()
        .filter(|n| *n > 0)
}

/// The last folder name of a path, lower case: `/media/movies` and `/movies` are both
/// `movies` (the managers and the server see the share at different mount points).
fn folder_name(path: &str) -> Option<String> {
    path.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(str::to_ascii_lowercase)
        .filter(|n| !n.is_empty())
}

/// The name of the folder that holds a title's folder: `movies` of
/// `/media/movies/John Wick (2014)`.
fn root_name(title_folder: &str) -> Option<String> {
    let trimmed = title_folder.trim_end_matches(['/', '\\']);
    let (parent, _) = trimmed.rsplit_once(['/', '\\'])?;
    folder_name(parent)
}

/// The libraries by id, to name where a title and each of its files are: the media server's
/// section when one holds it, else the manager's library.
pub struct Libraries(Vec<LibraryName>);

impl Libraries {
    pub async fn load(pool: &sqlx::PgPool) -> AppResult<Libraries> {
        Ok(Libraries(catalog_store::library_names(pool).await?))
    }

    fn get(&self, id: &str) -> Option<&LibraryName> {
        self.0.iter().find(|l| l.id == id)
    }

    /// The server section a manager's title folder is in: a section the manager looks after
    /// whose folders end in the name of the folder that holds the title's folder; else the
    /// one section the manager looks after, when there is one.
    fn section_of(&self, manager_library: &str, path: Option<&str>) -> Option<&LibraryName> {
        let manager = self.get(manager_library)?;
        let integration = manager.integration.as_deref().unwrap_or(&manager.id);
        let all: Vec<&LibraryName> = self
            .0
            .iter()
            .filter(|l| l.section_key.is_some() && l.managed_by.as_deref() == Some(integration))
            .collect();
        if let Some(root) = path.and_then(root_name)
            && let Some(hit) = all.iter().copied().find(|l| {
                l.locations
                    .iter()
                    .filter_map(|x| folder_name(x))
                    .any(|n| n == root)
            })
        {
            return Some(hit);
        }
        match all.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Where something of a manager's library is, as (library id, name): its server section,
    /// else the section the server matched the title in, else the manager's library.
    fn place(
        &self,
        manager_library: &str,
        path: Option<&str>,
        server: Option<&TitleServerRow>,
    ) -> (String, String) {
        let section = self.section_of(manager_library, path).or_else(|| {
            server
                .and_then(|s| s.library_id.as_deref())
                .and_then(|id| self.get(id))
        });
        match section.or_else(|| self.get(manager_library)) {
            Some(l) => (l.id.clone(), l.name.clone()),
            None => (manager_library.to_string(), manager_library.to_string()),
        }
    }

    /// The address of a server item in its media server's web app, when that server is
    /// configured and can make one (Plex's needs the server's machine identifier).
    fn server_link(
        &self,
        row: &TitleServerRow,
        servers: &[Arc<dyn MediaServer>],
    ) -> Option<PlexLink> {
        let machine = row
            .library_id
            .as_deref()
            .and_then(|id| self.get(id))
            .and_then(|l| l.machine_id.as_deref());
        let link = servers
            .iter()
            .find(|s| s.id() == row.server)?
            .item_link(machine, &row.rating_key)?;
        Some(PlexLink {
            url: link.url,
            key: link.key,
        })
    }
}

/// What the manager's probe of a file says under one key, as text.
fn probed(c: &CopyView, key: &str) -> Option<String> {
    match c.media_info.as_ref()?.get(key)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// The video codec of a tracked file, in the product's words (`labels`): the manager's probe
/// of the file first, since a file that was converted after its import no longer is what its
/// release name says, then the release name. Every page that says what a file is says it
/// with this.
pub(super) fn copy_codec(c: &CopyView) -> Option<String> {
    probed(c, "videoCodec")
        .as_deref()
        .and_then(labels::probe_codec)
        .or_else(|| c.held.video_codec.map(|v| labels::codec(v).to_string()))
}

/// The lines of a tracked file's resolution: 1080, 2160.
pub(super) fn copy_lines(c: &CopyView) -> Option<i32> {
    c.held.resolution.and_then(|r| lines(r.label()))
}

/// One file in the shape of the title record's `library.editions[].files[]`.
fn library_file(
    c: &CopyView,
    policy: &Policy,
    detail: &TitleDetail,
    libs: &Libraries,
    server: Option<&TitleServerRow>,
) -> LibraryFile {
    let info = c.media_info.as_ref();
    let field = |key: &str| info.and_then(|m| m.get(key));
    let held = &c.held;
    // What a file is, in the product's words (`labels`) and from the same reading as the
    // library tables: the manager's probe of the file first, since a file that was converted
    // after its import no longer is what its release name says, then the release name.
    let probe = |key: &str| probed(c, key);
    let codec = copy_codec(c);
    let hdr = probe("videoDynamicRangeType")
        .as_deref()
        .and_then(labels::probe_hdr)
        .or_else(|| labels::hdr(&held.hdr));
    let channels = probe("audioChannels");
    let audio = probe("audioCodec")
        .as_deref()
        .and_then(|a| labels::probe_audio(a, channels.as_deref()))
        .or_else(|| labels::release_audio(&labels::HeldFacts::from_release(held)));
    let langs = split_list(field("audioLanguages"));
    let engine = detail
        .engines
        .iter()
        .find(|e| e.engine == c.engine && e.library == c.library)
        .or_else(|| detail.engines.iter().find(|e| e.library == c.library));
    let (_, library) = libs.place(&c.library, engine.and_then(|e| e.path.as_deref()), server);
    LibraryFile {
        library,
        profile: engine.and_then(|e| e.profile.clone()),
        resolution: copy_lines(c),
        source: held.source.map(|s| labels::source(s).to_string()),
        quality: c
            .arr
            .quality
            .clone()
            .or_else(|| c.singularr.quality.clone()),
        codec,
        bit_depth: field("videoBitDepth")
            .and_then(serde_json::Value::as_i64)
            .or(held.bit_depth.map(i64::from)),
        hdr,
        audio,
        audio_langs: if langs.is_empty() {
            c.languages.clone()
        } else {
            langs
        },
        subs: split_list(field("subtitles")),
        size: c.size_bytes,
        runtime: field("runTime")
            .and_then(serde_json::Value::as_str)
            .and_then(runtime_minutes),
        fps: field("videoFps").and_then(serde_json::Value::as_f64),
        group: held.group.clone(),
        file_name: file_name(&c.relative_path),
        scene_name: c.scene_name.clone(),
        added: c.date_added,
        score: c.verdict.as_ref().map(|v| v.verdict.score),
        formats: c.arr.custom_formats.clone(),
        cutoff_met: c.verdict.as_ref().is_some_and(|v| {
            policy
                .lane(&v.lane)
                .is_some_and(|l| v.verdict.score >= l.cutoff_score)
        }),
        trace: c
            .verdict
            .as_ref()
            .map(|v| {
                v.verdict
                    .trace
                    .iter()
                    .map(|t| TraceLine {
                        label: t.label.clone(),
                        points: t.points,
                        why: t.why.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
        // Marked by the caller, which knows the title's runs.
        processing: None,
    }
}

/// Episode counts of a show, specials apart: with a file; wanted by the manager (monitored
/// and aired, or with a file); known to it. Each season counts what the engine that says most
/// about it says.
struct ShowCounts {
    have: i32,
    total: i32,
    known: i32,
    specials: Option<Specials>,
}

fn show_counts(detail: &TitleDetail) -> ShowCounts {
    let regular = || detail.seasons.iter().filter(|s| s.season_number > 0);
    ShowCounts {
        have: regular().map(|s| s.episode_file_count).sum(),
        total: regular().map(|s| s.episode_count).sum(),
        known: regular()
            .map(|s| s.total_episode_count.max(s.episode_count))
            .sum(),
        specials: detail
            .seasons
            .iter()
            .find(|s| s.season_number == 0)
            .map(|s| Specials {
                have: s.episode_file_count,
                total: s.total_episode_count.max(s.episode_count),
            }),
    }
}

/// The state word of a title in the catalog: a show with every wanted episode is
/// `available`, with some `partial`; otherwise what the cards say.
fn held_state(detail: &TitleDetail, card_state: Option<&str>) -> String {
    if detail.kind == TitleKind::Show.as_str() {
        let c = show_counts(detail);
        if c.have > 0 && c.have >= c.total {
            return "available".to_string();
        }
        if c.have > 0 {
            return "partial".to_string();
        }
    }
    card_state.unwrap_or("missing").to_string()
}

/// What the library has of a title, from its catalog detail and what the media servers say
/// about it (`server`, the most played item first); `servers` make the item's link.
fn library_facts(
    detail: &TitleDetail,
    state: &str,
    policy: &Policy,
    libs: &Libraries,
    server: &[TitleServerRow],
    servers: &[Arc<dyn MediaServer>],
) -> LibraryFacts {
    let engine = detail.engines.first();
    let is_movie = detail.kind == TitleKind::Movie.as_str();
    let first_server = server.first();
    let (library_id, library) = match engine {
        Some(e) => libs.place(&e.library, e.path.as_deref(), first_server),
        None => first_server
            .and_then(|s| s.library_id.as_deref())
            .and_then(|id| libs.get(id))
            .map(|l| (l.id.clone(), l.name.clone()))
            .unwrap_or_default(),
    };
    let editions = is_movie.then(|| {
        detail
            .editions
            .iter()
            .map(|e| LibraryEdition {
                key: e.key.clone(),
                label: e.label.clone(),
                default: e.is_default,
                wanted: e.wanted,
                files: e
                    .held
                    .iter()
                    .map(|h| {
                        match h
                            .copy_id
                            .and_then(|id| detail.copies.iter().find(|c| c.id == id))
                        {
                            Some(c) => library_file(c, policy, detail, libs, first_server),
                            // A file on disk that no engine tracks: what the scan read.
                            None => LibraryFile {
                                library: libs
                                    .place(
                                        &h.library,
                                        detail
                                            .engines
                                            .iter()
                                            .find(|e| e.library == h.library)
                                            .and_then(|e| e.path.as_deref()),
                                        first_server,
                                    )
                                    .1,
                                profile: None,
                                resolution: h.resolution.as_deref().and_then(lines),
                                source: None,
                                quality: h.quality.clone(),
                                codec: None,
                                bit_depth: None,
                                hdr: None,
                                audio: None,
                                audio_langs: Vec::new(),
                                subs: Vec::new(),
                                size: h.size_bytes,
                                runtime: None,
                                fps: None,
                                group: None,
                                file_name: file_name(&h.file_name),
                                scene_name: None,
                                added: None,
                                score: None,
                                formats: Vec::new(),
                                cutoff_met: false,
                                trace: Vec::new(),
                                processing: None,
                            },
                        }
                    })
                    .collect(),
            })
            .collect()
    });
    let counts = (!is_movie).then(|| show_counts(detail));
    LibraryFacts {
        state: state.to_string(),
        monitored: detail.engines.iter().any(|e| e.monitored),
        library,
        library_id,
        profile: engine.and_then(|e| e.profile.clone()),
        path: engine.and_then(|e| e.path.clone()),
        added: detail.engines.iter().filter_map(|e| e.added_at).min(),
        editions,
        protections: is_movie.then(|| detail.protections.clone()),
        size: (!is_movie).then(|| detail.engines.iter().map(|e| e.size_bytes).sum()),
        have: counts.as_ref().map(|c| c.have),
        known: counts.as_ref().map(|c| c.known),
        total: counts.as_ref().map(|c| c.total),
        specials: counts.and_then(|c| c.specials),
        plays: first_server.map(|_| Plays {
            count: server.iter().map(|s| s.plays).sum(),
            last: server.iter().filter_map(|s| s.last_played).max(),
        }),
        plex: first_server.and_then(|s| libs.server_link(s, servers)),
    }
}

/// Rotten Tomatoes as a media server reports it: a rating 0 to 10 whose image key is Rotten
/// Tomatoes', as a percentage.
pub fn rotten_tomatoes(row: &catalog_store::TitleServerRow) -> Option<RottenTomatoes> {
    let rt = |rating: Option<f64>, image: &Option<String>| {
        image
            .as_deref()
            .filter(|i| i.starts_with("rottentomatoes://"))
            .and(rating)
            .map(|r| (r * 10.0).round() as i32)
    };
    let critics = rt(row.critic_rating, &row.critic_image);
    let audience = rt(row.audience_rating, &row.audience_image);
    (critics.is_some() || audience.is_some()).then(|| RottenTomatoes {
        critics,
        audience,
        critics_image: critics.and(row.critic_image.clone()),
        audience_image: audience.and(row.audience_image.clone()),
    })
}

/// The TMDB path of a poster address the managers store (`.../t/p/original/abc.jpg`).
pub fn tmdb_poster(url: Option<&str>) -> Option<String> {
    let u = url?;
    let at = u.find("image.tmdb.org/t/p/")?;
    let rest = &u[at + "image.tmdb.org/t/p/".len()..];
    rest.find('/').map(|i| rest[i..].to_string())
}

/// A text field of an event body.
fn body_text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The journal's kinds that are part of a title's history, and how many a record carries.
const HISTORY_KINDS: [&str; 6] = [
    "title_added",
    "copy_added",
    "copy_replaced",
    "copy_removed",
    "download_added",
    "download_completed",
];
const HISTORY_MAX: u32 = 50;

/// The first episode an event body names (`season`, `episodes`).
fn episode_of(b: &Value) -> Option<EpisodeRef> {
    let season = i32::try_from(b.get("season")?.as_i64()?).ok()?;
    let number = i32::try_from(b.get("episodes")?.as_array()?.first()?.as_i64()?).ok()?;
    Some(EpisodeRef { season, number })
}

/// One journal event of a title as a line of its history. Events that are not part of a
/// title's history (score changes, stuck downloads) are left out.
fn history_item(e: &Event) -> Option<HistoryItem> {
    let b = &e.body;
    let quality_of = |c: &Value| body_text(c, "arr_quality").or_else(|| body_text(c, "quality"));
    let (event, copy, from): (&str, Option<&Value>, Option<String>) = match e.kind.as_str() {
        "title_added" => ("added", None, None),
        "copy_added" => ("imported", Some(b), None),
        "copy_replaced" => ("upgraded", b.get("new"), b.get("old").and_then(quality_of)),
        "copy_removed" => ("removed", Some(b), None),
        "download_added" => ("grabbed", None, None),
        "download_completed" => ("downloaded", None, None),
        _ => return None,
    };
    let episode = episode_of(b);
    let quality = copy.and_then(quality_of);
    let detail = match copy {
        Some(c) => body_text(c, "release_name").or_else(|| body_text(c, "relative_path")),
        None if e.subject_type == "download" => body_text(b, "name"),
        None => body_text(b, "library").map(|l| format!("Added to {l}")),
    };
    Some(HistoryItem {
        at: e.at,
        event: event.to_string(),
        episode,
        // The source in the product's words (`WEB-DL`); a quality name that does not read
        // keeps its own first part.
        source: quality.as_deref().and_then(|q| {
            labels::source_of_quality(q)
                .map(str::to_string)
                .or_else(|| {
                    q.split('-')
                        .next()
                        .map(str::to_string)
                        .filter(|s| !s.is_empty() && !s.ends_with('p'))
                })
        }),
        quality,
        from,
        group: copy.and_then(|c| body_text(c, "group")),
        detail,
    })
}

/// The name of an integration, by id.
fn integration_name(config: &Config, id: &str) -> String {
    config
        .integration(id)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| id.to_string())
}

/// Where one download of a title stands on the title's page, in agreement with Activity
/// (`labels::queue_stage`): `importing` (done, for its manager to take), `failed` (a usenet
/// download its client gave up on), `paused` or `queued` (finer words where Activity reads
/// downloading), else `downloading`. A torrent is done once all of it is there. A usenet
/// download is done once its client completed it. A client checking or unpacking a download
/// is still at work on it.
pub fn title_queue_stage(d: &Download) -> &'static str {
    if crate::lifecycle::downloads::complete(d) {
        return "importing";
    }
    match d.status.as_deref() {
        Some("failed") if d.usenet() => "failed",
        Some("paused") => "paused",
        Some("queued") => "queued",
        Some(_) => "downloading",
        // A row stored before the product's status was kept: by qBittorrent's word for it.
        None => {
            let state = d.state.as_str();
            if state.starts_with("paused") || state.starts_with("stopped") {
                "paused"
            } else if state.starts_with("queued") || state == "metaDL" || state == "checkingDL" {
                "queued"
            } else {
                "downloading"
            }
        }
    }
}

/// One download that is not done, as a line of the title's queue.
fn queue_item(d: &Download, kind: &str, config: &Config) -> QueueItem {
    let stage = title_queue_stage(d);
    let progress = (d.progress.clamp(0.0, 1.0) * 1000.0).round() / 10.0;
    QueueItem {
        id: d.hash.clone(),
        kind: kind.to_string(),
        release: d.name.clone(),
        stage: stage.to_string(),
        progress,
        size: d.size_bytes,
        done: (d.size_bytes as f64 * d.progress.clamp(0.0, 1.0)) as i64,
        added: d.added_at,
        manager: d.owner.as_deref().map(|o| integration_name(config, o)),
        client: (!d.client.is_empty()).then(|| integration_name(config, &d.client)),
        attention: d.stuck,
        reason: d.stuck_reason.clone(),
    }
}

/// One request of a title, as its page lists it.
fn title_request(r: &requests_store::TitleRequestRow, kind: TitleKind) -> TitleRequest {
    let mut seasons = r.seasons.clone();
    seasons.sort_unstable();
    seasons.dedup();
    let what = match kind {
        TitleKind::Movie => "movie",
        TitleKind::Show if seasons.is_empty() => "show",
        TitleKind::Show => "seasons",
    };
    TitleRequest {
        id: format!("r-{}", r.id),
        by: Requester {
            id: r.requested_by_id.map(|i| format!("user-{i}")),
            name: r.requested_by.clone(),
        },
        what: RequestWhat {
            kind: what.to_string(),
            seasons,
        },
        at: r.requested_at,
        status: r.status.clone(),
        target: if r.is_4k { "4K" } else { "HD" }.to_string(),
    }
}

// Handlers ---------------------------------------------------------------------------------------

/// A rich title record: facts, ratings, release dates, cast and crew (up to 40 each, with the
/// totals), videos, keywords, the collection, where to watch, recommendations and similar
/// titles, with what the library has (files, seasons, plays, the media server's item), the title's
/// history, its downloads, its requests, and what the processing services ran on its files.
#[utoipa::path(get, path = "/api/v1/metadata/titles/{kind}/{tmdb}", tag = "metadata",
    params(("kind" = String, Path, description = "`movie` or `show`"), ("tmdb" = i64, Path, description = "The TMDB id")),
    responses((status = 200, body = TitleRecord), (status = 404, body = ProblemDoc), (status = 502, description = "TMDB did not answer and nothing is stored", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title_record(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath((kind, tmdb)): ApiPath<(String, i64)>,
) -> ApiResult<TitleRecord> {
    let kind = parse_kind(&kind)?;
    let pool = &state.pool;
    let mut r = state.metadata.records(pool).title(kind, tmdb).await?;
    let mut lists = [&mut r.recommendations, &mut r.similar];
    mark(pool, &mut lists).await?;
    let key = (kind.as_str().to_string(), tmdb);
    let card_state = card_states(pool, std::slice::from_ref(&key))
        .await?
        .get(&key)
        .copied();
    r.requests = requests_store::title_requests(pool, kind.as_str(), tmdb)
        .await?
        .iter()
        .map(|q| title_request(q, kind))
        .collect();
    let catalog_id = catalog_title(pool, kind, tmdb, r.tvdb).await?;
    if let Some(id) = catalog_id {
        let detail = catalog::title(pool, &state.lane_slots(), id).await?;
        let libs = Libraries::load(pool).await?;
        let server = catalog_store::title_server(pool, id).await?;
        let word = held_state(&detail, card_state);
        let mut library = library_facts(
            &detail,
            &word,
            &state.policy.current().policy,
            &libs,
            &server,
            &state.engines.media_servers,
        );
        // Each file with what processing did or does to it.
        if let Some(editions) = library.editions.as_mut() {
            let marks = FileMarks::of(&state, id).await?;
            for file in editions.iter_mut().flat_map(|e| e.files.iter_mut()) {
                file.processing = marks.of_file(&file.file_name);
            }
        }
        r.library = Some(library);
        if let Some(first) = server.first() {
            r.ratings.rt = rotten_tomatoes(first);
        }
        if let Some(imdb) = detail.about.imdb_rating {
            r.ratings.imdb = Some(Score {
                score: imdb,
                votes: None,
            });
        }
        if let Some(seasons) = r.seasons.as_mut() {
            let held = catalog_store::held_episodes(pool, id).await?;
            for s in seasons.iter_mut() {
                s.in_library = held
                    .iter()
                    .filter(|(season, _)| *season == s.number)
                    .map(|(_, e)| *e)
                    .collect();
                s.have = i32::try_from(s.in_library.len()).unwrap_or(i32::MAX);
                if let Some(c) = detail.seasons.iter().find(|h| h.season_number == s.number) {
                    s.have = s.have.max(c.episode_file_count);
                    s.total = s.total.max(c.total_episode_count);
                }
                s.aired = s.aired.max(s.have).min(s.total.max(s.have));
            }
        }
        let events = state
            .events(&EventQuery {
                title: Some(id),
                kind: Some(HISTORY_KINDS.join(",")),
                per_page: Some(HISTORY_MAX),
                ..EventQuery::default()
            })
            .await?;
        r.history = events
            .items
            .iter()
            .filter_map(history_item)
            .take(HISTORY_MAX as usize)
            .collect();
        r.queue = downloads_store::title_queue(pool, id)
            .await?
            .iter()
            .map(|d| queue_item(d, kind.as_str(), &state.config))
            .collect();
    }
    r.processing = title_processing(&state, catalog_id).await?;
    r.plays = super::stats::title_plays(&state, &caller, catalog_id).await?;
    r.state = match &r.library {
        Some(l) => Some(l.state.clone()),
        None => card_state.map(str::to_string),
    };
    Ok(Json(r))
}

/// Everything of a title's cast and crew: the cast in billing order (a show's over all its
/// seasons, with episode counts), the crew with its departments, the key jobs first.
#[utoipa::path(get, path = "/api/v1/metadata/titles/{kind}/{tmdb}/credits", tag = "metadata",
    params(("kind" = String, Path, description = "`movie` or `show`"), ("tmdb" = i64, Path, description = "The TMDB id")),
    responses((status = 200, body = TitleCredits), (status = 404, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title_credits(
    State(state): State<AppState>,
    ApiPath((kind, tmdb)): ApiPath<(String, i64)>,
) -> ApiResult<TitleCredits> {
    let kind = parse_kind(&kind)?;
    Ok(Json(
        state
            .metadata
            .records(&state.pool)
            .credits(kind, tmdb)
            .await?,
    ))
}

/// The episodes of one season with air dates and stills, each with its library state and file.
#[utoipa::path(get, path = "/api/v1/metadata/shows/{tmdb}/seasons/{n}", tag = "metadata",
    params(("tmdb" = i64, Path, description = "The show's TMDB id"), ("n" = i32, Path, description = "The season number")),
    responses((status = 200, body = SeasonRecord), (status = 404, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn season_record(
    State(state): State<AppState>,
    ApiPath((tmdb, n)): ApiPath<(i64, i32)>,
) -> ApiResult<SeasonRecord> {
    let mut s = state.metadata.records(&state.pool).season(tmdb, n).await?;
    if let Some(id) = catalog_title(&state.pool, TitleKind::Show, tmdb, None).await? {
        let detail = catalog::title(&state.pool, &state.lane_slots(), id).await?;
        let libs = Libraries::load(&state.pool).await?;
        let copies = catalog::title_copies(
            &state.pool,
            id,
            &CopyQuery {
                season: Some(n),
                per_page: Some(200),
                ..CopyQuery::default()
            },
        )
        .await?
        .items;
        let marks = FileMarks::of(&state, id).await?;
        for ep in s.episodes.iter_mut() {
            if let Some(c) = copies
                .iter()
                .find(|c| c.season_number == Some(n) && c.episode_numbers.contains(&ep.number))
            {
                let file = library_file(c, &state.policy.current().policy, &detail, &libs, None);
                let processing = marks.of_file(&file.file_name);
                ep.state = "available".to_string();
                ep.file = Some(EpisodeFile {
                    quality: file.quality,
                    resolution: file.resolution,
                    source: file.source,
                    codec: file.codec,
                    hdr: file.hdr,
                    audio: file.audio,
                    size: file.size,
                    group: file.group,
                    score: file.score,
                    added: file.added,
                    file_name: file.file_name,
                    formats: file.formats,
                    cutoff_met: file.cutoff_met,
                    processing,
                });
            }
        }
        s.have = i32::try_from(s.episodes.iter().filter(|e| e.file.is_some()).count())
            .unwrap_or(i32::MAX);
    }
    Ok(Json(s))
}

/// A person and their credits, each a card with its library state, the most popular first.
#[utoipa::path(get, path = "/api/v1/metadata/people/{id}", tag = "metadata",
    params(("id" = String, Path, description = "`person-<tmdb>` or the TMDB id")),
    responses((status = 200, body = PersonRecord), (status = 404, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn person_record(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<PersonRecord> {
    let tmdb = parse_id(&id, "person")?;
    let mut p = state.metadata.records(&state.pool).person(tmdb).await?;
    let mut lists = [&mut p.credits.cast, &mut p.credits.crew];
    mark(&state.pool, &mut lists).await?;
    Ok(Json(p))
}

/// A collection's parts in release order as cards with their library state.
#[utoipa::path(get, path = "/api/v1/metadata/collections/{id}", tag = "metadata",
    params(("id" = String, Path, description = "`collection-<tmdb>` or the TMDB id")),
    responses((status = 200, body = CollectionRecord), (status = 404, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn collection_one(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<CollectionRecord> {
    let tmdb = parse_id(&id, "collection")?;
    let mut c = state.metadata.records(&state.pool).collection(tmdb).await?;
    let mut lists = [&mut c.parts];
    mark(&state.pool, &mut lists).await?;
    Ok(Json(c))
}

/// The collections the library has a part of, by id (`collection-<tmdb>`), each with its
/// parts and their library state. A collection whose TMDB record is not stored yet lists the
/// library's own parts with `rich` false; the records are fetched in the background, a few at
/// a time, so a later answer is complete.
#[utoipa::path(get, path = "/api/v1/collections", tag = "metadata",
    responses((status = 200, description = "Collections by id", body = BTreeMap<String, CollectionRecord>), (status = 401, body = ProblemDoc)))]
async fn collections(State(state): State<AppState>) -> ApiResult<CollectionMap> {
    let members = catalog_store::collection_members(&state.pool).await?;
    let mut by_id: BTreeMap<i64, Vec<&catalog_store::CollectionMember>> = BTreeMap::new();
    for m in &members {
        by_id.entry(m.collection_tmdb_id).or_default().push(m);
    }
    let paths: Vec<String> = by_id.keys().map(|id| collection_path(*id)).collect();
    let stored = state.metadata.records(&state.pool).stored(&paths).await?;
    let mut out = CollectionMap::new();
    let mut missing: Vec<String> = Vec::new();
    for (id, parts) in &by_id {
        let path = collection_path(*id);
        let record = match stored.get(&path) {
            Some(Some(raw)) => collection_record(raw),
            found => {
                if found.is_none() {
                    missing.push(path);
                }
                CollectionRecord {
                    id: format!("collection-{id}"),
                    name: parts
                        .iter()
                        .find_map(|p| p.collection.clone())
                        .unwrap_or_else(|| format!("Collection {id}")),
                    overview: None,
                    poster: None,
                    backdrop: None,
                    parts: parts
                        .iter()
                        .map(|p| Card {
                            id: title_id(TitleKind::Movie, p.tmdb_id),
                            kind: TitleKind::Movie.as_str().to_string(),
                            title: p.title.clone(),
                            year: p.year,
                            poster: tmdb_poster(p.poster_url.as_deref())
                                .or_else(|| p.poster_url.clone()),
                            rating: p.rating,
                            state: None,
                            role: None,
                            job: None,
                        })
                        .collect(),
                    rich: false,
                }
            }
        };
        out.insert(record.id.clone(), record);
    }
    let mut lists: Vec<&mut Vec<Card>> = out.values_mut().map(|c| &mut c.parts).collect();
    mark(&state.pool, &mut lists).await?;
    if !missing.is_empty() && state.metadata.tmdb.info().configured {
        let st = state.clone();
        tokio::spawn(async move {
            st.metadata
                .records(&st.pool)
                .warm(&missing, RECORD_TTL, WARM_PER_CALL)
                .await;
        });
    }
    Ok(Json(out))
}

#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct BrowseQuery {
    /// 1 to 500.
    pub page: Option<u32>,
}

/// The key of the recent requests row and its list.
const RECENT_REQUESTS: &str = "recent-requests";
/// Studios and networks on Discover.
const TILES_MAX: usize = 13;

/// The titles asked for most recently, as cards.
async fn recent_requests(pool: &sqlx::PgPool, limit: i64) -> AppResult<Vec<Card>> {
    Ok(requests_store::recent_titles(pool, limit)
        .await?
        .into_iter()
        .filter_map(|r| {
            let kind = TitleKind::parse(&r.kind)?;
            Some(Card {
                id: title_id(kind, r.tmdb_id),
                kind: r.kind,
                title: r.title.unwrap_or_else(|| format!("TMDB {}", r.tmdb_id)),
                year: r.year,
                poster: r.poster_path.filter(|p| p.starts_with('/')),
                rating: None,
                state: None,
                role: None,
                job: None,
            })
        })
        .collect())
}

/// The studios (`studio`) or networks (`network`) the library has most titles of, named as
/// the stored TMDB answers name them, so each has TMDB's id, its logo and its list. A name no
/// stored answer has is left out until a title of it is opened.
async fn library_tiles(pool: &sqlx::PgPool, field: &str) -> AppResult<Vec<BrowseTile>> {
    let counts = catalog_store::name_counts(pool, field).await?;
    let names: Vec<String> = counts.iter().map(|(n, _)| n.clone()).collect();
    let (source, prefix) = match field {
        "network" => ("networks", "network"),
        _ => ("production_companies", "studio"),
    };
    let known = metadata_store::companies_named(pool, source, &names).await?;
    Ok(counts
        .iter()
        .filter_map(|(name, _)| {
            let lower = name.to_lowercase();
            let (id, tmdb_name, logo) = known.iter().find(|(_, n, _)| n.to_lowercase() == lower)?;
            Some(BrowseTile {
                id: *id,
                key: format!("{prefix}-{id}"),
                name: tmdb_name.clone(),
                backdrop: None,
                logo: logo.clone().filter(|l| l.starts_with('/')),
            })
        })
        .take(TILES_MAX)
        .collect())
}

/// Discover: the billboard (trending titles with their backdrop, logo and facts), the rows
/// (trending, popular movies, upcoming movies, popular shows, on the air, recent requests),
/// the genres with a backdrop each, and the studios and networks the library has most titles
/// of. Every card and billboard title carries its library state. Without TMDB only the
/// library's parts are there.
#[utoipa::path(get, path = "/api/v1/discover", tag = "metadata",
    responses((status = 200, body = DiscoverPage), (status = 401, body = ProblemDoc)))]
async fn discover(State(state): State<AppState>) -> ApiResult<DiscoverPage> {
    let pool = &state.pool;
    let mut page = state.metadata.records(pool).discover().await;
    let recent = recent_requests(pool, 20).await?;
    if !recent.is_empty() {
        page.rows.push(DiscoverRow {
            id: RECENT_REQUESTS.to_string(),
            title: "Recent requests".to_string(),
            items: recent,
        });
    }
    page.studios = library_tiles(pool, "studio").await?;
    page.networks = library_tiles(pool, "network").await?;
    let mut lists: Vec<&mut Vec<Card>> = page.rows.iter_mut().map(|r| &mut r.items).collect();
    mark(pool, &mut lists).await?;
    let keys: Vec<(String, i64)> = page
        .billboard
        .iter()
        .filter_map(|b| {
            let (kind, id) = b.id.split_once('-')?;
            Some((kind.to_string(), id.parse().ok()?))
        })
        .collect();
    let states = card_states(pool, &keys).await?;
    for (b, key) in page.billboard.iter_mut().zip(&keys) {
        b.state = states.get(key).map(|s| s.to_string());
    }
    Ok(Json(page))
}

/// A browse list: `trending`, `popular-movies`, `upcoming-movies`, `popular-shows`,
/// `on-the-air`, `recent-requests`, `genre-movie-<id>`, `genre-show-<id>`, `studio-<id>`,
/// `network-<id>`.
#[utoipa::path(get, path = "/api/v1/discover/{list}", tag = "metadata",
    params(("list" = String, Path, description = "The list's key"), BrowseQuery),
    responses((status = 200, body = BrowseList), (status = 400, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn browse_list(
    State(state): State<AppState>,
    ApiPath(list): ApiPath<String>,
    ApiQuery(q): ApiQuery<BrowseQuery>,
) -> ApiResult<BrowseList> {
    if list.trim().eq_ignore_ascii_case(RECENT_REQUESTS) {
        let mut items = recent_requests(&state.pool, 100).await?;
        let mut lists = [&mut items];
        mark(&state.pool, &mut lists).await?;
        return Ok(Json(BrowseList {
            list: RECENT_REQUESTS.to_string(),
            title: "Recent requests".to_string(),
            kind: "all".to_string(),
            items,
            page: 1,
            total_pages: 1,
        }));
    }
    let key = ListKey::parse(&list).ok_or_else(|| {
        AppError::bad_request(format!(
            "`{list}` is not a list: trending, popular-movies, upcoming-movies, popular-shows, on-the-air, recent-requests, genre-movie-<id>, genre-show-<id>, studio-<id> or network-<id>"
        ))
    })?;
    let mut b = state
        .metadata
        .records(&state.pool)
        .browse(key, q.page.unwrap_or(1))
        .await?;
    let mut lists = [&mut b.items];
    mark(&state.pool, &mut lists).await?;
    Ok(Json(b))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/metadata/titles/{kind}/{tmdb}", get(title_record))
        .route("/metadata/titles/{kind}/{tmdb}/credits", get(title_credits))
        .route("/metadata/shows/{tmdb}/seasons/{n}", get(season_record))
        .route("/metadata/people/{id}", get(person_record))
        .route("/metadata/collections/{id}", get(collection_one))
        .route("/collections", get(collections))
        .route("/discover", get(discover))
        .route("/discover/{list}", get(browse_list))
}

#[derive(OpenApi)]
#[openapi(paths(
    title_record,
    title_credits,
    season_record,
    person_record,
    collection_one,
    collections,
    discover,
    browse_list
))]
struct MetadataDoc;

/// The metadata service's part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    MetadataDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::DownloadStatus;
    use chrono::Utc;

    fn in_queue(protocol: &str, state: &str, status: Option<&str>, progress: f64) -> Download {
        Download {
            hash: "A1".to_string(),
            client: "client".to_string(),
            protocol: protocol.to_string(),
            item_id: None,
            name: "Release".to_string(),
            category: None,
            state: state.to_string(),
            status: status.map(str::to_string),
            progress,
            size_bytes: 1,
            added_at: None,
            completed_at: None,
            seeding_seconds: 0,
            save_path: None,
            owner: Some("radarr".to_string()),
            owner_source: Some("queue".to_string()),
            title_id: Some(1),
            title: None,
            queue_status: None,
            queue_state: Some("downloading".to_string()),
            queue_health: None,
            queue_messages: serde_json::json!([]),
            stuck: false,
            stuck_reason: None,
            orphan: false,
            orphan_reason: None,
            first_seen_at: Utc::now(),
            updated_at: Utc::now(),
            speed_bytes: None,
            eta_seconds: None,
            seeds: None,
            peers: None,
            indexer: None,
        }
    }

    /// The title page reads a qBittorrent download as it read it before other clients came,
    /// by qBittorrent's state. For every client it agrees with Activity: a word of its own
    /// (paused, queued) only where Activity reads downloading.
    #[test]
    fn the_title_page_reads_a_download_as_activity_does() {
        fn before(state: &str, progress: f64) -> &'static str {
            if progress >= 1.0 {
                "importing"
            } else if state.starts_with("paused") || state.starts_with("stopped") {
                "paused"
            } else if state.starts_with("queued") || state == "metaDL" || state == "checkingDL" {
                "queued"
            } else {
                "downloading"
            }
        }
        for state in [
            "downloading",
            "forcedDL",
            "forcedMetaDL",
            "metaDL",
            "queuedDL",
            "queuedUP",
            "stalledDL",
            "pausedDL",
            "stoppedDL",
            "pausedUP",
            "stoppedUP",
            "uploading",
            "forcedUP",
            "stalledUP",
            "checkingDL",
            "checkingUP",
            "checkingResumeData",
            "moving",
            "error",
            "missingFiles",
            "unknown",
        ] {
            for progress in [0.0, 0.5, 1.0] {
                let status = crate::engines::qbit::status_of(state, progress);
                let row = in_queue("torrent", state, Some(status.as_str()), progress);
                let stored_before = in_queue("torrent", state, None, progress);
                for d in [row, stored_before] {
                    assert_eq!(
                        title_queue_stage(&d),
                        before(state, progress),
                        "{state} at {progress}, status {:?}",
                        d.status
                    );
                }
            }
        }
        for status in DownloadStatus::ALL {
            for (protocol, progress) in [
                ("torrent", 0.5),
                ("torrent", 1.0),
                ("usenet", 0.5),
                ("usenet", 1.0),
            ] {
                let d = in_queue(protocol, "word", Some(status.as_str()), progress);
                let title = title_queue_stage(&d);
                let activity =
                    labels::queue_stage(false, progress, true, d.usenet(), Some(status.as_str()));
                assert!(
                    title == activity
                        || (activity == "downloading" && matches!(title, "paused" | "queued")),
                    "{protocol} {status:?} at {progress}: {title} here, {activity} on Activity"
                );
            }
        }
    }

    #[test]
    fn ids_kinds_runtimes_and_posters_are_read() {
        assert_eq!(parse_id("person-6384", "person").unwrap(), 6384);
        assert_eq!(parse_id("6384", "person").unwrap(), 6384);
        assert!(parse_id("collection-x", "collection").is_err());
        assert!(parse_id("person-0", "person").is_err());
        assert_eq!(parse_kind("tv").unwrap(), TitleKind::Show);
        assert!(parse_kind("music").is_err());
        assert_eq!(runtime_minutes("1:52:03"), Some(112));
        assert_eq!(runtime_minutes("52:03"), Some(52));
        assert_eq!(runtime_minutes("x"), None);
        assert_eq!(
            tmdb_poster(Some("https://image.tmdb.org/t/p/original/abc.jpg")).as_deref(),
            Some("/abc.jpg")
        );
        assert_eq!(tmdb_poster(Some("/MediaCover/1/poster.jpg")), None);
        assert_eq!(lines("1080p"), Some(1080));
        assert_eq!(lines("2160p"), Some(2160));
        assert_eq!(lines("4K"), Some(2160));
        assert_eq!(lines("SD"), None);
        assert_eq!(
            root_name("/media/movies/John Wick (2014)").as_deref(),
            Some("movies")
        );
        assert_eq!(
            root_name("/4kmovies/X (2020)/").as_deref(),
            Some("4kmovies")
        );
    }

    fn library(
        id: &str,
        name: &str,
        section: Option<&str>,
        managed_by: Option<&str>,
        locations: &[&str],
    ) -> LibraryName {
        LibraryName {
            id: id.to_string(),
            name: name.to_string(),
            kind: Some("movie".to_string()),
            integration: Some(if section.is_some() { "plex" } else { id }.to_string()),
            section_key: section.map(str::to_string),
            managed_by: managed_by.map(str::to_string),
            machine_id: section.map(|_| "abc123".to_string()),
            locations: locations.iter().map(|l| l.to_string()).collect(),
        }
    }

    fn server_row(key: &str, library: &str) -> TitleServerRow {
        TitleServerRow {
            server: "plex".into(),
            rating_key: key.into(),
            library_id: Some(library.into()),
            critic_rating: None,
            critic_image: None,
            audience_rating: None,
            audience_image: None,
            plays: 2,
            last_played: Some(Utc::now()),
            edition: None,
        }
    }

    #[test]
    fn a_file_is_placed_in_the_section_that_holds_its_folder() {
        let libs = Libraries(vec![
            library("radarr", "Radarr", None, None, &[]),
            library("plex-1", "Movies", Some("1"), Some("radarr"), &["/movies"]),
            library(
                "plex-2",
                "4K Movies",
                Some("2"),
                Some("radarr"),
                &["/4kmovies"],
            ),
        ]);
        let place = |path: &str| libs.place("radarr", Some(path), None);
        assert_eq!(
            place("/media/movies/Heat (1995)"),
            ("plex-1".to_string(), "Movies".to_string())
        );
        assert_eq!(place("/media/4kmovies/Heat (1995)").1, "4K Movies");
        // A folder no section names: the section the server matched the title in.
        let row = server_row("77", "plex-2");
        assert_eq!(
            libs.place("radarr", Some("/elsewhere/Heat (1995)"), Some(&row))
                .1,
            "4K Movies"
        );
        // Nothing matches: the manager's library.
        assert_eq!(
            libs.place("radarr", Some("/elsewhere/Heat (1995)"), None).1,
            "Radarr"
        );
        // The item's link comes from its server's adapter: Plex's web app, by machine id.
        let plex = crate::engines::plex::Plex::new(
            &crate::core::config::MediaServerConfig {
                id: "plex".into(),
                kind: crate::core::config::MediaServerKind::Plex,
                url: Some("http://10.0.0.30:32400".into()),
                secret: Some("token".into()),
                web_url: None,
            },
            crate::core::outbound::Outbound::with_transport(
                crate::core::outbound::Allowlist::default(),
                crate::core::outbound::recorded::Recorded::new(),
            ),
        )
        .unwrap()
        .with_web("https://app.plex.tv/desktop");
        let servers: Vec<Arc<dyn MediaServer>> = vec![Arc::new(plex)];
        let link = libs.server_link(&row, &servers).unwrap();
        assert_eq!(link.key, "/library/metadata/77");
        assert_eq!(
            link.url,
            "https://app.plex.tv/desktop/#!/server/abc123/details?key=%2Flibrary%2Fmetadata%2F77"
        );
        let unknown = server_row("78", "radarr");
        assert!(libs.server_link(&unknown, &servers).is_none());
        assert!(libs.server_link(&row, &[]).is_none());
    }

    #[test]
    fn journal_events_read_as_a_title_history() {
        let event = |kind: &str, subject: &str, body: Value| Event {
            id: 1,
            at: Utc::now(),
            kind: kind.to_string(),
            subject_type: subject.to_string(),
            subject_id: "1".to_string(),
            actor: "sync:sonarr".to_string(),
            cause: None,
            body,
        };
        let upgrade = history_item(&event(
            "copy_replaced",
            "copy",
            serde_json::json!({
                "season": 2, "episodes": [5],
                "old": {"arr_quality": "HDTV-720p"},
                "new": {"arr_quality": "WEBDL-1080p", "group": "NTb", "release_name": "Show.S02E05.1080p.WEB-DL-NTb"}
            }),
        ))
        .unwrap();
        assert_eq!(upgrade.event, "upgraded");
        assert_eq!(
            upgrade.episode,
            Some(EpisodeRef {
                season: 2,
                number: 5
            })
        );
        assert_eq!(upgrade.quality.as_deref(), Some("WEBDL-1080p"));
        assert_eq!(upgrade.from.as_deref(), Some("HDTV-720p"));
        assert_eq!(upgrade.source.as_deref(), Some("WEB-DL"));
        assert_eq!(upgrade.group.as_deref(), Some("NTb"));
        let grab = history_item(&event(
            "download_added",
            "download",
            serde_json::json!({"name": "Show.S02E06.1080p-GRP"}),
        ))
        .unwrap();
        assert_eq!(
            (grab.event.as_str(), grab.detail.as_deref()),
            ("grabbed", Some("Show.S02E06.1080p-GRP"))
        );
        assert!(history_item(&event("score_changed", "copy", Value::Null)).is_none());
    }

    #[test]
    fn rotten_tomatoes_is_read_from_its_image_keys() {
        let row = |critic: Option<f64>, image: Option<&str>| catalog_store::TitleServerRow {
            server: "plex".into(),
            rating_key: "1".into(),
            library_id: None,
            critic_rating: critic,
            critic_image: image.map(str::to_string),
            audience_rating: Some(7.1),
            audience_image: Some("rottentomatoes://image.rating.upright".into()),
            plays: 0,
            last_played: None,
            edition: None,
        };
        let rt = rotten_tomatoes(&row(
            Some(5.7),
            Some("rottentomatoes://image.rating.rotten"),
        ))
        .unwrap();
        assert_eq!((rt.critics, rt.audience), (Some(57), Some(71)));
        assert_eq!(
            rt.critics_image.as_deref(),
            Some("rottentomatoes://image.rating.rotten")
        );
        // A rating from another source is not Rotten Tomatoes'.
        let other = rotten_tomatoes(&row(Some(8.0), Some("imdb://image.rating"))).unwrap();
        assert_eq!((other.critics, other.audience), (None, Some(71)));
    }

    #[test]
    fn the_routes_are_in_the_openapi_document() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        let paths = doc["paths"].as_object().unwrap();
        for path in [
            "/api/v1/metadata/titles/{kind}/{tmdb}",
            "/api/v1/metadata/titles/{kind}/{tmdb}/credits",
            "/api/v1/metadata/shows/{tmdb}/seasons/{n}",
            "/api/v1/metadata/people/{id}",
            "/api/v1/metadata/collections/{id}",
            "/api/v1/collections",
            "/api/v1/discover",
            "/api/v1/discover/{list}",
            "/api/v1/search",
            "/api/v1/search/status",
            "/api/v1/music/albums/{id}",
            "/api/v1/music/artists/{id}",
            "/api/v1/library/videos",
            "/api/v1/library/videos/{id}/still",
            "/api/v1/server/sessions",
            "/api/v1/music",
            "/api/v1/music/artists/{id}/photo",
        ] {
            assert!(paths.contains_key(path), "{path} is missing");
        }
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for name in [
            "TitleRecord",
            "TitleCredits",
            "SeasonRecord",
            "PersonRecord",
            "CollectionRecord",
            "Card",
            "BrowseList",
            "HistoryItem",
            "QueueItem",
            "TitleRequest",
            "DiscoverPage",
            "SearchStatus",
            "AlbumRecord",
            "Videos",
        ] {
            assert!(schemas.contains_key(name), "schema {name} is missing");
        }
    }
}
