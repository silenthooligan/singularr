//! The sync loops: one per engine and kind of read, each on its own interval.
//!
//! The engines are shared production services, so the loops are slow by default, start
//! with a random delay, read a show's files only when its file count changed (plus a full
//! pass every few hours), and never stop on a failure: the failure is recorded with its pass
//! (`integration_health`), a change between up and down goes to the journal, and the next
//! tick tries again.

use super::downloads::{Claims, reconcile};
use super::store;
use crate::catalog::ingest::Assess;
use crate::catalog::store as catalog_store;
use crate::catalog::sync::{ApplyReport, apply_files, apply_titles, reassess};
use crate::core::config::SyncConfig;
use crate::core::jobs::{self, Job, JobSpec};
use crate::core::journal::Journal;
use crate::core::observe;
use crate::core::units;
use crate::engines::service::{EngineSet, PassOk, pass_result};
use crate::engines::{DownloadClient, LibraryEngine, Protocol};
use futures::StreamExt;
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// Grab history records asked for per page.
const HISTORY_PAGE: u32 = 250;

#[derive(Clone)]
pub struct SyncDeps {
    pub config: SyncConfig,
    pub pool: PgPool,
    pub journal: Journal,
    pub engines: Arc<EngineSet>,
    pub assess: Arc<dyn Assess>,
    /// The `policy_loaded` event of this start, when the policy changed.
    pub policy_event: Option<i64>,
}

/// What the queue and history loops have gathered, read by the downloads loop.
#[derive(Default)]
struct ClaimBook {
    claims: Claims,
    queue_read: HashSet<String>,
    history_loaded: HashSet<String>,
}

type Book = Arc<Mutex<ClaimBook>>;

/// The running loops. Dropping the handle leaves them running; call [`SyncHandle::stop`].
pub struct SyncHandle {
    tasks: Vec<JoinHandle<()>>,
}

