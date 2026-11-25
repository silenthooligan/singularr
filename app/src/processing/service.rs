//! The processing pass, and how stored runs are read.
//!
//! The pass asks every processing service for its state and its jobs (GET, through its port),
//! links each job to the title whose folder its files lie in, and writes the jobs that are new
//! or changed. A job the service no longer lists stays: the table is the history. A file that
//! lies in no title's folder (its title moved since the run) is linked by the ids its path
//! names: `{tmdb-N}` or `{tvdb-N}` in a folder's name, then `{imdb-ttN}`.
//!
//! A pass reads at one of two cadences ([`Read`]). Every pass asks for the newest jobs alone,
//! which is light for the service. The whole list is asked for on the first pass of a process
//! and then when the last whole read is older than [`WHOLE_READ_EVERY`]; a [`Reader`] keeps
//! that time in memory, one per service.
//!
//! A service lists its newest jobs and no more than so many at once. Where it can be asked
//! for more, two things keep the table whole: every pass asks it by id for the stored open
//! runs its read did not carry, so an old open run closes within a pass instead of never; and
//! a whole read that came back full reads on, page by page, into the older jobs.
//!
//! One thing is done to a service: a file is handed to one of its flows ([`send`]), when the
//! owner asks for it and `core::outbound` lets the call through.
//!
//! What is being worked on is never stored. It is asked of the services when a page needs it
//! ([`live`]) and joined to the stored runs when they are read ([`Reading`]): a staged run a
//! worker is on is `working`, and an open run that started long ago with no worker on it is
//! stuck. A worker is on a run when its file is the run's queue path, or when it names the
//! footprint that the worker's own record of the run's queue file carries: a worker that runs
//! a flow works on a copy of the file in its own cache folder.
//!
//! A worker service has records of its own. Its libraries and flows are configuration: the
//! pass has the adapter read them with every whole read and keep them, and a run is placed in
//! the library whose folder its queue path lies below ([`placed`]). Where the file of an open
//! run stands in the worker's records is asked with the live state ([`live_files`]), never
//! stored, and says why a stuck run is stuck.

use super::model::{
    FlowStat, IMPORTED, Link, LiveFile, LiveWorker, Run, STUCK_AFTER, STUCK_AFTER_DEFAULT,
    file_name, fingerprint, is_stuck, queue_of, run_id, saved, shown_state, stuck_why, took,
    worker_by_footprint, worker_of,
};
use super::store;
use crate::catalog::store as catalog_store;
use crate::core::jobs::{self, JobSpec};
use crate::core::journal::{Journal, event};
use crate::core::observe;
use crate::core::units;
use crate::engines::service::{EngineSet, PassOk};
use crate::engines::{
    Acted, EngineError, FileAnswer, Processor, ProcessorFlow, ProcessorRun, ProcessorStatus,
    Refusal, SENDER, Sent, TriggerWords, WorkerAct, WorkerFile, WorkerFlow, WorkerLibrary,
    WorkerReport,
};
use crate::lifecycle::sync::jitter;
use crate::metadata::record::{
    EpisodeRef, ProcessingRun, RunEvent, RunFile, RunFlow, RunInWorker, RunLibrary, RunReplaced,
    RunTrigger, RunWorker,
};
use crate::settings::Settings;
use chrono::{DateTime, Utc};
use futures::future::join_all;
use serde_json::json;
use singularr_core::naming::parse_library_name;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// The name of the pass, in jobs, metrics and the journal.
pub const PASS: &str = "processing";
/// Jobs asked of a service on an ordinary pass: its newest.
pub const RUNS_NEWEST: u32 = 60;
/// Jobs asked of a service when its whole list is read: as many as a service gives at once.
pub const RUNS_LIMIT: u32 = 2000;
/// How long a read of a service's whole list stands before the next one.
///
/// A service lists its jobs newest first by when they started, so the newest few hold every
/// new job and every change to a recent one, and a small service answers them in a fraction of
/// a second. Its whole list is megabytes that take it seconds to write, and it has other work
/// in those seconds. So the whole list is read on the first pass of a process, which fills the
/// table, and after that only this often. The whole read is what sees an older job change: an
/// open run that newer jobs pushed out of the newest few closes in the table at the next whole
/// read, so this is also the longest such a run still reads as open.
pub const WHOLE_READ_EVERY: Duration = Duration::from_secs(30 * 60);
/// A stored run's `last_seen_at` moves at most this often, so a pass that finds nothing new
/// writes next to nothing.
const SEEN_STEP_SECS: i64 = 600;
/// The files of open runs a worker is asked about for one live answer, at most: the oldest
/// runs first, since those are the ones that may be stuck.
pub const FILES_ASKED: usize = 60;
/// Questions about files that are with a worker at once.
const FILES_AT_ONCE: usize = 8;
/// Pages of older jobs one whole read follows after a listing that came back full, at most.
pub const OLDER_PAGES: usize = 10;
/// Stored open runs a pass asks its service about by id, at most: the oldest first.
pub const OPEN_FOLLOWED: i64 = 400;
/// What a service is in the pipeline when it does the work itself.
const WORKER: &str = "worker";

/// Which of a service's jobs a pass reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// The newest few: every ordinary pass.
    Newest,
    /// Every job the service lists.
    Whole,
}

impl Read {
    /// How many jobs the read asks for.
    pub fn limit(self) -> u32 {
        match self {
            Read::Newest => RUNS_NEWEST,
            Read::Whole => RUNS_LIMIT,
        }
    }
}

/// The catalog's title folders, to find the title a file lies in.
#[derive(Debug, Clone, Default)]
pub struct Folders(HashMap<String, i64>);

impl Folders {
    /// From (title id, folder) as the managers name the folders.
    pub fn new(rows: impl IntoIterator<Item = (i64, String)>) -> Folders {
        Folders(
            rows.into_iter()
                .filter_map(|(id, path)| {
                    let folder = path.trim().trim_end_matches(['/', '\\']);
                    (!folder.is_empty()).then(|| (folder.to_string(), id))
                })
                .collect(),
        )
    }

    /// The title whose folder holds this file: the nearest folder above it that is a title's.
    pub fn title_of(&self, path: &str) -> Option<i64> {
        let mut at = path.trim();
        while let Some(cut) = at.rfind(['/', '\\']) {
            at = &at[..cut];
            if let Some(id) = self.0.get(at) {
                return Some(*id);
            }
        }
        None
    }
}

/// The season and the first episode a file's name reads as.
pub fn episode_of(path: &str) -> Option<(i32, i32)> {
    let parsed = singularr_core::release::parse(file_name(path));
    let season = parsed.season?;
    let episode = parsed.episodes.first()?;
    Some((i32::from(season), i32::from(*episode)))
}

/// The ids a path carries in its own names, as the managers and the media servers write them:
/// `{tmdb-N}` or `{tvdb-N}` in the name of a folder, `{imdb-ttN}` in any name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathIds {
    pub tmdb: Option<i64>,
    pub tvdb: Option<i64>,
    /// In lower case: `tt0320661`.
    pub imdb: Option<String>,
}

/// Read the ids a path names. The folders are asked from the file upward, since a title's own
/// folder lies below the folders of its library.
pub fn path_ids(path: &str) -> PathIds {
    let mut names: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|n| !n.trim().is_empty())
        .collect();
    let file = names.pop();
    let mut out = PathIds {
        imdb: file.and_then(|f| parse_library_name(f).imdb),
        ..PathIds::default()
    };
    for folder in names.iter().rev() {
        let name = parse_library_name(folder);
        if out.tmdb.is_none() && out.tvdb.is_none() {
            out.tmdb = name.tmdb.and_then(|n| i64::try_from(n).ok());
            out.tvdb = name.tvdb.and_then(|n| i64::try_from(n).ok());
        }
        if out.imdb.is_none() {
            out.imdb = name.imdb;
        }
    }
    out.imdb = out.imdb.map(|i| i.to_lowercase());
    out
}

/// The catalog's titles by the ids paths name, to find the title of a file that no title's
/// folder holds: a title whose folder moved since the run.
#[derive(Debug, Clone, Default)]
pub struct Named {
    /// By (is a show, TMDB id): a movie and a show may carry the same number.
    tmdb: HashMap<(bool, i64), i64>,
    /// Shows by TVDB id.
    tvdb: HashMap<i64, i64>,
    /// By (is a show, IMDb id).
    imdb: HashMap<(bool, String), i64>,
}

impl Named {
    pub fn new(titles: impl IntoIterator<Item = catalog_store::TitleIds>) -> Named {
        let mut out = Named::default();
        for t in titles {
            let show = t.kind == "show";
            if let Some(n) = t.tmdb_id {
                out.tmdb.entry((show, n)).or_insert(t.id);
            }
            if let (true, Some(n)) = (show, t.tvdb_id) {
                out.tvdb.entry(n).or_insert(t.id);
            }
            if let Some(i) = t.imdb_id {
                out.imdb.entry((show, i.to_lowercase())).or_insert(t.id);
            }
        }
        out
    }

    /// The title a path names: by the TMDB or TVDB id in a folder's name first, then by an
    /// IMDb id anywhere in it. A TVDB id names a show. For the other two the kind is the
    /// file's: a file whose name reads as an episode belongs to a show, any other to a movie.
    pub fn title_of(&self, path: &str) -> Option<i64> {
        let ids = path_ids(path);
        let show = episode_of(path).is_some();
        ids.tvdb
            .and_then(|n| self.tvdb.get(&n))
            .or_else(|| ids.tmdb.and_then(|n| self.tmdb.get(&(show, n))))
            .or_else(|| ids.imdb.and_then(|i| self.imdb.get(&(show, i))))
            .copied()
    }
}

/// The paths a job names that say where it belongs: its source and its result.
fn job_paths(run: &ProcessorRun) -> Vec<&str> {
    [run.source.as_ref(), run.result.as_ref()]
        .into_iter()
        .flatten()
        .map(|f| f.path.as_str())
        .collect()
}

/// Where a job belongs: the title whose folder its source or its result lies in, else the
/// title one of the two paths names by an id, and the episode the name of one of them reads
/// as.
pub fn link(run: &ProcessorRun, folders: &Folders, named: &Named) -> Link {
    let paths = job_paths(run);
    let (season, episode) = paths.iter().find_map(|p| episode_of(p)).unzip();
    Link {
        title_id: paths
            .iter()
            .find_map(|p| folders.title_of(p))
            .or_else(|| paths.iter().find_map(|p| named.title_of(p))),
        season,
        episode,
    }
}

/// The titles that carry the ids named by the paths of the jobs no title's folder holds,
/// asked with one query for all of them.
async fn named_titles(
    pool: &PgPool,
    jobs: &[ProcessorRun],
    folders: &Folders,
) -> Result<Named, sqlx::Error> {
    let (mut tmdb, mut tvdb, mut imdb) = (Vec::new(), Vec::new(), Vec::new());
    for job in jobs {
        let paths = job_paths(job);
        if paths.iter().any(|p| folders.title_of(p).is_some()) {
            continue;
        }
        for ids in paths.into_iter().map(path_ids) {
            tmdb.extend(ids.tmdb);
            tvdb.extend(ids.tvdb);
            imdb.extend(ids.imdb);
        }
    }
    for ids in [&mut tmdb, &mut tvdb] {
        ids.sort_unstable();
        ids.dedup();
    }
    imdb.sort_unstable();
    imdb.dedup();
    Ok(Named::new(
        catalog_store::titles_with_ids(pool, &tmdb, &tvdb, &imdb).await?,
    ))
}

/// What a whole read left of a service's jobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rest {
    /// Nothing: the read holds every job the service has.
    Nothing,
    /// The listing came back full, and the service gives no more than one listing.
    NotGiven,
    /// Jobs older than one whole read goes back to.
    Left,
    /// Reading on failed.
    Failed,
}

/// What one pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassReport {
    /// Which of the service's jobs were asked for.
    pub read: Read,
    /// Jobs the service listed.
    pub listed: usize,
    /// Jobs older than the listing, read page by page after a whole read that came back
    /// full.
    pub older: usize,
    /// What a whole read left unread.
    pub rest: Rest,
    /// Stored open runs the read did not carry, which the service gave when asked by id.
    pub followed: usize,
    pub added: usize,
    /// Stored runs written again: the service changed them, or their link moved.
    pub updated: usize,
    /// Jobs read (listed, and older) that are linked to a title.
    pub linked: usize,
    /// Jobs read that are open.
    pub open: usize,
}

impl PassReport {
    /// How many runs the pass read, counts only, for the pass's job: `1,019 runs` when it read
    /// the service's whole list, `60 newest runs` when it did not (a newest read; a whole read
    /// the service cut short or that goes no further back; a page that failed, which is said
    /// after the line), and the older open runs it followed by id.
    pub fn line(&self) -> String {
        let read = (self.listed + self.older) as i64;
        let mut line = match (self.read, self.rest) {
            (Read::Whole, Rest::Nothing) => units::counted(read, "run", "runs"),
            _ => units::counted(read, "newest run", "newest runs"),
        };
        if self.followed > 0 {
            line.push_str(&format!(
                ", {} older open",
                units::count(self.followed as i64)
            ));
        }
        line
    }
}

