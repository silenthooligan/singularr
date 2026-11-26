//! The sync: what Singularr reads from the integrations. The passes of the managers (titles,
//! queue, grab history) and of the download clients (B9b), of the media servers and of the Seerr
//! sites (requests, accounts, discover lists, B9c) are jobs of the job runtime
//! (`lifecycle::jobs`).
//!
//! The integrations are shared production services. The passes are slow by default. The first run
//! after a start waits a random delay. A show's files are read only when its file count changed,
//! plus a full file pass every few hours. A failure is recorded with its pass
//! (`integration_health`). A change between up and down goes to the journal. A later run tries
//! again: the runtime backs a failing schedule off (platform design, section 4.4).
//!
//! What the managers claim of the downloads are rows of `download_claims` ([`super::claims`]).
//! Each start reads them as the last one left them. A grab history whose marker is fresh is read
//! from its first page alone. A start whose last full file pass is recent makes none.

use super::claims;
use super::downloads::{DownloadsReport, reconcile};
use super::formats;
use crate::catalog::ingest::Assess;
use crate::catalog::store as catalog_store;
use crate::catalog::sync::{ApplyReport, apply_files, apply_titles, reassess};
use crate::core::checks::{self, Check};
use crate::core::config::SyncConfig;
use crate::core::journal::Journal;
use crate::core::observe;
use crate::core::runtime::{Cx, Done, Failed, Handler, Lease, LeaseError};
use crate::core::units;
use crate::engines::service::{EngineSet, PassOk};
use crate::engines::{
    ClientListing, DownloadClient, FormatBook, FormatChanges, LibraryEngine, MediaServer, Protocol,
    QueueItem, RequestSite,
};
use crate::requests::discover;
use crate::requests::rules::Rules;
use crate::requests::sync::{PassConfig, discover_pass, requests_pass, users_pass};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::{FutureExt, StreamExt};
use serde_json::json;
use sqlx::PgPool;
use std::collections::HashSet;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Grab history records asked for per page.
const HISTORY_PAGE: u32 = 250;

/// The kinds of the manager and download jobs: their handlers in the runtime.
pub const TITLES: &str = "manager.titles";
pub const QUEUE: &str = "manager.queue";
pub const HISTORY: &str = "manager.history";
pub const DOWNLOADS: &str = "download.listing";
pub const SERVER: &str = "server.pass";
pub const REQUESTS: &str = "requests.pass";
pub const ACCOUNTS: &str = "requests.accounts";
pub const DISCOVER: &str = "requests.discover";

/// The marker of a Seerr site's discover lists: when they were last stored.
pub const DISCOVER_AT: &str = "discover.at";

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

/// What the queue and history jobs heard since this start: the arrs whose last queue read
/// succeeded, the arrs whose grab history was read. The claims themselves are rows.
#[derive(Default)]
pub struct Heard {
    queue: Mutex<HashSet<String>>,
    history: Mutex<HashSet<String>>,
}

impl Heard {
    fn queue_read(&self, id: &str, read: bool) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if read {
            queue.insert(id.to_string());
        } else {
            queue.remove(id);
        }
    }

    fn history_read(&self, id: &str) {
        self.history
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_string());
    }

    /// Ownership is settled only when every arr of the configuration is configured and was heard
    /// since this start: its queue read and its grab history read. An arr that is not asked may
    /// own any download.
    pub fn settled(&self, engines: &EngineSet) -> bool {
        let queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        let history = self.history.lock().unwrap_or_else(PoisonError::into_inner);
        engines.every_arr_configured()
            && engines.arrs.iter().all(|e| {
                let id = &e.engine().id;
                queue.contains(id) && history.contains(id)
            })
    }
}

/// What the integrations' jobs run with. `reassessed` turns true once the stored copies were
/// assessed again at this start. A titles pass waits for it. Each engine's rows then keep a single
/// writer. `rules` are the request rules: the owner's accounts are marked when a Seerr site's
/// accounts are read.
pub struct SyncJobs {
    pub deps: SyncDeps,
    pub reassessed: watch::Receiver<bool>,
    pub heard: Heard,
    pub rules: Rules,
}

/// The running loops. Dropping the handle leaves them running; call [`SyncHandle::stop`].
pub struct SyncHandle {
    tasks: Vec<JoinHandle<()>>,
    jobs: Arc<SyncJobs>,
}

