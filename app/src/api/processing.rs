//! The `/api/v1` routes of processing: what the processing services did to each file after
//! its import, and what they do now.
//!
//! The runs come from the processing module's table, which its pass writes. How a service is
//! and what its workers are on is asked when a route needs it (through the `Processor` port,
//! kept a few seconds in the page cache) and joined to the runs as they are read: a staged run
//! a worker is on is `working`, and an open run that started long ago with no worker on it is
//! stuck. Where the file of an open run stands in a worker's own records is asked with it, and
//! says why a stuck run is stuck. A worker's libraries, flows and totals are what its pass
//! last read, never asked for a page. With the sync switched off no service is asked and the
//! routes answer with what is stored. Every route needs a caller with `read`.
//!
//! One route changes something outside Singularr: `POST /processing/runs` hands one file to one
//! flow of a service. It is the owner's alone, it is refused for a title whose manager entry
//! carries the tag the service's adapter names as protecting, and it is refused while the
//! access of its action `send` is `read` (here, with a sentence, and again on the way out, by
//! `core::outbound`). `GET /processing/options` says beforehand what a hand-off would do,
//! and changes nothing. A client names a file by its title and its name: where the file lies
//! is worked out here, from the title's own files.
//!
//! The title record and a season's episodes read the same runs through this module
//! (`title_processing`, `FileMarks`): a title's own runs, and which of its files a run made or
//! is working on.

use super::metadata;
use super::pages::labels;
use super::routes::{ApiJson, ApiPath, ApiQuery};
use super::state::AppState;
use crate::catalog::store::{self as catalog_store, TitleFile, TitleRef};
use crate::core::auth::Caller;
use crate::core::error::{AppError, AppResult};
use crate::core::outbound::{Access, OutboundError};
use crate::core::schema::ProblemDoc;
use crate::core::units;
use crate::engines::access::read_only_words;
use crate::engines::service::{integration_view, processing_role};
use crate::engines::store as engine_store;
use crate::engines::{
    EngineError, Processor, ProcessorNode, QueuedFile, Refusal, WorkerCounts, WorkerFlow,
    WorkerLibrary, WorkerQueue, WorkerReport, WorkerSetup, WorkerTotals,
};
use crate::metadata::record::{
    FileProcessing, ProcessingRun, RunFlow, RunLibrary, RunWorker, TitleProcessing,
};
use crate::processing::model::{
    IMPORTED, Moment, Run, RunFilter, StateFilter, VERIFYING, file_name, queue_of, run_id, unfit,
};
use crate::processing::service::{
    self as processing, FlowSummary, Live, LiveService, Reading, RunReports, SendOutcome, Sending,
    flow_modes, flow_summaries, run_of, run_worker, stuck_after,
};
use crate::processing::store;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use utoipa::{IntoParams, OpenApi, ToSchema};

type ApiResult<T> = Result<Json<T>, AppError>;

/// How long what the services say now is kept: a worker's progress moves by the second. An
/// answer that misses a service that did not answer is kept longer, so a service that is down
/// is not asked by every page load.
const LIVE_KEEP: Duration = Duration::from_secs(5);
const LIVE_KEEP_DOWN: Duration = Duration::from_secs(20);
/// How long the workers have to say where the files of the open runs stand in their records.
/// Past it, nothing is known of those files for this answer.
const FILES_WAIT: Duration = Duration::from_secs(4);
/// Closed runs on the page, and runs on a page of the list.
const RECENT: i64 = 25;
const LIMIT_DEFAULT: i64 = 50;
const LIMIT_MAX: i64 = 500;
/// Runs in a title's record: its newest. `total` says how many there are.
const TITLE_RUNS: i64 = 50;
/// How many days a run that failed or was refused stays in the attention lists.
const ALERT_DAYS: i64 = 7;
/// Runs that failed or were refused in the attention lists, at most.
const ALERT_MAX: i64 = 50;
/// How long the attention lists wait for the services to say what their workers are on.
const ALERT_WAIT: Duration = Duration::from_millis(1500);

// What the routes answer ---------------------------------------------------------------------------

/// How many workers of each kind: a node's limits, or the lengths of its queues.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerCountsView {
    pub transcode_gpu: i64,
    pub transcode_cpu: i64,
    pub health_gpu: i64,
    pub health_cpu: i64,
}

/// One node of a worker service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct NodeView {
    pub id: String,
    pub name: String,
    pub paused: bool,
    /// Workers busy now.
    pub busy: i64,
    pub limits: WorkerCountsView,
    pub queue: WorkerCountsView,
}

/// One processing service and how it is.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    /// The integration id.
    pub id: String,
    /// The integration type.
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    /// `bridge` (it hands files to a worker and has the results imported) or `worker`.
    pub role: String,
    /// `ok`; `bad` when it does not answer; `warn` when it is listed and not configured, or
    /// was not asked yet.
    pub state: String,
    /// Its state in a line, or why it is not `ok`.
    pub detail: String,
    pub version: Option<String>,
    /// The last pass that read it.
    pub last_sync: Option<DateTime<Utc>>,
    /// A bridge's own facts.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub facts: Option<Value>,
    /// A worker service's nodes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<NodeView>>,
    /// A worker service in numbers, as its last pass read them. Absent when it gave none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub totals: Option<WorkerTotalsView>,
    /// A worker service's libraries, in its own order of priority: the folders it watches and
    /// the flow it runs in each. Absent when it gave none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub libraries: Option<Vec<WorkerLibraryView>>,
    /// A worker service's flows, with their steps. Absent when it gave none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flows: Option<Vec<WorkerFlowView>>,
    /// The first files of a worker service's queue in its own order, as its pass last read
    /// them. Absent when it gave none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<WorkerQueueView>,
}

/// One file in a worker's queue.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueuedFileView {
    pub path: String,
    /// Its name, the last part of the path.
    pub file: String,
    /// The worker library it is in, named from the worker's own list.
    pub library: Option<RunLibrary>,
    pub size: Option<i64>,
    /// The video codec and resolution, in the worker's own words.
    pub codec: Option<String>,
    pub resolution: Option<String>,
    /// Moved to the front of the queue.
    pub bumped: bool,
    /// The worker holds it back until then.
    pub hold_until: Option<DateTime<Utc>>,
    /// When the worker first recorded it.
    pub added: Option<DateTime<Utc>>,
}

/// The first files of a worker's queue, in the order it takes them, and how many it holds.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerQueueView {
    pub total: i64,
    pub files: Vec<QueuedFileView>,
    pub read_at: DateTime<Utc>,
}

/// One report a worker keeps of a job it ran on a run's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReportView {
    /// The report's name: what `GET /api/v1/processing/runs/{id}/reports/{report}` takes.
    pub id: String,
    /// The job it reports, by the worker's id of it.
    pub job: Option<String>,
    /// What the job was, in the worker's word: `transcode`, `healthcheck`.
    pub kind: Option<String>,
    /// When the job started.
    pub at: Option<DateTime<Utc>>,
    /// The version of the worker that ran it.
    pub version: Option<String>,
}

/// The reports a worker keeps of the jobs it ran on a run's file, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RunReportsView {
    /// The integration id of the worker service.
    pub service: String,
    /// The worker's mark of the file.
    pub footprint: String,
    pub reports: Vec<ReportView>,
}

/// `GET /api/v1/processing/runs/{id}/reports/{report}`: one report with its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ReportTextView {
    #[serde(flatten)]
    pub report: ReportView,
    pub text: String,
    /// The job still runs, so the text still grows.
    pub running: bool,
    /// False: the worker has no such report.
    pub exists: bool,
}

/// How many files a worker service has in each of its tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerTablesView {
    /// Waiting to be transcoded.
    pub queue: i64,
    /// Transcoded, or not in need of it.
    pub done: i64,
    /// The transcode failed.
    pub failed: i64,
    /// Waiting for a health check.
    pub health_queue: i64,
    pub healthy: i64,
    /// The health check failed.
    pub health_failed: i64,
}

/// A worker service in numbers, over everything it ever did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerTotalsView {
    /// Files it holds a record of now.
    pub files: i64,
    pub transcodes: i64,
    pub health_checks: i64,
    /// Bytes its transcodes saved.
    pub saved_bytes: i64,
    pub tables: WorkerTablesView,
}

/// A flow of a worker service, by its id and its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WorkerFlowRef {
    pub id: String,
    pub name: String,
}

/// One library of a worker service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerLibraryView {
    pub id: String,
    pub name: String,
    /// The folder it watches.
    pub folder: String,
    /// The flow it runs on the files that appear there. A flow the worker does not list is
    /// named by its id.
    pub flow: Option<WorkerFlowRef>,
    pub priority: Option<i64>,
    /// New files are held back for a while before a worker takes them.
    pub held: bool,
    /// It watches its folder for new files.
    pub watching: bool,
    /// Files it transcoded.
    pub transcodes: i64,
    /// Bytes its transcodes saved.
    pub saved_bytes: i64,
}

/// One flow of a worker service: the steps a file goes through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WorkerFlowView {
    pub id: String,
    pub name: String,
    /// What it does, in its author's words.
    pub about: Option<String>,
    /// The names of its steps, in the worker's order.
    pub steps: Vec<String>,
}

/// A worker that is busy now, with the run it is on.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct WorkerView {
    #[serde(flatten)]
    pub worker: RunWorker,
    /// The id of the run whose file it works on, when a run waits there.
    pub run: Option<String>,
}