/// A pass that succeeded.
#[derive(Debug, Clone)]
pub struct Passed {
    /// For the engine state: the latency, the version and the counts.
    pub ok: PassOk,
    pub report: PassReport,
    /// What was read, in a line, for the pass's job.
    pub detail: String,
    /// What the service answers and the pass could not read all the same (a worker's own
    /// records, a bridge's older jobs or its jobs by id), in words.
    pub unread: Option<String>,
    /// A whole read that could not read what only a whole read reads, the service's own
    /// configuration: it does not stand, and the next pass makes it again.
    pub again: bool,
    /// The runs this pass saw close: stored open, listed closed ([`journal_closed`]).
    pub closed: Vec<Run>,
}

/// What storing a list of jobs did.
#[derive(Debug, Clone, Default)]
struct Stored {
    added: usize,
    updated: usize,
    linked: usize,
    open: usize,
    /// Runs that were stored open and are listed closed.
    closed: Vec<Run>,
}

/// Jobs of one service into the table: each linked to the title whose folder its files lie
/// in, and written when it is new, or when the service or its link changed it. `folders` is
/// the catalog's title folders, read the first time they are needed and kept for the caller's
/// next list.
async fn store_jobs(
    pool: &PgPool,
    integration: &str,
    jobs: &[ProcessorRun],
    folders: &mut Option<Folders>,
) -> Result<Stored, String> {
    let mut out = Stored::default();
    if jobs.is_empty() {
        return Ok(out);
    }
    if folders.is_none() {
        *folders = Some(Folders::new(
            catalog_store::title_folders(pool)
                .await
                .map_err(|e| e.to_string())?,
        ));
    }
    let Some(folders) = folders.as_ref() else {
        return Ok(out);
    };
    let named = named_titles(pool, jobs, folders)
        .await
        .map_err(|e| e.to_string())?;
    let now = Utc::now();
    let rows: Vec<(Run, String, &ProcessorRun)> = jobs
        .iter()
        .map(|job| {
            let run = Run::listed(integration, job, link(job, folders, &named), now);
            (run, fingerprint(job), job)
        })
        .collect();
    out.linked = rows.iter().filter(|(r, ..)| r.title_id.is_some()).count();
    out.open = rows.iter().filter(|(r, ..)| r.is_open()).count();
    let ids: Vec<String> = rows.iter().map(|(r, ..)| r.id.clone()).collect();
    let known: HashMap<String, store::Known> = store::known(pool, &ids)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|k| (k.id.clone(), k))
        .collect();
    let stale = now - chrono::Duration::seconds(SEEN_STEP_SECS);
    let mut write: Vec<&(Run, String, &ProcessorRun)> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for row in &rows {
        let (run, print, _) = row;
        match known.get(&run.id) {
            None => {
                out.added += 1;
                write.push(row);
            }
            Some(k)
                if k.fingerprint != *print
                    || k.title_id != run.title_id
                    || k.season_number != run.season
                    || k.episode_number != run.episode =>
            {
                out.updated += 1;
                if super::model::is_open(&k.state) && !run.is_open() {
                    out.closed.push(run.clone());
                }
                write.push(row);
            }
            Some(k) if k.last_seen_at < stale => seen.push(run.id.clone()),
            Some(_) => {}
        }
    }
    if !write.is_empty() || !seen.is_empty() {
        let stored = async {
            let mut tx = pool.begin().await?;
            for (run, print, job) in write {
                store::upsert(&mut tx, run, print, &job.raw).await?;
            }
            store::touch(&mut tx, &seen).await?;
            tx.commit().await
        };
        stored
            .await
            .map_err(|e: sqlx::Error| format!("the runs could not be stored: {e}"))?;
    }
    Ok(out)
}

/// One pass over one processing service: whether it answers, and the jobs `read` asks for
/// into the table; then, where the service can be asked for them, the jobs older than a
/// whole listing that came back full, and the stored open runs the read did not carry.
pub async fn pass(
    pool: &PgPool,
    processor: &Arc<dyn Processor>,
    read: Read,
) -> Result<Passed, String> {
    let started = Instant::now();
    let status = processor.status().await.map_err(|e| e.to_string())?;
    // The status call is small: its round trip is the service's latency.
    let took = started.elapsed();
    let service = processor.id();
    // A worker's own records: its libraries and flows with the whole read, its totals with
    // every pass. A service that answers and does not give them is not down: what was read
    // before stays, and the pass says what it could not read.
    let mut unread: Vec<String> = Vec::new();
    let mut again = false;
    if read == Read::Whole
        && let Err(e) = processor.read_setup().await
    {
        again = true;
        unread.push(format!("Libraries and flows not read: {e}"));
    }
    if let Err(e) = processor.read_totals().await {
        unread.push(format!("Totals not read: {e}"));
    }
    if let Err(e) = processor.read_queue().await {
        unread.push(format!("Queue not read: {e}"));
    }
    let listed = processor
        .runs(read.limit())
        .await
        .map_err(|e| e.to_string())?;
    let mut folders: Option<Folders> = None;
    let stored = store_jobs(pool, service, &listed, &mut folders).await?;
    let mut report = PassReport {
        read,
        listed: listed.len(),
        older: 0,
        rest: Rest::Nothing,
        followed: 0,
        added: stored.added,
        updated: stored.updated,
        linked: stored.linked,
        open: stored.open,
    };
    let mut closed = stored.closed;
    // The jobs this pass read, by the service's ids of them.
    let mut carried: HashSet<String> = listed.iter().map(|j| j.job_id.clone()).collect();

    // A whole listing that came back full: the service has older jobs. It is asked for them
    // page by page from the oldest job of the listing on, until it says there is none.
    if read == Read::Whole && listed.len() >= RUNS_LIMIT as usize {
        report.rest = Rest::NotGiven;
        let mut before = listed.last().map(|j| j.job_id.clone());
        for _ in 0..OLDER_PAGES {
            let Some(at) = before.take() else {
                break;
            };
            match processor.jobs_before(&at, RUNS_LIMIT).await {
                // The service gives no more than its listing.
                Ok(None) => break,
                Err(e) => {
                    report.rest = Rest::Failed;
                    unread.push(format!("Older runs not read: {e}"));
                    break;
                }
                Ok(Some(page)) => {
                    let stored = store_jobs(pool, service, &page.jobs, &mut folders).await?;
                    report.older += page.jobs.len();
                    report.added += stored.added;
                    report.updated += stored.updated;
                    report.linked += stored.linked;
                    report.open += stored.open;
                    closed.extend(stored.closed);
                    carried.extend(page.jobs.iter().map(|j| j.job_id.clone()));
                    report.rest = match page.next {
                        None => Rest::Nothing,
                        Some(next) => {
                            // A page that names itself as the next would never end.
                            if next != at {
                                before = Some(next);
                            }
                            Rest::Left
                        }
                    };
                }
            }
        }
    }

    // The open runs the table holds that this read did not carry: the service is asked for
    // them by id, so a run that newer jobs pushed out of its listing closes within a pass.
    let open_ids = store::open_job_ids(pool, service, OPEN_FOLLOWED)
        .await
        .map_err(|e| e.to_string())?;
    let ask: Vec<String> = open_ids
        .into_iter()
        .filter(|id| !carried.contains(id))
        .collect();
    if !ask.is_empty() {
        match processor.jobs_by_id(&ask).await {
            // It cannot be asked by id: those runs wait for a whole read.
            Ok(None) => {}
            Ok(Some(found)) => {
                let stored = store_jobs(pool, service, &found.jobs, &mut folders).await?;
                report.followed = found.jobs.len();
                report.added += stored.added;
                report.updated += stored.updated;
                closed.extend(stored.closed);
                if !found.missing.is_empty() {
                    tracing::warn!(
                        service,
                        missing = found.missing.len(),
                        "the service has no job for stored open runs"
                    );
                }
            }
            Err(e) => unread.push(format!("Open runs not read: {e}")),
        }
    }
    if !unread.is_empty() {
        tracing::warn!(service, unread = %unread.join("; "), "the processing service did not give everything the pass reads");
    }
    if report.added + report.updated > 0 {
        tracing::info!(
            service,
            read = ?read,
            listed = report.listed,
            older = report.older,
            followed = report.followed,
            added = report.added,
            updated = report.updated,
            linked = report.linked,
            "processing runs updated"
        );
    }
    // What the pass counted, for the integration's line: the runs of a service that lists
    // jobs, as the table holds them now (the pass may have read the newest alone), and a
    // worker's nodes.
    let lists_jobs = processor.role() == "bridge" || report.listed > 0;
    let mut counts = serde_json::Map::new();
    if lists_jobs {
        let (runs, open) = store::totals(pool, service)
            .await
            .map_err(|e| e.to_string())?;
        counts.insert("runs".to_string(), json!(runs));
        counts.insert("runs_open".to_string(), json!(open));
    }
    if let Some(nodes) = &status.nodes {
        counts.insert("nodes".to_string(), json!(nodes.len()));
        counts.insert(
            "workers_busy".to_string(),
            json!(nodes.iter().map(|n| n.busy).sum::<i64>()),
        );
    }
    let ok = PassOk {
        took,
        version: status.version,
        counts: serde_json::Value::Object(counts),
    };
    let mut detail = if lists_jobs {
        report.line()
    } else {
        ok.summary()
    };
    let unread = (!unread.is_empty()).then(|| unread.join("; "));
    if let Some(unread) = &unread {
        detail = format!("{detail}; {unread}");
    }
    Ok(Passed {
        ok,
        report,
        detail,
        unread,
        again,
        closed,
    })
}

/// A file to hand to a flow of a service.
#[derive(Debug, Clone, Copy)]
pub struct Sending<'a> {
    /// Who asks, as the journal names them.
    pub actor: &'a str,
    /// The catalog title the file belongs to.
    pub title: &'a catalog_store::TitleRef,
    /// The whole path of the file, as its manager names it.
    pub path: &'a str,
    /// The flow, by the id a person chooses it under.
    pub flow: &'a str,
}

/// A file a service took.
#[derive(Debug, Clone, PartialEq)]
pub struct Handed {
    /// What the service runs, by its own id of it.
    pub mode: Option<String>,
    /// The service's id of the job it opened, when it says.
    pub job: Option<String>,
    /// The run as stored: the service was asked for the job and it was written at once.
    /// `None` when the service named no job or could not be asked for it; the next pass
    /// finds the job then.
    pub run: Option<Run>,
}

/// How handing a file to a flow ended.
#[derive(Debug, Clone, PartialEq)]
pub enum SendOutcome {
    Taken(Box<Handed>),
    /// The service takes no file on demand.
    NotOffered,
    /// The service did not answer in time. It may still stage the file: the next pass finds
    /// the job if it did. The call is not made again.
    Unanswered,
    /// The service said no.
    Refused {
        why: Refusal,
        /// Its reason, in its own word.
        reason: Option<String>,
        /// Its own words for it.
        detail: Option<String>,
        /// Its id of the job that has the file.
        job: Option<String>,
    },
}

