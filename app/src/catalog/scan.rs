//! The read-only scan of the movie folders, for the files no engine tracks.
//!
//! Radarr tracks one file per movie. Other editions of the same movie may already sit in its
//! folder, and Radarr does not know them. Each movie folder an engine names is read one level
//! deep for video files; a file the engine tracks is skipped, every other video file is listed
//! in `untracked_files` with the edition its name belongs to, and the title's editions are
//! worked out again. Nothing is written to a folder, and nothing is acted on: a file on disk
//! that no engine tracks is listed only.
//!
//! Safety: a folder outside the configured roots is not read; a root that cannot be read
//! leaves every row under it as it is; a folder that cannot be read keeps its rows.

use super::store::{self, MovieFolder, UntrackedRow, UntrackedWrite};
use crate::core::journal::{Journal, NewEvent, event};
use crate::core::units;
use chrono::{DateTime, Utc};
use serde_json::json;
use singularr_core::EditionList;
use singularr_core::naming::parse_library_name;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

/// Video file extensions, lower case.
pub const VIDEO: &[&str] = &[
    "mkv", "mp4", "m4v", "avi", "m2ts", "ts", "wmv", "mov", "webm", "mpg", "mpeg", "iso",
];

/// Plex's local extras suffixes and samples: files that are not a cut of the movie.
const EXTRAS: &[&str] = &[
    "-trailer",
    "-featurette",
    "-behindthescenes",
    "-deleted",
    "-interview",
    "-scene",
    "-short",
    "-other",
    "-sample",
    ".sample",
    " sample",
];

/// One file of a folder listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundFile {
    pub name: String,
    pub size_bytes: i64,
    pub modified_at: Option<DateTime<Utc>>,
}

/// A path the scan may read: absolute, below one of the roots, with no `.` or `..` part.
pub fn under_root<'a>(path: &str, roots: &'a [String]) -> Option<&'a str> {
    if path.split('/').any(|part| part == ".." || part == ".") {
        return None;
    }
    roots
        .iter()
        .find(|root| {
            path.strip_prefix(root.as_str())
                .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
        })
        .map(String::as_str)
}

/// A video file that is a cut of the movie: a video extension, not hidden, not an extra or a
/// sample.
pub fn is_movie_file(name: &str) -> bool {
    if name.starts_with(['.', '@', '#']) {
        return false;
    }
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if !VIDEO.contains(&ext.to_ascii_lowercase().as_str()) {
        return false;
    }
    let lower = stem.to_ascii_lowercase();
    !EXTRAS.iter().any(|x| lower.ends_with(x)) && lower != "sample"
}

/// The files of a listing no engine tracks: video files whose name is not the name of a
/// tracked file. `tracked` holds paths below the folder; their last part is compared.
pub fn untracked<'a>(listing: &'a [FoundFile], tracked: &[String]) -> Vec<&'a FoundFile> {
    let tracked: HashSet<&str> = tracked
        .iter()
        .map(|p| p.rsplit(['/', '\\']).next().unwrap_or(p))
        .collect();
    listing
        .iter()
        .filter(|f| is_movie_file(&f.name) && !tracked.contains(f.name.as_str()))
        .collect()
}