impl SyncHandle {
    /// Stop every loop. A transaction in flight is rolled back by the database.
    pub fn stop(&self) {
        for t in &self.tasks {
            t.abort();
        }
    }

    /// What the manager and download jobs of this start run with.
    pub fn jobs(&self) -> Arc<SyncJobs> {
        self.jobs.clone()
    }
}

/// Start the re-assessment of the stored copies. The integrations' passes are jobs: they run with
/// [`SyncHandle::jobs`].
pub fn spawn(deps: SyncDeps, rules: Rules) -> SyncHandle {
    let (done, reassessed) = watch::channel(false);
    let mut tasks = Vec::new();

    // Copies assessed under another policy or build are assessed again before the titles passes
    // of this start run. Each engine's rows keep a single writer.
    {
        let deps = deps.clone();
        tasks.push(tokio::spawn(async move {
            let started = Instant::now();
            // A panic signals as a failure does: the copies keep their old scores. The channel then
            // closes without the signal at a stop alone.
            let assessed = AssertUnwindSafe(reassess(
                &deps.pool,
                &deps.journal,
                deps.assess.clone(),
                deps.policy_event,
            ))
            .catch_unwind()
            .await;
            match assessed {
                Ok(Ok(r)) if r.examined > 0 => tracing::info!(
                    examined = r.examined,
                    updated = r.updated,
                    score_changed = r.score_changed,
                    seconds = started.elapsed().as_secs(),
                    "stored copies were assessed again under the current policy and build"
                ),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::error!(error = %e, "assessing the stored copies again failed; they keep their old scores until the next start"),
                Err(_) => tracing::error!("assessing the stored copies again panicked; they keep their old scores until the next start"),
            }
            let _ = done.send(true);
        }));
    }
    SyncHandle {
        tasks,
        jobs: Arc::new(SyncJobs {
            deps,
            reassessed,
            heard: Heard::default(),
            rules,
        }),
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
/// that need it. Returns what it counted. It keeps no format book: a manager that reads its
/// files' custom formats apart asks about every file ([`titles_pass_with`]).
pub async fn titles_pass(
    deps: &SyncDeps,
    engine: &Arc<dyn LibraryEngine>,
    full_files: bool,
) -> Result<(PassOk, ApplyReport), String> {
    titles_pass_with(deps, engine, full_files, &FormatBook::default())
        .await
        .map(|(ok, report, _)| (ok, report))
}

/// [`titles_pass`] with the format book of the manager's files (`manager_file_formats`). Returns
/// what the read changed in the book too.
pub async fn titles_pass_with(
    deps: &SyncDeps,
    engine: &Arc<dyn LibraryEngine>,
    full_files: bool,
    book: &FormatBook,
) -> Result<(PassOk, ApplyReport, FormatChanges), String> {
    let started = Instant::now();
    let me = engine.engine().clone();
    let info = engine.status().await.map_err(|e| e.to_string())?;
    // The status call is small and constant: its round trip is the engine's latency.
    let latency = started.elapsed();
    let (titles, formats) = engine.titles_with(book).await.map_err(|e| e.to_string())?;
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
        formats,
    ))
}