/// The runs in numbers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CountsView {
    /// Runs that are not closed: working, waiting and verifying.
    pub open: i64,
    /// Staged, with a worker on them.
    pub working: i64,
    /// Staged, with no worker on them yet.
    pub waiting: i64,
    pub verifying: i64,
    /// Open for longer than the stuck threshold with no worker on them.
    pub stuck: i64,
    pub imported: i64,
    pub failed: i64,
    pub refused: i64,
    pub pruned: i64,
    pub superseded: i64,
    pub total: i64,
    /// Source size less result size, over the imported runs.
    pub saved_bytes: i64,
    /// The start of the oldest run known.
    pub since: Option<DateTime<Utc>>,
}

/// What a file must be for a flow to make sense. The interface greys a flow out with it; the
/// server enforces nothing with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NeedsView {
    /// The fewest lines the file must have.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_resolution: Option<i32>,
    /// Video codecs the file must not already be in, in the words the files are shown with.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_codec: Vec<String>,
}

/// One flow a person would choose, with its runs in numbers.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlowView {
    /// The first of its modes.
    pub id: String,
    pub label: String,
    /// One line about what it changes.
    pub about: Option<String>,
    /// The integration id of its service.
    pub service: String,
    /// The manager tag that starts it, where there is one.
    pub tag: Option<String>,
    /// Every id the service runs it under. A run's `flow` is one of them.
    pub modes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs: Option<NeedsView>,
    pub runs: i64,
    pub imported: i64,
    /// The start of its newest run.
    pub last_run: Option<DateTime<Utc>>,
    /// The share of their sources' size its imported runs saved.
    pub saved_percent: Option<f64>,
    /// The median of the seconds its imported runs took.
    pub median_seconds: Option<i64>,
}

/// The title a run is linked to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RunTitle {
    /// `movie-<tmdb>` or `show-<tmdb>`; null for a title without a TMDB id.
    pub id: Option<String>,
    /// `movie` or `show`.
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    /// A TMDB image path (`/abc.jpg`) or the full address the manager gives.
    pub poster: Option<String>,
}

/// One run, with the title it is linked to.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RunView {
    #[serde(flatten)]
    pub run: ProcessingRun,
    /// Null for a run whose files lie in no title's folder.
    pub title: Option<RunTitle>,
}

/// `GET /api/v1/processing/runs/{id}`: the run, and the job exactly as its service gave it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RunDetail {
    #[serde(flatten)]
    pub run: ProcessingRun,
    pub title: Option<RunTitle>,
    #[schema(value_type = Object)]
    pub raw: Value,
    /// The reports the worker keeps of the jobs it ran on the run's file, newest first; the
    /// text of each is asked on its own. Null when no worker holds a record of the file, or
    /// the worker keeps no reports.
    pub reports: Option<RunReportsView>,
}

/// `GET /api/v1/processing`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ProcessingPage {
    pub services: Vec<ServiceView>,
    /// What the workers are on now.
    pub workers: Vec<WorkerView>,
    pub counts: CountsView,
    /// The flows the services offer, in the order a person chooses from, then the flows runs
    /// name that no service offers.
    pub flows: Vec<FlowView>,
    /// Every run that is not closed, oldest first.
    pub open: Vec<RunView>,
    /// The 25 runs that closed last.
    pub recent: Vec<RunView>,
}

/// `GET /api/v1/processing/runs`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RunList {
    /// Newest first.
    pub runs: Vec<RunView>,
    /// Every run the filters match.
    pub total: i64,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RunsQuery {
    /// One state (`staged`, `working`, `verifying`, `imported`, `failed`, `refused`, `pruned`,
    /// `superseded`), or `open`, `closed` or `stuck`.
    pub state: Option<String>,
    /// A flow's id, for the runs of every mode of it; or one mode, a run's own `flow`.
    pub flow: Option<String>,
    /// An integration id.
    pub service: Option<String>,
    /// A title's id: `movie-603`, `show-1396`.
    pub title: Option<String>,
    /// Words of a file name: the source's, the result's, or that of what the worker made.
    pub q: Option<String>,
    /// Runs on a page: 50 when left out, at most 500.
    #[param(value_type = Option<i64>)]
    pub limit: Option<String>,
    /// Runs to skip.
    #[param(value_type = Option<i64>)]
    pub offset: Option<String>,
}

// Reading -------------------------------------------------------------------------------------------

/// How each processing service answers now and what their workers are on, kept a few seconds.
/// Nothing is asked when the integrations are not read.
async fn live(state: &AppState) -> Arc<Live> {
    state
        .pages
        .processing
        .make(|| async {
            if !state.config.sync.enabled || state.engines.processors.is_empty() {
                return (Live::default(), LIVE_KEEP);
            }
            let mut live = processing::live(&state.engines).await;
            // Where the files of the open runs stand in the workers' own records. A worker
            // that is slow to say costs this answer that knowledge, not the answer.
            match store::open(&state.pool).await {
                Ok(open) => {
                    let asked = processing::live_files(&state.engines, &live.workers, &open);
                    live.files = tokio::time::timeout(FILES_WAIT, asked)
                        .await
                        .unwrap_or_default();
                }
                Err(e) => tracing::warn!(error = %e, "the open runs could not be read"),
            }
            let keep = if live.all_answered() {
                LIVE_KEEP
            } else {
                LIVE_KEEP_DOWN
            };
            (live, keep)
        })
        .await
}

/// What the workers are on and what their records say, asked only when one of these runs
/// could be with a worker: an open run that waits at a path.
async fn live_for(state: &AppState, runs: &[Run]) -> Arc<Live> {
    if runs.iter().any(|r| r.is_open() && queue_of(r).is_some()) {
        live(state).await
    } else {
        Arc::new(Live::default())
    }
}

fn reading<'a>(state: &'a AppState, live: &'a Live) -> Reading<'a> {
    Reading {
        engines: &state.engines,
        workers: &live.workers,
        files: &live.files,
        asked_at: live.at,
        now: Utc::now(),
        stuck_after: stuck_after(&state.settings),
    }
}

/// The titles runs are linked to, by catalog id.
struct Titles(HashMap<i64, RunTitle>);

impl Titles {
    async fn of(state: &AppState, runs: &[&Run]) -> AppResult<Titles> {
        let mut ids: Vec<i64> = runs.iter().filter_map(|r| r.title_id).collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(Titles(
            catalog_store::title_cards(&state.pool, &ids)
                .await?
                .into_iter()
                .map(|c| {
                    let title = RunTitle {
                        id: labels::title_id(&c.kind, c.tmdb_id),
                        kind: c.kind,
                        title: c.title,
                        year: c.year,
                        poster: labels::poster(c.poster_url.as_deref()),
                    };
                    (c.id, title)
                })
                .collect(),
        ))
    }

    fn of_run(&self, run: &Run) -> Option<RunTitle> {
        run.title_id.and_then(|id| self.0.get(&id)).cloned()
    }

    /// A run with its title. The episode its file name reads as is kept for a run of a show.
    fn view(&self, reading: &Reading<'_>, run: &Run) -> RunView {
        let title = self.of_run(run);
        let mut record = reading.record(run);
        if !title.as_ref().is_some_and(|t| t.kind == "show") {
            record.episode = None;
        }
        RunView { run: record, title }
    }
}

fn worker_counts(c: WorkerCounts) -> WorkerCountsView {
    WorkerCountsView {
        transcode_gpu: c.transcode_gpu,
        transcode_cpu: c.transcode_cpu,
        health_gpu: c.health_gpu,
        health_cpu: c.health_cpu,
    }
}

fn totals_view(t: WorkerTotals) -> WorkerTotalsView {
    WorkerTotalsView {
        files: t.files,
        transcodes: t.transcodes,
        health_checks: t.health_checks,
        saved_bytes: t.saved_bytes,
        tables: WorkerTablesView {
            queue: t.tables.queue,
            done: t.tables.done,
            failed: t.tables.failed,
            health_queue: t.tables.health_queue,
            healthy: t.tables.healthy,
            health_failed: t.tables.health_failed,
        },
    }
}

/// A worker's library with the flow it runs, named from the worker's own list of flows.
fn library_view(l: &WorkerLibrary, setup: &WorkerSetup) -> WorkerLibraryView {
    WorkerLibraryView {
        id: l.id.clone(),
        name: l.name.clone(),
        folder: l.folder.clone(),
        flow: l.flow_id.as_deref().map(|id| WorkerFlowRef {
            id: id.to_string(),
            name: setup
                .flow(id)
                .map(|f| f.name.clone())
                .unwrap_or_else(|| id.to_string()),
        }),
        priority: l.priority,
        held: l.held,
        watching: l.watching,
        transcodes: l.transcodes,
        saved_bytes: l.saved_bytes,
    }
}

/// A worker's queue, each file's library named from the worker's own list when it was read.
pub(super) fn queue_view(q: &WorkerQueue, setup: Option<&WorkerSetup>) -> WorkerQueueView {
    let library = |id: &str| RunLibrary {
        id: id.to_string(),
        name: setup
            .and_then(|s| s.libraries.iter().find(|l| l.id == id))
            .map(|l| l.name.clone())
            .unwrap_or_else(|| id.to_string()),
    };
    WorkerQueueView {
        total: q.total,
        files: q
            .files
            .iter()
            .map(|f: &QueuedFile| QueuedFileView {
                file: file_name(&f.path).to_string(),
                path: f.path.clone(),
                library: f.library.as_deref().map(library),
                size: f.size,
                codec: f.codec.clone(),
                resolution: f.resolution.clone(),
                bumped: f.bumped,
                hold_until: f.hold_until,
                added: f.added,
            })
            .collect(),
        read_at: q.read_at,
    }
}