/// Hand one file to one flow of a service: the one thing Singularr does to a processing
/// service. The caller decides whether it may be asked for (the owner, a title that is not
/// protected); `core::outbound` lets the call through only while the integration's access is
/// `act`. After a good send the service is asked for the job it opened and the run is stored
/// at once, so it is on every page before the next pass, and a `processing_sent` event says
/// who sent what.
pub async fn send(
    pool: &PgPool,
    journal: &Journal,
    processor: &Arc<dyn Processor>,
    what: Sending<'_>,
) -> Result<SendOutcome, EngineError> {
    let (mode, job) = match processor.send(what.path, what.flow, SENDER).await? {
        Sent::Staged { mode, job, .. } => (mode, job),
        Sent::NotOffered => return Ok(SendOutcome::NotOffered),
        Sent::Unanswered => return Ok(SendOutcome::Unanswered),
        Sent::Refused {
            why,
            reason,
            detail,
            job,
        } => {
            return Ok(SendOutcome::Refused {
                why,
                reason,
                detail,
                job,
            });
        }
    };
    let service = processor.id();
    // The job the service opened, into the table now. A service that cannot be asked for it,
    // or does not give it, costs the answer its run, not the send: the next pass reads it.
    let mut run = None;
    if let Some(job) = &job {
        match processor.jobs_by_id(std::slice::from_ref(job)).await {
            Ok(Some(found)) => {
                let mut folders = None;
                match store_jobs(pool, service, &found.jobs, &mut folders).await {
                    Ok(_) => match store::one(pool, &run_id(service, job)).await {
                        Ok(stored) => run = stored,
                        Err(e) => {
                            tracing::warn!(service, job = %job, error = %e, "the run of a file that was sent could not be read back");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(service, job = %job, error = %e, "the run of a file that was sent could not be stored");
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(service, job = %job, error = %e, "the job of a file that was sent could not be read");
            }
        }
    }
    let mut body = json!({
        "title_id": what.title.id,
        "title": what.title.title,
        "kind": what.title.kind,
        "tmdb_id": what.title.tmdb_id,
        "file": file_name(what.path),
        "flow": what.flow,
        "flow_label": processor.flow_label(what.flow),
        "mode": mode,
        "service": service,
        "job": job,
        "run": run.as_ref().map(|r| r.id.clone()),
    });
    if what.title.kind == "show"
        && let Some((season, episode)) = episode_of(what.path)
    {
        body["season"] = json!(season);
        body["episodes"] = json!([episode]);
    }
    // The file is with the service whatever happens here: a journal that cannot be written
    // is logged, and the send stands.
    let journaled: Result<(), sqlx::Error> = async {
        let mut tx = journal.begin(pool).await?;
        tx.append_one(event(
            "processing_sent",
            "title",
            what.title.id,
            what.actor,
            body,
        ))
        .await?;
        tx.commit().await?;
        Ok(())
    }
    .await;
    if let Err(e) = journaled {
        tracing::error!(service, error = %e, "a file was sent and the journal could not record it");
    }
    Ok(SendOutcome::Taken(Box::new(Handed { mode, job, run })))
}

/// One processing service, and when its whole list was last read.
///
/// The time is kept in memory and nowhere else: a process that starts reads every list whole
/// once, which is also what fills an empty table.
pub struct Reader {
    processor: Arc<dyn Processor>,
    whole_at: Option<Instant>,
}

impl Reader {
    pub fn new(processor: Arc<dyn Processor>) -> Reader {
        Reader {
            processor,
            whole_at: None,
        }
    }

    /// Which read a pass that starts at `now` makes: the whole list when it was never read by
    /// this process or the last whole read is [`WHOLE_READ_EVERY`] old, else the newest jobs.
    pub fn read_at(&self, now: Instant) -> Read {
        match self.whole_at {
            Some(at) if now.saturating_duration_since(at) < WHOLE_READ_EVERY => Read::Newest,
            _ => Read::Whole,
        }
    }

    /// One pass that starts at `now`. A whole read stands only when the pass succeeded and
    /// read what only a whole read reads, so one that failed, or could not read the service's
    /// own configuration, is made again by the next pass.
    pub async fn pass(&mut self, pool: &PgPool, now: Instant) -> Result<Passed, String> {
        let read = self.read_at(now);
        let passed = pass(pool, &self.processor, read).await?;
        if read == Read::Whole && !passed.again {
            self.whole_at = Some(now);
        }
        Ok(passed)
    }
}

#[derive(Clone)]
pub struct ProcessingDeps {
    pub pool: PgPool,
    pub journal: Journal,
    pub engines: Arc<EngineSet>,
    pub interval: Duration,
    pub jitter: Duration,
    /// For the stuck threshold.
    pub settings: Arc<Settings>,
    /// The policy in force: the traits a processing rule can name.
    pub policy: crate::policy::PolicyHandle,
}

/// How often a service's open runs are looked at for ones that went stuck.
pub const STUCK_LOOK_EVERY: Duration = Duration::from_secs(300);
/// Open runs one look takes at most, the oldest first.
const STUCK_LOOKED: i64 = 50;

/// The titles of these runs, by catalog id.
async fn titles_of(
    pool: &PgPool,
    runs: &[&Run],
) -> Result<HashMap<i64, catalog_store::TitleRef>, sqlx::Error> {
    let mut ids: Vec<i64> = runs.iter().filter_map(|r| r.title_id).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(catalog_store::title_refs(pool, &ids)
        .await?
        .into_iter()
        .map(|t| (t.id, t))
        .collect())
}

/// A journal event about a run: on its title when it has one, else on the run itself. Its
/// actor is the service's pass.
fn run_event(
    kind: &str,
    run: &Run,
    title: Option<&catalog_store::TitleRef>,
    flow_label: String,
) -> crate::core::journal::NewEvent {
    let file = run
        .source_path
        .as_deref()
        .or(run.result_path.as_deref())
        .map(file_name);
    let mut body = json!({
        "run": run.id,
        "service": run.integration,
        "flow": run.flow,
        "flow_label": flow_label,
        "state": run.state,
        "file": file,
        "title_id": run.title_id,
        "title": title.map(|t| t.title.clone()),
        "kind": title.map(|t| t.kind.clone()),
        "tmdb_id": title.and_then(|t| t.tmdb_id),
        "detail": run.detail,
        "saved": saved(run),
        "took": took(run),
    });
    if let (Some(season), Some(episode)) = (run.season, run.episode) {
        body["season"] = json!(season);
        body["episodes"] = json!([episode]);
    }
    let actor = format!("sync:{}", run.integration);
    match run.title_id {
        Some(id) => event(kind, "title", id, &actor, body),
        None => event(kind, "processing_run", &run.id, &actor, body),
    }
}

/// The journal's events for runs a pass saw close: `processing_finished` for a run that was
/// imported, `processing_failed` for one that failed or was refused. A run cleared or staged
/// again is no news. Returns how many were journaled.
pub async fn journal_closed(
    pool: &PgPool,
    journal: &Journal,
    processor: &Arc<dyn Processor>,
    runs: &[Run],
) -> Result<usize, sqlx::Error> {
    let news: Vec<(&Run, &str)> = runs
        .iter()
        .filter_map(|r| match r.state.as_str() {
            IMPORTED => Some((r, "processing_finished")),
            "failed" | "refused" => Some((r, "processing_failed")),
            _ => None,
        })
        .collect();
    if news.is_empty() {
        return Ok(0);
    }
    let runs: Vec<&Run> = news.iter().map(|(r, _)| *r).collect();
    let titles = titles_of(pool, &runs).await?;
    let events = news
        .iter()
        .map(|(run, kind)| {
            let title = run.title_id.and_then(|id| titles.get(&id));
            run_event(kind, run, title, processor.flow_label(&run.flow))
        })
        .collect::<Vec<_>>();
    let mut tx = journal.begin(pool).await?;
    let appended = tx.append(events).await?.len();
    tx.commit().await?;
    Ok(appended)
}

/// The open runs of one service that went stuck, each journaled once (`processing_stuck`).
/// Whether a worker is on a run is asked of the services now, as a page asks it; when one of
/// them does not answer, nothing is journaled, since a worker may be on it. Returns how many
/// were journaled.
pub async fn journal_stuck(
    deps: &ProcessingDeps,
    processor: &Arc<dyn Processor>,
) -> Result<usize, String> {
    let after = stuck_after(&deps.settings);
    let now = Utc::now();
    let candidates = store::stuck_candidates(&deps.pool, processor.id(), now - after, STUCK_LOOKED)
        .await
        .map_err(|e| e.to_string())?;
    if candidates.is_empty() {
        return Ok(0);
    }
    let live = live(&deps.engines).await;
    if !live.all_answered() {
        return Ok(0);
    }
    let files = live_files(&deps.engines, &live.workers, &candidates).await;
    let reading = Reading {
        engines: &deps.engines,
        workers: &live.workers,
        files: &files,
        asked_at: live.at,
        now,
        stuck_after: after,
    };
    let stuck: Vec<&Run> = candidates.iter().filter(|r| reading.is_stuck(r)).collect();
    if stuck.is_empty() {
        return Ok(0);
    }
    let titles = titles_of(&deps.pool, &stuck)
        .await
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = stuck.iter().map(|r| r.id.clone()).collect();
    let journaled: Result<usize, sqlx::Error> = async {
        let mut tx = deps.journal.begin(&deps.pool).await?;
        let marked = store::note_stuck(tx.conn(), &ids, now).await?;
        let events: Vec<crate::core::journal::NewEvent> = stuck
            .iter()
            .filter(|r| marked.contains(&r.id))
            .map(|r| {
                let title = r.title_id.and_then(|id| titles.get(&id));
                let mut e = run_event("processing_stuck", r, title, processor.flow_label(&r.flow));
                e.body["open_for"] = json!((now - r.created_at).num_seconds());
                e
            })
            .collect();
        let appended = tx.append(events).await?.len();
        tx.commit().await?;
        Ok(appended)
    }
    .await;
    journaled.map_err(|e| e.to_string())
}

/// The name of the processing rules' pass, in jobs and logs.
pub const RULES_PASS: &str = "processing-rules";

/// Start one loop per configured processing service, and the loop of the processing rules.
pub fn spawn(deps: ProcessingDeps) -> Vec<JoinHandle<()>> {
    let mut loops: Vec<JoinHandle<()>> = deps
        .engines
        .processors
        .iter()
        .map(|p| tokio::spawn(service_loop(deps.clone(), p.clone())))
        .collect();
    if !deps.engines.processors.is_empty() {
        loops.push(tokio::spawn(rules_loop(deps)));
    }
    loops
}

/// The processing rules, against every import since the last pass, then the interval.
async fn rules_loop(deps: ProcessingDeps) {
    let job = jobs::registry().register(JobSpec {
        id: format!("singularr:{RULES_PASS}"),
        pass: RULES_PASS.to_string(),
        integration: None,
        every: deps.interval,
    });
    job.wait(jitter(deps.jitter)).await;
    loop {
        let started = job.begin();
        // The policy in force at the start of this pass: a saved profile takes effect at the next one.
        let active = deps.policy.current();
        let passed = super::rules::import_pass(
            &deps.pool,
            &deps.journal,
            &deps.engines,
            &active.policy,
            true,
        )
        .await;
        if let Err(e) = &passed {
            tracing::warn!(error = %e, "the processing rules pass failed");
        }
        job.end(
            started,
            passed.map(|p| {
                format!(
                    "{}, {}, {} sent",
                    crate::core::units::counted(p.imports as i64, "import", "imports"),
                    crate::core::units::counted(p.logged as i64, "match", "matches"),
                    crate::core::units::count(p.sent as i64)
                )
            }),
        );
        job.wait(deps.interval).await;
    }
}

/// A pass, its result recorded in the engine state and on its job, then the interval.
async fn service_loop(deps: ProcessingDeps, processor: Arc<dyn Processor>) {
    let id = processor.id().to_string();
    let job = jobs::registry().register(JobSpec::of(&id, PASS, deps.interval));
    let mut reader = Reader::new(processor.clone());
    let mut looked: Option<Instant> = None;
    job.wait(jitter(deps.jitter)).await;
    loop {
        let started = job.begin();
        let passed = reader.pass(&deps.pool, started).await;
        // The job says which read it was; the engine state keeps the counts.
        job.end(
            started,
            passed
                .as_ref()
                .map(|p| p.detail.clone())
                .map_err(Clone::clone),
        );
        // What closed and what went stuck is news for the journal, and from there for the
        // notification channels.
        if let Ok(p) = &passed {
            if let Err(e) = journal_closed(&deps.pool, &deps.journal, &processor, &p.closed).await {
                tracing::error!(service = %id, error = %e, "runs that closed could not be journaled");
            }
            if looked.is_none_or(|at| at.elapsed() >= STUCK_LOOK_EVERY) {
                looked = Some(Instant::now());
                if let Err(e) = journal_stuck(&deps, &processor).await {
                    tracing::warn!(service = %id, error = %e, "stuck runs could not be looked for");
                }
            }
        }
        let outcome = passed.map(|p| p.ok);
        observe::sync_pass(&id, PASS, started.elapsed(), outcome.is_ok());
        if let Err(e) = &outcome {
            tracing::warn!(service = %id, error = %e, "processing pass failed");
        }
        if let Err(e) = deps
            .engines
            .report(&deps.pool, &deps.journal, &id, PASS, outcome)
            .await
        {
            tracing::error!(service = %id, error = %e, "the engine state could not be recorded");
        }
        job.wait(deps.interval).await;
    }
}

/// One processing service as it answers now, or why it does not.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveService {
    /// The integration id.
    pub id: String,
    pub status: Result<ProcessorStatus, String>,
}

/// What the processing services say now: each one's state, what their workers are on, and
/// where the files of open runs stand in the workers' own records.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Live {
    pub services: Vec<LiveService>,
    pub workers: Vec<LiveWorker>,
    /// What the workers' own records say of the files of open runs ([`live_files`]).
    pub files: Vec<LiveFile>,
    /// When the services were asked. `None`: they were not.
    pub at: Option<DateTime<Utc>>,
}

impl Live {
    pub fn all_answered(&self) -> bool {
        self.services.iter().all(|s| s.status.is_ok())
    }

    /// The queue paths of the runs the workers are on: the paths the workers name, and the
    /// paths of the files whose records carry a footprint a busy worker names.
    pub fn busy_paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self
            .workers
            .iter()
            .filter_map(|w| w.worker.file.as_deref())
            .map(|f| f.trim().to_string())
            .collect();
        paths.extend(
            self.files
                .iter()
                .filter(|f| worker_by_footprint(f, &self.workers).is_some())
                .map(|f| f.path.clone()),
        );
        paths
    }
}

/// Ask every configured processing service how it is and what it works on, with GET. A
/// service that does not answer is in the answer with its error, and has no workers.
pub async fn live(engines: &EngineSet) -> Live {
    let asked = join_all(engines.processors.iter().map(|p| async move {
        let (status, workers) = tokio::join!(p.status(), p.workers());
        (p.id().to_string(), status, workers)
    }))
    .await;
    let mut out = Live {
        at: Some(Utc::now()),
        ..Live::default()
    };
    for (id, status, workers) in asked {
        let status = match (status, workers) {
            (Ok(status), Ok(workers)) => {
                out.workers
                    .extend(workers.into_iter().map(|worker| LiveWorker {
                        service: id.clone(),
                        worker,
                    }));
                Ok(status)
            }
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(service = %id, error = %e, "the processing service did not answer");
                Err(e.to_string())
            }
        };
        out.services.push(LiveService { id, status });
    }
    out
}