/// Read grab history pages, newest first, until a page holds a record at or below `loaded`: the
/// newest record id of the last read that ended well (the marker [`claims::HISTORY_LOADED`]). It
/// stops at the end of the history too. It stops after `max_pages` pages at most. A first page
/// whose newest record is below `loaded` voids the marker: the arr's database was restored or
/// rebuilt. The read then goes on to the end or the cap. Each page's download ids are stored in a
/// transaction that proves the run's lease. The log says how many pages were read. When the cap
/// came first it says that older grabs were not read. Returns the newest record id it saw too.
async fn history_pass(
    deps: &SyncDeps,
    engine: &Arc<dyn LibraryEngine>,
    lease: &Lease,
    loaded: Option<i64>,
    max_pages: u32,
) -> Result<(PassOk, Option<i64>), String> {
    let id = engine.engine().id.clone();
    let mut complete = false;
    let mut latency = Duration::ZERO;
    let mut pages = 0u32;
    let mut loaded = loaded;
    let mut newest = None;
    for page in 1..=max_pages {
        let asked = Instant::now();
        let grabs = engine
            .grabs(page, HISTORY_PAGE)
            .await
            .map_err(|e| e.to_string())?;
        pages = page;
        if page == 1 {
            latency = asked.elapsed();
            newest = grabs.record_ids.iter().copied().max();
            if let (Some(top), Some(mark)) = (newest, loaded)
                && top < mark
            {
                tracing::warn!(
                    engine = %id,
                    newest = top,
                    marker = mark,
                    "the grab history's newest record is older than the last read's: the history is read anew"
                );
                loaded = None;
            }
        }
        let end = grabs.records < HISTORY_PAGE as usize
            || (page as i64) * (HISTORY_PAGE as i64) >= grabs.total_records;
        let met = loaded.is_some_and(|mark| grabs.record_ids.iter().any(|r| *r <= mark));
        store_page(&deps.pool, lease, &id, &grabs.download_ids)
            .await
            .map_err(|e| format!("the grab history could not be stored: {e}"))?;
        if met || end {
            complete = true;
            break;
        }
        // A pause between pages: the history table is the arr's largest.
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if complete {
        tracing::info!(engine = %id, pages, "grab history read");
    } else {
        tracing::warn!(
            engine = %id,
            pages,
            "the grab history was read to its most pages before it met records it had: older grabs were not read"
        );
    }
    let known = claims::history_count(&deps.pool, &id)
        .await
        .map_err(|e| format!("the grab history could not be counted: {e}"))?;
    Ok((
        PassOk::new(
            latency,
            json!({ "grabs_known": known, "grab_history_complete": complete, "history_pages": pages }),
        ),
        newest,
    ))
}

/// One page of a grab history, in a transaction that proves the run's lease.
async fn store_page(
    pool: &PgPool,
    lease: &Lease,
    manager: &str,
    ids: &[String],
) -> Result<(), LeaseError> {
    let mut tx = pool.begin().await?;
    lease.prove(&mut tx).await?;
    claims::add_history(&mut tx, manager, ids).await?;
    tx.commit().await?;
    Ok(())
}

/// What one pass of a download client counted: its torrents, or a usenet client's downloads.
pub fn downloads_counts(protocol: Protocol, listed: usize, settled: bool) -> serde_json::Value {
    match protocol {
        Protocol::Torrent => json!({ "torrents": listed, "ownership_settled": settled }),
        Protocol::Usenet => json!({ "downloads": listed, "ownership_settled": settled }),
    }
}

/// Judge one client's listing by what the arrs claim of the ids it holds, then store it
/// ([`reconcile`]).
async fn store_listing(
    deps: &SyncDeps,
    journal: &Journal,
    client: &str,
    listing: ClientListing,
    settled: bool,
) -> Result<DownloadsReport, sqlx::Error> {
    let managers: Vec<String> = deps
        .engines
        .arrs
        .iter()
        .map(|e| e.engine().id.clone())
        .collect();
    let ids: Vec<String> = listing
        .downloads
        .iter()
        .map(|d| d.download_id.clone())
        .collect();
    let mut claims = claims::of_ids(&deps.pool, &managers, &ids).await?;
    claims.complete = settled;
    reconcile(&deps.pool, journal, client, listing, &claims).await
}

/// What a pass writes in the transaction that ends its job, once it succeeded.
pub(crate) enum Last {
    Nothing,
    /// An arr's queue as read: it replaces the arr's queue rows.
    Queue(Vec<QueueItem>),
    /// The marker of the arr's loaded grab history: the newest record id of the read.
    Loaded(i64),
    /// Markers of the integration with the time now.
    Markers(Vec<&'static str>),
    /// What a titles pass changed in the format book. `full_at`: the start of a full file pass.
    Titles {
        full_at: Option<DateTime<Utc>>,
        formats: FormatChanges,
    },
}

/// One run of a pass: its job, the journal its writes go through, the integration it reads and
/// the pass's name.
pub(crate) struct Run<'a> {
    pub(crate) cx: &'a Cx,
    pub(crate) journal: &'a Journal,
    pub(crate) engine: &'a str,
    pub(crate) pass: &'static str,
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

impl SyncJobs {
    /// The arr a job reads.
    fn arr(&self, cx: &Cx) -> Result<Arc<dyn LibraryEngine>, Failed> {
        let id = cx.job.integration.as_deref().unwrap_or_default();
        self.deps
            .engines
            .arrs
            .iter()
            .find(|e| e.engine().id == id)
            .cloned()
            .ok_or_else(|| Failed::from(format!("no manager `{id}` runs in this process")))
    }

    /// The download client a job reads.
    fn client(&self, cx: &Cx) -> Result<Arc<dyn DownloadClient>, Failed> {
        let id = cx.job.integration.as_deref().unwrap_or_default();
        self.deps
            .engines
            .download_clients
            .iter()
            .find(|c| c.id() == id)
            .cloned()
            .ok_or_else(|| Failed::from(format!("no download client `{id}` runs in this process")))
    }

    /// The media server a job reads.
    fn server(&self, cx: &Cx) -> Result<Arc<dyn MediaServer>, Failed> {
        let id = cx.job.integration.as_deref().unwrap_or_default();
        self.deps
            .engines
            .media_servers
            .iter()
            .find(|s| s.id() == id)
            .cloned()
            .ok_or_else(|| Failed::from(format!("no media server `{id}` runs in this process")))
    }

    /// The Seerr site a job reads.
    fn site(&self, cx: &Cx) -> Result<Arc<dyn RequestSite>, Failed> {
        let id = cx.job.integration.as_deref().unwrap_or_default();
        self.deps
            .engines
            .request_sites
            .iter()
            .find(|s| s.id() == id)
            .cloned()
            .ok_or_else(|| Failed::from(format!("no Seerr site `{id}` runs in this process")))
    }

    /// The deps of a run: the journal fenced by its lease.
    fn fenced(&self, cx: &Cx) -> SyncDeps {
        SyncDeps {
            journal: self.deps.journal.fenced(Arc::new(cx.lease.clone())),
            ..self.deps.clone()
        }
    }

    /// A pass ended ([`end_run`]): its job's detail is what the pass counted.
    async fn end(
        &self,
        run: &Run<'_>,
        started: Instant,
        outcome: Result<PassOk, String>,
        last: Last,
    ) -> (bool, Result<Done, Failed>) {
        end_run(
            &self.deps.pool,
            &self.deps.engines,
            run,
            started,
            outcome,
            None,
            last,
        )
        .await
    }
}

/// A pass ended. The metrics and the log hear how it went. One transaction holds the pass's last
/// writes, its pass row (`EngineSet::report_in`) and the end of its job. The run is a check of its
/// integration. `detail` is the job's line when the pass words it itself, else what it counted.
/// The answer holds whether that transaction committed and the run's outcome. A run whose lease
/// is lost writes nothing and counts for nothing.
pub(crate) async fn end_run(
    pool: &PgPool,
    engines: &EngineSet,
    run: &Run<'_>,
    started: Instant,
    outcome: Result<PassOk, String>,
    detail: Option<String>,
    last: Last,
) -> (bool, Result<Done, Failed>) {
    let (engine, pass) = (run.engine, run.pass);
    let took = started.elapsed();
    observe::sync_pass(engine, pass, took, outcome.is_ok());
    if let Err(e) = &outcome {
        tracing::warn!(engine, pass, error = %e, "sync pass failed");
    }
    let mut ended = match &outcome {
        Ok(ok) => Ok(Done::from(detail.unwrap_or_else(|| ok.summary()))),
        Err(e) => Err(Failed::from(e.clone())),
    };
    let committed = match write_end(pool, engines, run, &outcome, last, &ended).await {
        Ok(up) => {
            observe::engine_up(engine, up);
            true
        }
        Err(LeaseError::Lost) => {
            tracing::warn!(
                engine,
                pass,
                "the run's lease is lost: its pass is not recorded"
            );
            return (false, Err(Failed::from(LeaseError::Lost.to_string())));
        }
        // The end of the run is the runtime's to write then. The run counts as failed: the Jobs
        // page and the backoff hear it.
        Err(LeaseError::Db(e)) => {
            tracing::error!(engine, pass, error = %e, "the engine state could not be recorded");
            ended = Err(Failed::from(format!(
                "the end of the pass could not be written: {e}"
            )));
            false
        }
    };
    let detail = match &ended {
        Ok(done) => done.detail.clone(),
        Err(failed) => failed.error.clone(),
    };
    checks::recent().record(
        engine,
        Check {
            at: Utc::now(),
            ok: ended.is_ok(),
            via: pass.to_string(),
            ms: millis(took),
            detail,
        },
    );
    (committed, ended)
}

async fn write_end(
    pool: &PgPool,
    engines: &EngineSet,
    run: &Run<'_>,
    outcome: &Result<PassOk, String>,
    last: Last,
    ended: &Result<Done, Failed>,
) -> Result<bool, LeaseError> {
    let mut tx = run.journal.begin(pool).await?;
    if outcome.is_ok() {
        match last {
            Last::Nothing => {}
            Last::Queue(items) => claims::put_queue(tx.conn(), run.engine, &items).await?,
            Last::Loaded(newest) => claims::mark_loaded(tx.conn(), run.engine, newest).await?,
            Last::Markers(names) => {
                for name in names {
                    claims::mark(tx.conn(), run.engine, name, Utc::now()).await?;
                }
            }
            Last::Titles {
                full_at,
                formats: changes,
            } => {
                if let Some(at) = full_at {
                    claims::mark(tx.conn(), run.engine, claims::FILES_FULL_AT, at).await?;
                }
                formats::write(tx.conn(), run.engine, &changes).await?;
            }
        }
    }
    let up = engines
        .report_in(&mut tx, run.engine, run.pass, outcome)
        .await?;
    tx.finish(&run.cx.lease, ended).await?;
    tx.commit().await?;
    Ok(up)
}

/// `manager.titles`: one pass over an arr's titles ([`titles_pass_with`]), a full file pass when
/// `files.full_at` is as old as its interval. The arr reads its files' custom formats with the
/// book of `manager_file_formats`. What the read changed in the book is written in the transaction
/// that ends the job. Its writes go through a journal fenced by the run's lease.
pub struct TitlesJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for TitlesJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let engine = sync.arr(&cx)?;
        let id = engine.engine().id.clone();
        // The stored copies are assessed again before any titles pass of this start. A stop ends
        // the re-assessment without its signal: the job then waits for the stop. The runtime gives
        // a job a stop cuts short back to its queue, with no result and no backoff. The next start
        // runs it.
        let mut reassessed = sync.reassessed.clone();
        if reassessed.wait_for(|done| *done).await.is_err() {
            std::future::pending::<()>().await;
        }
        let full_at = claims::marked_at(&sync.deps.pool, &id, claims::FILES_FULL_AT)
            .await
            .map_err(|e| {
                Failed::from(format!(
                    "the marker of the file pass could not be read: {e}"
                ))
            })?;
        let full = claims::files_full_due(full_at, sync.deps.config.files_full, Utc::now());
        let deps = sync.fenced(&cx);
        let book = formats::book(&sync.deps.pool, &id)
            .await
            .map_err(|e| Failed::from(format!("the format book could not be read: {e}")))?;
        let (started, at) = (Instant::now(), Utc::now());
        let (outcome, last) = match titles_pass_with(&deps, &engine, full, &book).await {
            Ok((ok, _, formats)) => (
                Ok(ok),
                Last::Titles {
                    full_at: full.then_some(at),
                    formats,
                },
            ),
            Err(e) => (Err(e), Last::Nothing),
        };
        let run = Run {
            cx: &cx,
            journal: &deps.journal,
            engine: &id,
            pass: "titles",
        };
        sync.end(&run, started, outcome, last).await.1
    }
}

