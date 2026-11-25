//! The `/api/v1` routes of a worker service (a processing integration whose role is `worker`):
//! the changes the owner makes to it from Singularr.
//!
//! Each change is an act the worker's adapter lists (`Processor::act`): a node's worker limits,
//! pausing and resuming a node, cancelling what a worker is on, putting a file back in the
//! queue, scanning a library, switching a library's folder watching, and the flow a library
//! runs. Every route is the owner's alone, answers 403 `singularr.not_allowed` while Singularr may
//! only read the integration (before anything is asked of the service), and journals what the
//! service took as a `processing_worker_changed` event. `core::outbound` holds the same line on
//! the way out, each act in the one form its adapter declares.

use super::processing::{
    NodeView, SYNC_OFF, ServiceView, WorkerQueueView, not_allowed, problem, read_only_words,
};
use super::routes::{ApiJson, ApiPath};
use super::state::AppState;
use crate::core::auth::Caller;
use crate::core::error::AppError;
use crate::core::outbound::{Access, OutboundError};
use crate::core::schema::ProblemDoc;
use crate::engines::{Acted, EngineError, Processor, WorkerAct, WorkerKind, WorkerSetup};
use crate::processing::model::{file_name, queue_of};
use crate::processing::service::{self as processing, ActsDone};
use crate::processing::store;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use utoipa::{OpenApi, ToSchema};

/// The most workers of one kind a node is set to from here.
pub const LIMIT_MAX: i64 = 32;
/// The role of a worker service.
const WORKER: &str = "worker";

/// `POST .../nodes/{node}/limits`: how many workers of one kind the node runs.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LimitRequest {
    /// `transcodeGpu`, `transcodeCpu`, `healthGpu` or `healthCpu`.
    pub kind: String,
    /// The number of workers of that kind, 0 to 32.
    pub to: i64,
}

/// `POST .../nodes/{node}/pause`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PauseRequest {
    /// True pauses the node; false resumes it.
    pub paused: bool,
}

/// `POST .../requeue`: one file, by its path in the worker or by the run that waits there.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RequeueRequest {
    /// The file's path as the worker knows it: in the folder of one of its libraries.
    #[serde(default)]
    pub path: Option<String>,
    /// A run's id: the file it waits at.
    #[serde(default)]
    pub run: Option<String>,
}

/// `POST .../libraries/{library}/watch`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchRequest {
    /// True watches the library's folder for new files; false stops.
    pub on: bool,
}

/// `POST .../libraries/{library}/flow`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowRequest {
    /// The id of one of the worker's flows.
    pub flow: String,
}

/// A change the worker took.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ActAnswer {
    /// `limits`, `pause`, `cancel`, `requeue`, `scan`, `watch` or `flow`.
    pub act: String,
    /// The integration id of the worker service.
    pub service: String,
    /// The calls the worker took: a limit moves by one with each; 0 when nothing had to
    /// change.
    pub steps: usize,
}

/// Why a change is not asked of the worker: the answer to give instead.
pub struct Stop(Box<Response>);

impl From<Response> for Stop {
    fn from(r: Response) -> Stop {
        Stop(Box::new(r))
    }
}

impl From<AppError> for Stop {
    fn from(e: AppError) -> Stop {
        Stop(Box::new(e.into_response()))
    }
}

/// The worker service an act is for, with its name, once it may be asked: the caller is the
/// owner, the integration is a configured worker, Singularr may act on it, and the integrations
/// are read. Nothing is asked of the service before all of these hold.
fn acting(
    state: &AppState,
    caller: &Caller,
    service: &str,
) -> Result<(Arc<dyn Processor>, String), Stop> {
    if !matches!(caller, Caller::Owner { .. }) {
        return Err(Stop::from(
            AppError::Forbidden("Only the owner can change the worker.".to_string())
                .into_response(),
        ));
    }
    let Some(processor) = state
        .engines
        .processor(service)
        .filter(|p| p.role() == WORKER)
        .cloned()
    else {
        return Err(Stop::from(
            AppError::not_found(format!("There is no worker service `{service}`.")).into_response(),
        ));
    };
    let name = state
        .config
        .integration(service)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| service.to_string());
    if state.engines.access.of(service).access != Access::Act {
        return Err(Stop::from(not_allowed(read_only_words(&name))));
    }
    if !state.config.sync.enabled {
        return Err(Stop::from(problem(
            StatusCode::CONFLICT,
            "singularr.conflict",
            "Not changed",
            format!("{SYNC_OFF}."),
        )));
    }
    Ok((processor, name))
}