/// Where a file that waits at a path belongs in the workers' own configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    /// The integration id of the worker service.
    pub service: String,
    /// The library whose folder the path lies below.
    pub library: Option<WorkerLibrary>,
    /// The flow that library runs. One the worker does not list is named by its id.
    pub flow: Option<WorkerFlow>,
}

/// The worker a file that waits at `path` is for, from the configuration the workers last
/// gave: the worker one of whose libraries has the longest folder the path lies below. When no
/// library holds the path and the installation has one worker, it is that worker's, with no
/// library. `None` without a worker, and with several when no library says which.
pub fn placed(engines: &EngineSet, path: &str) -> Option<Placed> {
    let workers: Vec<&Arc<dyn Processor>> = engines
        .processors
        .iter()
        .filter(|p| p.role() == WORKER)
        .collect();
    let mut best: Option<Placed> = None;
    for worker in &workers {
        let Some(setup) = worker.setup() else {
            continue;
        };
        let Some(library) = setup.library_of(path) else {
            continue;
        };
        let longer = best
            .as_ref()
            .and_then(|b| b.library.as_ref())
            .is_none_or(|held| library.folder.len() > held.folder.len());
        if longer {
            best = Some(Placed {
                service: worker.id().to_string(),
                flow: library.flow_id.as_deref().map(|id| {
                    setup.flow(id).cloned().unwrap_or_else(|| WorkerFlow {
                        id: id.to_string(),
                        name: id.to_string(),
                        about: None,
                        steps: Vec::new(),
                    })
                }),
                library: Some(library.clone()),
            });
        }
    }
    best.or_else(|| match workers.as_slice() {
        [only] => Some(Placed {
            service: only.id().to_string(),
            library: None,
            flow: None,
        }),
        _ => None,
    })
}

/// Ask the workers what their own records say of the files of open runs: one question for
/// each run that waits at a path no worker names (a run whose path a worker is on needs no
/// question), at most [`FILES_ASKED`]: first the runs a busy worker may be on, then the
/// oldest runs. A question that fails is left out, so nothing is known of that file.
pub async fn live_files(
    engines: &EngineSet,
    workers: &[LiveWorker],
    open: &[Run],
) -> Vec<LiveFile> {
    // A worker that runs a flow works on a copy of the file under the file's own name. The
    // runs whose file has the name of a file such a worker is on are asked about first, so
    // the limit never costs the answer the record that says whose run it is. The name only
    // orders the questions: the footprint in the record is what joins a run to a worker.
    let names: Vec<&str> = workers
        .iter()
        .filter(|w| w.worker.footprint.is_some())
        .filter_map(|w| w.worker.file.as_deref())
        .map(file_name)
        .collect();
    let (first, rest): (Vec<&Run>, Vec<&Run>) = open
        .iter()
        .filter(|r| r.is_open())
        .partition(|r| queue_of(r).is_some_and(|q| names.contains(&file_name(q))));
    let mut asks: Vec<(Arc<dyn Processor>, String)> = Vec::new();
    for run in first.into_iter().chain(rest) {
        if asks.len() >= FILES_ASKED {
            break;
        }
        let Some(path) = queue_of(run) else {
            continue;
        };
        if worker_of(run, workers, &[]).is_some() {
            continue;
        }
        let Some(worker) =
            placed(engines, path).and_then(|p| engines.processor(&p.service).cloned())
        else {
            continue;
        };
        if !asks
            .iter()
            .any(|(asked, at)| asked.id() == worker.id() && at == path)
        {
            asks.push((worker, path.to_string()));
        }
    }
    // A few questions at a time, so a long list of open runs does not land on the worker at
    // once.
    let mut answers: Vec<(String, String, Result<FileAnswer, EngineError>)> = Vec::new();
    for some in asks.chunks(FILES_AT_ONCE) {
        answers.extend(
            join_all(some.iter().map(|(worker, path)| async move {
                (
                    worker.id().to_string(),
                    path.clone(),
                    worker.file(path).await,
                )
            }))
            .await,
        );
    }
    answers
        .into_iter()
        .filter_map(|(service, path, answer)| match answer {
            Ok(FileAnswer::Known(file)) => Some(LiveFile {
                service,
                path,
                file: Some(file),
            }),
            Ok(FileAnswer::Absent) => Some(LiveFile {
                service,
                path,
                file: None,
            }),
            Ok(FileAnswer::NotOffered) => None,
            Err(e) => {
                tracing::warn!(service = %service, error = %e, "the worker's record of a file could not be read");
                None
            }
        })
        .collect()
}

/// The longest the stuck threshold is taken to be: ten years. A larger setting means never.
const STUCK_AFTER_MAX: u64 = 315_360_000;

/// After how long an open run counts as stuck: the setting, or six hours.
pub fn stuck_after(settings: &Settings) -> chrono::Duration {
    let seconds = settings
        .value(STUCK_AFTER)
        .as_u64()
        .unwrap_or(STUCK_AFTER_DEFAULT)
        .min(STUCK_AFTER_MAX);
    chrono::Duration::seconds(seconds as i64)
}

/// A worker as a run or a page shows it.
pub fn run_worker(w: &LiveWorker) -> RunWorker {
    RunWorker {
        service: w.service.clone(),
        node: w.worker.node.clone(),
        file: w.worker.file.clone(),
        percent: w.worker.percent,
        fps: w.worker.fps,
        eta: w.worker.eta.clone(),
        status: w.worker.status.clone(),
        kind: w.worker.kind.clone(),
        started_at: w.worker.started_at,
    }
}

fn run_file(path: Option<&str>, size: Option<i64>) -> Option<RunFile> {
    let path = path?;
    Some(RunFile {
        file: file_name(path).to_string(),
        path: path.to_string(),
        size,
    })
}

/// What stored runs are read with: the adapters for the words of their flows and triggers,
/// what the workers are on now, what their records say of the files of open runs, the clock,
/// and the stuck threshold.
pub struct Reading<'a> {
    pub engines: &'a EngineSet,
    pub workers: &'a [LiveWorker],
    /// What the workers' own records say of the files of open runs, as last asked.
    pub files: &'a [LiveFile],
    /// When the workers were asked.
    pub asked_at: Option<DateTime<Utc>>,
    pub now: DateTime<Utc>,
    pub stuck_after: chrono::Duration,
}

/// Where a run is in a worker's own records, with what only the sentence of a stuck run needs.
struct WorkerFacts {
    record: RunInWorker,
    /// The worker holds the file back until then.
    held_until: Option<DateTime<Utc>>,
}

impl Reading<'_> {
    /// The worker that is on a run: by the path the run waits at, or by the footprint of the
    /// worker's record of its file.
    pub fn worker_of(&self, run: &Run) -> Option<&LiveWorker> {
        worker_of(run, self.workers, self.files)
    }

    pub fn is_stuck(&self, run: &Run) -> bool {
        is_stuck(
            run,
            self.worker_of(run).is_some(),
            self.now,
            self.stuck_after,
        )
    }

    /// Where a run is in a worker's own records. The library and the flow come from the
    /// worker's configuration as it was last read, for any run with a queue path. Where the
    /// file stands is known for an open run alone: `working` when a worker is on it, else
    /// what the worker's record of the file said when it was asked. `None` without a worker,
    /// and when nothing is known.
    fn worker_facts(&self, run: &Run, full: bool) -> Option<WorkerFacts> {
        let path = queue_of(run)?;
        let busy = self.worker_of(run);
        let placed = placed(self.engines, path).or_else(|| {
            busy.map(|w| Placed {
                service: w.service.clone(),
                library: None,
                flow: None,
            })
        })?;
        let (mut state, mut words, mut checked_at, mut held_until) = (None, None, None, None);
        if let Some(w) = busy {
            state = Some("working");
            words = w.worker.status.clone();
            checked_at = self.asked_at;
        } else if run.is_open()
            && let Some(asked) = self
                .files
                .iter()
                .find(|f| f.service == placed.service && f.path == path)
        {
            checked_at = self.asked_at;
            match &asked.file {
                None => state = Some("absent"),
                Some(file) => {
                    words = file.words.clone();
                    // A file that still waits may be held back; one the worker is done with
                    // is not.
                    held_until = file
                        .hold_until
                        .filter(|until| *until > self.now)
                        .filter(|_| matches!(file.state, None | Some("queued")));
                    state = if held_until.is_some() {
                        Some("held")
                    } else {
                        file.state
                    };
                }
            }
        }
        if placed.library.is_none() && state.is_none() {
            return None;
        }
        Some(WorkerFacts {
            record: RunInWorker {
                service: placed.service,
                library: placed.library.map(|l| RunLibrary {
                    id: l.id,
                    name: l.name,
                }),
                flow: placed.flow.map(|f| RunFlow {
                    id: f.id,
                    name: f.name,
                    about: f.about.filter(|_| full),
                    steps: full.then_some(f.steps),
                }),
                state: state.map(str::to_string),
                words,
                checked_at,
            },
            held_until,
        })
    }

    /// Where a run is in a worker's own records, as the pages show it. `full` adds what the
    /// flow is for and its steps, for the one place a run is read by its id.
    pub fn in_worker(&self, run: &Run, full: bool) -> Option<RunInWorker> {
        self.worker_facts(run, full).map(|f| f.record)
    }

    /// Why a stuck run is stuck, in one sentence, when the worker's records say. `None` for a
    /// run that is not stuck.
    pub fn stuck_why(&self, run: &Run) -> Option<String> {
        if !self.is_stuck(run) {
            return None;
        }
        let facts = self.worker_facts(run, false)?;
        stuck_why(
            facts.record.state.as_deref(),
            facts.record.words.as_deref(),
            facts.held_until,
        )
    }

    /// The label of a run's flow, in its adapter's words; the flow's id when the run's service
    /// is not configured any more.
    pub fn flow_label(&self, run: &Run) -> String {
        match self.engines.processor(&run.integration) {
            Some(p) => p.flow_label(&run.flow),
            None => run.flow.clone(),
        }
    }

    /// A stored run as the pages show it. The episode is the one its file name reads as; the
    /// caller leaves it out for a run that is not linked to a show.
    pub fn record(&self, run: &Run) -> ProcessingRun {
        let worker = self.worker_of(run);
        let stuck = is_stuck(run, worker.is_some(), self.now, self.stuck_after);
        let facts = self.worker_facts(run, false);
        let why = facts.as_ref().filter(|_| stuck).and_then(|f| {
            stuck_why(
                f.record.state.as_deref(),
                f.record.words.as_deref(),
                f.held_until,
            )
        });
        let trigger = match self.engines.processor(&run.integration) {
            Some(p) => p.trigger(&run.trigger),
            None => TriggerWords {
                kind: "other",
                label: run.trigger.clone(),
            },
        };
        ProcessingRun {
            id: run.id.clone(),
            service: run.integration.clone(),
            flow: run.flow.clone(),
            flow_label: self.flow_label(run),
            trigger: RunTrigger {
                kind: trigger.kind.to_string(),
                label: trigger.label,
                raw: run.trigger.clone(),
            },
            state: shown_state(run, worker.is_some()).to_string(),
            at: run.created_at,
            updated: run.updated_at,
            finished: run.finished_at,
            took: took(run),
            episode: match (run.season, run.episode) {
                (Some(season), Some(number)) => Some(EpisodeRef { season, number }),
                _ => None,
            },
            source: run_file(run.source_path.as_deref(), run.source_size),
            output: run_file(run.output_path.as_deref(), run.output_size),
            result: run_file(run.result_path.as_deref(), run.result_size),
            saved: saved(run),
            replaced: run
                .replaced
                .iter()
                .map(|r| RunReplaced {
                    file: file_name(&r.path).to_string(),
                    path: r.path.clone(),
                    size: r.size,
                    still_there: r.still_there,
                })
                .collect(),
            detail: run.detail.clone(),
            stuck,
            worker: worker.map(run_worker),
            events: run
                .events
                .iter()
                .map(|e| RunEvent {
                    at: e.at,
                    event: e.event.clone(),
                    actor: e.actor.clone(),
                    note: e.note.clone(),
                })
                .collect(),
            in_worker: facts.map(|f| f.record),
            stuck_why: why,
        }
    }
}

/// One flow of one service, with its runs in numbers when it has any.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowSummary {
    /// The integration id.
    pub service: String,
    pub flow: ProcessorFlow,
    pub stat: Option<FlowStat>,
}

/// What [`store::flow_stats`] groups by: for every mode of every flow of every configured
/// service, the integration, the mode, and the flow it belongs to.
pub fn flow_groups(engines: &EngineSet) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut out = (Vec::new(), Vec::new(), Vec::new());
    for p in &engines.processors {
        for flow in p.flows() {
            for mode in flow.modes {
                out.0.push(p.id().to_string());
                out.1.push(mode);
                out.2.push(flow.id.clone());
            }
        }
    }
    out
}