/// `manager.queue`: one read of an arr's queue. Its items replace the arr's queue rows in the
/// transaction that ends the job.
pub struct QueueJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for QueueJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let engine = sync.arr(&cx)?;
        let id = engine.engine().id.clone();
        let journal = sync.deps.journal.fenced(Arc::new(cx.lease.clone()));
        let started = Instant::now();
        let (outcome, last) = match engine.queue().await {
            Ok(items) => (
                Ok(PassOk::new(
                    started.elapsed(),
                    json!({ "queue": items.len() }),
                )),
                Last::Queue(items),
            ),
            Err(e) => {
                // A queue that cannot be read is unknown: until it is read again nothing new is
                // called an orphan.
                sync.heard.queue_read(&id, false);
                (Err(e.to_string()), Last::Nothing)
            }
        };
        let read = outcome.is_ok();
        let run = Run {
            cx: &cx,
            journal: &journal,
            engine: &id,
            pass: "queue",
        };
        let (committed, ended) = sync.end(&run, started, outcome, last).await;
        sync.heard.queue_read(&id, read && committed);
        ended
    }
}

/// `manager.history`: an arr's grab history, newest first, to the first page that holds a record
/// the last read that ended well saw ([`history_pass`]), at most `jobs.historyPages` pages. The
/// marker (`history.loaded`) is written in the transaction that ends the job.
pub struct HistoryJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for HistoryJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let engine = sync.arr(&cx)?;
        let id = engine.engine().id.clone();
        let journal = sync.deps.journal.fenced(Arc::new(cx.lease.clone()));
        let loaded = claims::loaded_id(&sync.deps.pool, &id).await.map_err(|e| {
            Failed::from(format!(
                "the marker of the grab history could not be read: {e}"
            ))
        })?;
        let started = Instant::now();
        let pages = sync.deps.config.history_max_pages;
        let passed = history_pass(&sync.deps, &engine, &cx.lease, loaded, pages).await;
        let (outcome, last) = match passed {
            Ok((ok, Some(newest))) => (Ok(ok), Last::Loaded(newest)),
            Ok((ok, None)) => (Ok(ok), Last::Nothing),
            Err(e) => (Err(e), Last::Nothing),
        };
        let read = outcome.is_ok();
        let run = Run {
            cx: &cx,
            journal: &journal,
            engine: &id,
            pass: "history",
        };
        let (committed, ended) = sync.end(&run, started, outcome, last).await;
        if read && committed {
            sync.heard.history_read(&id);
        }
        ended
    }
}