/// The regular files directly in a folder; symbolic links are skipped. A folder that does not
/// exist is an empty listing (a movie Radarr has no file for may have no folder yet).
pub fn read_folder(path: &Path) -> std::io::Result<Vec<FoundFile>> {
    let entries = match std::fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry?;
        let meta = match entry.path().symlink_metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        out.push(FoundFile {
            name,
            size_bytes: i64::try_from(meta.len()).unwrap_or(i64::MAX),
            modified_at: meta.modified().ok().map(DateTime::<Utc>::from),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// What one scan did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderScanReport {
    pub folders: usize,
    pub unreadable: usize,
    pub outside_roots: usize,
    pub video_files: usize,
    pub untracked: usize,
    pub found: usize,
    pub gone: usize,
    /// Roots that could not be read; nothing under them was changed.
    pub roots_down: Vec<String>,
}

impl FolderScanReport {
    pub fn counts(&self) -> serde_json::Value {
        json!({
            "folders": self.folders,
            "unreadable": self.unreadable,
            "outside_roots": self.outside_roots,
            "video_files": self.video_files,
            "untracked": self.untracked,
            "found": self.found,
            "gone": self.gone,
            "roots_down": self.roots_down,
        })
    }
}

/// One folder as read, with its untracked files ready to write.
struct ReadFolder {
    folder: MovieFolder,
    files: Vec<UntrackedWrite>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("the folder scan task failed: {0}")]
    Task(String),
}

/// Read every movie folder under the roots and list the files no engine tracks. Changes are
/// journaled as `untracked_file_found` and `untracked_file_gone`.
pub async fn scan(
    pool: &PgPool,
    journal: &Journal,
    editions: &EditionList,
    roots: &[String],
) -> Result<FolderScanReport, ScanError> {
    let started = Instant::now();
    let folders = store::movie_folders(pool).await?;
    let known = store::all_untracked(pool).await?;
    let list = editions.clone();
    let roots_owned = roots.to_vec();
    let (mut report, read, skipped_roots, unread) =
        tokio::task::spawn_blocking(move || read_all(&list, &roots_owned, folders))
            .await
            .map_err(|e| ScanError::Task(e.to_string()))?;

    let default_edition = editions.default_edition().key.clone();
    let mut by_folder: HashMap<(String, i64), Vec<UntrackedRow>> = HashMap::new();
    for row in known {
        by_folder
            .entry((row.engine.clone(), row.title_id))
            .or_default()
            .push(row);
    }
    let mut tx = journal.begin(pool).await?;
    let mut events: Vec<NewEvent> = Vec::new();
    // A folder that could not be read counts as seen: its rows stay until it can be read
    // again. Only a folder no engine names any more loses them.
    let mut seen: HashSet<(String, i64)> = unread.into_iter().collect();
    for ReadFolder { folder, files } in read {
        let key = (folder.engine.clone(), folder.title_id);
        seen.insert(key.clone());
        let before = by_folder.remove(&key).unwrap_or_default();
        if before.is_empty() && files.is_empty() {
            continue;
        }
        let mut changed = false;
        let now: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
        for gone in before.iter().filter(|r| !now.contains(r.path.as_str())) {
            store::delete_untracked(tx.conn(), gone.id).await?;
            report.gone += 1;
            changed = true;
            events.push(event(
                "untracked_file_gone",
                "title",
                folder.title_id,
                "scan:movies",
                json!({
                    "title_id": folder.title_id,
                    "title": folder.title,
                    "engine": folder.engine,
                    "library": folder.library_id,
                    "file_name": gone.file_name,
                    "edition": gone.edition_key,
                }),
            ));
        }
        for f in &files {
            let old = before.iter().find(|r| r.path == f.path);
            let is_new = old.is_none();
            let differs =
                old.is_some_and(|r| r.size_bytes != f.size_bytes || r.edition_key != f.edition_key);
            if !is_new && !differs {
                continue;
            }
            store::upsert_untracked(tx.conn(), f).await?;
            changed = true;
            if is_new {
                report.found += 1;
                events.push(event(
                    "untracked_file_found",
                    "title",
                    folder.title_id,
                    "scan:movies",
                    json!({
                        "title_id": folder.title_id,
                        "title": folder.title,
                        "engine": folder.engine,
                        "library": folder.library_id,
                        "file_name": f.file_name,
                        "size_bytes": f.size_bytes,
                        "edition": f.edition_key,
                        "edition_label": f.edition_label,
                    }),
                ));
            }
        }
        if changed {
            store::refresh_editions(tx.conn(), folder.title_id, &default_edition).await?;
        }
        if events.len() >= 500 {
            tx.append(std::mem::take(&mut events)).await?;
        }
    }
    // Rows of folders no engine names any more (the engine let go of the title), unless they
    // sit under a root that could not be read.
    for ((engine, title_id), rows) in by_folder {
        if seen.contains(&(engine.clone(), title_id)) {
            continue;
        }
        let mut changed = false;
        for row in rows {
            if skipped_roots
                .iter()
                .any(|r| under_root(&row.path, std::slice::from_ref(r)).is_some())
            {
                continue;
            }
            store::delete_untracked(tx.conn(), row.id).await?;
            report.gone += 1;
            changed = true;
            events.push(event(
                "untracked_file_gone",
                "title",
                title_id,
                "scan:movies",
                json!({
                    "title_id": title_id,
                    "engine": engine,
                    "file_name": row.file_name,
                    "edition": row.edition_key,
                }),
            ));
        }
        if changed {
            store::refresh_editions(tx.conn(), title_id, &default_edition).await?;
        }
    }
    tx.append(events).await?;
    tx.commit().await?;
    let ok = report.roots_down.is_empty() && report.unreadable == 0;
    let error = (!ok).then(|| {
        let mut parts = Vec::new();
        if !report.roots_down.is_empty() {
            parts.push(format!("Not read: {}", report.roots_down.join(", ")));
        }
        if report.unreadable > 0 {
            parts.push(format!(
                "{} not read",
                units::counted(report.unreadable as i64, "folder", "folders")
            ));
        }
        parts.join("; ")
    });
    store::record_folder_scan(
        pool,
        roots,
        ok,
        i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
        error.as_deref(),
        &report.counts(),
    )
    .await?;
    Ok(report)
}

/// What the blocking part gives back: the report so far, the folders it read, the roots that
/// are down, and the folders (engine, title) it could not read.
type ReadAll = (
    FolderScanReport,
    Vec<ReadFolder>,
    Vec<String>,
    Vec<(String, i64)>,
);

/// Whether a folder lies below this one root.
fn below(folder: &str, root: &String) -> bool {
    under_root(folder.trim_end_matches('/'), std::slice::from_ref(root)).is_some()
}

/// The blocking part: check the roots, read the folders, read each untracked name.
fn read_all(list: &EditionList, roots: &[String], folders: Vec<MovieFolder>) -> ReadAll {
    let mut report = FolderScanReport::default();
    let mut unread = Vec::new();
    let down: Vec<String> = roots
        .iter()
        .filter(|r| {
            let Ok(mut entries) = std::fs::read_dir(r.as_str()) else {
                return true;
            };
            // A root that lists nothing although engines name folders under it is a mount
            // point whose volume did not come up. Its folders would all read as empty.
            entries.next().is_none() && folders.iter().any(|f| below(&f.path, r))
        })
        .cloned()
        .collect();
    let mut read = Vec::new();
    for folder in folders {
        let path = folder.path.trim_end_matches('/').to_string();
        let Some(root) = under_root(&path, roots) else {
            report.outside_roots += 1;
            continue;
        };
        if down.iter().any(|d| d == root) {
            continue;
        }
        let listing = match read_folder(Path::new(&path)) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(folder = %path, error = %e, "a movie folder could not be read");
                report.unreadable += 1;
                unread.push((folder.engine.clone(), folder.title_id));
                continue;
            }
        };
        report.folders += 1;
        report.video_files += listing.iter().filter(|f| is_movie_file(&f.name)).count();
        let files: Vec<UntrackedWrite> = untracked(&listing, &folder.tracked)
            .into_iter()
            .map(|f| {
                let release = parse_library_name(&f.name).to_release();
                let edition = list.read_release(&release);
                UntrackedWrite {
                    engine: folder.engine.clone(),
                    library_id: folder.library_id.clone(),
                    title_id: folder.title_id,
                    path: format!("{path}/{}", f.name),
                    file_name: f.name.clone(),
                    size_bytes: f.size_bytes,
                    modified_at: f.modified_at,
                    edition_rank: i32::try_from(list.rank(&edition.key)).unwrap_or(i32::MAX),
                    edition_key: edition.key,
                    edition_label: edition.label,
                    edition_marked: edition.marked,
                    resolution: release.resolution.map(|r| r.label().to_string()),
                }
            })
            .collect();
        report.untracked += files.len();
        read.push(ReadFolder { folder, files });
    }
    report.roots_down = down.clone();
    (report, read, down, unread)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots() -> Vec<String> {
        vec!["/media/movies".to_string(), "/data/lib/movies".to_string()]
    }

    #[test]
    fn only_folders_below_a_root_are_read() {
        let r = roots();
        assert_eq!(
            under_root("/media/movies/Heat (1995) {tmdb-949}", &r),
            Some("/media/movies")
        );
        assert_eq!(under_root("/media/movies", &r), None);
        assert_eq!(under_root("/media/movies/", &r), None);
        assert_eq!(under_root("/media/moviesx/Heat", &r), None);
        assert_eq!(under_root("/media/movies/../music/x", &r), None);
        assert_eq!(under_root("/media/movies/./x", &r), None);
        assert_eq!(under_root("/etc/passwd", &r), None);
        assert_eq!(under_root("relative/movies/x", &r), None);
    }

    #[test]
    fn video_files_that_are_not_extras_count() {
        for name in [
            "Heat (1995) {imdb-tt0113277} [Bluray-1080p][x265]-SHR.mkv",
            "Heat.1995.Directors.Cut.1080p.BluRay.x264-GRP.MP4",
            "Heat (1995) - Director's Cut.m2ts",
        ] {
            assert!(is_movie_file(name), "{name}");
        }
        for name in [
            "Heat (1995) [Bluray-1080p].srt",
            "Heat (1995)-trailer.mkv",
            "Heat (1995) - Behind the Scenes-behindthescenes.mkv",
            "heat.1995.1080p-sample.mkv",
            "sample.mkv",
            ".Heat.mkv.partial",
            "@eaDir.mkv",
            "poster.jpg",
            "noextension",
        ] {
            assert!(!is_movie_file(name), "{name}");
        }
    }

    #[test]
    fn the_tracked_file_is_left_out() {
        let file = |name: &str| FoundFile {
            name: name.to_string(),
            size_bytes: 1,
            modified_at: None,
        };
        let listing = [
            file("Heat (1995) {imdb-tt0113277} [Bluray-1080p][x265]-SHR.mkv"),
            file(
                "Heat (1995) {imdb-tt0113277} {edition-Director's Cut} [Bluray-1080p][x265]-SHR.mkv",
            ),
            file("Heat (1995) [Bluray-1080p].en.srt"),
            file("Heat (1995)-trailer.mkv"),
        ];
        let tracked = vec!["Heat (1995) {imdb-tt0113277} [Bluray-1080p][x265]-SHR.mkv".to_string()];
        let found = untracked(&listing, &tracked);
        assert_eq!(found.len(), 1);
        assert!(found[0].name.contains("{edition-Director's Cut}"));
        // A tracked path with a folder in front is compared by its last part.
        let tracked =
            vec!["sub/Heat (1995) {imdb-tt0113277} [Bluray-1080p][x265]-SHR.mkv".to_string()];
        assert_eq!(untracked(&listing, &tracked).len(), 1);
    }

    #[test]
    fn a_folder_is_read_one_level_without_links() {
        let dir = std::env::temp_dir().join(format!("singularr-movies-{}", std::process::id()));
        let movie = dir.join("Heat (1995)");
        std::fs::create_dir_all(movie.join("Featurettes")).unwrap();
        std::fs::write(movie.join("Heat (1995) [Bluray-1080p].mkv"), b"x").unwrap();
        std::fs::write(movie.join("Featurettes").join("Making of.mkv"), b"x").unwrap();
        let listing = read_folder(&movie).unwrap();
        assert_eq!(
            listing.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            vec!["Heat (1995) [Bluray-1080p].mkv"]
        );
        assert!(read_folder(&dir.join("missing")).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