/// The flows of the configured services in the order a person chooses from, each with its
/// numbers; then the flows that runs name and no adapter offers (a mode that is not a flow, a
/// service that is not configured any more), with the id as the label.
pub fn flow_summaries(engines: &EngineSet, stats: Vec<FlowStat>) -> Vec<FlowSummary> {
    let mut stats: Vec<Option<FlowStat>> = stats.into_iter().map(Some).collect();
    let mut take = |service: &str, flow: &str| {
        stats
            .iter_mut()
            .find(|s| {
                s.as_ref()
                    .is_some_and(|s| s.integration == service && s.flow == flow)
            })
            .and_then(Option::take)
    };
    let mut out = Vec::new();
    for p in &engines.processors {
        for flow in p.flows() {
            out.push(FlowSummary {
                service: p.id().to_string(),
                stat: take(p.id(), &flow.id),
                flow,
            });
        }
    }
    for stat in stats.into_iter().flatten() {
        out.push(FlowSummary {
            service: stat.integration.clone(),
            flow: ProcessorFlow {
                id: stat.flow.clone(),
                label: stat.flow.clone(),
                about: None,
                tag: None,
                modes: vec![stat.flow.clone()],
                needs: None,
            },
            stat: Some(stat),
        });
    }
    out
}

/// The flows of a run that a `flow=` filter asks for: the word itself (a run's own flow), and
/// every mode of the flow it names. With `service`, only that service's flows are looked at.
pub fn flow_modes(engines: &EngineSet, service: Option<&str>, flow: &str) -> Vec<String> {
    let mut modes: Vec<String> = engines
        .processors
        .iter()
        .filter(|p| service.is_none_or(|s| p.id() == s))
        .flat_map(|p| p.flows())
        .filter(|f| f.id == flow)
        .flat_map(|f| f.modes)
        .collect();
    modes.push(flow.to_string());
    modes.sort();
    modes.dedup();
    modes
}

/// What acts on a worker service came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActsDone {
    /// How many the service took, in order.
    pub done: usize,
    /// How the last one asked for ended: `Done` when it took them all.
    pub last: Acted,
}