fn report_view(r: &WorkerReport) -> ReportView {
    ReportView {
        id: r.id.clone(),
        job: r.job.clone(),
        kind: r.kind.clone(),
        at: r.at,
        version: r.version.clone(),
    }
}

fn reports_view(r: &RunReports) -> RunReportsView {
    RunReportsView {
        service: r.service.clone(),
        footprint: r.footprint.clone(),
        reports: r.reports.iter().map(report_view).collect(),
    }
}

fn worker_flow_view(f: &WorkerFlow) -> WorkerFlowView {
    WorkerFlowView {
        id: f.id.clone(),
        name: f.name.clone(),
        about: f.about.clone(),
        steps: f.steps.clone(),
    }
}

/// A node as every answer shows it.
pub(super) fn node_view(n: &ProcessorNode) -> NodeView {
    node(n)
}

fn node(n: &ProcessorNode) -> NodeView {
    NodeView {
        id: n.id.clone(),
        name: n.name.clone(),
        paused: n.paused,
        busy: n.busy,
        limits: worker_counts(n.limits),
        queue: worker_counts(n.queue),
    }
}

/// Every processing integration of the configuration with how it is: as it answers now when it
/// was asked, else as its last pass left it. The bridges first, since the runs are theirs, then
/// the workers; each in the configuration's order.
/// Every processing integration with how it is, for the worker's own page.
pub(super) async fn service_views(state: &AppState, live: &Live) -> AppResult<Vec<ServiceView>> {
    services(state, live).await
}

/// What the services say now, kept a few seconds: for the worker's own page.
pub(super) async fn live_now(state: &AppState) -> Arc<Live> {
    live(state).await
}

/// The titles of these catalog ids as a run shows them.
pub(super) async fn run_titles(state: &AppState, ids: &[i64]) -> AppResult<HashMap<i64, RunTitle>> {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    Ok(catalog_store::title_cards(&state.pool, &ids)
        .await?
        .into_iter()
        .map(|c| {
            let title = RunTitle {
                id: labels::title_id(&c.kind, c.tmdb_id),
                kind: c.kind,
                title: c.title,
                year: c.year,
                poster: labels::poster(c.poster_url.as_deref()),
            };
            (c.id, title)
        })
        .collect())
}

async fn services(state: &AppState, live: &Live) -> AppResult<Vec<ServiceView>> {
    let rows = engine_store::list(&state.pool).await?;
    let mut out: Vec<ServiceView> = state
        .config
        .integrations
        .iter()
        .filter(|i| i.kind.processes())
        .map(|i| {
            let row = rows.iter().find(|r| r.engine == i.id);
            let stored = integration_view(i, row, &state.engines.access.of(&i.id));
            let asked = live.services.iter().find(|s| s.id == i.id);
            let mut view = ServiceView {
                id: i.id.clone(),
                kind: i.kind.as_str().to_string(),
                name: i.name.clone(),
                role: processing_role(i.kind).unwrap_or("worker").to_string(),
                state: match stored.state.as_str() {
                    "down" => "bad".to_string(),
                    other => other.to_string(),
                },
                detail: stored.detail,
                version: stored.version,
                last_sync: row.and_then(|r| r.last_ok_at),
                facts: None,
                nodes: None,
                totals: None,
                libraries: None,
                flows: None,
                queue: None,
            };
            // A worker's own records, as its pass last read them: never asked for a page.
            if let Some(processor) = state.engines.processor(&i.id) {
                view.totals = processor.totals().map(totals_view);
                let setup = processor.setup();
                view.queue = processor.queue().map(|q| queue_view(&q, setup.as_deref()));
                if let Some(setup) = setup {
                    view.libraries = Some(
                        setup
                            .libraries
                            .iter()
                            .map(|l| library_view(l, &setup))
                            .collect(),
                    );
                    view.flows = Some(setup.flows.iter().map(worker_flow_view).collect());
                }
            }
            match asked {
                Some(LiveService {
                    status: Ok(status), ..
                }) => {
                    view.state = "ok".to_string();
                    view.detail = status.detail.clone();
                    view.version = status.version.clone().or(view.version);
                    view.facts = status.facts.clone();
                    view.nodes = status
                        .nodes
                        .as_ref()
                        .map(|nodes| nodes.iter().map(node).collect());
                }
                Some(LiveService {
                    status: Err(why), ..
                }) => {
                    view.state = "bad".to_string();
                    view.detail = why.clone();
                }
                None => {}
            }
            view
        })
        .collect();
    out.sort_by_key(|s| s.role != "bridge");
    Ok(out)
}

fn flow_view(f: FlowSummary) -> FlowView {
    let stat = f.stat.as_ref();
    FlowView {
        runs: stat.map(|s| s.runs).unwrap_or(0),
        imported: stat.map(|s| s.imported).unwrap_or(0),
        last_run: stat.and_then(|s| s.last_run),
        saved_percent: stat.and_then(|s| s.saved_percent()),
        median_seconds: stat
            .and_then(|s| s.median_seconds)
            .map(|m| m.round() as i64),
        id: f.flow.id,
        label: f.flow.label,
        about: f.flow.about,
        service: f.service,
        tag: f.flow.tag,
        modes: f.flow.modes,
        needs: f.flow.needs.map(|n| NeedsView {
            min_resolution: n.min_resolution,
            not_codec: n.not_codec,
        }),
    }
}

async fn page(state: &AppState) -> AppResult<ProcessingPage> {
    let pool = &state.pool;
    let live = live(state).await;
    let reading = reading(state, &live);
    let open = store::open(pool).await?;
    let recent = store::recent_closed(pool, RECENT).await?;
    let stored = store::counts(pool).await?;
    let (integrations, modes, flows) = processing::flow_groups(&state.engines);
    let stats = store::flow_stats(pool, &integrations, &modes, &flows).await?;
    let both: Vec<&Run> = open.iter().chain(recent.iter()).collect();
    let titles = Titles::of(state, &both).await?;
    let working = open
        .iter()
        .filter(|r| reading.worker_of(r).is_some())
        .count() as i64;
    let verifying = open.iter().filter(|r| r.state == VERIFYING).count() as i64;
    let stuck = open.iter().filter(|r| reading.is_stuck(r)).count() as i64;
    Ok(ProcessingPage {
        services: services(state, &live).await?,
        workers: live
            .workers
            .iter()
            .map(|w| WorkerView {
                worker: run_worker(w),
                run: run_of(w, &open, &live.files).map(|r| r.id.clone()),
            })
            .collect(),
        counts: CountsView {
            open: open.len() as i64,
            working,
            waiting: open.len() as i64 - working - verifying,
            verifying,
            stuck,
            imported: stored.imported,
            failed: stored.failed,
            refused: stored.refused,
            pruned: stored.pruned,
            superseded: stored.superseded,
            total: stored.total,
            saved_bytes: stored.saved_bytes,
            since: stored.since,
        },
        flows: flow_summaries(&state.engines, stats)
            .into_iter()
            .map(flow_view)
            .collect(),
        open: open.iter().map(|r| titles.view(&reading, r)).collect(),
        recent: recent.iter().map(|r| titles.view(&reading, r)).collect(),
    })
}