impl SyncHandle {
    /// Stop every loop. A transaction in flight is rolled back by the database.
    pub fn stop(&self) {
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// A random delay up to `max`, so the loops of a fresh start do not all fire together.
pub fn jitter(max: Duration) -> Duration {
    let millis = max.as_millis() as u64;
    if millis == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis((uuid::Uuid::new_v4().as_u128() % (millis as u128 + 1)) as u64)
}

pub fn spawn(deps: SyncDeps) -> SyncHandle {
    let book: Book = Arc::new(Mutex::new(ClaimBook::default()));
    let mut tasks = Vec::new();

    // Copies assessed under another policy or build are assessed again before the title
    // loops start, so each engine's rows keep a single writer.
    {
        let deps = deps.clone();
        tasks.push(tokio::spawn(async move {
            let started = Instant::now();
            match reassess(&deps.pool, &deps.journal, deps.assess.clone(), deps.policy_event).await {
                Ok(r) if r.examined > 0 => tracing::info!(
                    examined = r.examined,
                    updated = r.updated,
                    score_changed = r.score_changed,
                    seconds = started.elapsed().as_secs(),
                    "stored copies were assessed again under the current policy and build"
                ),
                Ok(_) => {}
                Err(e) => tracing::error!(error = %e, "assessing the stored copies again failed; they keep their old scores until the next start"),
            }
            // The title loops run inside this task, so stopping it stops them too.
            futures::future::join_all(
                deps.engines
                    .arrs
                    .iter()
                    .map(|engine| titles_loop(deps.clone(), engine.clone())),
            )
            .await;
        }));
    }
    for engine in &deps.engines.arrs {
        tasks.push(tokio::spawn(queue_loop(
            deps.clone(),
            engine.clone(),
            book.clone(),
        )));
        tasks.push(tokio::spawn(history_loop(
            deps.clone(),
            engine.clone(),
            book.clone(),
        )));
    }
    // Each download client on its own loop: torrents and usenet downloads, each pass touching
    // that client's rows alone.
    for client in &deps.engines.download_clients {
        tasks.push(tokio::spawn(downloads_loop(
            deps.clone(),
            client.clone(),
            book.clone(),
        )));
    }
    for server in deps.engines.media_servers.iter().cloned() {
        tasks.push(tokio::spawn(super::server::server_loop(
            deps.clone(),
            server,
        )));
    }
    SyncHandle { tasks }
}

/// A pass ended: its job, the metrics, the log and the engine state hear how it went.
async fn report(
    deps: &SyncDeps,
    job: &Job,
    engine: &str,
    pass: &'static str,
    started: Instant,
    outcome: Result<PassOk, String>,
) {
    job.end(started, pass_result(&outcome));
    observe::sync_pass(engine, pass, started.elapsed(), outcome.is_ok());
    if let Err(e) = &outcome {
        tracing::warn!(engine, pass, error = %e, "sync pass failed");
    }
    if let Err(e) = deps
        .engines
        .report(&deps.pool, &deps.journal, engine, pass, outcome)
        .await
    {
        tracing::error!(engine, pass, error = %e, "the engine state could not be recorded");
    }
}

fn add(total: &mut ApplyReport, part: ApplyReport) {
    total.titles_added += part.titles_added;
    total.titles_updated += part.titles_updated;
    total.titles_removed += part.titles_removed;
    total.copies_added += part.copies_added;
    total.copies_replaced += part.copies_replaced;
    total.copies_removed += part.copies_removed;
    total.copies_changed += part.copies_changed;
    total.events += part.events;
}

/// One pass over an engine's titles, and for a show engine over the files of the shows
/// that need it. Returns what it counted.
pub async fn titles_pass(
    deps: &SyncDeps,
    engine: &Arc<dyn LibraryEngine>,
    full_files: bool,
) -> Result<(PassOk, ApplyReport), String> {
    let started = Instant::now();
    let me = engine.engine().clone();
    let info = engine.status().await.map_err(|e| e.to_string())?;
    // The status call is small and constant: its round trip is the engine's latency.
    let latency = started.elapsed();
    let titles = engine.titles().await.map_err(|e| e.to_string())?;
    let listed = titles.len();
    let shows = titles.iter().any(|t| t.files.is_none());
    let mut total = apply_titles(&deps.pool, &deps.journal, &me, titles, deps.assess.clone())
        .await
        .map_err(|e| e.to_string())?;

    let mut files_read = 0usize;
    let mut files_failed = 0usize;
    if shows {
        // A show's files are read when they were never read, when the engine's file count
        // moved since the last read, or when a full pass is due.
        let links = catalog_store::engine_links(&deps.pool, &me.id)
            .await
            .map_err(|e| e.to_string())?;
        let due: Vec<(i64, i32)> = links
            .into_iter()
            .filter(|l| {
                full_files
                    || l.files_synced_at.is_none()
                    || l.files_synced_count != Some(l.file_count)
            })
            .map(|l| (l.engine_title_id, l.file_count))
            .collect();
        let results: Vec<Result<ApplyReport, String>> = futures::stream::iter(due)
            .map(|(id, file_count)| {
                let engine = engine.clone();
                let deps = deps.clone();
                let me = me.clone();
                async move {
                    let files = engine.files(id).await.map_err(|e| e.to_string())?;
                    apply_files(
                        &deps.pool,
                        &deps.journal,
                        &me,
                        id,
                        file_count,
                        files,
                        deps.assess.clone(),
                    )
                    .await
                    .map_err(|e| e.to_string())
                }
            })
            .buffer_unordered(deps.config.files_concurrency)
            .collect()
            .await;
        let mut first_error = None;
        for r in results {
            match r {
                Ok(part) => {
                    files_read += 1;
                    add(&mut total, part);
                }
                Err(e) => {
                    files_failed += 1;
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                }
            }
        }
        // One show failing does not fail the pass; every show failing does.
        if let Some(e) = first_error {
            if files_read == 0 {
                return Err(format!(
                    "reading episode files failed for all {files_failed} shows asked: {e}"
                ));
            }
            tracing::warn!(engine = %me.id, failed = files_failed, error = %e, "some shows' files could not be read; they are asked again next pass");
        }
    }
    if total.changed() {
        tracing::info!(
            engine = %me.id,
            titles_added = total.titles_added,
            titles_removed = total.titles_removed,
            copies_added = total.copies_added,
            copies_replaced = total.copies_replaced,
            copies_removed = total.copies_removed,
            copies_changed = total.copies_changed,
            "catalog updated"
        );
    }
    Ok((
        PassOk {
            took: latency,
            version: info.version,
            counts: json!({ "titles": listed, "shows_read": files_read, "shows_failed": files_failed }),
        },
        total,
    ))
}

async fn titles_loop(deps: SyncDeps, engine: Arc<dyn LibraryEngine>) {
    let id = engine.engine().id.clone();
    let job = jobs::registry().register(JobSpec::of(&id, "titles", deps.config.titles));
    job.wait(jitter(deps.config.jitter)).await;
    let mut last_full: Option<Instant> = None;
    loop {
        let started = job.begin();
        let full = last_full.is_none_or(|t| t.elapsed() >= deps.config.files_full);
        let outcome = titles_pass(&deps, &engine, full).await;
        if outcome.is_ok() && full {
            last_full = Some(started);
        }
        report(
            &deps,
            &job,
            &id,
            "titles",
            started,
            outcome.map(|(ok, _)| ok),
        )
        .await;
        job.wait(deps.config.titles).await;
    }
}

fn settle(book: &mut ClaimBook, engines: &EngineSet) {
    // Ownership is settled only when every arr of the configuration is configured and has
    // been heard: an arr that is not asked may own any torrent.
    book.claims.complete = engines.every_arr_configured()
        && engines.arrs.iter().all(|e| {
            let id = &e.engine().id;
            book.queue_read.contains(id) && book.history_loaded.contains(id)
        });
}

async fn queue_loop(deps: SyncDeps, engine: Arc<dyn LibraryEngine>, book: Book) {
    let id = engine.engine().id.clone();
    let job = jobs::registry().register(JobSpec::of(&id, "queue", deps.config.queue));
    job.wait(jitter(deps.config.jitter)).await;
    loop {
        let started = job.begin();
        let outcome = match engine.queue().await {
            Ok(items) => {
                let count = items.len();
                let mut b = book.lock().unwrap();
                b.claims.queues.insert(id.clone(), items);
                b.queue_read.insert(id.clone());
                settle(&mut b, &deps.engines);
                Ok(PassOk::new(started.elapsed(), json!({ "queue": count })))
            }
            Err(e) => {
                // A queue that cannot be read is unknown, not empty: until it is read again
                // nothing new is called an orphan.
                let mut b = book.lock().unwrap();
                b.queue_read.remove(&id);
                settle(&mut b, &deps.engines);
                Err(e.to_string())
            }
        };
        report(&deps, &job, &id, "queue", started, outcome).await;
        job.wait(deps.config.queue).await;
    }
}

/// Read grab history pages, newest first, into the book. `max_pages` bounds the first full
/// read; later reads take the first page only.
async fn history_pass(
    deps: &SyncDeps,
    engine: &Arc<dyn LibraryEngine>,
    book: &Book,
    max_pages: u32,
) -> Result<PassOk, String> {
    let id = engine.engine().id.clone();
    let mut complete = false;
    let mut latency = Duration::ZERO;
    for page in 1..=max_pages {
        let asked = Instant::now();
        let grabs = engine
            .grabs(page, HISTORY_PAGE)
            .await
            .map_err(|e| e.to_string())?;
        if page == 1 {
            latency = asked.elapsed();
        }
        let end = grabs.records < HISTORY_PAGE as usize
            || (page as i64) * (HISTORY_PAGE as i64) >= grabs.total_records;
        book.lock()
            .unwrap()
            .claims
            .history
            .entry(id.clone())
            .or_default()
            .extend(grabs.download_ids);
        if end {
            complete = true;
            break;
        }
        // A pause between pages: the history table is the arr's largest.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let known = {
        let mut b = book.lock().unwrap();
        b.history_loaded.insert(id.clone());
        settle(&mut b, &deps.engines);
        b.claims.history.get(&id).map(|h| h.len()).unwrap_or(0)
    };
    Ok(PassOk::new(
        latency,
        json!({ "grabs_known": known, "grab_history_complete": complete || max_pages == 1 }),
    ))
}

async fn history_loop(deps: SyncDeps, engine: Arc<dyn LibraryEngine>, book: Book) {
    let id = engine.engine().id.clone();
    let job = jobs::registry().register(JobSpec::of(&id, "history", deps.config.history));
    job.wait(jitter(deps.config.jitter)).await;
    let mut loaded = false;
    loop {
        let started = job.begin();
        let pages = if loaded {
            1
        } else {
            deps.config.history_max_pages
        };
        let outcome = history_pass(&deps, &engine, &book, pages).await;
        loaded = loaded || outcome.is_ok();
        report(&deps, &job, &id, "history", started, outcome).await;
        job.wait(deps.config.history).await;
    }
}

/// What one pass of a download client counted: its torrents, or a usenet client's downloads.
pub fn downloads_counts(protocol: Protocol, listed: usize, settled: bool) -> serde_json::Value {
    match protocol {
        Protocol::Torrent => json!({ "torrents": listed, "ownership_settled": settled }),
        Protocol::Usenet => json!({ "downloads": listed, "ownership_settled": settled }),
    }
}

async fn downloads_loop(deps: SyncDeps, client: Arc<dyn DownloadClient>, book: Book) {
    let id = client.id().to_string();
    let job = jobs::registry().register(JobSpec::of(&id, "downloads", deps.config.downloads));
    job.wait(jitter(deps.config.jitter)).await;
    loop {
        let started = job.begin();
        let outcome = match client.listing().await {
            Ok(listing) => {
                let latency = started.elapsed();
                let claims = book.lock().unwrap().claims.clone();
                match reconcile(&deps.pool, &deps.journal, &id, listing, &claims).await {
                    Ok(r) => Ok(PassOk::new(
                        latency,
                        downloads_counts(client.protocol(), r.listed, claims.complete),
                    )),
                    Err(e) => Err(format!("the downloads could not be stored: {e}")),
                }
            }
            Err(e) => Err(e.to_string()),
        };
        report(&deps, &job, &id, "downloads", started, outcome).await;
        job.wait(deps.config.downloads).await;
    }
}

/// Refresh the gauges at /metrics from the stores.
pub async fn refresh_gauges(pool: &PgPool) -> Result<(), sqlx::Error> {
    let catalog = catalog_store::counts(pool).await?;
    observe::gauge_labeled(observe::TITLES, "kind", "movie", catalog.movies);
    observe::gauge_labeled(observe::TITLES, "kind", "show", catalog.shows);
    for (library, copies) in catalog_store::copies_by_library(pool).await? {
        observe::gauge_labeled(observe::COPIES, "library", &library, copies);
    }
    let downloads = store::counts(pool).await?;
    observe::gauge(observe::DOWNLOADS, downloads.total);
    observe::gauge(observe::DOWNLOAD_ORPHANS, downloads.orphans);
    observe::gauge(observe::DOWNLOADS_STUCK, downloads.stuck);
    let shadow = catalog_store::diff_counts(pool, None).await?;
    observe::gauge(observe::SHADOW_DISAGREEMENTS, shadow.any);
    Ok(())
}

/// Refresh the gauges every 30 seconds. Started by the server on its own, so the numbers
/// are there when the sync is switched off.
pub fn spawn_gauges(pool: PgPool) -> JoinHandle<()> {
    tokio::spawn(gauges_loop(pool))
}

/// The read-only scan of the movie folders for files no engine tracks, every `interval`. It
/// runs whether or not the sync does: the folders are mounts, not engines. The first scan
/// waits a few minutes, so the titles of a fresh start are in the catalog first.
pub fn spawn_folder_scans(
    pool: PgPool,
    journal: Journal,
    editions: Arc<singularr_core::EditionList>,
    roots: Vec<String>,
    interval: Duration,
    jitter_max: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        if roots.is_empty() {
            tracing::info!("SINGULARR_MOVIE_ROOTS is empty: the movie folders are not scanned");
            return;
        }
        let job = jobs::registry().register(JobSpec::folder("movie-folders", interval));
        job.wait(Duration::from_secs(180) + jitter(jitter_max))
            .await;
        loop {
            let started = job.begin();
            match crate::catalog::scan::scan(&pool, &journal, &editions, &roots).await {
                Ok(r) => {
                    tracing::info!(
                        folders = r.folders,
                        untracked = r.untracked,
                        found = r.found,
                        gone = r.gone,
                        unreadable = r.unreadable,
                        roots_down = ?r.roots_down,
                        "movie folders scanned"
                    );
                    job.end(
                        started,
                        Ok(format!(
                            "{}, {} untracked, {} new, {} gone",
                            units::counted(r.folders as i64, "folder", "folders"),
                            units::count(r.untracked as i64),
                            units::count(r.found as i64),
                            units::count(r.gone as i64)
                        )),
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "the movie folder scan failed");
                    job.end(started, Err(e.to_string()));
                }
            }
            job.wait(interval).await;
        }
    })
}

async fn gauges_loop(pool: PgPool) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    loop {
        tick.tick().await;
        if let Err(e) = refresh_gauges(&pool).await {
            tracing::debug!(error = %e, "gauges were not refreshed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_stays_within_its_bound() {
        assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
        for _ in 0..200 {
            assert!(jitter(Duration::from_secs(20)) <= Duration::from_secs(20));
        }
    }

    /// A torrent client's pass counts torrents, as qBittorrent's always did; a usenet client's
    /// counts downloads.
    #[test]
    fn a_pass_counts_in_its_protocol_s_word() {
        assert_eq!(
            downloads_counts(Protocol::Torrent, 4, true),
            json!({"torrents": 4, "ownership_settled": true})
        );
        assert_eq!(
            downloads_counts(Protocol::Usenet, 2, false),
            json!({"downloads": 2, "ownership_settled": false})
        );
    }
}