/// `download.listing`: one read of a download client. Each download is judged by what the arrs
/// claim of its id (`download_claims`). It is an orphan only once every arr was heard since this
/// start ([`Heard::settled`]).
pub struct DownloadsJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for DownloadsJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let client = sync.client(&cx)?;
        let id = client.id().to_string();
        let journal = sync.deps.journal.fenced(Arc::new(cx.lease.clone()));
        let started = Instant::now();
        let outcome = match client.listing().await {
            Ok(listing) => {
                let latency = started.elapsed();
                let settled = sync.heard.settled(&sync.deps.engines);
                match store_listing(&sync.deps, &journal, &id, listing, settled).await {
                    Ok(r) => Ok(PassOk::new(
                        latency,
                        downloads_counts(client.protocol(), r.listed, settled),
                    )),
                    Err(e) => Err(format!("the downloads could not be stored: {e}")),
                }
            }
            Err(e) => Err(e.to_string()),
        };
        let run = Run {
            cx: &cx,
            journal: &journal,
            engine: &id,
            pass: "downloads",
        };
        sync.end(&run, started, outcome, Last::Nothing).await.1
    }
}

/// `server.pass`: one pass over a media server ([`super::server::server_pass`]): its sections,
/// items, ratings, plays and music. Its writes go through a journal fenced by the run's lease.
pub struct ServerJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for ServerJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let server = sync.server(&cx)?;
        let id = server.id().to_string();
        let deps = sync.fenced(&cx);
        let started = Instant::now();
        let outcome = super::server::server_pass(&deps, &server).await;
        let run = Run {
            cx: &cx,
            journal: &deps.journal,
            engine: &id,
            pass: "server",
        };
        sync.end(&run, started, outcome, Last::Nothing).await.1
    }
}