/// A value of the query that says something: trimmed and not empty.
fn word(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A whole number of the query, or `default` when it is left out or empty.
fn number(v: &Option<String>, name: &str, default: i64) -> AppResult<i64> {
    match word(v) {
        None => Ok(default),
        Some(text) => text.parse::<i64>().map_err(|_| {
            AppError::bad_request(format!("`{name}` is a whole number, not `{text}`."))
        }),
    }
}

/// The catalog titles a title id names: `movie-603`, `show-1396`.
async fn title_ids(state: &AppState, id: &str) -> AppResult<Vec<i64>> {
    let named = id.split_once('-').and_then(|(kind, tmdb)| {
        let tmdb = tmdb.parse::<i64>().ok().filter(|n| *n > 0)?;
        matches!(kind, "movie" | "show").then_some((kind, tmdb))
    });
    let Some((kind, tmdb)) = named else {
        return Err(AppError::bad_request(format!(
            "`title` is a title id such as movie-603 or show-1396, not `{id}`."
        )));
    };
    Ok(catalog_store::title_links(&state.pool, &[tmdb], &[])
        .await?
        .into_iter()
        .filter(|l| l.kind == kind && l.tmdb_id == Some(tmdb))
        .map(|l| l.id)
        .collect())
}

async fn list(state: &AppState, q: &RunsQuery) -> AppResult<RunList> {
    let limit = number(&q.limit, "limit", LIMIT_DEFAULT)?.clamp(1, LIMIT_MAX);
    let offset = number(&q.offset, "offset", 0)?.max(0);
    let service = word(&q.service);
    let state_filter = match word(&q.state) {
        None => None,
        Some(w) => Some(StateFilter::parse(&w).ok_or_else(|| {
            AppError::bad_request(format!(
                "`state` is staged, working, verifying, imported, failed, refused, pruned, superseded, open, closed or stuck, not `{w}`."
            ))
        })?),
    };
    let filter = RunFilter {
        flows: word(&q.flow).map(|f| flow_modes(&state.engines, service.as_deref(), &f)),
        integration: service,
        titles: match word(&q.title) {
            None => None,
            Some(id) => Some(title_ids(state, &id).await?),
        },
        words: q
            .q
            .as_deref()
            .unwrap_or_default()
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        state: state_filter,
    };
    // A filter by what the workers are on needs to know it before the table is asked.
    let by_workers = matches!(
        filter.state,
        Some(StateFilter::Working | StateFilter::Waiting | StateFilter::Stuck)
    );
    let now = Utc::now();
    let before = if by_workers {
        Some(live(state).await)
    } else {
        None
    };
    let moment = Moment {
        busy_paths: before.as_ref().map(|l| l.busy_paths()).unwrap_or_default(),
        stuck_before: now - stuck_after(&state.settings),
    };
    let (rows, total) = store::list(&state.pool, &filter, &moment, limit, offset).await?;
    let live = match before {
        Some(live) => live,
        None => live_for(state, &rows).await,
    };
    let reading = reading(state, &live);
    let titles = Titles::of(state, &rows.iter().collect::<Vec<_>>()).await?;
    Ok(RunList {
        runs: rows.iter().map(|r| titles.view(&reading, r)).collect(),
        total,
        limit,
        offset,
    })
}

async fn one(state: &AppState, id: &str) -> AppResult<RunDetail> {
    let missing = || AppError::not_found(format!("There is no run `{id}`."));
    let run = store::one(&state.pool, id).await?.ok_or_else(missing)?;
    let raw = store::raw(&state.pool, id).await?.ok_or_else(missing)?;
    let live = live_for(state, std::slice::from_ref(&run)).await;
    let reading = reading(state, &live);
    let mut view = Titles::of(state, &[&run]).await?.view(&reading, &run);
    // The one place a run is read by its id: what its flow is for, and its steps, and the
    // reports the worker keeps of its file. A worker that does not say costs the answer its
    // reports, not the answer.
    view.run.in_worker = reading.in_worker(&run, true);
    let reports = if state.config.sync.enabled {
        match processing::run_reports(&reading, &run).await {
            Ok(found) => found.as_ref().map(reports_view),
            Err(e) => {
                tracing::warn!(run = %id, error = %e, "the worker's reports of a run could not be read");
                None
            }
        }
    } else {
        None
    };
    Ok(RunDetail {
        run: view.run,
        title: view.title,
        raw,
        reports,
    })
}

/// One report of a run's file, with its text: asked of the worker now, and only for a report
/// it lists for that file.
async fn report_text(state: &AppState, id: &str, report: &str) -> AppResult<ReportTextView> {
    let run = store::one(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no run `{id}`.")))?;
    let missing = || AppError::not_found(format!("There is no report `{report}` of this run."));
    if !state.config.sync.enabled {
        return Err(missing());
    }
    let live = live_for(state, std::slice::from_ref(&run)).await;
    let reading = reading(state, &live);
    let unreachable =
        |e: EngineError| AppError::Upstream(format!("Could not reach the worker: {e}"));
    let found = processing::run_reports(&reading, &run)
        .await
        .map_err(unreachable)?
        .ok_or_else(missing)?;
    let listed = found
        .reports
        .iter()
        .find(|r| r.id == report)
        .ok_or_else(missing)?;
    let worker = state
        .engines
        .processor(&found.service)
        .cloned()
        .ok_or_else(missing)?;
    let text = worker
        .report(&found.footprint, listed)
        .await
        .map_err(unreachable)?
        .ok_or_else(missing)?;
    Ok(ReportTextView {
        report: report_view(listed),
        text: text.text,
        running: text.running,
        exists: text.exists,
    })
}

// Sending a file to a flow --------------------------------------------------------------------------

/// What a hand-off is told while the integrations are not read.
pub(super) const SYNC_OFF: &str = "Integration sync is off";
/// Why a file cannot be sent while the access is `read`, in the options.
const READ_ONLY: &str = "Read-only";

/// Which file `GET /api/v1/processing/options` is asked about.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OptionsQuery {
    /// The title's id: `movie-603`, `show-1396`.
    pub title: Option<String>,
    /// The name of one of the title's files, as the title's record lists it. Never a path:
    /// the server finds where the file lies.
    pub file: Option<String>,
    /// The integration id of the service to send through. Left out: the installation's
    /// bridge.
    pub service: Option<String>,
}

/// The file a hand-off is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OptionFile {
    pub name: String,
    /// Where it lies, as its manager names it.
    pub path: String,
    pub size: i64,
}

/// The service a file would be sent through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct OptionService {
    /// The integration id.
    pub id: String,
    pub name: String,
    /// What Singularr may do with it: `read` or `act`.
    pub access: String,
    /// It takes a file on demand and says beforehand what it would do with one. False for a
    /// service without that, for one that does not answer, and while the integrations are
    /// not read.
    pub offers: bool,
}

/// Where a flow's file would be worked on: the worker library whose folder holds the flow's
/// queue, and the flow that library runs, with its steps.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct OptionWorker {
    /// The integration id of the worker service.
    pub service: String,
    pub library: Option<RunLibrary>,
    pub flow: Option<RunFlow>,
}

/// One flow a file could be sent to.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlowOption {
    /// The flow's id: what `POST /api/v1/processing/runs` takes as `flow`.
    pub flow: String,
    pub label: String,
    /// One line about what it changes.
    pub about: Option<String>,
    /// Whether the flow makes sense for this file, from what the flow needs and what the
    /// file is. Nothing is enforced with it.
    pub fits: bool,
    /// Why it does not, in a few words.
    pub unfit: Option<String>,
    /// What the service would run for this file under the flow, by its own id of it. Null
    /// when the service does not say.
    pub mode: Option<String>,
    /// Whether the service would take the file for this flow. Null when it does not say.
    pub would_send: Option<bool>,
    /// Why it would not, in the service's word: `already_staged`, `outside_roots`, `no_mode`,
    /// `not_found`.
    pub reason: Option<String>,
    /// The open run that has the file. Null also for a file that lies in a queue with no run
    /// that is open: there is none to show then.
    pub open_run: Option<String>,
    /// The queue folder the file would wait in.
    pub queue: Option<String>,
    /// Null when no worker library holds that queue folder.
    pub worker: Option<OptionWorker>,
    /// The flow's runs so far, in numbers.
    pub runs: i64,
    pub saved_percent: Option<f64>,
    pub median_seconds: Option<i64>,
    pub last_run: Option<DateTime<Utc>>,
}

/// `GET /api/v1/processing/options`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SendOptions {
    /// The title's id: `movie-603`.
    pub title: String,
    pub file: OptionFile,
    pub service: OptionService,
    /// Whether a file of this title can be sent now: the title is not protected in its
    /// manager, the service's `send` has write access, and the service takes files on demand. What
    /// the service would do with this file for each flow is the flow's own `wouldSend`.
    pub can_send: bool,
    /// What is missing when it cannot, in one sentence.
    pub why: Option<String>,
    /// The flows that fit the file and have runs first (most runs first), then the ones that
    /// fit, then the ones that do not.
    pub options: Vec<FlowOption>,
}

/// `POST /api/v1/processing/runs`: one file of a title, and the flow to send it to.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    /// The title's id: `movie-603`.
    pub title: String,
    /// The name of one of the title's files. Never a path.
    pub file: String,
    /// The flow's id, as the options list it.
    pub flow: String,
    /// The integration id of the service to send through. Left out: the installation's
    /// bridge.
    #[serde(default)]
    pub service: Option<String>,
}

/// A file that was sent.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SendAnswer {
    pub sent: bool,
    /// The integration id of the service that took it.
    pub service: String,
    pub flow: String,
    /// What the service runs for it, by its own id of it.
    pub mode: Option<String>,
    /// The run, stored before this answer. Null when the service named no job, or could not
    /// be asked for it: the next pass reads it then.
    pub run: Option<RunView>,
}

/// A problem document with a code of its own.
pub(super) fn problem(
    status: StatusCode,
    code: &str,
    title: &str,
    detail: impl Into<String>,
) -> Response {
    let mut p = crate::core::http::Problem::new(status, code, title);
    p.detail = Some(detail.into());
    p.into_response()
}

/// Singularr holds the line itself: the access is `read`, or the title is protected.
pub(super) fn not_allowed(why: impl Into<String>) -> Response {
    problem(
        StatusCode::FORBIDDEN,
        "singularr.not_allowed",
        "Not allowed",
        why,
    )
}

fn not_offered_words(name: &str) -> String {
    format!("Sending not supported by {name}")
}

fn protected_words(tag: &str) -> String {
    format!("Protected by tag {tag}")
}

/// What a send with no clear answer is answered with.
fn unconfirmed_words(name: &str) -> String {
    format!("{name} gave no clear answer and may have taken the file.")
}

/// The tag that keeps this file's title away from a service, when the title carries it in
/// the manager whose file this is. The word is the service's adapter's.
fn protected_by(processor: &dyn Processor, file: &TitleFile) -> Option<&'static str> {
    let tag = processor.protecting_tag()?;
    file.tags
        .iter()
        .any(|t| t.trim().eq_ignore_ascii_case(tag))
        .then_some(tag)
}