fn could_not_reach(name: &str, e: &EngineError) -> Response {
    AppError::Upstream(format!("Could not reach {name}: {e}")).into_response()
}

/// Send the acts and answer with what came of them.
async fn run(
    state: &AppState,
    caller: &Caller,
    processor: &Arc<dyn Processor>,
    name: &str,
    acts: &[WorkerAct],
    about: Value,
) -> Response {
    let Some(first) = acts.first() else {
        return Json(ActAnswer {
            act: "limits".to_string(),
            service: processor.id().to_string(),
            steps: 0,
        })
        .into_response();
    };
    let word = first.word();
    let actor = caller.actor();
    match processing::worker_acts(&state.pool, &state.journal, processor, acts, &actor, about).await
    {
        Ok(ActsDone {
            done,
            last: Acted::Done,
        }) => {
            // A library's settings changed: what the pages show is read again now.
            if matches!(first, WorkerAct::Watch { .. } | WorkerAct::Flow { .. })
                && let Err(e) = processor.read_setup().await
            {
                tracing::warn!(service = %processor.id(), error = %e, "the worker's libraries could not be read again");
            }
            Json(ActAnswer {
                act: word.to_string(),
                service: processor.id().to_string(),
                steps: done,
            })
            .into_response()
        }
        Ok(ActsDone {
            last: Acted::NotOffered,
            ..
        }) => problem(
            StatusCode::CONFLICT,
            "singularr.not_offered",
            "Not offered",
            format!("Not supported by {name}."),
        ),
        Ok(ActsDone {
            last: Acted::Refused(words),
            ..
        }) => problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "singularr.refused",
            "Refused",
            if words.is_empty() {
                format!("{name} refused the change.")
            } else {
                format!("{name} refused the change: {words}")
            },
        ),
        // The access was taken back while this was on its way.
        Err(EngineError::Outbound(OutboundError::NotAllowed { .. })) => {
            not_allowed(read_only_words(name))
        }
        Err(e) => could_not_reach(name, &e),
    }
}

/// The node of a worker by its id, as the worker answers now, with its name.
async fn node_of(
    processor: &Arc<dyn Processor>,
    name: &str,
    node: &str,
) -> Result<crate::engines::ProcessorNode, Stop> {
    let status = processor
        .status()
        .await
        .map_err(|e| could_not_reach(name, &e))?;
    let found = status
        .nodes
        .unwrap_or_default()
        .into_iter()
        .find(|n| n.id == node)
        .ok_or_else(|| AppError::not_found(format!("There is no node `{node}` on {name}.")))?;
    Ok(found)
}

/// The worker's configuration as last read, and one of its libraries.
fn library_of(
    processor: &Arc<dyn Processor>,
    name: &str,
    library: &str,
) -> Result<(Arc<WorkerSetup>, crate::engines::WorkerLibrary), Stop> {
    let missing = || {
        AppError::not_found(format!("There is no library `{library}` on {name}.")).into_response()
    };
    let setup = processor.setup().ok_or_else(missing)?;
    let found = setup
        .libraries
        .iter()
        .find(|l| l.id == library)
        .cloned()
        .ok_or_else(missing)?;
    Ok((setup, found))
}

fn either(result: Result<Response, Stop>) -> Response {
    result.unwrap_or_else(|e| *e.0)
}

// Handlers ------------------------------------------------------------------------------------