/// `requests.pass`: one read of a Seerr site's requests ([`requests_pass`]). Its writes go
/// through a journal fenced by the run's lease.
pub struct RequestsJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for RequestsJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let site = sync.site(&cx)?;
        let id = site.id().to_string();
        let journal = sync.deps.journal.fenced(Arc::new(cx.lease.clone()));
        let started = Instant::now();
        let outcome = requests_pass(&sync.deps.pool, &journal, &site, PassConfig::default())
            .await
            .map(|(ok, _)| ok);
        let run = Run {
            cx: &cx,
            journal: &journal,
            engine: &id,
            pass: "requests",
        };
        sync.end(&run, started, outcome, Last::Nothing).await.1
    }
}

/// `requests.accounts`: one read of a Seerr site's accounts ([`users_pass`]): who may sign in
/// to Singularr Requests. It writes no pass row, as the loop wrote none. Its rows replace what the
/// site no longer lists. A run that lost its lease writes the same rows the next run writes.
pub struct AccountsJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for AccountsJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let site = sync.site(&cx)?;
        match users_pass(&sync.deps.pool, &site, &sync.rules).await {
            Ok(r) => Ok(Done::from(format!(
                "{}, {} removed",
                units::counted(r.read as i64, "account", "accounts"),
                units::count(r.removed as i64)
            ))),
            Err(e) => {
                tracing::warn!(site = %site.id(), error = %e, "the accounts were not read");
                Err(Failed::from(e.to_string()))
            }
        }
    }
}