/// The service a file is sent through, with its name: the one asked for, else the first
/// bridge of the configuration. A service that offers no flow (a worker) is never one.
fn sender(state: &AppState, asked: Option<&str>) -> AppResult<(Arc<dyn Processor>, String)> {
    let processor = match asked {
        Some(id) => state.engines.processor(id).cloned().ok_or_else(|| {
            AppError::not_found(format!("There is no processing service `{id}`."))
        })?,
        None => state
            .engines
            .processors
            .iter()
            .find(|p| p.role() == "bridge")
            .cloned()
            .ok_or_else(|| {
                AppError::not_found("No processing service that takes a file is configured.")
            })?,
    };
    let name = state
        .config
        .integration(processor.id())
        .map(|i| i.name.clone())
        .unwrap_or_else(|| processor.id().to_string());
    if processor.flows().is_empty() {
        return Err(AppError::not_found(format!("{name} has no flows.")));
    }
    Ok((processor, name))
}

/// One file of a title, by the title's id and the file's name: the title, the file as the
/// catalog holds it, and where it lies. The path never comes from the caller.
async fn title_file(
    state: &AppState,
    title: &str,
    file: &str,
) -> AppResult<(TitleRef, TitleFile, String)> {
    let unknown = || AppError::not_found(format!("There is no title `{title}` in the library."));
    let id = title_ids(state, title)
        .await?
        .first()
        .copied()
        .ok_or_else(unknown)?;
    let title_ref = catalog_store::title_refs(&state.pool, &[id])
        .await?
        .into_iter()
        .next()
        .ok_or_else(unknown)?;
    let found = catalog_store::title_files_named(&state.pool, id, file)
        .await?
        .into_iter()
        .find_map(|f| f.full_path().map(|path| (f, path)));
    let Some((held, path)) = found else {
        return Err(AppError::not_found(format!(
            "`{file}` is not a file of {}.",
            title_ref.title
        )));
    };
    Ok((title_ref, held, path))
}

/// What a file is, for whether a flow fits it: its lines and its video codec, in the words
/// the title's record says them with.
async fn file_facts(
    state: &AppState,
    file: &TitleFile,
) -> AppResult<(Option<i32>, Option<String>)> {
    let Some(copy) = file.copy_id else {
        // A file no manager tracks: what its name says, and what the folder scan read.
        let named = singularr_core::release::parse(file_name(&file.path));
        return Ok((
            file.resolution
                .as_deref()
                .and_then(metadata::lines)
                .or_else(|| named.resolution.and_then(|r| metadata::lines(r.label()))),
            named.video_codec.map(|c| labels::codec(c).to_string()),
        ));
    };
    Ok(match catalog_store::copy(&state.pool, copy).await? {
        Some(row) => {
            let view = row.view();
            (metadata::copy_lines(&view), metadata::copy_codec(&view))
        }
        None => (None, None),
    })
}

/// The run of a job a service names, when the table holds it and it is open. The job that
/// has a file can also be one that is over while its file still lies in a queue: there is no
/// run to open then.
async fn open_run_of(
    state: &AppState,
    service: &str,
    job: Option<&str>,
) -> AppResult<Option<String>> {
    let Some(job) = job else {
        return Ok(None);
    };
    Ok(store::one(&state.pool, &run_id(service, job))
        .await?
        .filter(Run::is_open)
        .map(|r| r.id))
}

/// The worker library whose folder holds a path, with the flow it runs and that flow's
/// steps. `None` when no library holds it.
fn option_worker(state: &AppState, path: &str) -> Option<OptionWorker> {
    let placed = processing::placed(&state.engines, path)?;
    let library = placed.library?;
    Some(OptionWorker {
        service: placed.service,
        library: Some(RunLibrary {
            id: library.id,
            name: library.name,
        }),
        flow: placed.flow.map(|f| RunFlow {
            id: f.id,
            name: f.name,
            about: f.about,
            steps: Some(f.steps),
        }),
    })
}

async fn options(state: &AppState, q: &OptionsQuery) -> AppResult<SendOptions> {
    let title = word(&q.title).ok_or_else(|| {
        AppError::bad_request("`title` is missing: a title id such as movie-603 or show-1396.")
    })?;
    let file = word(&q.file).ok_or_else(|| {
        AppError::bad_request("`file` is missing: the name of one of the title's files.")
    })?;
    let (processor, name) = sender(state, word(&q.service).as_deref())?;
    let (title_ref, held, path) = title_file(state, &title, &file).await?;
    let service = processor.id().to_string();
    let access = state.engines.access.of(&service).of("send");
    let (lines, codec) = file_facts(state, &held).await?;

    // What the service would do with the file, flow by flow: asked now, with a GET that
    // changes nothing, and only while the integrations are read.
    let mut down: Option<String> = None;
    let said = if state.config.sync.enabled {
        match processor.options(&path).await {
            Ok(said) => said,
            Err(e) => {
                tracing::warn!(service = %service, error = %e, "the service did not say what it would do with a file");
                down = Some(e.to_string());
                None
            }
        }
    } else {
        None
    };
    // The open run that has the file. The service names the job that has it; without its
    // word, the table's own open run of that source.
    let mut by_job: HashMap<String, Option<String>> = HashMap::new();
    let mut of_source = None;
    match &said {
        Some(said) => {
            // A file that lies in a queue names one job for every flow.
            let mut jobs: Vec<&str> = said.flows.iter().filter_map(|f| f.job.as_deref()).collect();
            jobs.sort_unstable();
            jobs.dedup();
            for job in jobs {
                let open = open_run_of(state, &service, Some(job)).await?;
                by_job.insert(job.to_string(), open);
            }
        }
        None => of_source = store::open_of_source(&state.pool, &service, &path).await?,
    }
    let (integrations, modes, flows) = processing::flow_groups(&state.engines);
    let stats = store::flow_stats(&state.pool, &integrations, &modes, &flows).await?;

    let mut options: Vec<FlowOption> = processor
        .flows()
        .into_iter()
        .map(|flow| {
            let theirs = said
                .as_ref()
                .and_then(|s| s.flows.iter().find(|o| o.flow == flow.id));
            let stat = stats
                .iter()
                .find(|s| s.integration == service && s.flow == flow.id);
            let misfit = unfit(flow.needs.as_ref(), lines, codec.as_deref());
            // Where the file would wait: the path it would get, else its queue folder.
            let waits_at = theirs.and_then(|o| {
                o.queue_path.clone().or_else(|| {
                    o.queue
                        .as_deref()
                        .map(|q| format!("{}/", q.trim_end_matches(['/', '\\'])))
                })
            });
            FlowOption {
                fits: misfit.is_none(),
                unfit: misfit,
                mode: theirs.and_then(|o| o.mode.clone()),
                would_send: theirs.map(|o| o.would_stage),
                reason: theirs.and_then(|o| o.reason.clone()),
                open_run: match theirs {
                    Some(o) => o
                        .job
                        .as_deref()
                        .and_then(|job| by_job.get(job).cloned().flatten()),
                    None => of_source.clone(),
                },
                queue: theirs.and_then(|o| o.queue.clone()),
                worker: waits_at.and_then(|at| option_worker(state, &at)),
                runs: stat.map(|s| s.runs).unwrap_or(0),
                saved_percent: stat.and_then(|s| s.saved_percent()),
                median_seconds: stat
                    .and_then(|s| s.median_seconds)
                    .map(|m| m.round() as i64),
                last_run: stat.and_then(|s| s.last_run),
                flow: flow.id,
                label: flow.label,
                about: flow.about,
            }
        })
        .collect();
    // What fits first, with the flows that have runs ahead and the most used at the top.
    options.sort_by_key(|o| (!o.fits, o.runs == 0, std::cmp::Reverse(o.runs)));

    let offers = said.is_some();
    let why = if let Some(tag) = protected_by(processor.as_ref(), &held) {
        Some(protected_words(tag))
    } else if access != Access::Act {
        Some(READ_ONLY.to_string())
    } else if !state.config.sync.enabled {
        Some(SYNC_OFF.to_string())
    } else if let Some(why) = down {
        Some(format!("No answer from {name}: {why}"))
    } else if !offers {
        Some(not_offered_words(&name))
    } else {
        None
    };
    Ok(SendOptions {
        title: labels::title_id(&title_ref.kind, title_ref.tmdb_id).unwrap_or(title),
        file: OptionFile {
            name: file_name(&path).to_string(),
            size: held.size_bytes,
            path,
        },
        service: OptionService {
            id: service,
            name,
            access: access.as_str().to_string(),
            offers,
        },
        can_send: why.is_none(),
        why,
        options,
    })
}

