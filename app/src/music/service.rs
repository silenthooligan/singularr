//! What the API asks about the music library, the scan that keeps it, and the cover files.

use super::model::{
    AlbumDetail, AlbumPage, AlbumProblem, AlbumQuery, AlbumSummary, ArtistDetail, ArtistQuery,
    ArtistSummary, Track,
};
use super::scan::{self, AUDIO, CoverError, MusicTree, ScannedAlbum};
use super::store::{self, AlbumFilter, ArtistFilter};
use crate::core::error::{AppError, AppResult};
use crate::core::journal::{Journal, event};
use crate::core::model::{FolderScan, Page, Paging, ScanReport};
use crate::core::observe;
use crate::core::runtime::{self, Cx, Done, Failed, Handler};
use crate::core::units;
use async_trait::async_trait;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

pub const FOLDER: &str = "music";

/// The music folder and the lock that keeps two scans from running at once.
pub struct MusicLibrary {
    pub root: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl MusicLibrary {
    pub fn new(root: &str) -> Self {
        MusicLibrary {
            root: PathBuf::from(root),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn root_text(&self) -> String {
        self.root.display().to_string()
    }
}

fn words(q: Option<&str>) -> Vec<String> {
    q.unwrap_or("")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn format_filter(format: Option<&str>, mixed: bool) -> AppResult<Option<String>> {
    let Some(format) = format
        .map(|f| f.trim().to_ascii_lowercase())
        .filter(|f| !f.is_empty())
    else {
        return Ok(None);
    };
    if AUDIO.contains(&format.as_str()) || (mixed && format == "mixed") {
        Ok(Some(format))
    } else {
        Err(AppError::bad_request(format!(
            "format is one of {}{}, not `{format}`",
            AUDIO.join(", "),
            if mixed { ", mixed" } else { "" }
        )))
    }
}

pub async fn artists(pool: &PgPool, q: &ArtistQuery) -> AppResult<Page<ArtistSummary>> {
    let f = ArtistFilter {
        words: words(q.q.as_deref()),
        format: format_filter(q.format.as_deref(), false)?,
    };
    let paging = Paging::new(q.page, q.per_page);
    let (items, total) = store::artists(pool, &f, paging).await?;
    Ok(Page::new(items, paging, total))
}

/// What wants attention in an album folder.
pub fn problems(a: &AlbumSummary) -> Vec<AlbumProblem> {
    let mut out = Vec::new();
    let mut add = |kind: &str, text: String| {
        out.push(AlbumProblem {
            album_id: a.id,
            album: a.name.clone(),
            kind: kind.to_string(),
            text,
        })
    };
    if a.track_count == 0 {
        add("empty", "No audio files".to_string());
    }
    if !a.has_cover {
        add("missing_cover", "No cover image".to_string());
    }
    if a.year.is_none() {
        let text = if a.folder.trim_end().ends_with("(0000)") {
            "Year 0000 in folder name"
        } else {
            "No year in folder name"
        };
        add("no_year", text.to_string());
    }
    if a.duplicate {
        add("duplicate", "Duplicate album name".to_string());
    }
    if a.formats.len() > 1 {
        add(
            "mixed_formats",
            format!("Mixed formats: {}", a.formats.join(", ")),
        );
    }
    out
}

async fn details(pool: &PgPool, albums: Vec<AlbumSummary>) -> AppResult<Vec<AlbumDetail>> {
    let ids: Vec<i64> = albums.iter().map(|a| a.id).collect();
    let mut tracks: HashMap<i64, Vec<Track>> = HashMap::new();
    for row in store::tracks(pool, &ids).await? {
        tracks.entry(row.album_id).or_default().push(row.track);
    }
    let mut out = Vec::with_capacity(albums.len());
    for album in albums {
        let album = album.finish();
        let duplicates = if album.duplicate {
            store::duplicates_of(pool, album.id).await?
        } else {
            Vec::new()
        };
        out.push(AlbumDetail {
            tracks: tracks.remove(&album.id).unwrap_or_default(),
            problems: problems(&album),
            duplicates,
            album,
        });
    }
    Ok(out)
}

pub async fn artist(pool: &PgPool, id: i64) -> AppResult<ArtistDetail> {
    let artist = store::artist(pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no artist {id}.")))?;
    let albums = details(pool, store::artist_albums(pool, id).await?).await?;
    let problems = albums
        .iter()
        .flat_map(|a| a.problems.iter().cloned())
        .collect();
    Ok(ArtistDetail {
        artist,
        albums,
        problems,
    })
}

pub async fn albums(pool: &PgPool, q: &AlbumQuery) -> AppResult<AlbumPage> {
    let f = AlbumFilter {
        format: format_filter(q.format.as_deref(), true)?,
        missing_cover: q.missing_cover,
        no_year: q.no_year,
        duplicates: q.duplicates,
        artist: q.artist,
        words: words(q.q.as_deref()),
    };
    let sort = q
        .sort
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("artist");
    let (name, reversed) = match sort.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (sort, false),
    };
    // Names and the artist read A to Z; the numbers and the date read largest first.
    let natural_desc = matches!(name, "year" | "size" | "tracks" | "added");
    let order = store::album_order(name, natural_desc != reversed).ok_or_else(|| {
        AppError::bad_request(format!(
            "sort is one of artist, name, year, size, tracks, added, not `{sort}`"
        ))
    })?;
    let paging = Paging::new(q.page, q.per_page);
    let (items, total) = store::albums(pool, &f, order, paging).await?;
    Ok(AlbumPage {
        items: items.into_iter().map(AlbumSummary::finish).collect(),
        page: paging.page,
        per_page: paging.per_page,
        total,
        counts: store::counts(pool).await?,
    })
}

pub async fn album(pool: &PgPool, id: i64) -> AppResult<AlbumDetail> {
    let album = store::album(pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no album {id}.")))?;
    details(pool, vec![album])
        .await?
        .pop()
        .ok_or_else(|| AppError::Internal("the album view was not built".to_string()))
}

/// An album's cover from disk: its bytes and content type.
pub async fn cover(
    pool: &PgPool,
    lib: &MusicLibrary,
    id: i64,
) -> AppResult<(Vec<u8>, &'static str)> {
    let rel = store::cover_path(pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no album {id}.")))?
        .ok_or_else(|| AppError::not_found(format!("Album {id} has no cover in its folder.")))?;
    let root = lib.root.clone();
    let read = tokio::task::spawn_blocking(move || scan::read_cover(&root, &rel))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    match read {
        Ok(found) => Ok(found),
        Err(CoverError::TooLarge(bytes)) => Err(AppError::NotFound(format!(
            "The cover of album {id} is {bytes} bytes, more than the {} served.",
            scan::MAX_COVER_BYTES
        ))),
        Err(CoverError::NotFound(why)) => Err(AppError::NotFound(format!(
            "The cover of album {id} cannot be served: {why}."
        ))),
    }
}

/// The state of the last scan.
pub async fn state(pool: &PgPool, lib: &MusicLibrary) -> AppResult<FolderScan> {
    let row = store::scan_state(pool).await?;
    Ok(match row {
        Some(r) => FolderScan {
            folder: FOLDER.to_string(),
            root: r.root,
            ok: r.ok,
            last_scan_at: Some(r.last_scan_at),
            last_ok_at: r.last_ok_at,
            took_ms: r.took_ms,
            error: r.error,
            counts: r.counts,
        },
        None => FolderScan {
            folder: FOLDER.to_string(),
            root: lib.root_text(),
            ok: false,
            last_scan_at: None,
            last_ok_at: None,
            took_ms: None,
            error: None,
            counts: json!({}),
        },
    })
}

/// A scan that finds nothing where albums are known, or far fewer than are known, is refused:
/// a folder that is not mounted must not read as every album removed.
fn implausible(known: usize, found: usize) -> bool {
    (found == 0 && known > 0) || (known >= 20 && found * 2 < known)
}

fn facts_hash(album: &ScannedAlbum) -> String {
    let text = serde_json::to_string(album).unwrap_or_default();
    hex::encode(Sha256::digest(text.as_bytes()))
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(
        "the folder lists {found} albums where {known} are known; the scan was not applied (is the folder mounted?)"
    )]
    Implausible { known: usize, found: usize },
}

/// Write what a scan found: new albums, albums whose files changed, and albums that are gone,
/// with an `album_added` or `album_removed` event for each new or gone one.
pub async fn apply(
    pool: &PgPool,
    journal: &Journal,
    tree: &MusicTree,
    actor: &str,
) -> Result<ScanReport, ApplyError> {
    let mut tx = journal.begin(pool).await?;
    let stored = store::stored_albums(tx.conn()).await?;
    let (known, found) = (stored.len(), tree.albums.len());
    if implausible(known, found) {
        return Err(ApplyError::Implausible { known, found });
    }
    let mut artist_ids: HashMap<&str, i64> = HashMap::new();
    for folder in &tree.artists {
        let id = store::upsert_artist(tx.conn(), folder, &scan::sort_name(folder)).await?;
        artist_ids.insert(folder.as_str(), id);
    }
    let by_key: HashMap<(&str, &str), &store::StoredAlbum> = stored
        .iter()
        .map(|s| ((s.artist_folder.as_str(), s.folder.as_str()), s))
        .collect();
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    let mut report = ScanReport {
        folder: FOLDER.to_string(),
        found: found as i64,
        ..ScanReport::default()
    };
    let mut events = Vec::new();
    for album in &tree.albums {
        let key = (album.artist_folder.as_str(), album.folder.as_str());
        seen.insert(key);
        let Some(&artist_id) = artist_ids.get(album.artist_folder.as_str()) else {
            continue;
        };
        let hash = facts_hash(album);
        match by_key.get(&key) {
            None => {
                let id = store::insert_album(tx.conn(), artist_id, album, &hash).await?;
                store::replace_tracks(tx.conn(), id, &album.tracks).await?;
                report.added += 1;
                events.push(event(
                    "album_added",
                    "album",
                    id,
                    actor,
                    json!({
                        "artist": album.artist_folder,
                        "album": album.name,
                        "folder": album.folder,
                        "year": album.year,
                        "tracks": album.tracks.len(),
                        "formats": album.formats(),
                        "cover": album.cover_path.is_some(),
                    }),
                ));
            }
            Some(s) if s.facts_hash != hash => {
                store::update_album(tx.conn(), s.id, album, &hash).await?;
                store::replace_tracks(tx.conn(), s.id, &album.tracks).await?;
                report.changed += 1;
            }
            Some(_) => {}
        }
    }
    for s in &stored {
        if seen.contains(&(s.artist_folder.as_str(), s.folder.as_str())) {
            continue;
        }
        store::delete_album(tx.conn(), s.id).await?;
        report.removed += 1;
        events.push(event(
            "album_removed",
            "album",
            s.id,
            actor,
            json!({ "artist": s.artist_folder, "album": s.name, "folder": s.folder }),
        ));
    }
    store::delete_empty_artists(tx.conn()).await?;
    report.events = events.len() as i64;
    tx.append(events).await?;
    tx.commit().await?;
    Ok(report)
}

/// Scan the folder now and apply what it holds. One scan at a time; a second caller waits for
/// the first and then scans again.
pub async fn scan(
    pool: &PgPool,
    journal: &Journal,
    lib: &MusicLibrary,
    actor: &str,
) -> AppResult<ScanReport> {
    let _one_at_a_time = lib.lock.lock().await;
    let started = Instant::now();
    let root = lib.root.clone();
    let walked = tokio::task::spawn_blocking(move || scan::scan(&root))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let outcome = match walked {
        Ok(tree) => apply(pool, journal, &tree, actor)
            .await
            .map(|r| (r, tree))
            .map_err(|e| match e {
                ApplyError::Db(e) => AppError::Db(e),
                other => AppError::Upstream(other.to_string()),
            }),
        Err(e) => Err(AppError::Upstream(e)),
    };
    let took = started.elapsed();
    let took_ms = i64::try_from(took.as_millis()).unwrap_or(i64::MAX);
    observe::sync_pass(FOLDER, "scan", took, outcome.is_ok());
    match outcome {
        Ok((mut report, tree)) => {
            report.took_ms = took_ms;
            let counts = json!({
                "artists": tree.artists.len(),
                "albums": tree.albums.len(),
                "tracks": tree.albums.iter().map(|a| a.tracks.len()).sum::<usize>(),
                "skipped": tree.skipped,
            });
            store::record_scan(pool, &lib.root_text(), true, took_ms, None, &counts).await?;
            Ok(report)
        }
        // A scan whose lease was lost commits nothing: its scan state stays the last run's.
        Err(AppError::Db(e)) if crate::core::runtime::lost(&e) => Err(AppError::Db(e)),
        Err(e) => {
            let message = e.to_string();
            if let Err(db) = store::record_scan(
                pool,
                &lib.root_text(),
                false,
                took_ms,
                Some(&message),
                &json!({}),
            )
            .await
            {
                tracing::warn!(error = %db, "the music scan state could not be recorded");
            }
            Err(e)
        }
    }
}

/// The key of the music folder's scan in the job runtime.
pub const JOB: &str = "folder:music";

/// The music folder's job: the scan, its writes through a journal fenced by the run's lease. The
/// events name the payload's actor when a person asked for the run (`{"actor": ...}`). They name
/// `scan:music` otherwise. The run's value is the scan's report.
pub struct ScanJob {
    pub pool: PgPool,
    pub journal: Journal,
    pub lib: Arc<MusicLibrary>,
}

#[async_trait]
impl Handler for ScanJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let actor = cx
            .job
            .payload
            .get("actor")
            .and_then(Value::as_str)
            .unwrap_or("scan:music")
            .to_string();
        let journal = self.journal.fenced(Arc::new(cx.lease));
        match scan(&self.pool, &journal, &self.lib, &actor).await {
            Ok(r) => {
                if r.added + r.removed + r.changed > 0 {
                    tracing::info!(
                        added = r.added,
                        removed = r.removed,
                        changed = r.changed,
                        albums = r.found,
                        "music library updated"
                    );
                }
                Ok(Done {
                    detail: format!(
                        "{}, {} added, {} removed, {} changed",
                        units::counted(r.found, "album", "albums"),
                        units::count(r.added),
                        units::count(r.removed),
                        units::count(r.changed)
                    ),
                    value: serde_json::to_value(&r).ok(),
                    again: None,
                })
            }
            Err(e) => {
                tracing::warn!(root = %self.lib.root_text(), error = %e, "music scan failed");
                Err(Failed {
                    error: e.to_string(),
                    class: Some(e.code().to_string()),
                    again: None,
                })
            }
        }
    }
}

/// Scan the folder now through its job and answer with what the run found. A run that is going
/// when asked is let end. The folder is then scanned again. The events name `actor`.
pub async fn scan_now(pool: &PgPool, actor: &str) -> AppResult<ScanReport> {
    let ended = runtime::run_and_wait(pool, JOB, json!({ "actor": actor }))
        .await?
        .ok_or_else(|| AppError::Internal("the music folder has no scan job".to_string()))?;
    if ended.ok() {
        return serde_json::from_value(ended.value.unwrap_or_default())
            .map_err(|e| AppError::Internal(format!("the music scan's report: {e}")));
    }
    let error = ended.error.unwrap_or_else(|| ended.state.clone());
    Err(match ended.class.as_deref() {
        Some("singularr.upstream") => AppError::Upstream(error),
        Some("singularr.database") => AppError::Db(sqlx::Error::Protocol(error)),
        _ => AppError::Internal(error),
    })
}