/// `requests.discover`: the discover lists of the first Seerr site, as the owner's Discover page
/// asks them. A run whose marker (`discover.at`) is younger than the lists' life reads nothing:
/// its next run comes as they expire. A pass that stored a list writes the marker in the commit
/// that ends its job.
pub struct DiscoverJob(pub Arc<SyncJobs>);

#[async_trait]
impl Handler for DiscoverJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let sync = &self.0;
        let site = sync.site(&cx)?;
        let id = site.id().to_string();
        let stored_at = claims::marked_at(&sync.deps.pool, &id, DISCOVER_AT)
            .await
            .map_err(|e| Failed::from(format!("the marker of the lists could not be read: {e}")))?;
        // Fresh lists: the next run comes as they expire.
        let left = stored_at.and_then(|at| {
            let age = (Utc::now() - at).to_std().unwrap_or_default();
            discover::TTL
                .checked_sub(age)
                .filter(|left| !left.is_zero())
        });
        if let Some(left) = left {
            return Ok(Done {
                detail: "Lists fresh".to_string(),
                again: Some(left),
                ..Done::default()
            });
        }
        let at = Utc::now();
        let stored = discover_pass(&sync.deps.pool, &site).await;
        let done: Result<Done, Failed> =
            Ok(Done::from(units::counted(stored as i64, "list", "lists")));
        let journal = sync.deps.journal.fenced(Arc::new(cx.lease.clone()));
        let ended = async {
            let mut tx = journal.begin(&sync.deps.pool).await?;
            // A pass that stored no list leaves the marker: the next run reads them again.
            if stored > 0 {
                claims::mark(tx.conn(), &id, DISCOVER_AT, at).await?;
            }
            tx.finish(&cx.lease, &done).await?;
            tx.commit().await?;
            Ok::<(), LeaseError>(())
        }
        .await;
        match ended {
            Ok(()) => done,
            Err(LeaseError::Lost) => Err(Failed::from(LeaseError::Lost.to_string())),
            Err(LeaseError::Db(e)) => {
                tracing::warn!(site = %id, error = %e, "the marker of the lists was not written");
                done
            }
        }
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
    let downloads = super::store::counts(pool).await?;
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

/// The key of the movie folders' scan in the job runtime.
pub const MOVIE_FOLDERS_JOB: &str = "folder:movie-folders";

/// The movie folders' job: the read-only scan for files no engine tracks. It runs whether or not
/// the sync does: the folders are mounts. Its writes go through a journal fenced by the run's
/// lease.
pub struct FolderScan {
    pub pool: PgPool,
    pub journal: Journal,
    pub editions: Arc<singularr_core::EditionList>,
    pub roots: Vec<String>,
}

#[async_trait]
impl Handler for FolderScan {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let journal = self.journal.fenced(Arc::new(cx.lease));
        match crate::catalog::scan::scan(&self.pool, &journal, &self.editions, &self.roots).await {
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
                Ok(Done::from(format!(
                    "{}, {} untracked, {} new, {} gone",
                    units::counted(r.folders as i64, "folder", "folders"),
                    units::count(r.untracked as i64),
                    units::count(r.found as i64),
                    units::count(r.gone as i64)
                )))
            }
            Err(e) => {
                tracing::warn!(error = %e, "the movie folder scan failed");
                Err(Failed::from(e.to_string()))
            }
        }
    }
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