/// A send the service did not take, as a problem.
async fn not_taken(
    state: &AppState,
    service: &str,
    name: &str,
    outcome: SendOutcome,
) -> AppResult<Response> {
    let (why, reason, detail, job) = match outcome {
        SendOutcome::Taken(_) => {
            return Err(AppError::Internal(
                "a file that was taken is no refusal".to_string(),
            ));
        }
        SendOutcome::NotOffered => {
            return Ok(problem(
                StatusCode::CONFLICT,
                "singularr.not_offered",
                "Not offered",
                format!("{}.", not_offered_words(name)),
            ));
        }
        // The service may have taken the file: the words invite no second send.
        SendOutcome::Unanswered => {
            return Ok(problem(
                StatusCode::GATEWAY_TIMEOUT,
                "singularr.unconfirmed",
                "Unconfirmed",
                unconfirmed_words(name),
            ));
        }
        SendOutcome::Refused {
            why,
            reason,
            detail,
            job,
        } => (why, reason, detail, job),
    };
    // The service's own words: what it says, else its reason.
    let words = detail.or(reason);
    Ok(match why {
        Refusal::NotFound => AppError::not_found(format!(
            "{name} cannot find the file at its manager's path."
        ))
        .into_response(),
        Refusal::AlreadyStaged => {
            let detail = match open_run_of(state, service, job.as_deref()).await? {
                Some(run) => format!("{name} already has this file in run {run}."),
                None => format!("{name} already has this file in a queue."),
            };
            problem(
                StatusCode::CONFLICT,
                "singularr.already_staged",
                "Already queued",
                detail,
            )
        }
        Refusal::Refused => problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "singularr.refused",
            "File refused",
            match words {
                Some(words) => format!("{name} refused the file: {words}"),
                None => format!("{name} refused the file."),
            },
        ),
        Refusal::Unauthorized => {
            AppError::Upstream(format!("{name} did not accept the API key.")).into_response()
        }
        Refusal::Failed => AppError::Upstream(match words {
            Some(words) => format!("{name} failed to take the file: {words}"),
            None => format!("{name} failed to take the file."),
        })
        .into_response(),
    })
}

async fn send(state: &AppState, caller: &Caller, body: &SendRequest) -> AppResult<Response> {
    if !matches!(caller, Caller::Owner { .. }) {
        return Err(AppError::Forbidden(
            "Only the owner can send files.".to_string(),
        ));
    }
    let asked = body
        .service
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let (processor, name) = sender(state, asked)?;
    let (title_ref, held, path) = title_file(state, body.title.trim(), body.file.trim()).await?;
    let wanted = body.flow.trim();
    let offered = processor.flows();
    let Some(flow) = offered.iter().find(|f| f.id == wanted) else {
        let ids: Vec<&str> = offered.iter().map(|f| f.id.as_str()).collect();
        return Err(AppError::bad_request(format!(
            "`flow` is one of {}, not `{wanted}`.",
            ids.join(", ")
        )));
    };
    // The lines Singularr holds itself, whatever the service would do with the file: a title
    // its owner protects is never sent, and nothing is sent while the access is `read`.
    if let Some(tag) = protected_by(processor.as_ref(), &held) {
        return Ok(not_allowed(format!("{}.", protected_words(tag))));
    }
    let service = processor.id().to_string();
    let access = state.engines.access.of(&service);
    if !access.allows("send") {
        return Ok(not_allowed(read_only_words(access.label("send"), &name)));
    }
    if !state.config.sync.enabled {
        return Ok(problem(
            StatusCode::CONFLICT,
            "singularr.conflict",
            "Not sent",
            format!("{SYNC_OFF}."),
        ));
    }
    let actor = caller.actor();
    let what = Sending {
        actor: &actor,
        title: &title_ref,
        path: &path,
        flow: &flow.id,
    };
    let handed = match processing::send(&state.pool, &state.journal, &processor, what).await {
        Ok(SendOutcome::Taken(handed)) => *handed,
        Ok(other) => return not_taken(state, &service, &name, other).await,
        // The way out refused the act: the access was taken back while this was on its way.
        Err(EngineError::Outbound(OutboundError::NotAllowed { .. })) => {
            return Ok(not_allowed(read_only_words(access.label("send"), &name)));
        }
        // An error is a send that surely did not take place (`processing::send`).
        Err(e) => {
            return Err(AppError::Upstream(format!(
                "{name} did not take the file: {e}"
            )));
        }
    };
    // A claim of a processing rule on this file is settled: the service took the file.
    if let Err(e) = store::settle_claim_of_path(
        &state.pool,
        &path,
        &service,
        handed.run_id(&service).as_deref(),
    )
    .await
    {
        tracing::warn!(service = %service, error = %e, "a claim of the file was not settled");
    }
    // The run as the lists show it. The workers are not asked for this answer: the file was
    // staged a moment ago.
    let run = match &handed.run {
        Some(run) => {
            let quiet = Live::default();
            let reading = reading(state, &quiet);
            Some(Titles::of(state, &[run]).await?.view(&reading, run))
        }
        None => None,
    };
    Ok((
        StatusCode::CREATED,
        Json(SendAnswer {
            sent: true,
            service,
            flow: flow.id.clone(),
            mode: handed.mode,
            run,
        }),
    )
        .into_response())
}

// For the title record ------------------------------------------------------------------------------

/// The processing runs of a title for its record: the newest, with how many are open and how
/// many there are. `None` when the installation has no processing integration; no runs for a
/// title the catalog does not have.
pub(super) async fn title_processing(
    state: &AppState,
    title_id: Option<i64>,
) -> AppResult<Option<TitleProcessing>> {
    if state.config.processors.is_empty() {
        return Ok(None);
    }
    let Some(title_id) = title_id else {
        return Ok(Some(TitleProcessing {
            runs: Vec::new(),
            open: 0,
            total: 0,
        }));
    };
    let runs = store::of_title(&state.pool, title_id, TITLE_RUNS).await?;
    let (total, open) = store::title_counts(&state.pool, title_id).await?;
    let live = live_for(state, &runs).await;
    let reading = reading(state, &live);
    let show = catalog_store::title_refs(&state.pool, &[title_id])
        .await?
        .first()
        .is_some_and(|t| t.kind == "show");
    Ok(Some(TitleProcessing {
        runs: runs
            .iter()
            .map(|r| {
                let mut record = reading.record(r);
                if !show {
                    record.episode = None;
                }
                record
            })
            .collect(),
        open,
        total,
    }))
}

/// What processing did or does to the files of one title, by file name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FileMarks {
    /// File name to the newest imported run whose result it is.
    processed: HashMap<String, String>,
    /// File name to the newest open run whose source it is.
    processing: HashMap<String, String>,
}

impl FileMarks {
    /// The marks of a title's files. Empty without a processing integration.
    pub(super) async fn of(state: &AppState, title_id: i64) -> AppResult<FileMarks> {
        if state.config.processors.is_empty() {
            return Ok(FileMarks::default());
        }
        Ok(FileMarks::from_rows(
            store::marks(&state.pool, title_id)
                .await?
                .into_iter()
                .map(|m| (m.id, m.state, m.source_path, m.result_path)),
        ))
    }

    /// From runs given newest first as (id, state, source path, result path): the first run
    /// that names a file is the one the file is marked with.
    fn from_rows(
        rows: impl IntoIterator<Item = (String, String, Option<String>, Option<String>)>,
    ) -> FileMarks {
        let mut marks = FileMarks::default();
        for (id, state, source, result) in rows {
            let (into, path) = if state == IMPORTED {
                (&mut marks.processed, result)
            } else {
                (&mut marks.processing, source)
            };
            if let Some(path) = path {
                into.entry(file_name(&path).to_string()).or_insert(id);
            }
        }
        marks
    }

    /// The mark of a file: `processing` while an open run has it as its source, else
    /// `processed` when it is the result of an imported run.
    pub(super) fn of_file(&self, name: &str) -> Option<FileProcessing> {
        let mark = |state: &str, run: &String| FileProcessing {
            state: state.to_string(),
            run: run.clone(),
        };
        self.processing
            .get(name)
            .map(|run| mark("processing", run))
            .or_else(|| self.processed.get(name).map(|run| mark("processed", run)))
    }
}

// For the attention lists ---------------------------------------------------------------------------

/// A run that needs attention, for the bell and Home: one that is stuck, or one that failed or
/// was refused lately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunAlert {
    /// The run's id.
    pub id: String,
    /// The title's name, with the episode for a run of a show; the source's file name for a
    /// run no title holds.
    pub title: String,
    pub detail: String,
    /// `warn` for a stuck run, `bad` for one that failed or was refused.
    pub tone: &'static str,
    /// A stuck run's start; for the others, when they closed.
    pub since: Option<DateTime<Utc>>,
}

