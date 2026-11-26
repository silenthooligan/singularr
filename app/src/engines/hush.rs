//! The HUSH adapter, behind [`Processor`]. HUSH is a bridge in front of a worker: it stages a
//! file into a queue folder the worker watches (a job), checks that what the worker made is
//! the same content, has the manager import it, and keeps a ledger of every job.
//!
//! Endpoints used. With GET, without a key: `/health` (whether its watchers and its ledger
//! run, and what it offers beyond its history: `api`) and `/history.json?limit=N` (its jobs,
//! newest first, at most 2000); and on a bridge that says it offers them, `/jobs` (jobs by
//! id, and the jobs older than one) and `/stage?path=` (what staging a file would do, which
//! changes nothing). With POST, the one act ([`calls`]): `/stage` hands a file to a flow, with
//! the key as a bearer when one is configured. `core::outbound` lets that call through only
//! while the integration's access is `act`. A bridge whose health has no `api` is asked for
//! nothing but its health and its history.
//!
//! The words in this file are the bridge's own: its mode ids, its trigger strings, its job
//! states, the manager tags that start its flows and the one that keeps a title away from
//! it ([`KEEP_TAG`]). Nothing outside this file knows them.
//! Answers are read leniently: a field of an unexpected shape is no value, and a job without
//! an id or a start time is left out.

use super::access::Action;
use super::{
    EARLIER_SENDERS, EngineError, FlowNeeds, JobsById, JobsPage, Processor, ProcessorEvent,
    ProcessorFile, ProcessorFlow, ProcessorReplaced, ProcessorRun, ProcessorStatus,
    ProcessorWorker, Refusal, SENDER, Sent, StageOption, StageOptions, TriggerWords,
};
use crate::core::config::ProcessorConfig;
use crate::core::outbound::{
    Body, Calls, ConnectorError, Method, OutRequest, Outbound, encode_component, expect_ok,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::{Value, json};
use std::sync::{PoisonError, RwLock};

/// The kind of engine the bridge is, in the engine list.
pub const KIND: &str = "hush";
/// What it is in the pipeline: it hands files to a worker and has the results imported.
pub const ROLE: &str = "bridge";
/// Where a file is staged on demand: asked with GET, done with POST.
const STAGE_PATH: &str = "/stage";
/// Its jobs by id, and the jobs older than one.
const JOBS_PATH: &str = "/jobs";
/// The most ids asked for in one question. The route takes more, and the address of a
/// question has to stay well short of what the bridge's server reads of one (8 KB).
const IDS_MAX: usize = 100;
/// The manager tag with which the owner keeps a title away from the bridge. The bridge's own
/// rules and tags honor it; staging on demand has a path, not a title, and does not see it.
pub const KEEP_TAG: &str = "hush-keep";
/// What Singularr can do to the bridge beyond reading it, when the owner allows it.
pub const ACTIONS: &[Action] = &[Action {
    id: "send",
    label: "Send a file to a flow",
}];

/// The calls beyond GET the adapter makes: no read, and the one act, staging a file.
pub fn calls() -> Calls {
    Calls::default().act("send", Method::Post, STAGE_PATH)
}
/// The most jobs the history route gives in one answer.
pub const HISTORY_MAX: u32 = 2000;
/// The mode of a job whose mode the ledger does not hold.
const UNRECORDED: &str = "unrecorded";

/// The flows as a person chooses them: label, what it changes, the manager tag that starts it,
/// and the modes the bridge runs it under (it picks the 1080p or the 4K queue itself). A flow
/// is named by the first of its modes.
const FLOWS: &[(&str, &str, &str, &[&str])] = &[
    (
        "Convert to HEVC",
        "Re-encodes the video to HEVC",
        "hush-hevc",
        &["hevc"],
    ),
    (
        "Convert to AV1",
        "Re-encodes the video to AV1",
        "hush-av1",
        &["av1"],
    ),
    (
        "Master encode",
        "Quality-preserving re-encode from the best source",
        "hush-encode",
        &["encode", "uhd"],
    ),
    (
        "Rebuild audio",
        "Rebuilds the audio tracks",
        "hush-audio",
        &["audio", "uhd-audio"],
    ),
    (
        "Downscale to 1080p",
        "1080p file from a 4K source",
        "hush-downscale",
        &["downscale"],
    ),
    (
        "Shrink",
        "Re-encodes for a smaller file",
        "hush-shrink",
        &["shrink"],
    ),
    (
        "Master encode, two languages",
        "Quality-preserving re-encode from the best source",
        "hush-dual-encode",
        &["dual-encode"],
    ),
    (
        "Rebuild audio, two languages",
        "Rebuilds the audio tracks",
        "hush-dual-audio",
        &["dual-audio"],
    ),
    (
        "Strip image subtitles",
        "Removes image subtitle tracks",
        "hush-strip",
        &["strip"],
    ),
];

/// What a file must be for a flow to make sense: a downscale needs a 4K file, and a file is
/// not converted to the codec it already has (nor from AV1 back to HEVC).
fn needs(flow: &str) -> Option<FlowNeeds> {
    let not = |codecs: &[&str]| FlowNeeds {
        min_resolution: None,
        not_codec: codecs.iter().map(|c| c.to_string()).collect(),
    };
    match flow {
        "downscale" => Some(FlowNeeds {
            min_resolution: Some(2160),
            not_codec: Vec::new(),
        }),
        "hevc" => Some(not(&["HEVC", "AV1"])),
        "av1" => Some(not(&["AV1"])),
        _ => None,
    }
}

/// The bridge's flows, in the order a person chooses from.
pub fn flows() -> Vec<ProcessorFlow> {
    FLOWS
        .iter()
        .map(|(label, about, tag, modes)| ProcessorFlow {
            id: modes[0].to_string(),
            label: label.to_string(),
            about: Some(about.to_string()),
            tag: Some(tag.to_string()),
            modes: modes.iter().map(|m| m.to_string()).collect(),
            needs: needs(modes[0]),
        })
        .collect()
}

/// The label of a mode; the mode itself for one that is not a flow (`unrecorded`).
pub fn flow_label(mode: &str) -> String {
    FLOWS
        .iter()
        .find(|(_, _, _, modes)| modes.contains(&mode))
        .map(|(label, ..)| label.to_string())
        .unwrap_or_else(|| mode.to_string())
}

/// How the bridge names the trigger of a job that a send through its route opened:
/// `api:<requester>`. Singularr's sends name [`SENDER`] as their requester ([`sent_trigger`]).
/// This is the bridge's vocabulary, named here once: it stands until the SDK's run carries a
/// typed requester (B11).
pub const API_TRIGGER: &str = "api:";

/// The trigger of a job that one of Singularr's sends opened ([`API_TRIGGER`]).
pub fn sent_trigger() -> String {
    format!("{API_TRIGGER}{SENDER}")
}

/// What started a job, from the bridge's trigger text: its import rules, the drain of its
/// backlog, a tag on the title in the manager, or a person.
pub fn trigger_words(raw: &str) -> TriggerWords {
    let raw = raw.trim();
    let (kind, label) = match raw {
        "hush_rules" => ("rule", "Import rule".to_string()),
        "backlog drain" => ("backlog", "Backlog".to_string()),
        "manual" => ("manual", "By hand".to_string()),
        _ => match raw.split_once(':').map(|(what, who)| (what, who.trim())) {
            // Staged on demand through the bridge's own route ([`API_TRIGGER`]). Singularr
            // stages under its name now or an earlier one. Another program stages under its own.
            Some(("api", who)) if who == SENDER || EARLIER_SENDERS.contains(&who) => {
                ("api", "Singularr".to_string())
            }
            Some(("api", _)) => ("api", "API".to_string()),
            Some(("tag", tag)) if !tag.is_empty() => ("tag", format!("Tag {tag}")),
            _ => ("other", raw.to_string()),
        },
    };
    TriggerWords { kind, label }
}

fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn flag(v: &Value, key: &str) -> Option<bool> {
    v.get(key)?.as_bool()
}

fn list<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// A time as the bridge writes it: RFC 3339 (`2026-11-23T14:55:00Z`). One without a zone is
/// read as UTC.
fn time(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let s = v.get(key)?.as_str()?.trim();
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|t| t.and_utc())
        })
}