/// Change something in a worker service: the acts go out one after the other (each refused
/// on the way out unless the integration's access is `act`), stopping at the first the
/// service does not take. What it took is one `processing_worker_changed` event, with
/// `about` (what the act names, in the words the journal shows) and how many it took. The
/// caller decides whether the acts may be asked for: the owner, and an act the adapter lists.
pub async fn worker_acts(
    pool: &PgPool,
    journal: &Journal,
    processor: &Arc<dyn Processor>,
    acts: &[WorkerAct],
    actor: &str,
    about: serde_json::Value,
) -> Result<ActsDone, EngineError> {
    let mut done = 0;
    let mut last = Acted::Done;
    let mut failed = None;
    for act in acts {
        match processor.act(act).await {
            Ok(Acted::Done) => done += 1,
            Ok(other) => {
                last = other;
                break;
            }
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    if done > 0 {
        let service = processor.id();
        let mut body = json!({
            "service": service,
            "act": acts.first().map(WorkerAct::word),
            "steps": done,
        });
        if let (Some(into), serde_json::Value::Object(extra)) = (body.as_object_mut(), about) {
            into.extend(extra);
        }
        // What the service did stands whatever happens here: a journal that cannot be written
        // is logged.
        let journaled: Result<(), sqlx::Error> = async {
            let mut tx = journal.begin(pool).await?;
            tx.append_one(event(
                "processing_worker_changed",
                "engine",
                service,
                actor,
                body,
            ))
            .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        if let Err(e) = journaled {
            tracing::error!(service, error = %e, "a worker was changed and the journal could not record it");
        }
    }
    match failed {
        Some(e) => Err(e),
        None => Ok(ActsDone { done, last }),
    }
}

/// The reports a worker keeps of the jobs it ran on a run's file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReports {
    /// The integration id of the worker service.
    pub service: String,
    /// The worker's mark of the file, which its reports are filed under.
    pub footprint: String,
    /// Newest first.
    pub reports: Vec<WorkerReport>,
}

/// The reports of a run's file, asked of the worker whose library holds the run's queue path.
/// The file is known by its footprint: the one a busy worker on the run names, else the one
/// the worker's record of the file carries. `None` for a run that waits at no path, a path no
/// worker holds, a file the worker has no record of, and a worker that keeps no reports.
pub async fn run_reports(
    reading: &Reading<'_>,
    run: &Run,
) -> Result<Option<RunReports>, EngineError> {
    let Some(path) = queue_of(run) else {
        return Ok(None);
    };
    let busy = reading.worker_of(run);
    let service = match placed(reading.engines, path) {
        Some(p) => p.service,
        None => match busy {
            Some(w) => w.service.clone(),
            None => return Ok(None),
        },
    };
    let Some(worker) = reading.engines.processor(&service).cloned() else {
        return Ok(None);
    };
    let footprint = match busy
        .filter(|w| w.service == service)
        .and_then(|w| w.worker.footprint.clone())
    {
        Some(mark) => mark,
        None => match worker.file(path).await? {
            FileAnswer::Known(WorkerFile {
                footprint: Some(mark),
                ..
            }) => mark,
            _ => return Ok(None),
        },
    };
    Ok(worker.reports(&footprint).await?.map(|reports| RunReports {
        service,
        footprint,
        reports,
    }))
}

/// The run a worker is on: the newest staged run that waits where the worker works, or whose
/// file carries the footprint the worker names (`files`: what the workers' records said of
/// the files of open runs).
pub fn run_of<'r>(worker: &LiveWorker, open: &'r [Run], files: &[LiveFile]) -> Option<&'r Run> {
    open.iter()
        .filter(|run| worker_of(run, std::slice::from_ref(worker), files).is_some())
        .max_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{ProcessorConfig, ProcessorKind};
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method, Outbound};
    use crate::engines::hush::{self, Hush};
    use crate::engines::{ProcessorFile, ProcessorWorker, WorkerFile, WorkerSetup};
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::Mutex;

    const BRIDGE: &str = "http://10.0.0.60:8787";

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// The jobs of the recorded ledger, newest first.
    fn ledger() -> Vec<ProcessorRun> {
        let answer: Value = serde_json::from_str(&fixture("hush_history.json")).unwrap();
        answer["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(hush::run)
            .collect()
    }

    /// The bridge as a processing service, over a transport that may reach it.
    fn bridge(recorded: Arc<Recorded>) -> Arc<dyn Processor> {
        let config = ProcessorConfig {
            id: "hush".to_string(),
            kind: ProcessorKind::Hush,
            url: Some(BRIDGE.to_string()),
            api_key: None,
        };
        let mut allow = Allowlist::default();
        allow.allow_engine("hush", BRIDGE, &[]).unwrap();
        Arc::new(Hush::new(&config, Outbound::with_transport(allow, recorded)).unwrap())
    }

    /// An engine set with the bridge as its one processing service. Nothing is asked of it.
    fn engines() -> EngineSet {
        EngineSet::new(Vec::new(), None, Vec::new()).with_processors(vec![bridge(Recorded::new())])
    }

    #[test]
    fn a_pass_says_which_read_it_was() {
        let report = |read: Read, listed: usize, older: usize, rest: Rest, followed: usize| {
            PassReport {
                read,
                listed,
                older,
                rest,
                followed,
                added: 0,
                updated: 0,
                linked: 0,
                open: 0,
            }
            .line()
        };
        let all = Rest::Nothing;
        assert_eq!((Read::Newest.limit(), Read::Whole.limit()), (60, 2000));
        assert_eq!(report(Read::Newest, 60, 0, all, 0), "60 newest runs");
        assert_eq!(report(Read::Newest, 1, 0, all, 0), "1 newest run");
        assert_eq!(report(Read::Whole, 1017, 0, all, 0), "1,017 runs");
        assert_eq!(report(Read::Whole, 0, 0, all, 0), "0 runs");
        // A list as long as the most a service gives at once may be longer than that.
        assert_eq!(
            report(Read::Whole, 2000, 0, Rest::NotGiven, 0),
            "2,000 newest runs"
        );
        // A service that gives its older jobs page by page is read to the end of them...
        assert_eq!(report(Read::Whole, 2000, 1417, all, 0), "3,417 runs");
        // ...unless there are more than one whole read goes back to, or a page failed (why
        // is said after the line).
        assert_eq!(
            report(Read::Whole, 2000, 20000, Rest::Left, 0),
            "22,000 newest runs"
        );
        assert_eq!(
            report(Read::Whole, 2000, 2000, Rest::Failed, 0),
            "4,000 newest runs"
        );
        // The open runs a read did not carry, which the service gave by id.
        assert_eq!(
            report(Read::Newest, 60, 0, all, 2),
            "60 newest runs, 2 older open"
        );
        assert_eq!(report(Read::Whole, 12, 0, all, 1), "12 runs, 1 older open");
        assert_eq!((OLDER_PAGES, OPEN_FOLLOWED), (10, 400));
    }

    #[tokio::test]
    async fn the_whole_list_is_read_first_and_then_only_when_the_last_whole_read_is_old() {
        // The bridge answers its state and fails its list of jobs, so every pass here ends
        // before it reaches the database. What each pass asked for is on record.
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/health", 200, &fixture("hush_health.json"));
        recorded.answer(Method::Get, "/history.json", 503, "");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://x@127.0.0.1:1/x")
            .unwrap();
        let mut reader = Reader::new(bridge(recorded.clone()));
        let start = Instant::now();
        let minute = Duration::from_secs(60);
        let second = Duration::from_secs(1);

        // The first pass of a process reads the whole list. It failed, so the next one reads
        // the whole list again.
        assert_eq!(reader.read_at(start), Read::Whole);
        assert!(reader.pass(&pool, start).await.is_err());
        assert_eq!(reader.read_at(start + minute), Read::Whole);
        assert!(reader.pass(&pool, start + minute).await.is_err());

        // After a whole read that succeeded the passes ask for the newest jobs alone...
        reader.whole_at = Some(start);
        assert!(reader.pass(&pool, start + minute).await.is_err());
        let late = start + WHOLE_READ_EVERY;
        assert!(reader.pass(&pool, late - second).await.is_err());
        // ...until that read is as old as a whole read stands.
        assert!(reader.pass(&pool, late).await.is_err());
        assert_eq!(reader.read_at(late + minute), Read::Whole, "it failed too");

        let limits: Vec<String> = recorded
            .urls_seen()
            .iter()
            .filter_map(|u| u.split_once("/history.json?limit="))
            .map(|(_, limit)| limit.to_string())
            .collect();
        assert_eq!(limits, ["2000", "2000", "60", "60", "2000"]);
        // Nothing but GET was sent.
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
        // The setting that says how often a service is read names how long a whole read
        // stands.
        let help = crate::settings::registry::field("jobs.processing.syncEvery")
            .unwrap()
            .help;
        let minutes = WHOLE_READ_EVERY.as_secs() / 60;
        assert!(help.contains(&format!("every {minutes} min")), "{help}");
    }

    fn folders() -> Folders {
        Folders::new([
            (
                1,
                "/media/shows/Avatar - The Last Airbender (2024) {tvdb-385925}/".to_string(),
            ),
            (2, "/media/movies/Damsel (2024) {tmdb-763215}".to_string()),
            (3, "/media/movies".to_string()),
            (4, "  ".to_string()),
        ])
    }

    fn stored(jobs: &[ProcessorRun]) -> Vec<Run> {
        let (folders, named) = (folders(), Named::default());
        let seen = at("2026-11-23T16:00:00Z");
        jobs.iter()
            .map(|j| Run::listed("hush", j, link(j, &folders, &named), seen))
            .collect()
    }

    fn title(
        id: i64,
        kind: &str,
        tmdb: Option<i64>,
        tvdb: Option<i64>,
        imdb: Option<&str>,
    ) -> catalog_store::TitleIds {
        catalog_store::TitleIds {
            id,
            kind: kind.to_string(),
            tmdb_id: tmdb,
            tvdb_id: tvdb,
            imdb_id: imdb.map(str::to_string),
        }
    }

    #[test]
    fn a_file_no_folder_holds_is_linked_by_the_ids_its_path_names() {
        // Two movies whose folders moved to another root since their runs.
        let apes = "/media/4kmovies/War for the Planet of the Apes (2017) {tmdb-281338}/War for the Planet of the Apes (2017) {imdb-tt3450958} [Remux-2160p][HDR10][Opus 7.1][HEVC]-SHR.mkv";
        let kingdom = "/media/movies/Kingdom of Heaven (2005) {tmdb-1495}/Kingdom of Heaven (2005) {imdb-tt0320661} {edition-Director's Cut} [Bluray-1080p][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv";
        assert_eq!(
            path_ids(apes),
            PathIds {
                tmdb: Some(281_338),
                tvdb: None,
                imdb: Some("tt3450958".to_string())
            }
        );
        assert_eq!(
            path_ids(kingdom),
            PathIds {
                tmdb: Some(1495),
                tvdb: None,
                imdb: Some("tt0320661".to_string())
            }
        );
        let episode = "/media/shows/The Expanse {tvdb-280619}/Season 01/The Expanse (2015) - S01E07 - Windmills [WEBDL-1080p][EAC3 5.1][h264]-playWEB.mkv";
        assert_eq!(
            path_ids(episode),
            PathIds {
                tmdb: None,
                tvdb: Some(280_619),
                imdb: None
            }
        );
        // The folder nearest to the file is the title's; an id in a file's name is no folder's.
        assert_eq!(
            path_ids("/lib {tmdb-1}/A (2001) {tmdb-2}/a {tmdb-3} {imdb-TT0000009}.mkv"),
            PathIds {
                tmdb: Some(2),
                tvdb: None,
                imdb: Some("tt0000009".to_string())
            }
        );
        assert_eq!(
            path_ids("D:\\Movies\\B (2002) {imdb-tt0000008}\\b.mkv")
                .imdb
                .as_deref(),
            Some("tt0000008")
        );
        assert_eq!(path_ids("/plain/folder/file.mkv"), PathIds::default());

        let named = Named::new([
            title(7, "movie", Some(281_338), None, Some("tt3450958")),
            title(8, "movie", Some(1495), None, Some("TT0320661")),
            // A show that carries the same TMDB number as a movie, and one known by TVDB.
            title(9, "show", Some(1495), Some(280_619), Some("tt3230854")),
        ]);
        assert_eq!(named.title_of(apes), Some(7));
        assert_eq!(
            named.title_of(kingdom),
            Some(8),
            "a movie's file names a movie"
        );
        assert_eq!(named.title_of(episode), Some(9));
        // Without an id in a folder's name, the IMDb id of the file's name says which title.
        assert_eq!(
            named.title_of(
                "/elsewhere/Kingdom of Heaven (2005) {imdb-tt0320661} [Bluray-1080p].mkv"
            ),
            Some(8)
        );
        // An episode in a folder named by TMDB id belongs to the show with that id.
        assert_eq!(
            named.title_of("/shows/A Show {tmdb-1495}/Season 02/A Show - S02E01 - One.mkv"),
            Some(9)
        );
        assert_eq!(
            named.title_of("/movies/Unknown (1999) {tmdb-42}/u.mkv"),
            None
        );
        assert_eq!(Named::default().title_of(apes), None);

        // A job is linked by its folder first, by the ids its paths name when no folder holds
        // them, and stays unlinked when neither says anything.
        let jobs = ledger();
        let folders = Folders::new([
            // The catalog knows these two movies in the folders they moved to.
            (
                7,
                "/media/movies/War for the Planet of the Apes (2017) {tmdb-281338}".to_string(),
            ),
            (
                8,
                "/media/4kmovies/Kingdom of Heaven (2005) {tmdb-1495}".to_string(),
            ),
            (2, "/media/movies/Damsel (2024) {tmdb-763215}".to_string()),
        ]);
        let by_folder_alone: Vec<Option<i64>> = jobs
            .iter()
            .map(|j| link(j, &folders, &Named::default()).title_id)
            .collect();
        assert_eq!(
            (by_folder_alone[2], by_folder_alone[5], by_folder_alone[6]),
            (Some(2), None, None)
        );
        let links: Vec<Link> = jobs.iter().map(|j| link(j, &folders, &named)).collect();
        assert_eq!(
            links[5].title_id,
            Some(7),
            "the imported run of a movie that moved"
        );
        assert_eq!(
            links[6].title_id,
            Some(8),
            "the staged run of a movie that moved"
        );
        assert_eq!(links[2].title_id, Some(2), "the folder says so");
        assert_eq!(
            links[1],
            Link {
                title_id: Some(9),
                season: Some(1),
                episode: Some(7)
            }
        );
        assert_eq!(
            links[0].title_id, None,
            "no folder holds it and no title carries its id"
        );
    }

    fn worker(file: &str) -> LiveWorker {
        LiveWorker {
            service: "tdarr".to_string(),
            worker: ProcessorWorker {
                id: None,
                node_id: None,
                node: Some("node-1".to_string()),
                file: Some(file.to_string()),
                percent: Some(43.2),
                fps: Some(58.0),
                eta: Some("0:12:00".to_string()),
                status: Some("Processing".to_string()),
                kind: Some("transcodegpu".to_string()),
                started_at: Some(at("2026-11-23T15:01:00Z")),
                footprint: None,
                raw: Value::Null,
            },
        }
    }

    #[test]
    fn a_file_is_linked_to_the_title_whose_folder_it_lies_in() {
        let f = folders();
        assert_eq!(
            f.title_of(
                "/media/shows/Avatar - The Last Airbender (2024) {tvdb-385925}/Season 01/a.mkv"
            ),
            Some(1)
        );
        // The nearest folder that is a title's wins over one further up.
        assert_eq!(
            f.title_of("/media/movies/Damsel (2024) {tmdb-763215}/d.mkv"),
            Some(2)
        );
        assert_eq!(f.title_of("/media/movies/Other (2020)/o.mkv"), Some(3));
        assert_eq!(f.title_of("/media/shows/Unknown/Season 01/u.mkv"), None);
        assert_eq!(f.title_of("u.mkv"), None);
        // A folder is not a file of itself, and a longer name is another folder.
        assert_eq!(
            f.title_of("/media/movies/Damsel (2024) {tmdb-763215}"),
            Some(3)
        );
        assert_eq!(f.title_of("/media/movies2/x.mkv"), None);
        let windows = Folders::new([(9, "D:\\Shows\\A Show\\".to_string())]);
        assert_eq!(
            windows.title_of("D:\\Shows\\A Show\\Season 01\\e.mkv"),
            Some(9)
        );

        assert_eq!(
            episode_of("/x/Season 01/The Expanse (2015) - S01E07 - Windmills [WEBDL-1080p].mkv"),
            Some((1, 7))
        );
        assert_eq!(
            episode_of("Show (2020) - S02E03-E04 - Two Parts [HDTV-1080p].mkv"),
            Some((2, 3))
        );
        assert_eq!(
            episode_of(
                "/movies/Damsel (2024) {imdb-tt13452446} [WEBDL-1080p][Opus 5.1][HEVC]-FLUX.mkv"
            ),
            None
        );

        // The ledger's jobs: an episode in a show's folder, a movie, and one nothing holds.
        let jobs = ledger();
        let none = Named::default();
        let links: Vec<Link> = jobs.iter().map(|j| link(j, &f, &none)).collect();
        assert_eq!(
            links[0],
            Link {
                title_id: Some(1),
                season: Some(1),
                episode: Some(1)
            }
        );
        assert_eq!(
            links[2],
            Link {
                title_id: Some(2),
                season: None,
                episode: None
            }
        );
        assert_eq!(
            links[1],
            Link {
                title_id: None,
                season: Some(1),
                episode: Some(7)
            },
            "an episode of a show the catalog does not have stays unlinked"
        );
        assert_eq!(
            links[6].title_id,
            Some(3),
            "a movie whose folder is the root's"
        );
    }

    #[test]
    fn the_ledger_s_runs_read_in_the_product_s_words() {
        let engines = engines();
        let runs = stored(&ledger());
        let reading = Reading {
            engines: &engines,
            workers: &[],
            files: &[],
            asked_at: None,
            now: at("2026-11-23T16:00:00Z"),
            stuck_after: chrono::Duration::seconds(21_600),
        };
        let records: Vec<ProcessingRun> = runs.iter().map(|r| reading.record(r)).collect();
        let states: Vec<&str> = records.iter().map(|r| r.state.as_str()).collect();
        assert_eq!(
            states,
            vec![
                "imported",
                "imported",
                "imported",
                "imported",
                "imported",
                "imported",
                "staged",
                "staged",
                "superseded",
                "failed",
                "pruned",
                "refused"
            ]
        );
        // An imported run: what it saved and how long it took.
        let first = &records[0];
        assert_eq!(first.id, "hush:20261123T145500Z-57ec91da");
        assert_eq!(
            (
                first.service.as_str(),
                first.flow.as_str(),
                first.flow_label.as_str()
            ),
            ("hush", "hevc", "Convert to HEVC")
        );
        assert_eq!(
            first.trigger,
            RunTrigger {
                kind: "rule".to_string(),
                label: "Import rule".to_string(),
                raw: "hush_rules".to_string()
            }
        );
        assert_eq!(first.at, at("2026-11-23T14:55:00Z"));
        assert_eq!(first.finished, Some(at("2026-11-23T15:06:15Z")));
        assert_eq!((first.took, first.saved), (Some(675), Some(540_857_787)));
        assert_eq!(
            first.episode,
            Some(EpisodeRef {
                season: 1,
                number: 1
            })
        );
        let source = first.source.as_ref().unwrap();
        assert_eq!(
            source.file,
            "Avatar - The Last Airbender (2024) - S01E01 - Aang [WEBDL-1080p][EAC3 Atmos 5.1][x264]-Kitsune.mkv"
        );
        assert_eq!(source.size, Some(2_562_033_987));
        assert!(source.path.starts_with("/media/shows/Avatar"));
        assert_eq!(
            first.result.as_ref().unwrap().file,
            "Avatar - The Last Airbender (2024) - S01E01 - Aang [WEBDL-1080p][Opus 5.1][HEVC]-Kitsune.mkv"
        );
        assert_eq!(first.output.as_ref().unwrap().size, Some(2_021_176_200));
        assert_eq!(first.replaced.len(), 1);
        assert_eq!(
            (
                first.replaced[0].file.as_str(),
                first.replaced[0].size,
                first.replaced[0].still_there
            ),
            (source.file.as_str(), Some(2_562_033_987), Some(false))
        );
        assert!(!first.stuck && first.worker.is_none() && first.detail.is_none());
        let notes: Vec<Option<&str>> = first.events.iter().map(|e| e.note.as_deref()).collect();
        assert_eq!(
            notes,
            vec![
                None,
                None,
                Some("content identity, 1 file(s)"),
                None,
                Some("import confirmed")
            ]
        );
        // Every flow and trigger of the ledger in words.
        let words: Vec<(&str, &str, &str)> = records
            .iter()
            .map(|r| {
                (
                    r.flow_label.as_str(),
                    r.trigger.kind.as_str(),
                    r.trigger.label.as_str(),
                )
            })
            .collect();
        assert_eq!(words[3], ("Master encode", "rule", "Import rule"));
        assert_eq!(words[4], ("Rebuild audio", "rule", "Import rule"));
        assert_eq!(
            words[5],
            ("Downscale to 1080p", "tag", "Tag hush-downscale")
        );
        assert_eq!(words[6], ("Rebuild audio", "tag", "Tag hush-audio"));
        assert_eq!(words[7], ("Convert to HEVC", "backlog", "Backlog"));
        // What each import saved and took; the others saved nothing.
        let saved: Vec<Option<i64>> = records.iter().map(|r| r.saved).collect();
        assert_eq!(
            saved,
            vec![
                Some(540_857_787),
                Some(1_062_609_589),
                Some(1_483_110_958),
                Some(7_840_495_250),
                Some(509_868_476),
                Some(26_972_009_488),
                None,
                None,
                None,
                None,
                None,
                None
            ]
        );
        let took: Vec<Option<i64>> = records.iter().map(|r| r.took).collect();
        assert_eq!(
            took,
            vec![
                Some(675),
                Some(468),
                Some(1193),
                Some(1275),
                Some(831),
                Some(1888),
                None,
                None,
                Some(19_186),
                Some(821),
                Some(134_676),
                Some(834)
            ]
        );
        // The two staged runs started days ago and no worker is on them: stuck. A closed run
        // never is.
        let stuck: Vec<bool> = records.iter().map(|r| r.stuck).collect();
        assert_eq!(
            stuck,
            vec![
                false, false, false, false, false, false, true, true, false, false, false, false
            ]
        );
        assert_eq!(
            records[9].detail.as_deref(),
            Some("import not confirmed; the output stays in staging")
        );
        assert!(records[9].output.is_some() && records[9].result.is_none());
        assert!(
            records[11]
                .detail
                .as_deref()
                .unwrap()
                .starts_with("the arr now holds")
        );
        // A run of a service that is not configured any more keeps its own words.
        let mut orphan = runs[0].clone();
        orphan.integration = "gone".to_string();
        let record = reading.record(&orphan);
        assert_eq!(
            (record.flow_label.as_str(), record.trigger.kind.as_str()),
            ("hevc", "other")
        );
        assert_eq!(record.trigger.label, "hush_rules");
    }

    #[test]
    fn a_worker_on_a_run_s_queue_path_makes_it_working_and_never_stuck() {
        let engines = engines();
        let runs = stored(&ledger());
        let staged = &runs[6];
        let queue = staged.queue_path.clone().unwrap();
        assert!(queue.contains("/hush/queue/movies/audio_1080p/"));
        let workers = [worker("/somewhere/else.mkv"), worker(&queue)];
        let reading = Reading {
            engines: &engines,
            workers: &workers,
            files: &[],
            asked_at: None,
            now: at("2026-11-23T16:00:00Z"),
            stuck_after: chrono::Duration::seconds(21_600),
        };
        let record = reading.record(staged);
        assert_eq!(record.state, "working");
        assert!(!record.stuck, "a worker is on it");
        let on_it = record.worker.unwrap();
        assert_eq!(
            (on_it.service.as_str(), on_it.node.as_deref(), on_it.percent),
            ("tdarr", Some("node-1"), Some(43.2))
        );
        assert_eq!(on_it.file.as_deref(), Some(queue.as_str()));
        assert_eq!(on_it.started_at, Some(at("2026-11-23T15:01:00Z")));
        // The other staged run still waits, and is stuck.
        let other = reading.record(&runs[7]);
        assert_eq!((other.state.as_str(), other.stuck), ("staged", true));
        assert!(other.worker.is_none());
        // The superseded run waited in the same place as the staged one: only the staged run
        // is the worker's.
        let same_place = [worker(runs[7].queue_path.as_deref().unwrap())];
        assert_eq!(runs[7].queue_path, runs[8].queue_path);
        let open: Vec<Run> = runs.iter().filter(|r| r.is_open()).cloned().collect();
        assert_eq!(open.len(), 2);
        assert_eq!(
            run_of(&same_place[0], &open, &[]).map(|r| r.id.as_str()),
            Some("hush:20261111T214740Z-e90af04d")
        );
        assert_eq!(run_of(&workers[0], &open, &[]), None);
        let closed = Reading {
            workers: &same_place,
            ..reading
        };
        assert_eq!(closed.record(&runs[8]).state, "superseded");
        assert!(closed.record(&runs[8]).worker.is_none());
        // With a longer threshold nothing is stuck yet.
        let patient = Reading {
            engines: &engines,
            workers: &[],
            files: &[],
            asked_at: None,
            now: at("2026-11-23T16:00:00Z"),
            stuck_after: chrono::Duration::days(30),
        };
        assert!(runs.iter().all(|r| !patient.is_stuck(r)));
        let live = Live {
            workers: workers.to_vec(),
            ..Live::default()
        };
        assert_eq!(
            live.busy_paths(),
            vec!["/somewhere/else.mkv".to_string(), queue]
        );
    }

    #[test]
    fn modes_that_share_a_label_are_one_flow() {
        let engines = engines();
        let (integrations, modes, flows) = flow_groups(&engines);
        assert_eq!(integrations.len(), 11);
        assert!(integrations.iter().all(|i| i == "hush"));
        let group = |mode: &str| {
            modes
                .iter()
                .position(|m| m == mode)
                .map(|i| flows[i].as_str())
        };
        assert_eq!(group("uhd"), Some("encode"));
        assert_eq!(group("encode"), Some("encode"));
        assert_eq!(group("uhd-audio"), Some("audio"));
        assert_eq!(group("hevc"), Some("hevc"));
        assert_eq!(group("unrecorded"), None);

        // A filter by a flow's id asks for all of its modes; by a mode, for that mode.
        assert_eq!(
            flow_modes(&engines, None, "audio"),
            vec!["audio", "uhd-audio"]
        );
        assert_eq!(
            flow_modes(&engines, Some("hush"), "encode"),
            vec!["encode", "uhd"]
        );
        assert_eq!(flow_modes(&engines, None, "uhd-audio"), vec!["uhd-audio"]);
        assert_eq!(flow_modes(&engines, Some("other"), "audio"), vec!["audio"]);
        assert_eq!(flow_modes(&engines, None, "unrecorded"), vec!["unrecorded"]);

        let stat = |integration: &str, flow: &str, runs: i64| FlowStat {
            integration: integration.to_string(),
            flow: flow.to_string(),
            runs,
            imported: runs,
            last_run: None,
            source_bytes: None,
            result_bytes: None,
            median_seconds: None,
        };
        let all = flow_summaries(
            &engines,
            vec![
                stat("gone", "x", 1),
                stat("hush", "audio", 2),
                stat("hush", "unrecorded", 3),
            ],
        );
        let ids: Vec<(&str, &str)> = all
            .iter()
            .map(|f| (f.service.as_str(), f.flow.id.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("hush", "hevc"),
                ("hush", "av1"),
                ("hush", "encode"),
                ("hush", "audio"),
                ("hush", "downscale"),
                ("hush", "shrink"),
                ("hush", "dual-encode"),
                ("hush", "dual-audio"),
                ("hush", "strip"),
                ("gone", "x"),
                ("hush", "unrecorded"),
            ]
        );
        assert_eq!(all[3].stat.as_ref().map(|s| s.runs), Some(2));
        assert!(
            all[0].stat.is_none(),
            "a flow no run has used yet is still offered"
        );
        // A flow no adapter offers is named by its id and says nothing more.
        let unknown = &all[10];
        assert_eq!(
            (
                unknown.flow.label.as_str(),
                &unknown.flow.about,
                &unknown.flow.tag
            ),
            ("unrecorded", &None, &None)
        );
        assert_eq!(unknown.flow.modes, vec!["unrecorded"]);
        assert_eq!(unknown.stat.as_ref().map(|s| s.runs), Some(3));
    }

    #[test]
    fn a_job_no_title_holds_stays_unlinked() {
        let job = ProcessorRun {
            job_id: "j9".to_string(),
            flow: "hevc".to_string(),
            trigger: String::new(),
            state: "staged".to_string(),
            created: at("2026-11-23T10:00:00Z"),
            updated: None,
            finished: None,
            queue_path: None,
            source: Some(ProcessorFile {
                path: "/nowhere/file.mkv".to_string(),
                size: None,
            }),
            output: None,
            result: None,
            manager_file_id: None,
            replaced: Vec::new(),
            detail: None,
            events: Vec::new(),
            raw: Value::Null,
        };
        assert_eq!(link(&job, &folders(), &Named::default()), Link::default());
        let run = Run::listed("hush", &job, Link::default(), at("2026-11-23T10:00:05Z"));
        assert_eq!(run.id, "hush:j9", "a run keeps its service in its id");
        assert_eq!(
            (run.source_path.as_deref(), run.source_size, run.title_id),
            (Some("/nowhere/file.mkv"), None, None)
        );
        assert!(run.is_open() && run.replaced.is_empty() && run.events.is_empty());
    }

    /// A worker service with a configuration of its own and records of some files.
    struct Worker {
        id: &'static str,
        setup: Option<Arc<WorkerSetup>>,
        /// What its records say of a file, by the path it is asked for. A path that is not
        /// here fails the question.
        files: Vec<(String, FileAnswer)>,
        asked: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Processor for Worker {
        fn id(&self) -> &str {
            self.id
        }
        fn kind(&self) -> &'static str {
            "worker"
        }
        fn role(&self) -> &'static str {
            "worker"
        }
        async fn status(&self) -> Result<ProcessorStatus, EngineError> {
            Ok(ProcessorStatus {
                version: None,
                detail: String::new(),
                facts: None,
                nodes: None,
            })
        }
        async fn runs(&self, _limit: u32) -> Result<Vec<ProcessorRun>, EngineError> {
            Ok(Vec::new())
        }
        async fn workers(&self) -> Result<Vec<ProcessorWorker>, EngineError> {
            Ok(Vec::new())
        }
        fn flows(&self) -> Vec<ProcessorFlow> {
            Vec::new()
        }
        fn setup(&self) -> Option<Arc<WorkerSetup>> {
            self.setup.clone()
        }
        async fn file(&self, path: &str) -> Result<FileAnswer, EngineError> {
            self.asked.lock().unwrap().push(path.to_string());
            match self.files.iter().find(|(known, _)| known == path) {
                Some((_, answer)) => Ok(answer.clone()),
                None => Err(EngineError::Decode {
                    engine: self.id.to_string(),
                    what: "file",
                    message: "no answer".to_string(),
                }),
            }
        }
    }

    const AUDIO_QUEUE: &str = "/media/hush/queue/movies/audio_1080p";
    const SHOWS_QUEUE: &str = "/media/hush/queue/shows/hevc_1080p";

    /// A worker's configuration: a library for each of two queue folders, and their flows.
    fn setup() -> Arc<WorkerSetup> {
        let library = |id: &str, name: &str, folder: &str, flow: &str| WorkerLibrary {
            id: id.to_string(),
            name: name.to_string(),
            folder: folder.to_string(),
            flow_id: Some(flow.to_string()),
            priority: Some(1),
            held: false,
            watching: true,
            transcodes: 0,
            health_checks: 0,
            saved_bytes: 0,
        };
        Arc::new(WorkerSetup {
            libraries: vec![
                library("lib-audio", "Movies, audio", AUDIO_QUEUE, "master"),
                library("lib-shows", "Shows, HEVC", SHOWS_QUEUE, "gone"),
            ],
            flows: vec![WorkerFlow {
                id: "master".to_string(),
                name: "Master encode".to_string(),
                about: Some("The quality work.".to_string()),
                steps: vec!["Input".to_string(), "Encode".to_string()],
            }],
        })
    }

    fn worker_service(
        id: &'static str,
        setup: Option<Arc<WorkerSetup>>,
        files: Vec<(String, FileAnswer)>,
    ) -> Arc<Worker> {
        Arc::new(Worker {
            id,
            setup,
            files,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn with_workers(workers: &[Arc<Worker>]) -> EngineSet {
        let mut all: Vec<Arc<dyn Processor>> = vec![bridge(Recorded::new())];
        all.extend(workers.iter().map(|w| w.clone() as Arc<dyn Processor>));
        EngineSet::new(Vec::new(), None, Vec::new()).with_processors(all)
    }

    fn record_of(state: Option<&'static str>, words: &str, hold_until: Option<&str>) -> WorkerFile {
        WorkerFile {
            library: Some("lib".to_string()),
            state,
            words: Some(words.to_string()),
            health: None,
            old_size: None,
            new_size: None,
            transcoded_at: None,
            hold_until: hold_until.map(at),
            footprint: None,
        }
    }

    #[tokio::test]
    async fn a_run_says_where_it_is_in_the_worker_s_records() {
        let runs = stored(&ledger());
        // The two staged runs of the ledger, the older first, and the paths they wait at.
        let (in_audio, in_shows) = (&runs[6], &runs[7]);
        let audio_path = in_audio.queue_path.clone().unwrap();
        let shows_path = in_shows.queue_path.clone().unwrap();
        assert!(audio_path.starts_with(AUDIO_QUEUE) && shows_path.starts_with(SHOWS_QUEUE));
        let open = [in_shows.clone(), in_audio.clone()];
        let now = at("2026-11-23T16:00:00Z");
        let asked_at = Some(at("2026-11-23T15:59:58Z"));
        let six_hours = chrono::Duration::seconds(21_600);

        // The worker has a record of the one and none of the other.
        let tdarr = worker_service(
            "tdarr",
            Some(setup()),
            vec![
                (
                    audio_path.clone(),
                    FileAnswer::Known(record_of(Some("queued"), "Queued", None)),
                ),
                (shows_path.clone(), FileAnswer::Absent),
            ],
        );
        let engines = with_workers(std::slice::from_ref(&tdarr));
        // A file is placed in the library whose folder it lies below, with that library's
        // flow. A flow the worker does not list is named by its id.
        let placed_audio = placed(&engines, &audio_path).unwrap();
        assert_eq!(
            (
                placed_audio.service.as_str(),
                placed_audio.library.as_ref().map(|l| l.id.as_str()),
                placed_audio.flow.as_ref().map(|f| f.name.as_str())
            ),
            ("tdarr", Some("lib-audio"), Some("Master encode"))
        );
        let placed_shows = placed(&engines, &shows_path).unwrap();
        assert_eq!(
            placed_shows.flow,
            Some(WorkerFlow {
                id: "gone".to_string(),
                name: "gone".to_string(),
                about: None,
                steps: Vec::new()
            })
        );
        // A path in no library is the one worker's all the same, with no library.
        assert_eq!(
            placed(&engines, "/elsewhere/a.mkv"),
            Some(Placed {
                service: "tdarr".to_string(),
                library: None,
                flow: None
            })
        );

        // The files of the open runs are asked for, the oldest run first, once each. A closed
        // run is not asked about.
        let files = live_files(&engines, &[], &runs).await;
        assert_eq!(
            *tdarr.asked.lock().unwrap(),
            vec![audio_path.clone(), shows_path.clone()],
            "in the order given, and the superseded run at the same path is not asked about"
        );
        tdarr.asked.lock().unwrap().clear();
        let files_again = live_files(&engines, &[], &open).await;
        assert_eq!(files_again.len(), 2);
        assert_eq!(
            files,
            vec![
                LiveFile {
                    service: "tdarr".to_string(),
                    path: audio_path.clone(),
                    file: Some(record_of(Some("queued"), "Queued", None)),
                },
                LiveFile {
                    service: "tdarr".to_string(),
                    path: shows_path.clone(),
                    file: None,
                },
            ]
        );
        let reading = Reading {
            engines: &engines,
            workers: &[],
            files: &files,
            asked_at,
            now,
            stuck_after: six_hours,
        };
        // An open run: its library, its flow, where its file stands now, and when that was
        // asked. A stuck one says why in a sentence.
        let waiting = reading.record(in_audio);
        assert_eq!(
            serde_json::to_value(&waiting.in_worker).unwrap(),
            json!({
                "service": "tdarr",
                "library": {"id": "lib-audio", "name": "Movies, audio"},
                "flow": {"id": "master", "name": "Master encode"},
                "state": "queued", "words": "Queued", "checkedAt": "2026-11-23T15:59:58Z"
            })
        );
        assert!(waiting.stuck);
        assert_eq!(waiting.stuck_why.as_deref(), Some("In worker queue"));
        assert_eq!(reading.stuck_why(in_audio), waiting.stuck_why);
        let lost = reading.record(in_shows);
        let lost_in = lost.in_worker.as_ref().unwrap();
        assert_eq!(
            (lost_in.state.as_deref(), lost_in.words.as_deref()),
            (Some("absent"), None)
        );
        assert_eq!(
            lost_in.library.as_ref().map(|l| l.name.as_str()),
            Some("Shows, HEVC")
        );
        assert_eq!(lost.stuck_why.as_deref(), Some("Not in worker queue"));
        // Read by its id, a run also says what its flow is for, and its steps.
        let full = reading.in_worker(in_audio, true).unwrap();
        let flow = full.flow.unwrap();
        assert_eq!(flow.about.as_deref(), Some("The quality work."));
        assert_eq!(
            flow.steps,
            Some(vec!["Input".to_string(), "Encode".to_string()])
        );
        let unknown_flow = reading.in_worker(in_shows, true).unwrap().flow.unwrap();
        assert_eq!(
            (unknown_flow.about, unknown_flow.steps),
            (None, Some(Vec::new()))
        );
        // A closed run: where it was, and nothing of now. The superseded run waited at the
        // path of the staged one, whose record is not its own.
        let closed = reading.record(&runs[8]);
        assert_eq!(
            serde_json::to_value(&closed.in_worker).unwrap(),
            json!({
                "service": "tdarr",
                "library": {"id": "lib-shows", "name": "Shows, HEVC"},
                "flow": {"id": "gone", "name": "gone"},
                "state": null, "words": null, "checkedAt": null
            })
        );
        assert_eq!(closed.stuck_why, None);
        // A run whose queue no library holds, closed: nothing is known of it.
        assert!(
            runs[5]
                .queue_path
                .as_deref()
                .unwrap()
                .contains("/downscale/")
        );
        assert_eq!(reading.record(&runs[5]).in_worker, None);
        // A run that is not stuck yet has no sentence, whatever the worker says.
        let patient = Reading {
            stuck_after: chrono::Duration::days(30),
            ..reading
        };
        let young = patient.record(in_shows);
        assert!(!young.stuck && young.stuck_why.is_none());
        assert_eq!(young.in_worker.unwrap().state.as_deref(), Some("absent"));
        assert_eq!(patient.stuck_why(in_shows), None);

        // What the worker's records can say of a file that waits, and what a stuck run makes
        // of it.
        let said = |file: WorkerFile| {
            let files = [LiveFile {
                service: "tdarr".to_string(),
                path: audio_path.clone(),
                file: Some(file),
            }];
            let record = Reading {
                files: &files,
                ..reading
            }
            .record(in_audio);
            let in_worker = record.in_worker.unwrap();
            (in_worker.state, in_worker.words, record.stuck_why)
        };
        assert_eq!(
            said(record_of(Some("failed"), "Transcode error", None)),
            (
                Some("failed".to_string()),
                Some("Transcode error".to_string()),
                Some("Worker failed: Transcode error".to_string())
            )
        );
        // Held back until a time that is still ahead: held. Until one that has passed: queued.
        assert_eq!(
            said(record_of(
                Some("queued"),
                "Queued",
                Some("2026-11-24T03:10:00Z")
            )),
            (
                Some("held".to_string()),
                Some("Queued".to_string()),
                Some("Held until 2026-11-24 03:10 UTC".to_string())
            )
        );
        assert_eq!(
            said(record_of(
                Some("queued"),
                "Queued",
                Some("2026-11-23T03:10:00Z")
            ))
            .0,
            Some("queued".to_string())
        );
        // What the worker is done with says so, and explains nothing: no sentence.
        for (state, words) in [
            (Some("done"), "Transcode success"),
            (Some("notRequired"), "Not required"),
            (None, "Something new"),
        ] {
            let (shown, said_words, why) =
                said(record_of(state, words, Some("2026-11-24T03:10:00Z")));
            if state.is_some() {
                assert_eq!(shown.as_deref(), state, "{words}");
            } else {
                assert_eq!(
                    shown.as_deref(),
                    Some("held"),
                    "words it does not know, held back"
                );
            }
            assert_eq!(said_words.as_deref(), Some(words));
            assert_eq!(why.is_some(), state.is_none(), "{words}");
        }

        // A worker on the run: it is working, in the worker's own word, and its record is not
        // asked for.
        let busy = [worker(&audio_path)];
        assert!(
            live_files(&engines, &busy, std::slice::from_ref(in_audio))
                .await
                .is_empty()
        );
        let on_it = Reading {
            workers: &busy,
            files: &[],
            ..reading
        }
        .record(in_audio);
        let in_worker = on_it.in_worker.unwrap();
        assert_eq!(
            (
                in_worker.state.as_deref(),
                in_worker.words.as_deref(),
                in_worker.checked_at
            ),
            (Some("working"), Some("Processing"), asked_at)
        );
        assert!(!on_it.stuck && on_it.stuck_why.is_none());

        // A question the worker fails is no answer: the library is known, the state is not.
        let silent = worker_service("tdarr", Some(setup()), Vec::new());
        let engines = with_workers(std::slice::from_ref(&silent));
        assert!(live_files(&engines, &[], &open).await.is_empty());
        assert_eq!(silent.asked.lock().unwrap().len(), 2);
        let unknown = Reading {
            engines: &engines,
            files: &[],
            ..reading
        }
        .record(in_audio);
        let in_worker = unknown.in_worker.unwrap();
        assert_eq!((in_worker.state, in_worker.checked_at), (None, None));
        assert!(in_worker.library.is_some());
        assert!(unknown.stuck && unknown.stuck_why.is_none());

        // A worker that gave no configuration: its records still say where a file stands.
        let bare = worker_service(
            "tdarr",
            None,
            vec![(audio_path.clone(), FileAnswer::Absent)],
        );
        let engines = with_workers(std::slice::from_ref(&bare));
        let files = live_files(&engines, &[], std::slice::from_ref(in_audio)).await;
        let record = Reading {
            engines: &engines,
            files: &files,
            ..reading
        }
        .record(in_audio);
        assert_eq!(
            serde_json::to_value(&record.in_worker).unwrap(),
            json!({
                "service": "tdarr", "library": null, "flow": null,
                "state": "absent", "words": null, "checkedAt": "2026-11-23T15:59:58Z"
            })
        );
        // And nothing is known of a closed run of such a worker.
        assert_eq!(
            Reading {
                engines: &engines,
                files: &files,
                ..reading
            }
            .record(&runs[0])
            .in_worker,
            None
        );

        // Two workers: a file is the one's whose library holds it; in no library, nobody's.
        let other = worker_service("other", None, Vec::new());
        let engines = with_workers(&[tdarr.clone(), other]);
        assert_eq!(placed(&engines, &audio_path).unwrap().service, "tdarr");
        assert_eq!(placed(&engines, "/elsewhere/a.mkv"), None);
        // No worker at all, or a run that waits nowhere: nothing is said.
        let bridge_alone = self::engines();
        assert_eq!(placed(&bridge_alone, &audio_path), None);
        let alone = Reading {
            engines: &bridge_alone,
            files: &[],
            ..reading
        };
        assert_eq!(alone.record(in_audio).in_worker, None);
        assert!(live_files(&bridge_alone, &[], &open).await.is_empty());
        let mut nowhere = in_audio.clone();
        nowhere.queue_path = None;
        assert_eq!(reading.record(&nowhere).in_worker, None);
    }

    /// Where the run waited that a worker was on when the worker's answers were saved.
    const TRON_QUEUE: &str = "/media/hush/queue/movies/audio_4k/Tron (1982) {tmdb-97}/Tron (1982) {imdb-tt0084827} [Remux-2160p Proper][DV HDR10][TrueHD Atmos 7.1][HEVC]-TRiToN.mkv";

    #[tokio::test]
    async fn a_run_is_working_while_its_worker_runs_a_flow_on_a_copy_of_its_file() {
        use crate::engines::tdarr;
        // The worker's answers as they were saved while it ran a flow on a file: its nodes
        // with the busy worker, and its record of the file that waited in the queue.
        let nodes: Value = serde_json::from_str(&fixture("tdarr_nodes_busy_live.json")).unwrap();
        let live_workers = |nodes: &Value| -> Vec<LiveWorker> {
            tdarr::workers(nodes)
                .into_iter()
                .map(|worker| LiveWorker {
                    service: "tdarr".to_string(),
                    worker,
                })
                .collect()
        };
        let busy = live_workers(&nodes);
        assert_eq!(busy.len(), 1);
        let waiting: Value =
            serde_json::from_str(&fixture("tdarr_file_working_live.json")).unwrap();
        let record = tdarr::file_record(&waiting).unwrap();
        // The worker is on a copy of the file in its own folder, under the file's own name,
        // and names the footprint its record of the waiting file carries.
        let on = busy[0].worker.file.clone().unwrap();
        assert_ne!(on, TRON_QUEUE);
        assert_eq!(file_name(&on), file_name(TRON_QUEUE));
        assert_eq!(busy[0].worker.footprint.as_deref(), Some("QEdffjm"));
        assert_eq!(record.footprint.as_deref(), Some("QEdffjm"));

        // The run as the bridge has it: staged days ago, waiting at its queue path.
        let base = ledger()[6].clone();
        assert_eq!(base.state, "staged");
        let staged_at = |job_id: &str, queue: &str| {
            let mut job = base.clone();
            job.job_id = job_id.to_string();
            job.queue_path = Some(queue.to_string());
            Run::listed("hush", &job, Link::default(), at("2026-11-23T10:00:05Z"))
        };
        let run = staged_at("tron", TRON_QUEUE);
        let worker = worker_service(
            "tdarr",
            None,
            vec![(TRON_QUEUE.to_string(), FileAnswer::Known(record.clone()))],
        );
        let engines = with_workers(std::slice::from_ref(&worker));

        // The worker is asked for its record of the run's file, and the footprint joins the
        // run to the worker: the run is working, with that worker's progress, and not stuck.
        let open = [run.clone()];
        let files = live_files(&engines, &busy, &open).await;
        assert_eq!(
            files,
            vec![LiveFile {
                service: "tdarr".to_string(),
                path: TRON_QUEUE.to_string(),
                file: Some(record.clone()),
            }]
        );
        let reading = Reading {
            engines: &engines,
            workers: &busy,
            files: &files,
            asked_at: Some(at("2026-11-23T22:19:00Z")),
            now: at("2026-11-23T22:19:05Z"),
            stuck_after: chrono::Duration::seconds(21_600),
        };
        let read = reading.record(&run);
        assert_eq!(read.state, "working");
        assert!(!read.stuck && read.stuck_why.is_none());
        let on_it = read.worker.unwrap();
        assert_eq!(
            (
                on_it.service.as_str(),
                on_it.status.as_deref(),
                on_it.file.as_deref()
            ),
            ("tdarr", Some("Movie Finalizer (Audio)"), Some(on.as_str()))
        );
        let in_worker = read.in_worker.unwrap();
        assert_eq!(
            (in_worker.state.as_deref(), in_worker.words.as_deref()),
            (Some("working"), Some("Movie Finalizer (Audio)"))
        );
        assert_eq!(
            run_of(&busy[0], &open, &files).map(|r| r.id.as_str()),
            Some("hush:tron")
        );
        // The filters by state know the run by the path it waits at.
        let live = Live {
            workers: busy.clone(),
            files: files.clone(),
            ..Live::default()
        };
        assert_eq!(live.busy_paths(), vec![on.clone(), TRON_QUEUE.to_string()]);

        // A worker that names another footprint, or none because it reports no job, leaves
        // the run waiting, whatever the name of its file: the run is stuck, and what the
        // worker's record says of its file stands.
        let mut other = busy.clone();
        other[0].worker.footprint = Some("another".to_string());
        let mut jobless = nodes.clone();
        for node in jobless.as_object_mut().unwrap().values_mut() {
            for entry in node["workers"].as_object_mut().unwrap().values_mut() {
                entry.as_object_mut().unwrap().remove("job");
            }
        }
        let jobless = live_workers(&jobless);
        assert_eq!(
            (jobless.len(), jobless[0].worker.footprint.as_deref()),
            (1, None)
        );
        for workers in [&other, &jobless] {
            let waiting = Reading {
                workers: workers.as_slice(),
                ..reading
            }
            .record(&run);
            assert_eq!(waiting.state, "staged");
            assert!(waiting.worker.is_none() && waiting.stuck);
            let in_worker = waiting.in_worker.unwrap();
            assert_eq!(
                (in_worker.state.as_deref(), in_worker.words.as_deref()),
                (Some("done"), Some("Transcode success"))
            );
            assert_eq!(run_of(&workers[0], &open, &files), None);
            let live = Live {
                workers: workers.clone(),
                files: files.clone(),
                ..Live::default()
            };
            assert_eq!(live.busy_paths(), vec![on.clone()]);
        }

        // The runs a busy worker may be on are asked about first, so the limit of questions
        // never costs the answer the record that joins them. The name only orders the
        // questions.
        let mut many: Vec<Run> = (0..FILES_ASKED + 5)
            .map(|n| {
                staged_at(
                    &format!("old-{n}"),
                    &format!("/media/hush/queue/movies/audio_4k/Old {n}/old {n}.mkv"),
                )
            })
            .collect();
        many.push(run.clone());
        worker.asked.lock().unwrap().clear();
        let files = live_files(&engines, &busy, &many).await;
        let asked = worker.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), FILES_ASKED);
        assert_eq!(asked[0], TRON_QUEUE);
        assert_eq!(files.len(), 1, "the worker has a record of that file alone");
        // Without a worker that names a footprint the oldest runs are asked about first, and
        // the newest is past the limit.
        let nobody: Vec<LiveWorker> = Vec::new();
        for workers in [&jobless, &nobody] {
            worker.asked.lock().unwrap().clear();
            live_files(&engines, workers, &many).await;
            let asked = worker.asked.lock().unwrap().clone();
            assert_eq!(asked.len(), FILES_ASKED);
            assert!(!asked.iter().any(|path| path == TRON_QUEUE));
        }
    }
}