/// Text with its first letter in lower case, to follow a comma: `not in worker queue`.
fn lower_first(text: &str) -> String {
    let mut c = text.chars();
    match c.next() {
        Some(first) => first.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

/// One run as the attention lists show it: the title is its subject, the detail a few words
/// (`Stuck 8 d, not in worker queue`, `Failed: import not confirmed`). A run that is not
/// `stuck` is one that failed or was refused. `why` is what the worker's own records say of a
/// stuck run, when they say.
fn alert(
    run: &Run,
    title: Option<&RunTitle>,
    stuck: bool,
    why: Option<&str>,
    now: DateTime<Utc>,
) -> RunAlert {
    let name = match title {
        Some(t) => match (t.kind.as_str(), run.season, run.episode) {
            ("show", Some(season), Some(episode)) => {
                format!("{} S{season:02} E{episode:02}", t.title)
            }
            _ => t.title.clone(),
        },
        None => run
            .source_path
            .as_deref()
            .map(file_name)
            .unwrap_or(&run.job_id)
            .to_string(),
    };
    let (detail, tone, since) = if stuck {
        let waiting = units::span(now - run.created_at);
        (
            format!(
                "Stuck {waiting}, {}",
                why.map(lower_first)
                    .unwrap_or_else(|| "no worker".to_string())
            ),
            "warn",
            Some(run.created_at),
        )
    } else {
        let what = if run.state == "refused" {
            "Refused"
        } else {
            "Failed"
        };
        (
            match run.detail.as_deref() {
                Some(why) => format!("{what}: {why}"),
                None => what.to_string(),
            },
            "bad",
            run.finished_at.or(run.updated_at),
        )
    };
    RunAlert {
        id: run.id.clone(),
        title: name,
        detail,
        tone,
        since,
    }
}

/// The runs that need attention now: every stuck run, and the runs that failed or were
/// refused in the last [`ALERT_DAYS`] days. None without a processing integration.
pub(super) async fn alerts(state: &AppState) -> AppResult<Vec<RunAlert>> {
    if state.config.processors.is_empty() {
        return Ok(Vec::new());
    }
    let now = Utc::now();
    let after = stuck_after(&state.settings);
    let old: Vec<Run> = store::open(&state.pool)
        .await?
        .into_iter()
        .filter(|r| now - r.created_at > after)
        .collect();
    // What the workers are on is asked only when a run is old enough to be stuck, and waited
    // for only so long: a service that is slow to answer does not hold a page. Without an
    // answer no worker is known to be on a run.
    let live = tokio::time::timeout(ALERT_WAIT, live_for(state, &old))
        .await
        .unwrap_or_default();
    let reading = Reading {
        engines: &state.engines,
        workers: &live.workers,
        files: &live.files,
        asked_at: live.at,
        now,
        stuck_after: after,
    };
    let closed = store::closed_since(
        &state.pool,
        &["failed", "refused"],
        now - chrono::Duration::days(ALERT_DAYS),
        ALERT_MAX,
    )
    .await?;
    let listed: Vec<(&Run, bool)> = old
        .iter()
        .filter(|r| reading.is_stuck(r))
        .map(|r| (r, true))
        .chain(closed.iter().map(|r| (r, false)))
        .collect();
    let runs: Vec<&Run> = listed.iter().map(|(r, _)| *r).collect();
    let titles = Titles::of(state, &runs).await?;
    Ok(listed
        .into_iter()
        .map(|(run, stuck)| {
            let why = if stuck { reading.stuck_why(run) } else { None };
            alert(run, titles.of_run(run).as_ref(), stuck, why.as_deref(), now)
        })
        .collect())
}

// Handlers ------------------------------------------------------------------------------------------

/// Processing on one page: each service and how it is, what the workers are on now, the runs
/// in numbers, the flows with what they saved and how long they take, every open run and the
/// runs that closed last. A service that does not answer is `bad` with why; the runs are
/// answered from what is stored either way.
#[utoipa::path(get, path = "/api/v1/processing", tag = "processing",
    responses((status = 200, body = ProcessingPage), (status = 401, body = ProblemDoc)))]
async fn processing_page(State(state): State<AppState>) -> ApiResult<ProcessingPage> {
    Ok(Json(page(&state).await?))
}