/// A file a job names: `{path, size}`.
fn file(v: Option<&Value>) -> Option<ProcessorFile> {
    let v = v?;
    Some(ProcessorFile {
        path: text(v, "path")?,
        size: int(v, "size").filter(|n| *n >= 0),
    })
}

/// What an event says beyond its name: the first of its note, reason, error or detail.
fn note(event: &Value) -> Option<String> {
    ["note", "reason", "error", "detail"]
        .into_iter()
        .find_map(|key| text(event, key))
}

fn event(e: &Value) -> Option<ProcessorEvent> {
    Some(ProcessorEvent {
        at: time(e, "ts")?,
        event: text(e, "event")?,
        actor: text(e, "actor"),
        note: note(e),
    })
}

/// A job's state in the product's words: a staged job waits for a worker, a job whose output
/// is staged is being verified, a promoted job is imported. The other states keep their names.
pub fn product_state(raw: &str) -> String {
    match raw {
        "staged" => "staged",
        "output_staged" => "verifying",
        "promoted" => "imported",
        other => other,
    }
    .to_string()
}

/// One job of the ledger as a run. `None` without an id or a start time.
pub fn run(job: &Value) -> Option<ProcessorRun> {
    let job_id = text(job, "id")?;
    let raw_state = text(job, "state").unwrap_or_default().to_ascii_lowercase();
    let raw_events = list(job, "events");
    let events: Vec<ProcessorEvent> = raw_events.iter().filter_map(event).collect();
    let updated = time(job, "updated");
    let created = time(job, "created")
        .or_else(|| events.first().map(|e| e.at))
        .or(updated)?;
    // The event that put the job in the state it is in: the last one that carries the state,
    // else the last one named after it.
    let named = |e: &&Value, key: &str| text(e, key).is_some_and(|s| s == raw_state);
    let closing = raw_events
        .iter()
        .rev()
        .find(|e| named(e, "state"))
        .or_else(|| raw_events.iter().rev().find(|e| named(e, "event")));
    let open = matches!(raw_state.as_str(), "staged" | "output_staged");
    let promoted = job.get("promoted");
    // Why a job ended without an import: the refusal, else the note of the event that closed
    // it. An import needs no reason.
    let detail = job
        .get("refusal")
        .and_then(|r| text(r, "detail"))
        .or_else(|| {
            if open || raw_state == "promoted" {
                None
            } else {
                closing.and_then(note)
            }
        });
    Some(ProcessorRun {
        job_id,
        flow: text(job, "mode").unwrap_or_else(|| UNRECORDED.to_string()),
        trigger: text(job, "trigger").unwrap_or_default(),
        state: product_state(&raw_state),
        created,
        updated,
        finished: if open {
            None
        } else {
            closing.and_then(|e| time(e, "ts")).or(updated)
        },
        queue_path: text(job, "queue_path"),
        source: file(job.get("source")),
        output: file(job.get("output"))
            .or_else(|| text(job, "staged_output").map(|path| ProcessorFile { path, size: None })),
        result: file(promoted),
        manager_file_id: promoted.and_then(|p| int(p, "arr_file_id")),
        replaced: promoted
            .map(|p| {
                list(p, "replaced")
                    .iter()
                    .filter_map(|r| {
                        Some(ProcessorReplaced {
                            path: text(r, "path")?,
                            size: int(r, "size").filter(|n| *n >= 0),
                            still_there: flag(r, "still_there"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        detail,
        events,
        raw: job.clone(),
    })
}

/// What `/health` says, as the bridge's status: whether its ledger is on, whether its tag
/// watcher and its strip worker run, and how it treats an output whose identity does not match.
pub fn status(health: &Value) -> ProcessorStatus {
    let part = |key: &str| health.get(key).unwrap_or(&Value::Null);
    let (history, watcher, strip) = (part("history"), part("tag_watcher"), part("strip_worker"));
    let ledger = flag(history, "ledger").unwrap_or(false);
    let tag_watcher = flag(watcher, "running").unwrap_or(false);
    let watcher_word = if tag_watcher {
        "running"
    } else if flag(watcher, "enabled").unwrap_or(false) {
        "stopped"
    } else {
        "off"
    };
    let mut facts = json!({
        "tagWatcher": tag_watcher,
        "stripWorker": flag(strip, "running").unwrap_or(false),
        "ledger": ledger,
        "identity": text(history, "identity"),
    });
    // What it offers beyond its history, when it says: staging on demand, jobs by id, and
    // whether staging wants a key.
    let api = part("api");
    if api.is_object() {
        let offered = offered(health);
        facts["api"] = json!({
            "stage": offered.stage,
            "jobs": offered.jobs,
            "auth": text(api, "auth"),
        });
    }
    ProcessorStatus {
        version: text(health, "version"),
        detail: format!(
            "Ledger {}, tag watcher {watcher_word}",
            if ledger { "on" } else { "off" }
        ),
        facts: Some(facts),
        nodes: None,
    }
}

/// What a bridge offers beyond its health and its history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Offered {
    /// It says what staging a file would do, and stages one on demand.
    pub stage: bool,
    /// It gives jobs by id, and the jobs older than one.
    pub jobs: bool,
}

/// What `/health` says the bridge offers: its `api`. A bridge that does not say offers
/// neither.
pub fn offered(health: &Value) -> Offered {
    let api = health.get("api").unwrap_or(&Value::Null);
    Offered {
        stage: flag(api, "stage").unwrap_or(false),
        jobs: flag(api, "jobs").unwrap_or(false),
    }
}

/// One flow of a `GET /stage` answer. `None` without the flow's name.
fn stage_option(v: &Value) -> Option<StageOption> {
    Some(StageOption {
        flow: text(v, "flow")?,
        mode: text(v, "mode"),
        would_stage: flag(v, "would_stage").unwrap_or(false),
        queue: text(v, "queue"),
        queue_path: text(v, "queue_path"),
        reason: text(v, "reason"),
        job: text(v, "job"),
    })
}

/// How a `POST /stage` ended, from its status and its body. `None` for a status that says
/// there is no such route with a body that is not the route's own: a bridge without it.
/// [`Sent::Unanswered`] for an answer that does not settle whether the bridge staged the file.
/// A 504 is one. A body that says staged under a status that says otherwise is one. A 5xx is one
/// unless the bridge's own body says `"staged": false`. An answer without the route's own body is
/// one unless the taxonomy reads its status as a no (a 4xx).
pub fn sent(status: u16, answer: &Value) -> Option<Sent> {
    let staged = flag(answer, "staged");
    let reason = text(answer, "reason");
    // The route's own body says whether it staged, or at least why it did not.
    let own = staged.is_some() || reason.is_some();
    if !own && matches!(status, 404 | 405) {
        return None;
    }
    if staged == Some(true) && matches!(status, 200..=299) {
        return Some(Sent::Staged {
            mode: text(answer, "mode"),
            job: text(answer, "job"),
            queue_path: text(answer, "queue_path"),
        });
    }
    // The bridge gave up waiting on its own staging, or what stands in front of it gave up
    // waiting on the bridge. A body that says staged under a status that says otherwise. A 5xx
    // without the bridge's own word that it did not stage. A status without the route's own body
    // that the taxonomy does not read as a no.
    let unclear = status == 504
        || staged == Some(true)
        || (status >= 500 && staged != Some(false))
        || (!own && !ConnectorError::from_status(status, "").act_settled_no());
    if unclear {
        return Some(Sent::Unanswered);
    }
    Some(Sent::Refused {
        why: match status {
            _ if !own => Refusal::Failed,
            404 => Refusal::NotFound,
            409 => Refusal::AlreadyStaged,
            400 | 422 => Refusal::Refused,
            401 | 403 => Refusal::Unauthorized,
            _ => Refusal::Failed,
        },
        reason,
        detail: text(answer, "detail").or_else(|| text(answer, "message")),
        job: text(answer, "job"),
    })
}

/// The jobs of an answer of the jobs route, newest first.
fn runs_of(jobs: &[Value]) -> Vec<ProcessorRun> {
    let mut runs: Vec<ProcessorRun> = jobs.iter().filter_map(run).collect();
    runs.sort_by(|a, b| {
        b.created
            .cmp(&a.created)
            .then_with(|| b.job_id.cmp(&a.job_id))
    });
    runs
}

pub struct Hush {
    id: String,
    base: String,
    /// The key the bridge wants before it stages a file, when it wants one.
    api_key: Option<String>,
    out: Outbound,
    /// What the bridge last said it offers. `None`: its health was not read yet.
    offered: RwLock<Option<Offered>>,
}

impl Hush {
    /// `None` when the integration has no address.
    pub fn new(config: &ProcessorConfig, out: Outbound) -> Option<Self> {
        Some(Hush {
            id: config.id.clone(),
            base: config.url.clone()?,
            api_key: config.api_key.clone(),
            out,
            offered: RwLock::new(None),
        })
    }

    /// What the bridge offers, as its health last said; its health is read first when it was
    /// never read.
    async fn offers(&self) -> Result<Offered, EngineError> {
        let known = *self.offered.read().unwrap_or_else(PoisonError::into_inner);
        match known {
            Some(offered) => Ok(offered),
            None => {
                self.status().await?;
                Ok(self
                    .offered
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .unwrap_or_default())
            }
        }
    }

    fn remember(&self, offered: Option<Offered>) {
        *self.offered.write().unwrap_or_else(PoisonError::into_inner) = offered;
    }

    /// The `jobs` list of an answer of the jobs route.
    async fn jobs(&self, query: &str) -> Result<(Vec<ProcessorRun>, Value), EngineError> {
        let answer = self.get("jobs", &format!("{JOBS_PATH}?{query}")).await?;
        let jobs = answer
            .get("jobs")
            .and_then(Value::as_array)
            .ok_or_else(|| self.unreadable("jobs", "the answer has no `jobs` list"))?;
        Ok((runs_of(jobs), answer))
    }

    fn unreadable(&self, what: &'static str, message: impl Into<String>) -> EngineError {
        EngineError::Decode {
            engine: self.id.clone(),
            what,
            message: message.into(),
        }
    }

    async fn get(&self, what: &'static str, path: &str) -> Result<Value, EngineError> {
        let url = format!("{}{path}", self.base);
        let req = OutRequest::get(&url).header("Accept", "application/json");
        let res = self.out.send(req).await?;
        expect_ok(&url, &res)?;
        serde_json::from_slice(&res.body).map_err(|e| self.unreadable(what, e.to_string()))
    }
}

#[async_trait]
impl Processor for Hush {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        KIND
    }

    fn role(&self) -> &'static str {
        ROLE
    }

    async fn status(&self) -> Result<ProcessorStatus, EngineError> {
        let health = self.get("health", "/health").await?;
        if !health.is_object() {
            return Err(self.unreadable("health", "the answer is not an object"));
        }
        self.remember(Some(offered(&health)));
        Ok(status(&health))
    }

    async fn runs(&self, limit: u32) -> Result<Vec<ProcessorRun>, EngineError> {
        let limit = limit.clamp(1, HISTORY_MAX);
        let answer = self
            .get("history", &format!("/history.json?limit={limit}"))
            .await?;
        // An answer without the list is not an empty ledger.
        let jobs = answer
            .get("jobs")
            .and_then(Value::as_array)
            .ok_or_else(|| self.unreadable("history", "the answer has no `jobs` list"))?;
        Ok(runs_of(jobs))
    }

    /// The bridge does no work itself: its worker does.
    async fn workers(&self) -> Result<Vec<ProcessorWorker>, EngineError> {
        Ok(Vec::new())
    }

    fn flows(&self) -> Vec<ProcessorFlow> {
        flows()
    }

    fn flow_label(&self, flow: &str) -> String {
        flow_label(flow)
    }

    fn trigger(&self, raw: &str) -> TriggerWords {
        trigger_words(raw)
    }

    fn protecting_tag(&self) -> Option<&'static str> {
        Some(KEEP_TAG)
    }

    /// What staging one file would do, flow by flow: a GET that changes nothing.
    async fn options(&self, path: &str) -> Result<Option<StageOptions>, EngineError> {
        if !self.offers().await?.stage {
            return Ok(None);
        }
        let url = format!("{}{STAGE_PATH}?path={}", self.base, encode_component(path));
        let req = OutRequest::get(&url).header("Accept", "application/json");
        let res = self.out.send(req).await?;
        if matches!(res.status, 404 | 405) {
            // The route is gone since the bridge said it has it: it is asked again later.
            self.remember(None);
            return Ok(None);
        }
        expect_ok(&url, &res)?;
        let answer: Value = serde_json::from_slice(&res.body)
            .map_err(|e| self.unreadable("stage", e.to_string()))?;
        let flows = answer
            .get("flows")
            .and_then(Value::as_array)
            .ok_or_else(|| self.unreadable("stage", "the answer has no `flows` list"))?;
        Ok(Some(StageOptions {
            exists: flag(&answer, "exists").unwrap_or(false),
            flows: flows.iter().filter_map(stage_option).collect(),
        }))
    }

    /// Stage one file for one flow: the one call that changes something in the bridge. The
    /// key travels as a bearer when one is configured, and never otherwise.
    async fn send(&self, path: &str, flow: &str, requested_by: &str) -> Result<Sent, EngineError> {
        // What the bridge offers is read before the act. A read that fails sends nothing: the
        // act was not sent.
        let offered = match self.offers().await {
            Ok(offered) => offered,
            Err(e) => {
                return Err(EngineError::NotSent {
                    engine: self.id.clone(),
                    message: e.to_string(),
                });
            }
        };
        if !offered.stage {
            return Ok(Sent::NotOffered);
        }
        let url = format!("{}{STAGE_PATH}", self.base);
        let body = json!({"path": path, "flow": flow, "requested_by": requested_by});
        let mut req = OutRequest::post(&url, Body::Json(body.to_string()))
            .header("Accept", "application/json");
        if let Some(key) = &self.api_key {
            req = req.header("Authorization", format!("Bearer {key}"));
        }
        let res = match self.out.send(req).await {
            Ok(res) => res,
            // The bridge can wait minutes on its own staging lock, longer than a call waits
            // for its answer. An error that does not say the act surely did not take place is
            // no clear answer: a timeout, a connection that broke after the call went out, an
            // answer too large to read. The next pass finds the job if the bridge staged the
            // file. The call is not made again.
            Err(e) => {
                let e = EngineError::from(e);
                return if e.act_settled_no() {
                    Err(e)
                } else {
                    Ok(Sent::Unanswered)
                };
            }
        };
        let answer: Value = serde_json::from_slice(&res.body).unwrap_or(Value::Null);
        Ok(match sent(res.status, &answer) {
            Some(sent) => sent,
            None => {
                self.remember(None);
                Sent::NotOffered
            }
        })
    }

    async fn jobs_by_id(&self, ids: &[String]) -> Result<Option<JobsById>, EngineError> {
        if !self.offers().await?.jobs {
            return Ok(None);
        }
        let mut out = JobsById {
            jobs: Vec::new(),
            missing: Vec::new(),
        };
        // The route takes so many ids at once.
        for some in ids.chunks(IDS_MAX) {
            let asked: Vec<String> = some.iter().map(|id| encode_component(id)).collect();
            let (jobs, answer) = self.jobs(&format!("ids={}", asked.join(","))).await?;
            out.jobs.extend(jobs);
            out.missing.extend(
                list(&answer, "missing")
                    .iter()
                    .filter_map(|id| id.as_str().map(str::to_string)),
            );
        }
        Ok(Some(out))
    }

    async fn jobs_before(&self, before: &str, limit: u32) -> Result<Option<JobsPage>, EngineError> {
        if !self.offers().await?.jobs {
            return Ok(None);
        }
        let limit = limit.clamp(1, HISTORY_MAX);
        let query = format!("before={}&limit={limit}", encode_component(before));
        let (jobs, answer) = self.jobs(&query).await?;
        Ok(Some(JobsPage {
            jobs,
            next: text(&answer, "next"),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ProcessorKind;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{
        Access, AccessSwitch, Allowlist, OutboundError, TIMED_OUT, TOO_LARGE,
    };
    use std::sync::Arc;

    const BASE: &str = "http://10.0.0.60:8787";

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn hush(recorded: Arc<Recorded>) -> Hush {
        let mut allow = Allowlist::default();
        allow.allow_engine("hush", BASE, &[]).unwrap();
        let config = ProcessorConfig {
            id: "hush".to_string(),
            kind: ProcessorKind::Hush,
            url: Some(BASE.to_string()),
            api_key: None,
        };
        Hush::new(&config, Outbound::with_transport(allow, recorded)).unwrap()
    }

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[tokio::test]
    async fn the_ledger_is_read_with_get_alone_in_the_product_s_states() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}/history.json?limit=2000"),
            200,
            &fixture("hush_history.json"),
        );
        let h = hush(recorded.clone());
        // More than the bridge gives in one answer is asked for as its most.
        let runs = h.runs(5000).await.unwrap();
        assert_eq!(runs.len(), 12);
        assert!(
            runs.windows(2).all(|w| w[0].created >= w[1].created),
            "newest first"
        );
        let states: Vec<&str> = runs.iter().map(|r| r.state.as_str()).collect();
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
        let flows: Vec<&str> = runs.iter().map(|r| r.flow.as_str()).collect();
        assert_eq!(
            flows,
            vec![
                "hevc",
                "hevc",
                "hevc",
                "encode",
                "uhd-audio",
                "downscale",
                "audio",
                "hevc",
                "hevc",
                "hevc",
                "hevc",
                "hevc"
            ]
        );
        let triggers: Vec<&str> = runs.iter().map(|r| r.trigger.as_str()).collect();
        assert_eq!(
            triggers,
            vec![
                "hush_rules",
                "hush_rules",
                "hush_rules",
                "hush_rules",
                "hush_rules",
                "tag:hush-downscale",
                "tag:hush-audio",
                "backlog drain",
                "backlog drain",
                "backlog drain",
                "hush_rules",
                "hush_rules"
            ]
        );

        // An imported job: the source, what the worker made, the imported file and what it
        // replaced, each with its size.
        let imported = &runs[0];
        assert_eq!(imported.job_id, "20261123T145500Z-57ec91da");
        assert_eq!(imported.created, at("2026-11-23T14:55:00Z"));
        assert_eq!(imported.updated, Some(at("2026-11-23T15:06:15Z")));
        assert_eq!(imported.finished, Some(at("2026-11-23T15:06:15Z")));
        let source = imported.source.as_ref().unwrap();
        assert!(
            source
                .path
                .ends_with("[WEBDL-1080p][EAC3 Atmos 5.1][x264]-Kitsune.mkv")
        );
        assert_eq!(source.size, Some(2_562_033_987));
        let output = imported.output.as_ref().unwrap();
        assert!(output.path.contains("/hush/staging/"));
        assert_eq!(output.size, Some(2_021_176_200));
        let result = imported.result.as_ref().unwrap();
        assert!(
            result
                .path
                .ends_with("[WEBDL-1080p][Opus 5.1][HEVC]-Kitsune.mkv")
        );
        assert_eq!(result.size, Some(2_021_176_200));
        assert_eq!(imported.manager_file_id, Some(34462));
        assert_eq!(imported.replaced.len(), 1);
        assert_eq!(imported.replaced[0].path, source.path);
        assert_eq!(
            (imported.replaced[0].size, imported.replaced[0].still_there),
            (Some(2_562_033_987), Some(false))
        );
        assert_eq!(imported.detail, None);
        assert!(
            imported
                .queue_path
                .as_deref()
                .unwrap()
                .contains("/hush/queue/")
        );
        assert_eq!(imported.raw["identity"], "match");
        // Its events, with what each says beyond its name.
        let events: Vec<(&str, Option<&str>, Option<&str>)> = imported
            .events
            .iter()
            .map(|e| (e.event.as_str(), e.actor.as_deref(), e.note.as_deref()))
            .collect();
        assert_eq!(
            events,
            vec![
                ("staged", Some("hushq"), None),
                ("output_staged", Some("sonarr-finalizer"), None),
                (
                    "identity_ok",
                    Some("hush-api"),
                    Some("content identity, 1 file(s)")
                ),
                ("promoted", Some("hush-api"), None),
                ("removed", Some("hush-api"), Some("import confirmed")),
            ]
        );
        assert_eq!(imported.events[0].at, at("2026-11-23T14:55:00Z"));

        // A job finished when it was imported, whatever the bridge cleaned up later.
        let cleaned = &runs[4];
        assert_eq!(cleaned.finished, Some(at("2026-11-20T18:59:51Z")));
        assert_eq!(cleaned.updated, Some(at("2026-11-23T04:00:28Z")));
        assert_eq!(cleaned.source.as_ref().unwrap().size, Some(42_390_678_232));
        assert_eq!(cleaned.result.as_ref().unwrap().size, Some(41_880_809_756));

        // A staged job is open: nothing made, nothing imported, no end.
        let staged = &runs[6];
        assert_eq!(
            (&staged.finished, &staged.output, &staged.result),
            (&None, &None, &None)
        );
        assert_eq!(staged.detail, None);
        assert_eq!(staged.source.as_ref().unwrap().size, Some(9_295_592_609));
        assert!(staged.replaced.is_empty() && staged.manager_file_id.is_none());

        // The jobs that ended without an import say why, in the bridge's words.
        let superseded = &runs[8];
        assert_eq!(superseded.detail.as_deref(), Some("staged again"));
        assert_eq!(superseded.finished, Some(at("2026-11-11T21:47:40Z")));
        let failed = &runs[9];
        assert_eq!(
            failed.detail.as_deref(),
            Some("import not confirmed; the output stays in staging")
        );
        assert_eq!(failed.finished, Some(at("2026-11-11T15:22:34Z")));
        assert_eq!(failed.updated, Some(at("2026-11-13T04:01:39Z")));
        assert_eq!(failed.output.as_ref().unwrap().size, Some(1_928_452_244));
        assert!(failed.result.is_none());
        let pruned = &runs[10];
        assert_eq!(pruned.detail.as_deref(), Some("never finished"));
        assert_eq!(pruned.finished, Some(at("2026-11-13T04:01:27Z")));
        assert!(pruned.output.is_none());
        let refused = &runs[11];
        assert!(
            refused
                .detail
                .as_deref()
                .unwrap()
                .starts_with("the arr now holds /media/shows/"),
            "{:?}",
            refused.detail
        );
        assert_eq!(refused.finished, Some(at("2026-11-11T14:49:35Z")));
        let notes: Vec<(&str, bool)> = refused
            .events
            .iter()
            .map(|e| (e.event.as_str(), e.note.is_some()))
            .collect();
        assert_eq!(
            notes,
            vec![
                ("staged", false),
                ("output_staged", false),
                ("discarded", true),
                ("refused", false),
                ("removed", true),
            ]
        );
        assert!(
            refused.events[2]
                .note
                .as_deref()
                .unwrap()
                .starts_with("source_changed: ")
        );
        assert_eq!(refused.events[4].note.as_deref(), Some("older than 24 h"));

        assert_eq!(recorded.methods_seen(), vec![Method::Get]);
        assert!(recorded.urls_seen()[0].ends_with("/history.json?limit=2000"));
        // The bridge works on nothing itself.
        assert!(h.workers().await.unwrap().is_empty());
        assert_eq!((h.kind(), h.role()), ("hush", "bridge"));
    }

    #[test]
    fn a_job_is_read_leniently() {
        assert!(run(&json!({"state": "staged", "created": "2026-11-23T14:55:00Z"})).is_none());
        assert!(run(&json!({"id": "a", "state": "staged"})).is_none());
        // Without `created` the first event starts it; a time without a zone is UTC.
        let verifying = run(&json!({
            "id": "b", "state": "output_staged", "trigger": "manual",
            "staged_output": "/q/staging/b.mkv",
            "events": [
                {"ts": "2026-11-23T10:00:00", "event": "staged", "actor": "hushq"},
                {"event": "no time"},
                {"ts": "2026-11-23T10:20:00Z", "event": "output_staged", "state": "output_staged",
                 "error": "second", "note": "first"}
            ]
        }))
        .unwrap();
        assert_eq!(verifying.state, "verifying");
        assert_eq!(verifying.created, at("2026-11-23T10:00:00Z"));
        assert_eq!(verifying.flow, "unrecorded");
        assert_eq!(verifying.finished, None, "a job being verified is open");
        assert_eq!(
            verifying.output,
            Some(ProcessorFile {
                path: "/q/staging/b.mkv".to_string(),
                size: None
            })
        );
        assert_eq!(verifying.events.len(), 2);
        assert_eq!(verifying.events[1].note.as_deref(), Some("first"));
        assert_eq!(verifying.detail, None);
        // A state the adapter does not know keeps its name and counts as closed.
        let adopted = run(&json!({
            "id": "c", "state": "Adopted", "mode": "unrecorded", "trigger": "adopted at finalize",
            "created": "2026-11-23T10:00:00Z", "updated": "2026-11-23T11:00:00Z",
            "promoted": {"path": "/library/c.mkv", "size": "12", "replaced": "none"}
        }))
        .unwrap();
        assert_eq!(adopted.state, "adopted");
        assert_eq!(adopted.finished, Some(at("2026-11-23T11:00:00Z")));
        assert_eq!(adopted.result.as_ref().unwrap().size, Some(12));
        assert!(adopted.source.is_none() && adopted.replaced.is_empty());
        // A failed job without a note has no reason, and the event named after its state ends it.
        let failed = run(&json!({
            "id": "d", "state": "failed", "created": "2026-11-23T10:00:00Z",
            "events": [{"ts": "2026-11-23T10:05:00Z", "event": "failed", "error": "no space left"}]
        }))
        .unwrap();
        assert_eq!(failed.detail.as_deref(), Some("no space left"));
        assert_eq!(failed.finished, Some(at("2026-11-23T10:05:00Z")));
    }

    #[tokio::test]
    async fn its_health_is_its_status() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}/health"),
            200,
            &fixture("hush_health.json"),
        );
        let h = hush(recorded.clone());
        let s = h.status().await.unwrap();
        assert_eq!(s.detail, "Ledger on, tag watcher running");
        assert_eq!(s.version, None);
        assert_eq!(
            s.facts,
            Some(json!({
                "tagWatcher": true, "stripWorker": true, "ledger": true, "identity": "enforce"
            }))
        );
        assert!(s.nodes.is_none());
        assert_eq!(recorded.methods_seen(), vec![Method::Get]);
        // What is not said is not running.
        let bare = status(&json!({"tag_watcher": {"enabled": true, "running": false}}));
        assert_eq!(bare.detail, "Ledger off, tag watcher stopped");
        assert_eq!(bare.facts.as_ref().unwrap()["identity"], Value::Null);
        assert_eq!(status(&json!({})).detail, "Ledger off, tag watcher off");
        // An answer that is not the health object is not a healthy bridge.
        let odd = Recorded::new();
        odd.answer(Method::Get, "/health", 200, "[]");
        assert!(matches!(
            hush(odd).status().await,
            Err(EngineError::Decode { .. })
        ));
    }

    #[tokio::test]
    async fn an_answer_without_jobs_is_not_an_empty_ledger() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/history.json", 200, r#"{"error": "busy"}"#);
        let err = hush(recorded).runs(10).await.unwrap_err();
        assert!(matches!(err, EngineError::Decode { .. }), "{err}");
        let down = Recorded::new();
        down.answer(Method::Get, "/history.json", 503, "");
        assert!(hush(down).runs(10).await.is_err());
    }

    #[test]
    fn flows_and_triggers_have_the_bridge_s_words() {
        let all = flows();
        let ids: Vec<&str> = all.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "hevc",
                "av1",
                "encode",
                "audio",
                "downscale",
                "shrink",
                "dual-encode",
                "dual-audio",
                "strip"
            ]
        );
        let flow = |id: &str| all.iter().find(|f| f.id == id).unwrap();
        // The modes that share a label are one flow, named by the first.
        assert_eq!(flow("encode").modes, vec!["encode", "uhd"]);
        assert_eq!(flow("audio").modes, vec!["audio", "uhd-audio"]);
        assert_eq!(flow("hevc").modes, vec!["hevc"]);
        assert_eq!(
            (
                flow("hevc").label.as_str(),
                flow("hevc").about.as_deref(),
                flow("hevc").tag.as_deref()
            ),
            (
                "Convert to HEVC",
                Some("Re-encodes the video to HEVC"),
                Some("hush-hevc")
            )
        );
        assert_eq!(flow("dual-audio").label, "Rebuild audio, two languages");
        assert_eq!(flow("dual-audio").about, flow("audio").about);
        assert_eq!(flow("dual-encode").about, flow("encode").about);
        assert_eq!(flow("strip").tag.as_deref(), Some("hush-strip"));
        let mut labels: Vec<&str> = all.iter().map(|f| f.label.as_str()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), all.len(), "one flow per label");
        // What a file must be for a flow to make sense.
        assert_eq!(
            flow("downscale").needs,
            Some(FlowNeeds {
                min_resolution: Some(2160),
                not_codec: vec![]
            })
        );
        assert_eq!(
            flow("hevc").needs.as_ref().unwrap().not_codec,
            vec!["HEVC", "AV1"]
        );
        assert_eq!(flow("av1").needs.as_ref().unwrap().not_codec, vec!["AV1"]);
        assert!(
            all.iter()
                .filter(|f| !["downscale", "hevc", "av1"].contains(&f.id.as_str()))
                .all(|f| f.needs.is_none())
        );
        // A run keeps its own mode and takes its flow's label.
        assert_eq!(flow_label("uhd"), "Master encode");
        assert_eq!(flow_label("uhd-audio"), "Rebuild audio");
        assert_eq!(flow_label("shrink"), "Shrink");
        assert_eq!(flow_label("unrecorded"), "unrecorded");

        let words = |raw: &str| {
            let t = trigger_words(raw);
            (t.kind, t.label)
        };
        assert_eq!(words("hush_rules"), ("rule", "Import rule".to_string()));
        assert_eq!(words("backlog drain"), ("backlog", "Backlog".to_string()));
        assert_eq!(
            words("tag:hush-audio"),
            ("tag", "Tag hush-audio".to_string())
        );
        assert_eq!(words("manual"), ("manual", "By hand".to_string()));
        assert_eq!(
            words("adopted at finalize"),
            ("other", "adopted at finalize".to_string())
        );
        assert_eq!(words("tag:"), ("other", "tag:".to_string()));
        // A file staged on demand: by Singularr, under its name now or an earlier one, or by
        // another program.
        assert_eq!(words("api:singularr"), ("api", "Singularr".to_string()));
        assert_eq!(words("api:marquee"), ("api", "Singularr".to_string()));
        for other in ["api:cli", "api:api", "api: someone@example.org", "api:"] {
            assert_eq!(words(other), ("api", "API".to_string()), "{other}");
        }
        assert_eq!(words("api"), ("other", "api".to_string()));
        assert_eq!(
            words("tag: hush-audio "),
            ("tag", "Tag hush-audio".to_string())
        );
        for word in ["staged", "output_staged", "promoted", "failed", "pruned"] {
            let state = product_state(word);
            assert!(!state.is_empty() && !state.contains('_'), "{state}");
        }
        assert_eq!(product_state("output_staged"), "verifying");
        assert_eq!(product_state("promoted"), "imported");
        assert_eq!(product_state("refused"), "refused");
        assert_eq!(product_state("superseded"), "superseded");
    }

    const DAMSEL: &str = "/media/movies/Damsel (2024) {tmdb-763215}/Damsel (2024) {imdb-tt13452446} [WEBDL-1080p][Opus 5.1][HEVC]-FLUX.mkv";

    /// A bridge that says what it offers, with the one act its adapter declares and the
    /// switch that allows it.
    fn offering(recorded: Arc<Recorded>, key: Option<&str>) -> (Hush, AccessSwitch) {
        recorded.answer(
            Method::Get,
            &format!("{BASE}/health"),
            200,
            &fixture("hush_health_live.json"),
        );
        let mut allow = Allowlist::default();
        allow.allow_integration("hush", BASE, calls()).unwrap();
        let switch = allow.switch("hush").unwrap();
        let config = ProcessorConfig {
            id: "hush".to_string(),
            kind: ProcessorKind::Hush,
            url: Some(BASE.to_string()),
            api_key: key.map(str::to_string),
        };
        let bridge = Hush::new(&config, Outbound::with_transport(allow, recorded)).unwrap();
        (bridge, switch)
    }

    #[tokio::test]
    async fn a_bridge_that_does_not_say_what_it_offers_is_asked_for_nothing_new() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}/health"),
            200,
            &fixture("hush_health.json"),
        );
        let h = hush(recorded.clone());
        assert_eq!(h.options(DAMSEL).await.unwrap(), None);
        assert_eq!(
            h.send(DAMSEL, "hevc", SENDER).await.unwrap(),
            Sent::NotOffered
        );
        assert_eq!(h.jobs_by_id(&["a".to_string()]).await.unwrap(), None);
        assert_eq!(h.jobs_before("a", 2000).await.unwrap(), None);
        // Its health was read once, to know, and nothing else was asked.
        assert_eq!(recorded.urls_seen(), vec![format!("{BASE}/health")]);
        assert_eq!(offered(&json!({})), Offered::default());
        assert_eq!(
            offered(&json!({"api": {"stage": true, "jobs": "yes"}})),
            Offered {
                stage: true,
                jobs: false
            }
        );
        // A bridge that says so has it among its facts.
        let facts = status(&serde_json::from_str(&fixture("hush_health_live.json")).unwrap()).facts;
        assert_eq!(
            facts.unwrap()["api"],
            json!({"stage": true, "jobs": true, "auth": "none"})
        );
        // Whether it wants a key is its own word, kept as it says it.
        let keyed = status(&json!({"api": {"stage": true, "jobs": true, "auth": "bearer"}}));
        assert_eq!(keyed.facts.unwrap()["api"]["auth"], "bearer");
        // The tag that keeps a title away from it is the adapter's to name.
        assert_eq!(h.protecting_tag(), Some("hush-keep"));
    }

    #[tokio::test]
    async fn it_says_what_staging_a_file_would_do_and_changes_nothing() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}/stage?path="),
            200,
            &fixture("hush_stage_movie_live.json"),
        );
        let (h, _) = offering(recorded.clone(), None);
        let options = h.options(DAMSEL).await.unwrap().unwrap();
        assert!(options.exists);
        // One entry per flow, by the ids the flows have here.
        let named: Vec<&str> = options.flows.iter().map(|f| f.flow.as_str()).collect();
        let mut here: Vec<String> = flows().into_iter().map(|f| f.id).collect();
        let mut theirs: Vec<String> = named.iter().map(|f| f.to_string()).collect();
        here.sort();
        theirs.sort();
        assert_eq!(
            here, theirs,
            "the bridge's flows are the flows a person chooses"
        );
        let flow = |id: &str| options.flows.iter().find(|f| f.flow == id).unwrap();
        assert_eq!(
            flow("hevc"),
            &StageOption {
                flow: "hevc".to_string(),
                mode: Some("hevc".to_string()),
                would_stage: true,
                queue: Some("/media/hush/queue/movies/hevc_1080p".to_string()),
                queue_path: Some(
                    DAMSEL.replace("/media/movies/", "/media/hush/queue/movies/hevc_1080p/")
                ),
                reason: None,
                job: None,
            }
        );
        // It would take this file for every flow, and one of them has no queue folder.
        assert!(
            options
                .flows
                .iter()
                .all(|f| f.would_stage && f.reason.is_none() && f.job.is_none())
        );
        let strip = flow("strip");
        assert!(strip.would_stage && strip.queue.is_none() && strip.queue_path.is_none());
        // It was asked with GET, the path as one value of the address.
        assert_eq!(recorded.methods_seen(), vec![Method::Get; 2]);
        assert_eq!(
            recorded.urls_seen()[1],
            format!("{BASE}/stage?path={}", encode_component(DAMSEL))
        );
        assert!(!recorded.urls_seen()[1].contains(' '));

        // A path outside what the bridge serves: it does not look at it, and takes it for no
        // flow.
        let outside = Recorded::new();
        outside.answer(
            Method::Get,
            "/stage?path=",
            200,
            &fixture("hush_stage_outside_live.json"),
        );
        let (h, _) = offering(outside, None);
        let options = h.options("/elsewhere/a.mkv").await.unwrap().unwrap();
        assert!(!options.exists);
        assert_eq!(options.flows.len(), 9);
        assert!(options.flows.iter().all(|f| {
            !f.would_stage && f.mode.is_none() && f.reason.as_deref() == Some("outside_roots")
        }));

        // A file that is staged already: a flow says so, with the job that has it.
        let taken = Recorded::new();
        taken.answer(
            Method::Get,
            "/stage?path=",
            200,
            r#"{"path": "/library/a.mkv", "exists": true, "flows": [
                {"flow": "hevc", "mode": "hevc", "would_stage": false, "queue": "/q/hevc",
                 "queue_path": "/q/hevc/a.mkv", "reason": "already_staged", "job": "j1"},
                {"mode": "no name"}]}"#,
        );
        let (h, _) = offering(taken, None);
        let options = h.options("/library/a.mkv").await.unwrap().unwrap();
        assert_eq!(
            options.flows.len(),
            1,
            "an entry without a flow is left out"
        );
        assert_eq!(
            (
                options.flows[0].would_stage,
                options.flows[0].reason.as_deref(),
                options.flows[0].job.as_deref()
            ),
            (false, Some("already_staged"), Some("j1"))
        );
        // An answer without the list is not an answer; a route that is gone is not offered,
        // and the bridge is asked what it offers again.
        let odd = Recorded::new();
        odd.answer(Method::Get, "/stage?path=", 200, r#"{"exists": true}"#);
        let (h, _) = offering(odd, None);
        assert!(matches!(
            h.options(DAMSEL).await,
            Err(EngineError::Decode { .. })
        ));
        let gone = Recorded::new();
        gone.answer(Method::Get, "/stage?path=", 404, "Not Found");
        let (h, _) = offering(gone.clone(), None);
        assert_eq!(h.options(DAMSEL).await.unwrap(), None);
        assert_eq!(h.options(DAMSEL).await.unwrap(), None);
        let health = gone
            .urls_seen()
            .iter()
            .filter(|u| u.ends_with("/health"))
            .count();
        assert_eq!(
            health, 2,
            "its health is read again after the route was gone"
        );
    }

    #[tokio::test]
    async fn sending_a_file_is_the_one_act_and_passes_only_with_the_access() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Post,
            &format!("{BASE}/stage"),
            201,
            &fixture("hush_stage_staged.json"),
        );
        let (h, switch) = offering(recorded.clone(), Some("bridge-key"));
        // While the access is `read` the call is refused on the way out: nothing but the
        // bridge's health was asked.
        let refused = h.send(DAMSEL, "audio", SENDER).await.unwrap_err();
        assert!(
            matches!(
                refused,
                EngineError::Outbound(OutboundError::NotAllowed { .. })
            ),
            "{refused}"
        );
        assert_eq!(recorded.methods_seen(), vec![Method::Get]);

        switch.set(Access::Act);
        let staged = h.send(DAMSEL, "audio", SENDER).await.unwrap();
        assert_eq!(
            staged,
            Sent::Staged {
                mode: Some("audio".to_string()),
                job: Some("20261123T160500Z-0a1b2c3d".to_string()),
                queue_path: Some(
                    DAMSEL.replace("/media/movies/", "/media/hush/queue/movies/audio_1080p/")
                ),
            }
        );
        // What was sent: the file, the flow and who asks, with the key as a bearer.
        let sent_calls = recorded.calls_sent();
        let call = sent_calls.last().unwrap();
        assert_eq!(
            (call.method, call.url.as_str()),
            (Method::Post, "http://10.0.0.60:8787/stage")
        );
        let Body::Json(text) = &call.body else {
            panic!("{:?}", call.body)
        };
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({"path": DAMSEL, "flow": "audio", "requested_by": "singularr"})
        );
        assert!(
            call.headers
                .iter()
                .any(|(n, v)| n == "Authorization" && v == "Bearer bridge-key")
        );
        assert_eq!(
            recorded.methods_seen(),
            vec![Method::Get, Method::Post],
            "its health is not read again for each call"
        );
        // Without a key no such header is sent, and the reads never carry one.
        let open = Recorded::new();
        open.answer(
            Method::Post,
            "/stage",
            202,
            r#"{"staged": true, "flow": "strip", "mode": "strip"}"#,
        );
        open.answer(
            Method::Get,
            "/stage?path=",
            200,
            r#"{"exists": false, "flows": []}"#,
        );
        let (h, switch) = offering(open.clone(), None);
        switch.set(Access::Act);
        assert_eq!(
            h.send(DAMSEL, "strip", SENDER).await.unwrap(),
            Sent::Staged {
                mode: Some("strip".to_string()),
                job: None,
                queue_path: None
            }
        );
        let (keyed, _) = offering(open.clone(), Some("bridge-key"));
        assert!(!keyed.options(DAMSEL).await.unwrap().unwrap().exists);
        for call in open.calls_sent() {
            assert!(
                !call
                    .headers
                    .iter()
                    .any(|(n, _)| n.eq_ignore_ascii_case("authorization")),
                "{} {}",
                call.method.as_str(),
                call.url
            );
        }

        // Every way the bridge says no, by its status and in its own words.
        let refusal = |status: u16, body: Value| match sent(status, &body) {
            Some(Sent::Refused {
                why,
                reason,
                detail,
                job,
            }) => (why, reason, detail, job),
            other => panic!("{status}: {other:?}"),
        };
        let word = |w: &str| Some(w.to_string());
        assert_eq!(
            refusal(
                409,
                json!({"staged": false, "reason": "already_staged", "job": "j1"})
            ),
            (
                Refusal::AlreadyStaged,
                word("already_staged"),
                None,
                word("j1")
            )
        );
        assert_eq!(
            refusal(
                422,
                json!({"staged": false, "reason": "refused", "detail": "hushq: not a video file"})
            ),
            (
                Refusal::Refused,
                word("refused"),
                word("hushq: not a video file"),
                None
            )
        );
        assert_eq!(
            refusal(422, json!({"staged": false, "reason": "outside_roots"})).0,
            Refusal::Refused
        );
        assert_eq!(
            refusal(
                400,
                json!({"staged": false, "reason": "bad_request", "detail": "no such flow"})
            )
            .0,
            Refusal::Refused
        );
        assert_eq!(
            refusal(404, json!({"staged": false, "reason": "not_found"})).0,
            Refusal::NotFound
        );
        assert_eq!(
            refusal(401, json!({"staged": false, "reason": "unauthorized"})).0,
            Refusal::Unauthorized
        );
        assert_eq!(refusal(500, json!({"staged": false})).0, Refusal::Failed);
        // A bad request says why, without saying whether it staged.
        assert_eq!(
            refusal(
                400,
                json!({"reason": "bad_request", "detail": "unknown flow"})
            ),
            (
                Refusal::Refused,
                word("bad_request"),
                word("unknown flow"),
                None
            )
        );
        // No answer in time is not a no: the bridge gave up waiting on its own staging, or
        // what stands in front of it gave up waiting on the bridge.
        assert_eq!(
            sent(504, &json!({"staged": false, "reason": "timeout"})),
            Some(Sent::Unanswered)
        );
        assert_eq!(sent(504, &json!("Gateway Timeout")), Some(Sent::Unanswered));
        // A body that says staged under a status that says otherwise settles nothing. An answer
        // without the route's own body settles nothing either, unless its status is a no: a 4xx
        // refuses. A 404 or a 405 is no route at all.
        assert_eq!(sent(500, &json!({"staged": true})), Some(Sent::Unanswered));
        for status in [200, 201, 500, 502, 503] {
            assert_eq!(
                sent(status, &Value::Null),
                Some(Sent::Unanswered),
                "{status}"
            );
        }
        assert_eq!(
            sent(502, &json!("<html>Bad Gateway</html>")),
            Some(Sent::Unanswered)
        );
        assert_eq!(refusal(400, Value::Null).0, Refusal::Failed);
        assert_eq!(refusal(403, json!("Forbidden")).0, Refusal::Failed);
        // A 5xx refuses only with the bridge's own word that it did not stage.
        assert_eq!(
            sent(500, &json!({"reason": "error", "detail": "ledger locked"})),
            Some(Sent::Unanswered)
        );
        assert_eq!(
            sent(503, &json!({"reason": "busy"})),
            Some(Sent::Unanswered)
        );
        assert_eq!(
            refusal(500, json!({"staged": false, "reason": "error"})).0,
            Refusal::Failed
        );
        assert_eq!(sent(404, &Value::Null), None);
        assert_eq!(sent(405, &json!("Method Not Allowed")), None);
        let old = Recorded::new();
        old.answer(Method::Post, "/stage", 404, "<html>Not Found</html>");
        let (h, switch) = offering(old.clone(), None);
        switch.set(Access::Act);
        assert_eq!(
            h.send(DAMSEL, "hevc", SENDER).await.unwrap(),
            Sent::NotOffered
        );
        // A call that gets no answer in time is made once and not again: the bridge may
        // still stage the file.
        let slow = Recorded::new();
        slow.fail(Method::Post, "/stage", TIMED_OUT);
        let (h, switch) = offering(slow.clone(), None);
        switch.set(Access::Act);
        assert_eq!(
            h.send(DAMSEL, "hevc", SENDER).await.unwrap(),
            Sent::Unanswered
        );
        assert_eq!(slow.methods_seen(), vec![Method::Get, Method::Post]);
        // A connection that breaks after the call went out is no clear answer either. So is a
        // 502 from what stands in front of the bridge. Each call is made once.
        let reset = Recorded::new();
        reset.fail(Method::Post, "/stage", "connection reset by peer");
        let (h, switch) = offering(reset.clone(), None);
        switch.set(Access::Act);
        assert_eq!(
            h.send(DAMSEL, "hevc", SENDER).await.unwrap(),
            Sent::Unanswered
        );
        assert_eq!(reset.methods_seen(), vec![Method::Get, Method::Post]);
        let gateway = Recorded::new();
        gateway.answer(Method::Post, "/stage", 502, "<html>Bad Gateway</html>");
        let (h, switch) = offering(gateway.clone(), None);
        switch.set(Access::Act);
        assert_eq!(
            h.send(DAMSEL, "hevc", SENDER).await.unwrap(),
            Sent::Unanswered
        );
        assert_eq!(gateway.methods_seen(), vec![Method::Get, Method::Post]);
        // An answer too large to read came after the call went out: no clear answer either.
        let large = Recorded::new();
        large.fail(
            Method::Post,
            "/stage",
            &format!("{TOO_LARGE}: more than 8 bytes"),
        );
        let (h, switch) = offering(large.clone(), None);
        switch.set(Access::Act);
        assert_eq!(
            h.send(DAMSEL, "hevc", SENDER).await.unwrap(),
            Sent::Unanswered
        );
        // A read of what the bridge offers that fails before the act sends nothing: the act was
        // not sent. That is a clear no. The error carries the read's own words.
        let blind = Recorded::new();
        let (h, switch) = offering(blind.clone(), None);
        blind.fail(Method::Get, &format!("{BASE}/health"), TIMED_OUT);
        switch.set(Access::Act);
        let not_sent = h.send(DAMSEL, "hevc", SENDER).await.unwrap_err();
        assert!(
            matches!(&not_sent, EngineError::NotSent { message, .. } if message.contains(TIMED_OUT)),
            "{not_sent}"
        );
        assert!(not_sent.act_settled_no());
        assert_eq!(blind.methods_seen(), vec![Method::Get]);
        // A bridge that cannot be reached is an error: the call reached nothing.
        let down = Recorded::new();
        down.fail(Method::Post, "/stage", "connection refused or unreachable");
        let (h, switch) = offering(down, None);
        switch.set(Access::Act);
        assert!(matches!(
            h.send(DAMSEL, "hevc", SENDER).await,
            Err(EngineError::Outbound(OutboundError::Transport { .. }))
        ));
        // The adapter's one act is the only call it could ever make that is not a GET.
        let declared = calls();
        assert!(declared.reads.is_empty());
        assert_eq!(
            declared
                .acts
                .iter()
                .map(|c| (c.method, c.path.as_str()))
                .collect::<Vec<_>>(),
            vec![(Method::Post, "/stage")]
        );
        assert_eq!(ACTIONS.len(), 1);
        assert_eq!(
            (ACTIONS[0].id, ACTIONS[0].label),
            ("send", "Send a file to a flow")
        );
    }

    #[tokio::test]
    async fn jobs_are_asked_for_by_id_and_page_by_page() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}/jobs?ids="),
            200,
            &fixture("hush_jobs_by_id_live.json"),
        );
        let (h, _) = offering(recorded.clone(), None);
        let asked = [
            "20261115T001015Z-cb34e7ec".to_string(),
            "20991231T000000Z-00000000".to_string(),
        ];
        let found = h.jobs_by_id(&asked).await.unwrap().unwrap();
        // The job it has, and the id it has no job for.
        assert_eq!(found.missing, vec!["20991231T000000Z-00000000"]);
        assert_eq!(found.jobs.len(), 1);
        let job = &found.jobs[0];
        assert_eq!(
            (job.job_id.as_str(), job.state.as_str(), job.flow.as_str()),
            ("20261115T001015Z-cb34e7ec", "staged", "audio")
        );
        assert_eq!(job.source.as_ref().unwrap().size, Some(9_295_592_609));
        assert_eq!(
            recorded.urls_seen()[1],
            format!("{BASE}/jobs?ids={}", asked.join(","))
        );
        // A job Singularr sent says so in its trigger (this capture is from before the
        // rename: `api:marquee`).
        let ours: Value = serde_json::from_str(&fixture("hush_jobs_by_id.json")).unwrap();
        let ours = run(&ours["jobs"][0]).unwrap();
        assert_eq!(ours.source.as_ref().unwrap().path, DAMSEL);
        let words = trigger_words(&ours.trigger);
        assert_eq!((words.kind, words.label.as_str()), ("api", "Singularr"));

        // More ids than the route takes at once are asked for in several questions, and the
        // ids the bridge does not have come back as missing.
        let many = Recorded::new();
        many.answer(
            Method::Get,
            "/jobs?ids=",
            200,
            r#"{"jobs": [], "missing": ["gone-1", 7]}"#,
        );
        let (h, _) = offering(many.clone(), None);
        let ids: Vec<String> = (0..401).map(|n| format!("id-{n}")).collect();
        let found = h.jobs_by_id(&ids).await.unwrap().unwrap();
        assert!(found.jobs.is_empty());
        assert_eq!(found.missing, vec!["gone-1"; 5]);
        let asked: Vec<usize> = many
            .urls_seen()
            .iter()
            .filter_map(|u| u.split_once("/jobs?ids="))
            .map(|(_, ids)| ids.split(',').count())
            .collect();
        assert_eq!(asked, vec![100, 100, 100, 100, 1]);
        assert_eq!(h.jobs_by_id(&[]).await.unwrap().unwrap().jobs.len(), 0);

        // The jobs older than one: a page, newest first, and what to ask the next page with.
        let pages = Recorded::new();
        pages.answer(
            Method::Get,
            "/jobs?before=",
            200,
            &fixture("hush_jobs_page_live.json"),
        );
        let (h, _) = offering(pages.clone(), None);
        let page = h
            .jobs_before("20261123T000000Z-00000000", 5000)
            .await
            .unwrap()
            .unwrap();
        let ids: Vec<&str> = page.jobs.iter().map(|j| j.job_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "20261122T234200Z-5562ed5c",
                "20261122T211500Z-767a28c6",
                "20261122T205900Z-99ad7d62"
            ],
            "newest first"
        );
        assert!(page.jobs.iter().all(|j| j.state == "imported"));
        // What the next, older page is asked with: the oldest job of this one.
        assert_eq!(page.next.as_deref(), Some("20261122T205900Z-99ad7d62"));
        assert_eq!(
            pages.urls_seen()[1],
            format!("{BASE}/jobs?before=20261123T000000Z-00000000&limit=2000"),
            "no more than the bridge gives at once is asked for"
        );
        let last = Recorded::new();
        last.answer(
            Method::Get,
            "/jobs?before=",
            200,
            r#"{"jobs": [], "next": null}"#,
        );
        let (h, _) = offering(last, None);
        let page = h.jobs_before("a", 10).await.unwrap().unwrap();
        assert!(page.jobs.is_empty() && page.next.is_none());
        // An answer without the list is not an empty page.
        let odd = Recorded::new();
        odd.answer(Method::Get, "/jobs?", 200, r#"{"error": "busy"}"#);
        let (h, _) = offering(odd, None);
        assert!(matches!(
            h.jobs_before("a", 10).await,
            Err(EngineError::Decode { .. })
        ));
        assert!(h.jobs_by_id(&["a".to_string()]).await.is_err());
        // Every one of these was a GET, and none asked the bridge for its jobs by state,
        // which has it read its whole ledger.
        for seen in [&recorded, &many, &pages] {
            assert!(seen.methods_seen().iter().all(|m| *m == Method::Get));
            assert!(seen.urls_seen().iter().all(|u| !u.contains("state=")));
        }
    }
}