/// Set how many workers of one kind a node runs: `{"kind": "transcodeGpu", "to": 3}`. The
/// worker takes a limit one step at a time, so a change of three is three calls. The owner
/// alone, while Singularr may act on the worker.
#[utoipa::path(post, path = "/api/v1/processing/workers/{service}/nodes/{node}/limits", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id"), ("node" = String, Path, description = "The node's id")),
    request_body = LimitRequest,
    responses((status = 200, body = ActAnswer), (status = 400, body = ProblemDoc),
        (status = 403, description = "Not the owner; or `singularr.not_allowed`: the worker is read-only", body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 422, description = "`singularr.refused`: the worker said no", body = ProblemDoc),
        (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_limits(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath((service, node)): ApiPath<(String, String)>,
    ApiJson(body): ApiJson<LimitRequest>,
) -> Response {
    either(
        async {
            let (processor, name) = acting(&state, &caller, &service)?;
            let kind = WorkerKind::parse(&body.kind).ok_or_else(|| {
                AppError::bad_request(format!(
                    "`kind` is transcodeGpu, transcodeCpu, healthGpu or healthCpu, not `{}`.",
                    body.kind
                ))
                .into_response()
            })?;
            if !(0..=LIMIT_MAX).contains(&body.to) {
                return Err(Stop::from(
                    AppError::bad_request(format!("`to` is 0 to {LIMIT_MAX}, not {}.", body.to))
                        .into_response(),
                ));
            }
            let found = node_of(&processor, &name, &node).await?;
            let from = kind.of(&found.limits);
            let up = body.to > from;
            let steps = (body.to - from).unsigned_abs() as usize;
            let acts = vec![
                WorkerAct::Limit {
                    node: node.clone(),
                    kind,
                    up,
                };
                steps
            ];
            let about = json!({
                "node": node, "node_name": found.name, "kind": kind.as_str(),
                "from": from, "to": body.to,
            });
            Ok::<Response, Stop>(run(&state, &caller, &processor, &name, &acts, about).await)
        }
        .await,
    )
}

/// Pause a node (`{"paused": true}`) or resume it. The owner alone, while Singularr may act on
/// the worker.
#[utoipa::path(post, path = "/api/v1/processing/workers/{service}/nodes/{node}/pause", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id"), ("node" = String, Path, description = "The node's id")),
    request_body = PauseRequest,
    responses((status = 200, body = ActAnswer), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_pause(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath((service, node)): ApiPath<(String, String)>,
    ApiJson(body): ApiJson<PauseRequest>,
) -> Response {
    either(
        async {
            let (processor, name) = acting(&state, &caller, &service)?;
            let found = node_of(&processor, &name, &node).await?;
            let acts = [WorkerAct::Pause {
                node: node.clone(),
                paused: body.paused,
            }];
            let about = json!({"node": node, "node_name": found.name, "paused": body.paused});
            Ok::<Response, Stop>(run(&state, &caller, &processor, &name, &acts, about).await)
        }
        .await,
    )
}

/// Cancel what one worker of a node is on. The owner alone, while Singularr may act on the
/// worker service.
#[utoipa::path(post, path = "/api/v1/processing/workers/{service}/nodes/{node}/workers/{worker}/cancel", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id"), ("node" = String, Path, description = "The node's id"),
        ("worker" = String, Path, description = "The worker's own id, as `workers[].id` names it")),
    responses((status = 200, body = ActAnswer), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_cancel(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath((service, node, worker)): ApiPath<(String, String, String)>,
) -> Response {
    either(
        async {
            let (processor, name) = acting(&state, &caller, &service)?;
            let busy = processor
                .workers()
                .await
                .map_err(|e| could_not_reach(&name, &e))?;
            let on = busy
                .into_iter()
                .find(|w| {
                    w.id.as_deref() == Some(worker.as_str())
                        && w.node_id.as_deref() == Some(node.as_str())
                })
                .ok_or_else(|| {
                    AppError::not_found(format!("No worker `{worker}` is busy on node `{node}`."))
                        .into_response()
                })?;
            let acts = [WorkerAct::Cancel {
                node: node.clone(),
                worker: worker.clone(),
            }];
            let about = json!({
                "node": node, "node_name": on.node, "worker": worker,
                "file": on.file.as_deref().map(file_name),
            });
            Ok::<Response, Stop>(run(&state, &caller, &processor, &name, &acts, about).await)
        }
        .await,
    )
}

/// Put one file back in the worker's transcode queue: by its path in the worker (it lies in the
/// folder of one of the worker's libraries), or by a run, whose file waits at its queue path.
/// The owner alone, while Singularr may act on the worker.
#[utoipa::path(post, path = "/api/v1/processing/workers/{service}/requeue", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id")),
    request_body = RequeueRequest,
    responses((status = 200, body = ActAnswer), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_requeue(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(service): ApiPath<String>,
    ApiJson(body): ApiJson<RequeueRequest>,
) -> Response {
    either(
        async {
            let (processor, name) = acting(&state, &caller, &service)?;
            let word = |v: &Option<String>| {
                v.as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            let (path, run_id) = match (word(&body.path), word(&body.run)) {
                (Some(path), None) => (path, None),
                (None, Some(id)) => {
                    let found = store::one(&state.pool, &id)
                        .await
                        .map_err(|e| AppError::from(e).into_response())?
                        .ok_or_else(|| {
                            AppError::not_found(format!("There is no run `{id}`.")).into_response()
                        })?;
                    let path = queue_of(&found).map(str::to_string).ok_or_else(|| {
                        AppError::bad_request("This run waits at no path.").into_response()
                    })?;
                    (path, Some(id))
                }
                _ => {
                    return Err(Stop::from(
                        AppError::bad_request("Give `path` or `run`, one of them.").into_response(),
                    ));
                }
            };
            let library = processor
                .setup()
                .and_then(|s| s.library_of(&path).cloned())
                .ok_or_else(|| {
                    AppError::bad_request(format!("`path` is in no library of {name}."))
                        .into_response()
                })?;
            let acts = [WorkerAct::Requeue { path: path.clone() }];
            let about = json!({
                "path": path, "file": file_name(&path), "library": library.id,
                "library_name": library.name, "run": run_id,
            });
            Ok::<Response, Stop>(run(&state, &caller, &processor, &name, &acts, about).await)
        }
        .await,
    )
}

/// Look for new files in a library's folder. The owner alone, while Singularr may act on the
/// worker.
#[utoipa::path(post, path = "/api/v1/processing/workers/{service}/libraries/{library}/scan", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id"), ("library" = String, Path, description = "The library's id in the worker")),
    responses((status = 200, body = ActAnswer), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_scan(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath((service, library)): ApiPath<(String, String)>,
) -> Response {
    either(
        async {
            let (processor, name) = acting(&state, &caller, &service)?;
            let (_, found) = library_of(&processor, &name, &library)?;
            let acts = [WorkerAct::Scan {
                library: found.id.clone(),
                folder: found.folder.clone(),
            }];
            let about = json!({"library": found.id, "library_name": found.name});
            Ok::<Response, Stop>(run(&state, &caller, &processor, &name, &acts, about).await)
        }
        .await,
    )
}

/// Watch a library's folder for new files (`{"on": true}`), or stop. The owner alone, while
/// Singularr may act on the worker.
#[utoipa::path(post, path = "/api/v1/processing/workers/{service}/libraries/{library}/watch", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id"), ("library" = String, Path, description = "The library's id in the worker")),
    request_body = WatchRequest,
    responses((status = 200, body = ActAnswer), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_watch(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath((service, library)): ApiPath<(String, String)>,
    ApiJson(body): ApiJson<WatchRequest>,
) -> Response {
    either(
        async {
            let (processor, name) = acting(&state, &caller, &service)?;
            let (_, found) = library_of(&processor, &name, &library)?;
            let acts = [WorkerAct::Watch {
                library: found.id.clone(),
                folder: found.folder.clone(),
                on: body.on,
            }];
            let about = json!({"library": found.id, "library_name": found.name, "on": body.on});
            Ok::<Response, Stop>(run(&state, &caller, &processor, &name, &acts, about).await)
        }
        .await,
    )
}

/// Run one of the worker's flows in a library: `{"flow": "<flow id>"}`. The owner alone,
/// while Singularr may act on the worker.
#[utoipa::path(post, path = "/api/v1/processing/workers/{service}/libraries/{library}/flow", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id"), ("library" = String, Path, description = "The library's id in the worker")),
    request_body = FlowRequest,
    responses((status = 200, body = ActAnswer), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_flow(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath((service, library)): ApiPath<(String, String)>,
    ApiJson(body): ApiJson<FlowRequest>,
) -> Response {
    either(
        async {
            let (processor, name) = acting(&state, &caller, &service)?;
            let (setup, found) = library_of(&processor, &name, &library)?;
            let wanted = body.flow.trim();
            let flow = setup.flow(wanted).cloned().ok_or_else(|| {
                AppError::bad_request(format!("There is no flow `{wanted}` on {name}."))
                    .into_response()
            })?;
            let before = found.flow_id.clone();
            let acts = [WorkerAct::Flow {
                library: found.id.clone(),
                flow: flow.id.clone(),
            }];
            let about = json!({
                "library": found.id, "library_name": found.name, "flow": flow.id,
                "flow_name": flow.name,
                "from": before,
                "from_name": before.as_deref().and_then(|id| setup.flow(id)).map(|f| f.name.clone()),
            });
            Ok::<Response, Stop>(run(&state, &caller, &processor, &name, &acts, about).await)
        }
        .await,
    )
}

// Reading -------------------------------------------------------------------------------------

/// A worker busy now, on a node.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BusyView {
    /// The worker's own id: what cancelling what it is on names.
    pub id: Option<String>,
    pub file: Option<String>,
    pub percent: Option<f64>,
    pub fps: Option<f64>,
    pub eta: Option<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The run whose file it works on, when a run waits there.
    pub run: Option<String>,
}

/// One node, with its limits and what each of its workers is on.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct NodeOfWorker {
    #[serde(flatten)]
    pub node: NodeView,
    pub workers: Vec<BusyView>,
}

/// One period of the worker's history: what it saved, and how its jobs ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeriodView {
    pub bucket: String,
    pub saved_bytes: Option<i64>,
    pub transcoded: Option<i64>,
    pub transcode_failed: Option<i64>,
    pub not_required: Option<i64>,
    pub healthy: Option<i64>,
    pub health_failed: Option<i64>,
}

/// What the worker's server uses now.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesView {
    pub cpu_percent: Option<f64>,
    pub memory_used: Option<i64>,
    pub memory_total: Option<i64>,
    pub process_memory: Option<i64>,
    pub uptime_seconds: Option<i64>,
}

/// `GET /api/v1/processing/workers/{service}`: the worker as it answers now.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct WorkerPage {
    /// The service as the processing page lists it.
    pub service: ServiceView,
    /// What Singularr may do with it: `read` or `act`.
    pub access: String,
    /// Its nodes with what each worker is on. Empty when it does not answer.
    pub nodes: Vec<NodeOfWorker>,
    /// What does not answer, in a few words: the nodes, the queue, the resources, the history.
    pub unread: Vec<String>,
    /// The queue in its own order, read now.
    pub queue: Option<WorkerQueueView>,
    /// What its server uses now.
    pub resources: Option<ResourcesView>,
    /// Its history, period by period.
    pub history: Option<Vec<PeriodView>>,
}

/// The periods of a history, the saved bytes and the verdicts of one period side by side.
fn periods(history: &crate::engines::WorkerHistory) -> Vec<PeriodView> {
    let mut out: Vec<PeriodView> = Vec::new();
    for s in &history.saved {
        out.push(PeriodView {
            bucket: s.bucket.clone(),
            saved_bytes: Some(s.saved_bytes),
            transcoded: None,
            transcode_failed: None,
            not_required: None,
            healthy: None,
            health_failed: None,
        });
    }
    for v in &history.verdicts {
        let at = match out.iter().position(|p| p.bucket == v.bucket) {
            Some(i) => i,
            None => {
                out.push(PeriodView {
                    bucket: v.bucket.clone(),
                    saved_bytes: None,
                    transcoded: None,
                    transcode_failed: None,
                    not_required: None,
                    healthy: None,
                    health_failed: None,
                });
                out.len() - 1
            }
        };
        let p = &mut out[at];
        p.transcoded = Some(v.transcoded);
        p.transcode_failed = Some(v.transcode_failed);
        p.not_required = Some(v.not_required);
        p.healthy = Some(v.healthy);
        p.health_failed = Some(v.health_failed);
    }
    out
}

/// The worker as it answers now: its nodes and what each worker is on, its queue, its
/// libraries and flows (as its pass last read them), what its server uses and its history.
/// A part it does not give is null and named in `unread`; nothing is changed by reading it.
#[utoipa::path(get, path = "/api/v1/processing/workers/{service}", tag = "processing",
    params(("service" = String, Path, description = "The worker's integration id")),
    responses((status = 200, body = WorkerPage), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn worker_page(
    State(state): State<AppState>,
    ApiPath(service): ApiPath<String>,
) -> Result<Json<WorkerPage>, AppError> {
    let processor = state
        .engines
        .processor(&service)
        .filter(|p| p.role() == WORKER)
        .cloned()
        .ok_or_else(|| AppError::not_found(format!("There is no worker service `{service}`.")))?;
    let live = super::processing::live_now(&state).await;
    let listed = super::processing::service_views(&state, &live).await?;
    let view = listed
        .into_iter()
        .find(|s| s.id == service)
        .ok_or_else(|| AppError::not_found(format!("There is no worker service `{service}`.")))?;
    let mut unread = Vec::new();
    let (mut nodes, mut queue, mut resources, mut history) = (Vec::new(), None, None, None);
    if state.config.sync.enabled {
        let open = store::open(&state.pool).await?;
        let (status, busy, asked_queue, used, kept) = tokio::join!(
            processor.status(),
            processor.workers(),
            processor.read_queue(),
            processor.resources(),
            processor.history()
        );
        match (status, busy) {
            (Ok(status), Ok(busy)) => {
                let live_workers: Vec<_> = busy
                    .iter()
                    .map(|w| crate::processing::model::LiveWorker {
                        service: service.clone(),
                        worker: w.clone(),
                    })
                    .collect();
                for n in status.nodes.unwrap_or_default() {
                    let workers = live_workers
                        .iter()
                        .filter(|w| w.worker.node_id.as_deref() == Some(n.id.as_str()))
                        .map(|w| BusyView {
                            id: w.worker.id.clone(),
                            file: w.worker.file.clone(),
                            percent: w.worker.percent,
                            fps: w.worker.fps,
                            eta: w.worker.eta.clone(),
                            status: w.worker.status.clone(),
                            kind: w.worker.kind.clone(),
                            started_at: w.worker.started_at,
                            run: processing::run_of(w, &open, &live.files).map(|r| r.id.clone()),
                        })
                        .collect();
                    nodes.push(NodeOfWorker {
                        node: super::processing::node_view(&n),
                        workers,
                    });
                }
            }
            _ => unread.push("Nodes".to_string()),
        }
        match asked_queue {
            Ok(()) => {
                queue = processor
                    .queue()
                    .map(|q| super::processing::queue_view(&q, processor.setup().as_deref()))
            }
            Err(_) => unread.push("Queue".to_string()),
        }
        match used {
            Ok(r) => {
                resources = r.map(|r| ResourcesView {
                    cpu_percent: r.cpu_percent,
                    memory_used: r.memory_used,
                    memory_total: r.memory_total,
                    process_memory: r.process_memory,
                    uptime_seconds: r.uptime_seconds,
                })
            }
            Err(_) => unread.push("Resources".to_string()),
        }
        match kept {
            Ok(h) => history = h.as_ref().map(periods),
            Err(_) => unread.push("History".to_string()),
        }
    }
    Ok(Json(WorkerPage {
        access: state
            .engines
            .access
            .of(&service)
            .access
            .as_str()
            .to_string(),
        service: view,
        nodes,
        unread,
        queue,
        resources,
        history,
    }))
}

/// The worker routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    let at = "/processing/workers/{service}";
    Router::new()
        .route(at, get(worker_page))
        .route(&format!("{at}/nodes/{{node}}/limits"), post(worker_limits))
        .route(&format!("{at}/nodes/{{node}}/pause"), post(worker_pause))
        .route(
            &format!("{at}/nodes/{{node}}/workers/{{worker}}/cancel"),
            post(worker_cancel),
        )
        .route(&format!("{at}/requeue"), post(worker_requeue))
        .route(
            &format!("{at}/libraries/{{library}}/scan"),
            post(worker_scan),
        )
        .route(
            &format!("{at}/libraries/{{library}}/watch"),
            post(worker_watch),
        )
        .route(
            &format!("{at}/libraries/{{library}}/flow"),
            post(worker_flow),
        )
}

#[derive(OpenApi)]
#[openapi(paths(
    worker_page,
    worker_limits,
    worker_pause,
    worker_cancel,
    worker_requeue,
    worker_scan,
    worker_watch,
    worker_flow
))]
struct WorkerDoc;

/// The worker routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    WorkerDoc::openapi()
}