/// The runs, newest first, filtered and paged. `total` counts every run that matches.
#[utoipa::path(get, path = "/api/v1/processing/runs", tag = "processing", params(RunsQuery),
    responses((status = 200, body = RunList), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn processing_runs(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<RunsQuery>,
) -> ApiResult<RunList> {
    Ok(Json(list(&state, &q).await?))
}

/// One run, with the job exactly as its service gave it (`raw`).
#[utoipa::path(get, path = "/api/v1/processing/runs/{id}", tag = "processing",
    params(("id" = String, Path, description = "The run's id: the integration id, a colon, and the service's id of the job")),
    responses((status = 200, body = RunDetail), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn processing_run(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<RunDetail> {
    Ok(Json(one(&state, &id).await?))
}

/// One report the worker keeps of a job it ran on a run's file, with its text, as the run's
/// `reports` lists it. Asked of the worker now.
#[utoipa::path(get, path = "/api/v1/processing/runs/{id}/reports/{report}", tag = "processing",
    params(
        ("id" = String, Path, description = "The run's id"),
        ("report" = String, Path, description = "The report's id, as the run's `reports` lists it")
    ),
    responses((status = 200, body = ReportTextView), (status = 404, body = ProblemDoc),
        (status = 502, description = "The worker did not answer", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn processing_report(
    State(state): State<AppState>,
    ApiPath((id, report)): ApiPath<(String, String)>,
) -> ApiResult<ReportTextView> {
    Ok(Json(report_text(&state, &id, &report).await?))
}

/// What handing one file of a title to a service would do, flow by flow, before anything is
/// sent: whether each flow fits the file, what the service says it would do with it (asked
/// now, with a GET that changes nothing), the worker library and flow the file would go to,
/// and the flow's runs so far. `canSend` and `why` say whether Singularr would send at all.
#[utoipa::path(get, path = "/api/v1/processing/options", tag = "processing", params(OptionsQuery),
    responses((status = 200, body = SendOptions), (status = 400, body = ProblemDoc), (status = 404, description = "No such title, no such file of it, or no service that takes a file", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn processing_options(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<OptionsQuery>,
) -> ApiResult<SendOptions> {
    Ok(Json(options(&state, &q).await?))
}

/// Send one file of a title to a flow: `{"title": "movie-603", "file": "<its name>", "flow":
/// "hevc"}`. The owner alone may, while the service's `send` has write access. The run is
/// stored before the answer when the service names its job. Each send is a `processing_sent`
/// event.
#[utoipa::path(post, path = "/api/v1/processing/runs", tag = "processing", request_body = SendRequest,
    responses((status = 201, body = SendAnswer), (status = 400, body = ProblemDoc),
        (status = 403, description = "Not the owner; or `singularr.not_allowed`: Singularr may only read the service, or the title is protected in its manager", body = ProblemDoc),
        (status = 404, description = "No such title or file", body = ProblemDoc),
        (status = 409, description = "`singularr.already_staged`: the service has the file already; `singularr.not_offered`: it takes no file on demand", body = ProblemDoc),
        (status = 422, description = "`singularr.refused`: the service refused, in its own words", body = ProblemDoc),
        (status = 502, description = "The service failed", body = ProblemDoc),
        (status = 504, description = "`singularr.unconfirmed`: no clear answer; the service may have taken the file", body = ProblemDoc),
        (status = 401, body = ProblemDoc)))]
async fn processing_send(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(body): ApiJson<SendRequest>,
) -> Result<Response, AppError> {
    send(&state, &caller, &body).await
}

/// The processing routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/processing", get(processing_page))
        .route("/processing/options", get(processing_options))
        .route(
            "/processing/runs",
            get(processing_runs).post(processing_send),
        )
        .route("/processing/runs/{id}", get(processing_run))
        .route(
            "/processing/runs/{id}/reports/{report}",
            get(processing_report),
        )
}

#[derive(OpenApi)]
#[openapi(
    paths(processing_page, processing_runs, processing_run, processing_report, processing_options, processing_send),
    tags((name = "processing", description = "What the processing services did to each file after its import, and what they do now"))
)]
struct ProcessingDoc;

/// The processing routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    ProcessingDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use singularr_core::VideoCodec;

    #[test]
    fn the_openapi_document_lists_the_processing_routes() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        let paths = doc["paths"].as_object().unwrap();
        for path in [
            "/api/v1/processing",
            "/api/v1/processing/options",
            "/api/v1/processing/runs",
            "/api/v1/processing/runs/{id}",
        ] {
            assert!(
                paths.get(path).and_then(|p| p.get("get")).is_some(),
                "GET {path} is missing from the OpenAPI document"
            );
        }
        // The one route that sends something, with every way it can end.
        let send = &paths["/api/v1/processing/runs"]["post"];
        let ends: Vec<&str> = send["responses"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            ends,
            vec![
                "201", "400", "401", "403", "404", "409", "422", "502", "504"
            ]
        );
        let asked: Vec<&str> = doc["paths"]["/api/v1/processing/options"]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(asked, vec!["title", "file", "service"]);
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for name in [
            "ProcessingPage",
            "ServiceView",
            "WorkerView",
            "CountsView",
            "FlowView",
            "RunList",
            "RunView",
            "RunDetail",
            "ProcessingRun",
            "RunTrigger",
            "RunFile",
            "RunEvent",
            "RunInWorker",
            "RunLibrary",
            "RunFlow",
            "WorkerTotalsView",
            "WorkerLibraryView",
            "WorkerFlowView",
            "SendOptions",
            "FlowOption",
            "OptionWorker",
            "SendRequest",
            "SendAnswer",
        ] {
            assert!(schemas.contains_key(name), "schema {name} is missing");
        }
        let filters: Vec<&str> = doc["paths"]["/api/v1/processing/runs"]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            filters,
            vec!["state", "flow", "service", "title", "q", "limit", "offset"]
        );
    }

    #[test]
    fn a_flow_reads_with_its_numbers_and_what_it_needs() {
        use crate::engines::{FlowNeeds, ProcessorFlow};
        use crate::processing::model::FlowStat;
        let flow = |needs: Option<FlowNeeds>, stat: Option<FlowStat>| {
            flow_view(FlowSummary {
                service: "bridge".to_string(),
                flow: ProcessorFlow {
                    id: "convert".to_string(),
                    label: "Convert".to_string(),
                    about: Some("Converts.".to_string()),
                    tag: None,
                    modes: vec!["convert".to_string(), "convert-4k".to_string()],
                    needs,
                },
                stat,
            })
        };
        // A flow no run has used: it is offered, and its numbers are empty.
        let unused = serde_json::to_value(flow(None, None)).unwrap();
        assert_eq!(
            unused,
            serde_json::json!({
                "id": "convert", "label": "Convert", "about": "Converts.", "service": "bridge",
                "tag": null, "modes": ["convert", "convert-4k"], "runs": 0, "imported": 0,
                "lastRun": null, "savedPercent": null, "medianSeconds": null
            })
        );
        let used = serde_json::to_value(flow(
            Some(FlowNeeds {
                min_resolution: None,
                not_codec: vec![labels::codec(VideoCodec::H265).to_string()],
            }),
            Some(FlowStat {
                integration: "bridge".to_string(),
                flow: "convert".to_string(),
                runs: 364,
                imported: 350,
                last_run: Some("2026-11-23T14:55:00Z".parse().unwrap()),
                source_bytes: Some(1000),
                result_bytes: Some(786),
                median_seconds: Some(689.5),
            }),
        ))
        .unwrap();
        assert_eq!(used["needs"], serde_json::json!({"notCodec": ["HEVC"]}));
        assert_eq!(
            (&used["runs"], &used["imported"], &used["savedPercent"]),
            (
                &serde_json::json!(364),
                &serde_json::json!(350),
                &serde_json::json!(21.4)
            )
        );
        assert_eq!(used["medianSeconds"], 690);
        assert_eq!(used["lastRun"], "2026-11-23T14:55:00Z");
        let sized = serde_json::to_value(flow(
            Some(FlowNeeds {
                min_resolution: Some(2160),
                not_codec: Vec::new(),
            }),
            None,
        ))
        .unwrap();
        assert_eq!(sized["needs"], serde_json::json!({"minResolution": 2160}));
        // The codec words a flow's needs are written in are the words files are shown with.
        let shown: Vec<&str> = [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1]
            .into_iter()
            .map(labels::codec)
            .collect();
        for p in crate::engines::hush::flows() {
            for codec in p.needs.map(|n| n.not_codec).unwrap_or_default() {
                assert!(shown.contains(&codec.as_str()), "{codec}");
            }
        }
    }

    #[test]
    fn a_file_is_marked_by_the_newest_run_that_names_it() {
        let row = |id: &str, state: &str, source: &str, result: Option<&str>| {
            (
                id.to_string(),
                state.to_string(),
                Some(source.to_string()),
                result.map(str::to_string),
            )
        };
        let marks = FileMarks::from_rows([
            row("s:3", "staged", "/library/T/new name.mkv", None),
            row(
                "s:2",
                "imported",
                "/library/T/old.mkv",
                Some("/library/T/new name.mkv"),
            ),
            row(
                "s:1",
                "imported",
                "/library/T/older.mkv",
                Some("/library/T/new name.mkv"),
            ),
            row(
                "s:0",
                "imported",
                "/library/T/b.mkv",
                Some("D:\\Library\\T\\b new.mkv"),
            ),
        ]);
        // A file an open run was staged from is being processed, whatever made it.
        assert_eq!(
            marks.of_file("new name.mkv"),
            Some(FileProcessing {
                state: "processing".to_string(),
                run: "s:3".to_string()
            })
        );
        assert_eq!(
            marks.of_file("b new.mkv"),
            Some(FileProcessing {
                state: "processed".to_string(),
                run: "s:0".to_string()
            })
        );
        // A source that was replaced is no file of the library any more.
        assert_eq!(marks.of_file("old.mkv"), None);
        assert_eq!(FileMarks::default().of_file("new name.mkv"), None);
        // Without the open run, the newest import is the one that made the file.
        let marks = FileMarks::from_rows([
            row(
                "s:2",
                "imported",
                "/library/T/old.mkv",
                Some("/library/T/new name.mkv"),
            ),
            row(
                "s:1",
                "imported",
                "/library/T/older.mkv",
                Some("/library/T/new name.mkv"),
            ),
        ]);
        assert_eq!(marks.of_file("new name.mkv").unwrap().run, "s:2");
    }

    #[test]
    fn a_run_that_needs_attention_says_what_and_since_when() {
        use crate::processing::model::Link;
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/hush_history.json"
        );
        let ledger: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let seen: DateTime<Utc> = "2026-11-23T16:00:00Z".parse().unwrap();
        let episode = Link {
            title_id: None,
            season: Some(1),
            episode: Some(3),
        };
        let runs: Vec<Run> = ledger["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(crate::engines::hush::run)
            .map(|job| Run::listed("hush", &job, episode, seen))
            .collect();
        let title = |kind: &str, name: &str| RunTitle {
            id: None,
            kind: kind.to_string(),
            title: name.to_string(),
            year: None,
            poster: None,
        };
        // A stuck run: how long it has waited, in days from two days on, since its start.
        let staged = &runs[6];
        let movie = title("movie", "Kingdom of Heaven");
        let stuck = alert(staged, Some(&movie), true, None, seen);
        assert_eq!(
            stuck,
            RunAlert {
                id: "hush:20261115T001015Z-cb34e7ec".to_string(),
                title: "Kingdom of Heaven".to_string(),
                detail: "Stuck 8 d, no worker".to_string(),
                tone: "warn",
                since: Some("2026-11-15T00:10:15Z".parse().unwrap()),
            }
        );
        let soon = staged.created_at + chrono::Duration::hours(30);
        assert_eq!(
            alert(staged, Some(&movie), true, None, soon).detail,
            "Stuck 30 h, no worker"
        );
        // What the worker's own records say of a stuck run is said in its place.
        assert_eq!(
            alert(
                staged,
                Some(&movie),
                true,
                Some("Not in worker queue"),
                seen
            )
            .detail,
            "Stuck 8 d, not in worker queue"
        );
        assert_eq!(lower_first("In worker queue"), "in worker queue");
        assert_eq!(lower_first(""), "");
        // A run of a show names its episode; a run no title holds is named by its file.
        let show = title("show", "Shoresy");
        assert_eq!(
            alert(&runs[7], Some(&show), true, None, seen).title,
            "Shoresy S01 E03"
        );
        assert_eq!(
            alert(&runs[7], None, true, None, seen).title,
            "Shoresy (2022) - S01E03 - Know Your Role [HDTV-1080p][EAC3 5.1][h264].mkv"
        );
        // A run that failed or was refused: what the service said, since it closed.
        let failed = alert(&runs[9], None, false, None, seen);
        assert_eq!(
            (failed.detail.as_str(), failed.tone, failed.since),
            (
                "Failed: import not confirmed; the output stays in staging",
                "bad",
                runs[9].finished_at
            )
        );
        assert!(failed.since.is_some());
        let refused = alert(&runs[11], Some(&show), false, None, seen);
        assert!(
            refused.detail.starts_with("Refused: the arr now holds"),
            "{}",
            refused.detail
        );
        let mut silent = runs[9].clone();
        silent.detail = None;
        assert_eq!(alert(&silent, None, false, None, seen).detail, "Failed");
    }

    #[test]
    fn a_hand_off_says_why_in_one_sentence() {
        assert_eq!(
            read_only_words("Send a file to a flow", "HUSH"),
            "Send a file to a flow is read-only on HUSH."
        );
        assert_eq!(protected_words("keep-away"), "Protected by tag keep-away");
        assert_eq!(
            unconfirmed_words("HUSH"),
            "HUSH gave no clear answer and may have taken the file."
        );
        assert_eq!(not_offered_words("HUSH"), "Sending not supported by HUSH");
        // A flow as an option, in the names the client reads.
        let option = FlowOption {
            flow: "convert".to_string(),
            label: "Convert".to_string(),
            about: None,
            fits: false,
            unfit: Some("Already HEVC".to_string()),
            mode: None,
            would_send: None,
            reason: None,
            open_run: None,
            queue: None,
            worker: None,
            runs: 0,
            saved_percent: None,
            median_seconds: None,
            last_run: None,
        };
        let json = serde_json::to_value(&option).unwrap();
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "about",
                "fits",
                "flow",
                "label",
                "lastRun",
                "medianSeconds",
                "mode",
                "openRun",
                "queue",
                "reason",
                "runs",
                "savedPercent",
                "unfit",
                "worker",
                "wouldSend"
            ]
        );
        // What the service does not say is null, not false.
        assert!(json["wouldSend"].is_null() && json["mode"].is_null());
        // The body of a send takes its four names and no other: never a path.
        let body = |text: &str| serde_json::from_str::<SendRequest>(text);
        assert!(body(r#"{"title": "movie-1", "file": "a.mkv", "flow": "convert"}"#).is_ok());
        assert!(
            body(r#"{"title": "movie-1", "file": "a.mkv", "flow": "convert", "service": "s"}"#)
                .is_ok()
        );
        assert!(
            body(r#"{"title": "movie-1", "file": "a.mkv", "flow": "convert", "path": "/a.mkv"}"#)
                .is_err()
        );
        assert!(body(r#"{"title": "movie-1", "flow": "convert"}"#).is_err());
    }

    #[test]
    fn query_values_are_read_leniently() {
        assert_eq!(word(&Some("  open ".to_string())).as_deref(), Some("open"));
        assert_eq!(word(&Some("  ".to_string())), None);
        assert_eq!(word(&None), None);
        assert_eq!(number(&None, "limit", 50).unwrap(), 50);
        assert_eq!(number(&Some(String::new()), "limit", 50).unwrap(), 50);
        assert_eq!(number(&Some("7".to_string()), "limit", 50).unwrap(), 7);
        assert!(matches!(
            number(&Some("many".to_string()), "limit", 50),
            Err(AppError::BadRequest(_))
        ));
    }
}
