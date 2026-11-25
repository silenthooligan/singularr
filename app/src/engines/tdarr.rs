//! The Tdarr adapter (API v2), read-only, behind [`Processor`]. Tdarr is a worker: it
//! transcodes the files that appear in the folders it watches.
//!
//! Endpoints used, with the key in `x-api-key` when one is configured. With GET:
//! `/api/v2/status` (its version) and `/api/v2/get-nodes` (its nodes: their worker limits,
//! their queue lengths and the workers busy now). With POST, because Tdarr takes the question
//! in a body, and only for questions that read ([`calls`], each with the body that makes it
//! one): `/api/v2/cruddb` in the modes `getAll` and `getById` (its libraries, its flows, its
//! totals, one file by its path), `/api/v2/client/status-tables` for one of its six tables
//! (its queue, in its own order), `/api/v2/list-footprintId-reports` and
//! `/api/v2/read-job-file` (the reports of the jobs it ran on a file, and one report's text),
//! `/api/v2/stats/get-space-saved` and `/api/v2/stats/get-worker-verdict-hist` (its history)
//! and `/api/v2/get-res-stats` (what its server uses now). No other mode of its record store
//! is ever sent, and `core::outbound` refuses one. Its job history is not a list Singularr reads
//! as runs: [`Processor::runs`] answers nothing.
//!
//! The libraries and flows are configuration: they are kept in memory and read again only
//! when the port is told to ([`Processor::read_setup`]). The words Tdarr has for where a
//! file's transcode stands are this file's to turn into the product's.
//!
//! Answers are read leniently: a field of an unexpected shape is no value. Of a busy worker
//! only what the service reports is taken; the whole entry is kept beside it.
//!
//! A worker that runs a flow works on a copy of the file in its cache folder, so the file it
//! reports is not the path the file waits at. What ties the two is Tdarr's footprint of the
//! file: the worker's `job` names it, and the record of the waiting file carries it.

use super::access::Action;
use super::{
    Acted, EngineError, FileAnswer, Processor, ProcessorFlow, ProcessorNode, ProcessorRun,
    ProcessorStatus, ProcessorWorker, QueuedFile, ReportText, SavedBucket, VerdictBucket,
    WorkerAct, WorkerCounts, WorkerFile, WorkerFlow, WorkerHistory, WorkerKind, WorkerLibrary,
    WorkerQueue, WorkerReport, WorkerResources, WorkerSetup, WorkerTables, WorkerTotals,
};
use crate::core::config::ProcessorConfig;
use crate::core::outbound::{
    Body, BodyRule, Calls, Method, OutRequest, OutResponse, Outbound, OutboundError, expect_ok,
};
use crate::core::units;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::sync::{Arc, PoisonError, RwLock};

/// The kind of engine Tdarr is, in the engine list.
pub const KIND: &str = "tdarr";
/// What it is in the pipeline: it does the work.
pub const ROLE: &str = "worker";
const STATUS_PATH: &str = "/api/v2/status";
const NODES_PATH: &str = "/api/v2/get-nodes";
/// Its record store. One path for every operation: the body's `data.mode` says which.
const RECORDS_PATH: &str = "/api/v2/cruddb";
/// The files of one of its tables.
const TABLES_PATH: &str = "/api/v2/client/status-tables";
/// The reports of the jobs a worker ran on one file, by the file's footprint.
const REPORTS_PATH: &str = "/api/v2/list-footprintId-reports";
/// The text of one report.
const REPORT_PATH: &str = "/api/v2/read-job-file";
/// Its history of space saved, and of how its jobs ended.
const SAVED_PATH: &str = "/api/v2/stats/get-space-saved";
const VERDICTS_PATH: &str = "/api/v2/stats/get-worker-verdict-hist";
/// What its server uses now.
const RESOURCES_PATH: &str = "/api/v2/get-res-stats";
/// The modes of the record store that only read.
const READ_MODES: [&str; 2] = ["getAll", "getById"];
/// Its six tables of files: waiting to be transcoded, transcoded or not in need of it,
/// transcode failed, waiting for a health check, healthy, health check failed.
const TABLES: [&str; 6] = ["table1", "table2", "table3", "table4", "table5", "table6"];
const LIBRARIES: &str = "LibrarySettingsJSONDB";
const FLOWS: &str = "FlowsJSONDB";
const STATISTICS: &str = "StatisticsJSONDB";
const FILES: &str = "FileJSONDB";
/// Bytes in what Tdarr calls a GB, and an MB.
const GIB: f64 = 1_073_741_824.0;
const MIB: f64 = 1_048_576.0;
/// The files of its queue a read keeps: the first, in its own order.
pub const QUEUE_SHOWN: u32 = 50;
/// What parts a report's name is made of: `<footprint>()<version>()<kind>()<job>()<start>.txt`.
const REPORT_PARTS: &str = "()";

/// The acts: one worker more or fewer, a node's own settings (here only its pause), cancel
/// what a worker is on, set files (here one file back in the queue), scan a library, switch a
/// library's folder watching. A library's flow is set in the record store.
const LIMIT_PATH: &str = "/api/v2/alter-worker-limit";
const NODE_PATH: &str = "/api/v2/update-node";
const CANCEL_PATH: &str = "/api/v2/cancel-worker-item";
const REQUEUE_PATH: &str = "/api/v2/bulk-update-files";
const SCAN_PATH: &str = "/api/v2/scan-files";
const WATCH_PATH: &str = "/api/v2/toggle-folder-watch";
/// Tdarr's words for its four kinds of worker.
const WORKER_TYPES: [&str; 4] = [
    "transcodegpu",
    "transcodecpu",
    "healthcheckgpu",
    "healthcheckcpu",
];
/// What a file waiting to be transcoded says, in Tdarr's words.
const QUEUED: &str = "Queued";
/// Why a worker's item is cancelled, as the request names it.
const CANCEL_CAUSE: &str = "user";
/// Paths refused whatever the method, GET included: the ones that change something on a GET
/// (the interface's restart, the end of a duplicate search), and everything under admin.
const NEVER: [&str; 3] = [
    "/api/v2/restart-ui",
    "/api/v2/stop-dedupe",
    "/api/v2/admin/",
];

/// What Singularr can do to the worker beyond reading it, when the owner allows it.
pub const ACTIONS: &[Action] = &[
    Action {
        id: "limits",
        label: "Change worker limits",
    },
    Action {
        id: "pause",
        label: "Pause and resume nodes",
    },
    Action {
        id: "cancel",
        label: "Cancel a worker's file",
    },
    Action {
        id: "requeue",
        label: "Requeue a file",
    },
    Action {
        id: "scan",
        label: "Scan a library",
    },
    Action {
        id: "watch",
        label: "Switch folder watching",
    },
    Action {
        id: "flow",
        label: "Assign a flow to a library",
    },
];

/// A question about the whole server, with no filter: its body is `{"data": {}}`.
fn whole() -> BodyRule {
    BodyRule::new().only("/data", &[])
}

/// The acts, each in the one form the adapter sends it. Where one path does more than one
/// thing, the form is the narrowest that does this act: a node's settings carry its pause
/// alone, the files set are one and only put back in the queue, a scan only finds new files,
/// and the record store takes the update of one library's flow and nothing else.
fn acts(calls: Calls) -> Calls {
    calls
        .act_with(
            "limits",
            Method::Post,
            LIMIT_PATH,
            BodyRule::new()
                .only("/data", &["nodeID", "process", "workerType"])
                .text("/data/nodeID")
                .one_of("/data/process", &["increase", "decrease"])
                .one_of("/data/workerType", &WORKER_TYPES),
        )
        .act_with(
            "pause",
            Method::Post,
            NODE_PATH,
            BodyRule::new()
                .only("/data", &["nodeID", "nodeUpdates"])
                .text("/data/nodeID")
                .only("/data/nodeUpdates", &["nodePaused"])
                .flag("/data/nodeUpdates/nodePaused"),
        )
        .act_with(
            "cancel",
            Method::Post,
            CANCEL_PATH,
            BodyRule::new()
                .only("/data", &["nodeID", "workerID", "cause"])
                .text("/data/nodeID")
                .text("/data/workerID")
                .one_of("/data/cause", &[CANCEL_CAUSE]),
        )
        .act_with(
            "requeue",
            Method::Post,
            REQUEUE_PATH,
            BodyRule::new()
                .only("/data", &["fileIds", "updatedObj"])
                .texts("/data/fileIds", 1)
                .only("/data/updatedObj", &["TranscodeDecisionMaker"])
                .one_of("/data/updatedObj/TranscodeDecisionMaker", &[QUEUED]),
        )
        .act_with(
            "scan",
            Method::Post,
            SCAN_PATH,
            BodyRule::new()
                .only("/data", &["scanConfig"])
                .only("/data/scanConfig", &["dbID", "mode", "arrayOrPath"])
                .text("/data/scanConfig/dbID")
                .one_of("/data/scanConfig/mode", &["scanFindNew"])
                .text("/data/scanConfig/arrayOrPath"),
        )
        .act_with(
            "watch",
            Method::Post,
            WATCH_PATH,
            BodyRule::new()
                .only("/data", &["auto", "folder", "dbID", "status"])
                .equals("/data/auto", Value::Bool(false))
                .text("/data/folder")
                .text("/data/dbID")
                .flag("/data/status"),
        )
        .act_with(
            "flow",
            Method::Post,
            RECORDS_PATH,
            BodyRule::new()
                .only("/data", &["collection", "mode", "docID", "obj"])
                .one_of("/data/collection", &[LIBRARIES])
                .one_of("/data/mode", &["update"])
                .text("/data/docID")
                .only("/data/obj", &["flowId"])
                .text("/data/obj/flowId"),
        )
}

/// The body of an act, as [`calls`] declares it.
fn act_body(act: &WorkerAct) -> (&'static str, Value) {
    match act {
        WorkerAct::Limit { node, kind, up } => (
            LIMIT_PATH,
            json!({"data": {
                "nodeID": node,
                "process": if *up { "increase" } else { "decrease" },
                "workerType": worker_type(*kind),
            }}),
        ),
        WorkerAct::Pause { node, paused } => (
            NODE_PATH,
            json!({"data": {"nodeID": node, "nodeUpdates": {"nodePaused": paused}}}),
        ),
        WorkerAct::Cancel { node, worker } => (
            CANCEL_PATH,
            json!({"data": {"nodeID": node, "workerID": worker, "cause": CANCEL_CAUSE}}),
        ),
        WorkerAct::Requeue { path } => (
            REQUEUE_PATH,
            json!({"data": {"fileIds": [path], "updatedObj": {"TranscodeDecisionMaker": QUEUED}}}),
        ),
        WorkerAct::Scan { library, folder } => (
            SCAN_PATH,
            json!({"data": {"scanConfig": {"dbID": library, "mode": "scanFindNew", "arrayOrPath": folder}}}),
        ),
        WorkerAct::Watch {
            library,
            folder,
            on,
        } => (
            WATCH_PATH,
            json!({"data": {"auto": false, "folder": folder, "dbID": library, "status": on}}),
        ),
        WorkerAct::Flow { library, flow } => (
            RECORDS_PATH,
            json!({"data": {"collection": LIBRARIES, "mode": "update", "docID": library, "obj": {"flowId": flow}}}),
        ),
    }
}

/// Tdarr's word for a kind of worker.
fn worker_type(kind: WorkerKind) -> &'static str {
    match kind {
        WorkerKind::TranscodeGpu => WORKER_TYPES[0],
        WorkerKind::TranscodeCpu => WORKER_TYPES[1],
        WorkerKind::HealthGpu => WORKER_TYPES[2],
        WorkerKind::HealthCpu => WORKER_TYPES[3],
    }
}

/// The calls beyond GET the adapter makes. Its reads, each with the body that makes it a read:
/// the record store in its reading modes for the four collections it reads (never its users or
/// its keys), one of its six tables (and nothing that tables
/// update), the reports of one footprint and one report's text, its history and its server's
/// resources. Its acts ([`ACTIONS`]), each in its one form. And the paths refused whatever
/// the method. Never declared, whatever is asked: deleting files or reports, restarting the
/// server or a node, removing a library's files, uploading, anything under admin.
pub fn calls() -> Calls {
    let reads = Calls::default()
        .read_with(
            Method::Post,
            RECORDS_PATH,
            BodyRule::new()
                .one_of("/data/collection", &[LIBRARIES, FLOWS, STATISTICS, FILES])
                .one_of("/data/mode", &READ_MODES),
        )
        .read_with(
            Method::Post,
            TABLES_PATH,
            BodyRule::new()
                .only("/data/opts", &["table"])
                .one_of("/data/opts/table", &TABLES),
        )
        .read_with(
            Method::Post,
            REPORTS_PATH,
            BodyRule::new()
                .only("/data", &["footprintId"])
                .text("/data/footprintId"),
        )
        .read_with(
            Method::Post,
            REPORT_PATH,
            BodyRule::new()
                .only("/data", &["footprintId", "jobId", "jobFileId"])
                .text("/data/footprintId")
                .text("/data/jobId")
                .text("/data/jobFileId"),
        )
        .read_with(Method::Post, SAVED_PATH, whole())
        .read_with(Method::Post, VERDICTS_PATH, whole())
        .read_with(Method::Post, RESOURCES_PATH, whole());
    NEVER
        .iter()
        .fold(acts(reads), |calls, path| calls.never(path))
}

pub struct Tdarr {
    id: String,
    base: String,
    api_key: Option<String>,
    out: Outbound,
    /// Its libraries and flows, as last read.
    setup: RwLock<Option<Arc<WorkerSetup>>>,
    /// Its totals, as last read.
    totals: RwLock<Option<WorkerTotals>>,
    /// The first files of its queue, as last read.
    queue: RwLock<Option<WorkerQueue>>,
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

fn float(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .filter(|n: &f64| n.is_finite())
}

/// A count, which Tdarr writes as a whole number or as a float (`106.0`). One it has not
/// started is 0, as Tdarr itself shows it.
fn count(v: &Value, key: &str) -> i64 {
    float(v, key).map(|n| n.round() as i64).unwrap_or(0).max(0)
}

/// A size Tdarr gives in GB (of 1024 MB), in bytes.
fn gib(v: &Value, key: &str) -> Option<i64> {
    float(v, key).map(|n| (n * GIB).round() as i64)
}

/// The documents of a `getAll` answer: a list, or one document.
fn documents(answer: &Value) -> Vec<&Value> {
    match answer {
        Value::Array(list) => list.iter().filter(|d| d.is_object()).collect(),
        Value::Object(_) => vec![answer],
        _ => Vec::new(),
    }
}

/// The libraries of a `getAll` answer of the library settings, in the worker's own order of
/// priority.
pub fn libraries(answer: &Value) -> Vec<WorkerLibrary> {
    let mut out: Vec<WorkerLibrary> = documents(answer)
        .into_iter()
        .filter_map(|d| {
            let id = text(d, "_id")?;
            Some(WorkerLibrary {
                name: text(d, "name").unwrap_or_else(|| id.clone()),
                folder: text(d, "folder")?.trim_end_matches(['/', '\\']).to_string(),
                flow_id: text(d, "flowId"),
                priority: int(d, "priority"),
                held: d
                    .get("holdNewFiles")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                watching: d
                    .get("folderWatching")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                transcodes: count(d, "totalTranscodeCount"),
                health_checks: count(d, "totalHealthCheckCount"),
                saved_bytes: gib(d, "sizeDiff").unwrap_or(0),
                id,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        (a.priority.is_none(), a.priority)
            .cmp(&(b.priority.is_none(), b.priority))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// The flows of a `getAll` answer of the flows, by name. A flow's steps are the names of its
/// plugins, in the order it lists them.
pub fn flows(answer: &Value) -> Vec<WorkerFlow> {
    let mut out: Vec<WorkerFlow> = documents(answer)
        .into_iter()
        .filter_map(|d| {
            let id = text(d, "_id")?;
            Some(WorkerFlow {
                name: text(d, "name").unwrap_or_else(|| id.clone()),
                about: text(d, "description"),
                steps: d
                    .get("flowPlugins")
                    .and_then(Value::as_array)
                    .map(|plugins| plugins.iter().filter_map(|p| text(p, "name")).collect())
                    .unwrap_or_default(),
                id,
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    out
}

/// The totals of a `getAll` answer of the statistics, which hold one document. `None` when
/// it holds none.
pub fn totals(answer: &Value) -> Option<WorkerTotals> {
    let d = documents(answer).into_iter().next()?;
    Some(WorkerTotals {
        files: count(d, "totalFileCount"),
        transcodes: count(d, "totalTranscodeCount"),
        health_checks: count(d, "totalHealthCheckCount"),
        saved_bytes: gib(d, "sizeDiff").unwrap_or(0),
        tables: WorkerTables {
            queue: count(d, "table1Count"),
            done: count(d, "table2Count"),
            failed: count(d, "table3Count"),
            health_queue: count(d, "table4Count"),
            healthy: count(d, "table5Count"),
            health_failed: count(d, "table6Count"),
        },
    })
}

/// Where a file's transcode stands, from Tdarr's own words for it: `Queued`,
/// `Transcode success`, `Transcode error`, `Not required`. `None` for words that are none of
/// these.
pub fn transcode_state(words: &str) -> Option<&'static str> {
    let w = words.trim().to_ascii_lowercase();
    if w.contains("not required") {
        Some("notRequired")
    } else if w.contains("success") {
        Some("done")
    } else if w.contains("error") || w.contains("fail") || w.contains("cancel") {
        Some("failed")
    } else if w.contains("queue") {
        Some("queued")
    } else {
        None
    }
}

/// One file as Tdarr's records hold it. `None` for a document that says nothing of a file.
pub fn file_record(d: &Value) -> Option<WorkerFile> {
    if !d.is_object() || (text(d, "_id").is_none() && text(d, "DB").is_none()) {
        return None;
    }
    let words = text(d, "TranscodeDecisionMaker");
    Some(WorkerFile {
        library: text(d, "DB"),
        state: words.as_deref().and_then(transcode_state),
        words,
        health: text(d, "HealthCheck"),
        old_size: gib(d, "oldSize").filter(|n| *n > 0),
        new_size: gib(d, "newSize").filter(|n| *n > 0),
        transcoded_at: time(d, "lastTranscodeDate"),
        // 0 says that it is not held.
        hold_until: time(d, "holdUntil"),
        footprint: text(d, "footprintId"),
    })
}

/// The first files of one of Tdarr's tables, and how many the table holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TablePage {
    pub total: i64,
    /// Each file by its path, as its record holds it.
    pub files: Vec<(String, WorkerFile)>,
}

/// A `status-tables` answer: `{array, totalCount}`.
pub fn table_page(answer: &Value) -> TablePage {
    let files: Vec<(String, WorkerFile)> = answer
        .get("array")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| Some((text(row, "_id")?, file_record(row)?)))
                .collect()
        })
        .unwrap_or_default();
    TablePage {
        total: int(answer, "totalCount").unwrap_or(files.len() as i64),
        files,
    }
}

/// The files of a `status-tables` answer, in the order it gives them: for the queue, the
/// order the worker takes them in. Its size is the one the file system gave the worker
/// (`statSync.size`), else its own figure in MB.
pub fn queue_files(answer: &Value) -> Vec<QueuedFile> {
    answer
        .get("array")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let path = text(row, "_id").or_else(|| text(row, "file"))?;
                    let stat = row.get("statSync").and_then(|s| int(s, "size"));
                    let size = stat
                        .or_else(|| float(row, "file_size").map(|mb| (mb * MIB).round() as i64))
                        .filter(|n| *n > 0);
                    Some(QueuedFile {
                        library: text(row, "DB"),
                        size,
                        codec: text(row, "video_codec_name"),
                        resolution: text(row, "video_resolution"),
                        bumped: row.get("bumped").and_then(Value::as_bool).unwrap_or(false),
                        hold_until: time(row, "holdUntil"),
                        added: time(row, "createdAt"),
                        path,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A report's name, read for what it says. Tdarr names a job's report
/// `<footprint>()<version>()<kind>()<job>()<start>.txt`, as a busy worker's `job.fileId`
/// shows; a name of another form is kept as it is, with nothing read from it.
pub fn report(name: &str) -> WorkerReport {
    let name = name.trim();
    let stem = name.strip_suffix(".txt").unwrap_or(name);
    let parts: Vec<&str> = stem.split(REPORT_PARTS).map(str::trim).collect();
    let part = |i: usize| {
        parts
            .get(i)
            .filter(|p| !p.is_empty() && parts.len() == 5)
            .map(|p| p.to_string())
    };
    WorkerReport {
        id: name.to_string(),
        version: part(1),
        kind: part(2),
        job: part(3),
        at: part(4)
            .and_then(|ms| ms.parse::<i64>().ok())
            .and_then(DateTime::from_timestamp_millis),
    }
}

/// The reports of a `list-footprintId-reports` answer, a list of names, newest first. An
/// entry that is an object names itself by `name`, `fileName` or `filename`.
pub fn reports(answer: &Value) -> Vec<WorkerReport> {
    let mut out: Vec<WorkerReport> = answer
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|entry| match entry {
                    Value::String(name) => Some(name.trim().to_string()),
                    Value::Object(_) => text(entry, "name")
                        .or_else(|| text(entry, "fileName"))
                        .or_else(|| text(entry, "filename")),
                    _ => None,
                })
                .filter(|name| !name.is_empty())
                .map(|name| report(&name))
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| b.id.cmp(&a.id)));
    out
}

/// A `read-job-file` answer: `{text, isJobRunning, jobReportExists}`.
pub fn report_text(answer: &Value) -> ReportText {
    let text = answer
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    ReportText {
        running: answer
            .get("isJobRunning")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        exists: answer
            .get("jobReportExists")
            .and_then(Value::as_bool)
            .unwrap_or(!text.is_empty()),
        text,
    }
}

/// A `get-res-stats` answer: `{process: {uptime, heapUsedMB, heapTotalMB}, os: {cpuPerc,
/// memUsedGB, memTotalGB}}`, numbers that may come as text.
pub fn resources(answer: &Value) -> WorkerResources {
    let process = answer.get("process").unwrap_or(&Value::Null);
    let os = answer.get("os").unwrap_or(&Value::Null);
    let mib = |v: &Value, key: &str| float(v, key).map(|n| (n * MIB).round() as i64);
    WorkerResources {
        cpu_percent: float(os, "cpuPerc"),
        memory_used: gib(os, "memUsedGB"),
        memory_total: gib(os, "memTotalGB"),
        process_memory: mib(process, "heapUsedMB"),
        process_memory_peak: mib(process, "heapTotalMB"),
        uptime_seconds: float(process, "uptime").map(|n| n.round() as i64),
    }
}

/// The periods of a history answer: `{buckets: [...]}`. `None` for an answer without that
/// list.
fn buckets(answer: &Value) -> Option<Vec<&Value>> {
    let list = answer.get("buckets")?.as_array()?;
    Some(list.iter().filter(|b| b.is_object()).collect())
}

/// A `get-space-saved` answer, period by period: `space_saved` in GB, as Tdarr writes sizes.
pub fn saved_history(answer: &Value) -> Vec<SavedBucket> {
    buckets(answer)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|b| {
            Some(SavedBucket {
                bucket: text(b, "bucket")?,
                saved_bytes: gib(b, "space_saved").unwrap_or(0),
            })
        })
        .collect()
}

/// A `get-worker-verdict-hist` answer, period by period: transcodes that succeeded, failed or
/// were not needed (`tSuc`, `tErr`, `tNq`), health checks that passed or failed (`hSuc`,
/// `hErr`).
pub fn verdict_history(answer: &Value) -> Vec<VerdictBucket> {
    buckets(answer)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|b| {
            Some(VerdictBucket {
                bucket: text(b, "bucket")?,
                transcoded: count(b, "tSuc"),
                transcode_failed: count(b, "tErr"),
                not_required: count(b, "tNq"),
                healthy: count(b, "hSuc"),
                health_failed: count(b, "hErr"),
            })
        })
        .collect()
}

/// A time as the service writes it: Unix milliseconds (or seconds), as a number or as text,
/// or an RFC 3339 text.
fn time(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let unix = |n: f64| {
        if !n.is_finite() || n <= 0.0 {
            return None;
        }
        // Seconds stay below this for thousands of years; milliseconds passed it in 1973.
        let millis = if n >= 100_000_000_000.0 {
            n
        } else {
            n * 1000.0
        };
        DateTime::from_timestamp_millis(millis as i64)
    };
    match v.get(key)? {
        Value::Number(n) => unix(n.as_f64()?),
        Value::String(s) => {
            let s = s.trim();
            match s.parse::<f64>() {
                Ok(n) => unix(n),
                Err(_) => DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|t| t.with_timezone(&Utc)),
            }
        }
        _ => None,
    }
}

/// A node's limits or queue lengths, by the service's names for its four kinds of worker.
fn counts(v: Option<&Value>) -> WorkerCounts {
    let v = v.unwrap_or(&Value::Null);
    let n = |key: &str| int(v, key).unwrap_or(0).max(0);
    WorkerCounts {
        transcode_gpu: n("transcodegpu"),
        transcode_cpu: n("transcodecpu"),
        health_gpu: n("healthcheckgpu"),
        health_cpu: n("healthcheckcpu"),
    }
}

/// The entries of a node's `workers`: an object by worker id (the key is kept), or a list.
fn worker_entries(node: &Value) -> Vec<(Option<&str>, &Value)> {
    match node.get("workers") {
        Some(Value::Object(map)) => map.iter().map(|(k, w)| (Some(k.as_str()), w)).collect(),
        Some(Value::Array(list)) => list.iter().map(|w| (None, w)).collect(),
        _ => Vec::new(),
    }
}

/// The nodes of a `/get-nodes` answer (an object by node id), by name.
pub fn nodes(answer: &Value) -> Vec<ProcessorNode> {
    let mut out: Vec<ProcessorNode> = answer
        .as_object()
        .map(|map| {
            map.iter()
                .filter(|(_, node)| node.is_object())
                .map(|(key, node)| {
                    let id = text(node, "_id").unwrap_or_else(|| key.clone());
                    ProcessorNode {
                        name: text(node, "nodeName").unwrap_or_else(|| id.clone()),
                        id,
                        paused: node
                            .get("nodePaused")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        busy: worker_entries(node).len() as i64,
                        limits: counts(node.get("workerLimits")),
                        queue: counts(node.get("queueLengths")),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
    out
}

/// What the nodes of a `/get-nodes` answer work on now.
pub fn workers(answer: &Value) -> Vec<ProcessorWorker> {
    let Some(map) = answer.as_object() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, node) in map {
        let node_id = text(node, "_id").unwrap_or_else(|| key.clone());
        let name = text(node, "nodeName").unwrap_or_else(|| node_id.clone());
        for (worker_key, w) in worker_entries(node) {
            out.push(ProcessorWorker {
                id: text(w, "_id").or_else(|| worker_key.map(str::to_string)),
                node_id: Some(node_id.clone()),
                node: Some(name.clone()),
                file: text(w, "file"),
                percent: float(w, "percentage"),
                fps: float(w, "fps"),
                eta: text(w, "ETA"),
                status: text(w, "status"),
                kind: text(w, "workerType"),
                started_at: time(w, "startTime"),
                // A worker without a job, or a job without a footprint, names none.
                footprint: w.get("job").and_then(|job| text(job, "footprintId")),
                raw: w.clone(),
            });
        }
    }
    out
}

/// The nodes in one line, counts only: `1 node, 0 busy`.
pub fn detail(nodes: &[ProcessorNode]) -> String {
    if nodes.is_empty() {
        return "No nodes".to_string();
    }
    let busy: i64 = nodes.iter().map(|n| n.busy).sum();
    let paused = nodes.iter().filter(|n| n.paused).count();
    let mut parts = vec![
        units::counted(nodes.len() as i64, "node", "nodes"),
        format!("{} busy", units::count(busy)),
    ];
    if paused > 0 {
        parts.push(format!("{} paused", units::count(paused as i64)));
    }
    parts.join(", ")
}

impl Tdarr {
    /// `None` when the integration has no address. A key is sent only when one is configured.
    pub fn new(config: &ProcessorConfig, out: Outbound) -> Option<Self> {
        Some(Tdarr {
            id: config.id.clone(),
            base: config.url.clone()?,
            api_key: config.api_key.clone(),
            out,
            setup: RwLock::new(None),
            totals: RwLock::new(None),
            queue: RwLock::new(None),
        })
    }

    fn unreadable(&self, what: &'static str, message: impl Into<String>) -> EngineError {
        EngineError::Decode {
            engine: self.id.clone(),
            what,
            message: message.into(),
        }
    }

    /// POST a body to a path, and give the answer whatever its status.
    async fn post(&self, path: &str, body: &Value) -> Result<(String, OutResponse), EngineError> {
        let url = format!("{}{path}", self.base);
        let mut req = OutRequest::post(&url, Body::Json(body.to_string()))
            .header("Accept", "application/json");
        if let Some(key) = &self.api_key {
            req = req.header("x-api-key", key.clone());
        }
        let res = self.out.send(req).await?;
        Ok((url, res))
    }

    /// POST a question that only reads, and give the answer's bytes: a record that is not
    /// there is answered with nothing at all.
    async fn ask(&self, path: &str, body: Value) -> Result<Vec<u8>, EngineError> {
        let (url, res) = self.post(path, &body).await?;
        expect_ok(&url, &res)?;
        Ok(res.body)
    }

    /// A question that only reads, its answer as JSON.
    async fn ask_json(
        &self,
        what: &'static str,
        path: &str,
        body: Value,
    ) -> Result<Value, EngineError> {
        let bytes = self.ask(path, body).await?;
        serde_json::from_slice(&bytes).map_err(|e| self.unreadable(what, e.to_string()))
    }

    /// The answer of one of its tables (1 to 6): the first `page_size` files in its own order,
    /// and how many it holds.
    async fn table_answer(&self, table: u8, page_size: u32) -> Result<Value, EngineError> {
        let answer = self
            .ask_json(
                "table",
                TABLES_PATH,
                json!({"data": {
                    "start": 0, "pageSize": page_size, "filters": [], "sorts": [],
                    "opts": {"table": format!("table{table}")}
                }}),
            )
            .await?;
        if answer.get("array").and_then(Value::as_array).is_none() {
            return Err(self.unreadable("table", "the answer has no `array` list"));
        }
        Ok(answer)
    }

    /// Every document of one collection of the record store.
    async fn all(&self, what: &'static str, collection: &str) -> Result<Value, EngineError> {
        let bytes = self
            .ask(
                RECORDS_PATH,
                json!({"data": {"collection": collection, "mode": READ_MODES[0]}}),
            )
            .await?;
        serde_json::from_slice(&bytes).map_err(|e| self.unreadable(what, e.to_string()))
    }

    /// The first `page_size` files of one of its tables (1 to 6: waiting to be transcoded,
    /// transcoded or not in need of it, transcode failed, waiting for a health check, healthy,
    /// health check failed), and how many the table holds.
    pub async fn table(&self, table: u8, page_size: u32) -> Result<TablePage, EngineError> {
        Ok(table_page(&self.table_answer(table, page_size).await?))
    }

    async fn get(&self, what: &'static str, path: &str) -> Result<Value, EngineError> {
        let url = format!("{}{path}", self.base);
        let mut req = OutRequest::get(&url).header("Accept", "application/json");
        if let Some(key) = &self.api_key {
            req = req.header("x-api-key", key.clone());
        }
        let res = self.out.send(req).await?;
        expect_ok(&url, &res)?;
        serde_json::from_slice(&res.body).map_err(|e| EngineError::Decode {
            engine: self.id.clone(),
            what,
            message: e.to_string(),
        })
    }
}

#[async_trait]
impl Processor for Tdarr {
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
        let status = self.get("status", STATUS_PATH).await?;
        let nodes = nodes(&self.get("nodes", NODES_PATH).await?);
        Ok(ProcessorStatus {
            version: text(&status, "version"),
            detail: detail(&nodes),
            facts: None,
            nodes: Some(nodes),
        })
    }

    /// Tdarr keeps no list of jobs that Singularr reads: what it did to a file is in that
    /// file's own record ([`Processor::file`]).
    async fn runs(&self, _limit: u32) -> Result<Vec<ProcessorRun>, EngineError> {
        Ok(Vec::new())
    }

    async fn workers(&self) -> Result<Vec<ProcessorWorker>, EngineError> {
        Ok(workers(&self.get("nodes", NODES_PATH).await?))
    }

    /// It offers nothing a person chooses from here: what its libraries run is its own
    /// configuration ([`Processor::setup`]).
    fn flows(&self) -> Vec<ProcessorFlow> {
        Vec::new()
    }

    async fn read_setup(&self) -> Result<(), EngineError> {
        let libraries = self.all("libraries", LIBRARIES).await?;
        let flows = self.all("flows", FLOWS).await?;
        // An answer that is no list is not a worker without libraries.
        if !libraries.is_array() || !flows.is_array() {
            return Err(self.unreadable("libraries", "the answer is not a list"));
        }
        let setup = WorkerSetup {
            libraries: self::libraries(&libraries),
            flows: self::flows(&flows),
        };
        *self.setup.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(setup));
        Ok(())
    }

    fn setup(&self) -> Option<Arc<WorkerSetup>> {
        self.setup
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn read_totals(&self) -> Result<(), EngineError> {
        let answer = self.all("statistics", STATISTICS).await?;
        let totals = self::totals(&answer)
            .ok_or_else(|| self.unreadable("statistics", "the answer holds no document"))?;
        *self.totals.write().unwrap_or_else(PoisonError::into_inner) = Some(totals);
        Ok(())
    }

    fn totals(&self) -> Option<WorkerTotals> {
        *self.totals.read().unwrap_or_else(PoisonError::into_inner)
    }

    /// A file by its path, which is its id in Tdarr's records. Tdarr answers a file it has no
    /// record of with an empty body.
    async fn file(&self, path: &str) -> Result<FileAnswer, EngineError> {
        let bytes = self
            .ask(
                RECORDS_PATH,
                json!({"data": {"collection": FILES, "mode": READ_MODES[1], "docID": path}}),
            )
            .await?;
        if bytes.iter().all(u8::is_ascii_whitespace) {
            return Ok(FileAnswer::Absent);
        }
        let answer: Value =
            serde_json::from_slice(&bytes).map_err(|e| self.unreadable("file", e.to_string()))?;
        Ok(match file_record(&answer) {
            Some(file) => FileAnswer::Known(file),
            None => FileAnswer::Absent,
        })
    }

    async fn reports(&self, footprint: &str) -> Result<Option<Vec<WorkerReport>>, EngineError> {
        let answer = self
            .ask_json(
                "reports",
                REPORTS_PATH,
                json!({"data": {"footprintId": footprint}}),
            )
            .await?;
        if !answer.is_array() {
            return Err(self.unreadable("reports", "the answer is not a list"));
        }
        Ok(Some(reports(&answer)))
    }

    /// The text of a report whose name says its job. Tdarr answers a report it does not have
    /// with 400 and the same shape as a text.
    async fn report(
        &self,
        footprint: &str,
        report: &WorkerReport,
    ) -> Result<Option<ReportText>, EngineError> {
        let Some(job) = report.job.as_deref() else {
            return Ok(None);
        };
        let body =
            json!({"data": {"footprintId": footprint, "jobId": job, "jobFileId": report.id}});
        let (url, res) = self.post(REPORT_PATH, &body).await?;
        if res.status != 400 {
            expect_ok(&url, &res)?;
        }
        let answer: Value = serde_json::from_slice(&res.body)
            .map_err(|e| self.unreadable("report", e.to_string()))?;
        if !answer.is_object() {
            return Err(self.unreadable("report", "the answer is not an object"));
        }
        Ok(Some(report_text(&answer)))
    }

    async fn read_queue(&self) -> Result<(), EngineError> {
        let answer = self.table_answer(1, QUEUE_SHOWN).await?;
        let files = queue_files(&answer);
        let queue = WorkerQueue {
            total: int(&answer, "totalCount").unwrap_or(files.len() as i64),
            files,
            read_at: Utc::now(),
        };
        *self.queue.write().unwrap_or_else(PoisonError::into_inner) = Some(queue);
        Ok(())
    }

    fn queue(&self) -> Option<WorkerQueue> {
        self.queue
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn resources(&self) -> Result<Option<WorkerResources>, EngineError> {
        let answer = self
            .ask_json("resources", RESOURCES_PATH, json!({"data": {}}))
            .await?;
        if !answer.is_object() {
            return Err(self.unreadable("resources", "the answer is not an object"));
        }
        Ok(Some(resources(&answer)))
    }

    async fn history(&self) -> Result<Option<WorkerHistory>, EngineError> {
        // Tdarr keeps these statistics for Tdarr Pro: a server without it answers 403. That is a
        // history the worker does not offer, not one that could not be read.
        let pro_only = |e: &EngineError| {
            matches!(
                e,
                EngineError::Outbound(OutboundError::Status { status: 403, .. })
            )
        };
        let saved = match self
            .ask_json("space saved", SAVED_PATH, json!({"data": {}}))
            .await
        {
            Ok(v) => v,
            Err(e) if pro_only(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let verdicts = match self
            .ask_json("verdicts", VERDICTS_PATH, json!({"data": {}}))
            .await
        {
            Ok(v) => v,
            Err(e) if pro_only(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        if buckets(&saved).is_none() || buckets(&verdicts).is_none() {
            return Err(self.unreadable("history", "the answer has no `buckets` list"));
        }
        Ok(Some(WorkerHistory {
            saved: saved_history(&saved),
            verdicts: verdict_history(&verdicts),
        }))
    }

    /// One act, in the form [`calls`] declares. Tdarr answers a change it takes with 200, and
    /// one it cannot make (a node that is gone, a worker that did not answer) with 400 and
    /// its words.
    async fn act(&self, act: &WorkerAct) -> Result<Acted, EngineError> {
        let (path, body) = act_body(act);
        let (url, res) = self.post(path, &body).await?;
        if res.status == 400 {
            let words = serde_json::from_slice::<Value>(&res.body)
                .ok()
                .and_then(|v| match v {
                    Value::String(s) => Some(s),
                    other => text(&other, "message").or_else(|| text(&other, "error")),
                })
                .unwrap_or_else(|| res.text());
            return Ok(Acted::Refused(words.trim().to_string()));
        }
        expect_ok(&url, &res)?;
        Ok(Acted::Done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ProcessorKind;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, OutboundError};

    const BASE: &str = "http://10.0.0.60:8265";

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// The way out to the worker, with the calls beyond GET the adapter declares.
    fn out(recorded: Arc<Recorded>) -> Outbound {
        let mut allow = Allowlist::default();
        allow.allow_integration("tdarr", BASE, calls()).unwrap();
        Outbound::with_transport(allow, recorded)
    }

    fn tdarr(recorded: Arc<Recorded>) -> Tdarr {
        let config = ProcessorConfig {
            id: "tdarr".to_string(),
            kind: ProcessorKind::Tdarr,
            url: Some(BASE.to_string()),
            api_key: Some("test-key".to_string()),
        };
        Tdarr::new(&config, out(recorded)).unwrap()
    }

    /// The path of the file whose record was saved.
    const KNOWN_FILE: &str = "/media/hush/queue/shows/hevc_1080p/Avatar - The Last Airbender (2024) {tvdb-385925}/Season 01/Avatar - The Last Airbender (2024) - S01E01 - Aang [WEBDL-1080p][EAC3 Atmos 5.1][x264]-Kitsune.mkv";

    /// The worker's records as they were saved, each answered to the question that asks for
    /// it: a collection in whole, or one file by its path. Any other file has no record,
    /// which Tdarr answers with an empty body.
    fn records() -> Arc<Recorded> {
        let recorded = Recorded::new();
        let at = format!("{BASE}{RECORDS_PATH}");
        for (collection, file) in [
            (LIBRARIES, "tdarr_libraries.json"),
            (FLOWS, "tdarr_flows.json"),
            (STATISTICS, "tdarr_statistics.json"),
        ] {
            recorded.answer_body(Method::Post, &at, collection, 200, &fixture(file));
        }
        recorded.answer_body(Method::Post, &at, FILES, 200, "");
        recorded.answer_body(
            Method::Post,
            &at,
            "S01E01 - Aang [WEBDL-1080p][EAC3 Atmos 5.1][x264]-Kitsune.mkv",
            200,
            &fixture("tdarr_file.json"),
        );
        recorded
    }

    /// Where the file waited that a worker was on when its answers were saved.
    const WAITING_FILE: &str = "/media/hush/queue/movies/audio_4k/Tron (1982) {tmdb-97}/Tron (1982) {imdb-tt0084827} [Remux-2160p Proper][DV HDR10][TrueHD Atmos 7.1][HEVC]-TRiToN.mkv";

    #[tokio::test]
    async fn its_version_nodes_and_workers_are_read_with_get_alone() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}{STATUS_PATH}"),
            200,
            &fixture("tdarr_status.json"),
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE}{NODES_PATH}"),
            200,
            &fixture("tdarr_nodes_idle.json"),
        );
        let t = tdarr(recorded.clone());
        let s = t.status().await.unwrap();
        assert_eq!(s.version.as_deref(), Some("3.09.00"));
        assert_eq!(s.detail, "1 node, 0 busy");
        assert!(s.facts.is_none());
        let nodes = s.nodes.unwrap();
        assert_eq!(nodes.len(), 1);
        let node = &nodes[0];
        assert_eq!(
            (node.id.as_str(), node.name.as_str(), node.paused, node.busy),
            ("TNUn37R", "Tdarr-Node-5090", false, 0)
        );
        assert_eq!(
            node.limits,
            WorkerCounts {
                transcode_gpu: 4,
                transcode_cpu: 0,
                health_gpu: 0,
                health_cpu: 2
            }
        );
        assert_eq!(node.queue, WorkerCounts::default());
        assert!(t.workers().await.unwrap().is_empty());
        // Its history and its flows are not read.
        assert!(t.runs(2000).await.unwrap().is_empty());
        assert!(t.flows().is_empty());
        assert_eq!(t.flow_label("anything"), "anything");
        let words = t.trigger("a plugin");
        assert_eq!((words.kind, words.label.as_str()), ("other", "a plugin"));
        assert_eq!((t.kind(), t.role()), ("tdarr", "worker"));
        assert_eq!(recorded.methods_seen(), vec![Method::Get; 3]);
        // The key travels in a header, never in the address.
        assert!(recorded.urls_seen().iter().all(|u| !u.contains("test-key")));
    }

    #[test]
    fn a_busy_worker_is_read_for_what_it_reports() {
        // The nodes as they answered while a worker ran a flow on a file.
        let answer: Value = serde_json::from_str(&fixture("tdarr_nodes_busy_live.json")).unwrap();
        let all = workers(&answer);
        assert_eq!(all.len(), 1);
        let busy = &all[0];
        assert_eq!(busy.node.as_deref(), Some("Tdarr-Node-5090"));
        // The worker's own id and its node's: what cancelling what it is on names.
        assert_eq!(
            (busy.id.as_deref(), busy.node_id.as_deref()),
            (Some("yearly-yak"), Some("TNUn37R"))
        );
        // While a flow runs, its file is its own copy in the cache folder, under the file's
        // own name: not the path the file waits at.
        let file = busy.file.as_deref().unwrap();
        assert!(
            file.starts_with("/media/transcode/") && file.ends_with("[HEVC]-TRiToN.mkv"),
            "{file}"
        );
        assert_ne!(file, WAITING_FILE);
        assert_eq!((busy.percent, busy.fps), (Some(0.0), Some(1982.0)));
        assert_eq!(busy.eta.as_deref(), Some("0:00:01"));
        assert_eq!(busy.status.as_deref(), Some("Movie Finalizer (Audio)"));
        assert_eq!(busy.kind.as_deref(), Some("transcodegpu"));
        assert_eq!(
            busy.started_at,
            DateTime::from_timestamp_millis(1_795_471_912_770)
        );
        assert_eq!(busy.raw["job"]["jobId"], "d99SYzi");
        // The footprint its job names is the one its record of the waiting file carries.
        assert_eq!(busy.footprint.as_deref(), Some("QEdffjm"));
        let waiting: Value =
            serde_json::from_str(&fixture("tdarr_file_working_live.json")).unwrap();
        assert_eq!(text(&waiting, "_id").as_deref(), Some(WAITING_FILE));
        let record = file_record(&waiting).unwrap();
        assert_eq!(record.footprint, busy.footprint);
        assert_eq!(
            (
                record.state,
                record.words.as_deref(),
                record.library.as_deref()
            ),
            (Some("done"), Some("Transcode success"), Some("Locrm0i_h"))
        );
        let read = nodes(&answer);
        assert_eq!(read[0].busy, 1);
        assert_eq!(detail(&read), "1 node, 1 busy");

        // What a worker does not report is absent, and its entry is kept as it came: one
        // that says nothing of itself, and ones whose job names no footprint.
        let mut odd = answer.clone();
        odd["TNUn37R"]["workers"] = json!({
            "a": {"idle": true},
            "b": {"file": "/cache/b.mkv", "job": {"jobId": "x"}},
            "c": {"file": "/cache/c.mkv", "job": "transcode"},
            "d": {"file": "/cache/d.mkv", "job": {"footprintId": "  "}}
        });
        let all = workers(&odd);
        assert_eq!(all.len(), 4);
        assert!(all.iter().all(|w| w.footprint.is_none()));
        let bare = all.iter().find(|w| w.file.is_none()).unwrap();
        assert_eq!(
            (
                &bare.percent,
                &bare.fps,
                &bare.eta,
                &bare.status,
                &bare.kind,
                &bare.started_at
            ),
            (&None, &None, &None, &None, &None, &None)
        );
        assert_eq!(bare.raw, json!({"idle": true}));
        assert_eq!(detail(&nodes(&odd)), "1 node, 4 busy");
        // A record that carries no footprint has none.
        let unmarked = file_record(&json!({"_id": "/queue/a.mkv"})).unwrap();
        assert_eq!(unmarked.footprint, None);
    }

    #[test]
    fn nodes_and_times_are_read_leniently() {
        assert!(nodes(&json!([])).is_empty());
        assert!(workers(&json!("down")).is_empty());
        assert_eq!(detail(&[]), "No nodes");
        let two = nodes(&json!({
            "b": {"nodeName": "Second", "nodePaused": true, "workers": {"w": {}},
                  "workerLimits": {"transcodecpu": "3", "transcodegpu": -1}},
            "a": {"_id": "a1", "workers": []},
            "note": "not a node"
        }));
        assert_eq!(two.len(), 2);
        assert_eq!(
            (two[0].id.as_str(), two[0].name.as_str(), two[0].busy),
            ("b", "Second", 1)
        );
        assert_eq!(
            (two[0].limits.transcode_cpu, two[0].limits.transcode_gpu),
            (3, 0)
        );
        assert_eq!((two[1].id.as_str(), two[1].name.as_str()), ("a1", "a1"));
        assert_eq!(detail(&two), "2 nodes, 1 busy, 1 paused");
        // Milliseconds, seconds, text or a written time.
        let when = Some("2026-11-23T15:01:00Z".parse::<DateTime<Utc>>().unwrap());
        for value in [
            json!(1795446060000_i64),
            json!(1795446060),
            json!("1795446060000"),
            json!("2026-11-23T15:01:00Z"),
        ] {
            assert_eq!(time(&json!({"startTime": value}), "startTime"), when);
        }
        assert_eq!(time(&json!({"startTime": 0}), "startTime"), None);
        assert_eq!(time(&json!({"startTime": "soon"}), "startTime"), None);
        assert_eq!(float(&json!({"fps": "58.5"}), "fps"), Some(58.5));
        assert_eq!(float(&json!({"fps": true}), "fps"), None);
    }

    #[tokio::test]
    async fn its_libraries_and_flows_are_read_with_post_and_kept() {
        let recorded = records();
        let t = tdarr(recorded.clone());
        assert!(t.setup().is_none(), "nothing is known before it is read");
        t.read_setup().await.unwrap();
        let setup = t.setup().unwrap();

        // The libraries, in the worker's order of priority: the folder each watches, the flow
        // it runs, what it did so far. Sizes are bytes.
        let ids: Vec<&str> = setup.libraries.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "jGVfjOD1m",
                "DRItYe5ns",
                "d448URBcV",
                "JellMoV01",
                "HushMovHEVC_1080",
                "HushMovHEVC_4K",
                "HushShowHEVC_1080"
            ]
        );
        assert_eq!(
            setup.libraries[0],
            WorkerLibrary {
                id: "jGVfjOD1m".to_string(),
                name: "Movies - HUSH 4K Downscale".to_string(),
                folder: "/media/hush/queue/movies/downscale".to_string(),
                flow_id: Some("hush_pipeline_movie_1080_master_v1".to_string()),
                priority: Some(1),
                held: false,
                watching: true,
                transcodes: 106,
                health_checks: 281,
                saved_bytes: 3_620_245_492_141,
            }
        );
        let shows = &setup.libraries[6];
        assert_eq!(
            (
                shows.name.as_str(),
                shows.priority,
                shows.transcodes,
                shows.health_checks
            ),
            ("TV - HUSH HEVC 1080p", Some(19), 1194, 2358)
        );
        assert_eq!(shows.saved_bytes, 883_913_332_363);
        // A library that has done nothing yet has no counters: they read as 0.
        let fresh = &setup.libraries[5];
        assert_eq!(
            (
                fresh.id.as_str(),
                fresh.transcodes,
                fresh.saved_bytes,
                fresh.held
            ),
            ("HushMovHEVC_4K", 0, 0, false)
        );

        // The flows: what each is for, and its steps in order.
        let names: Vec<&str> = setup.flows.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "HUSH Movie 1080p Master",
                "HUSH Movie HEVC 1080p (SHH)",
                "HUSH Movie HEVC 4K (SHH)",
                "HUSH TV HEVC 1080p (SHH)"
            ]
        );
        let master = setup.flow("hush_pipeline_movie_1080_master_v1").unwrap();
        assert_eq!(master.steps.len(), 24);
        assert_eq!(
            &master.steps[..3],
            [
                "Input",
                "Check File Size (Skip tiny samples)",
                "HUSH Grain Gate"
            ]
        );
        assert!(
            master
                .about
                .as_deref()
                .unwrap()
                .starts_with("Master encode (SHR), the quality work.")
        );
        let hevc = setup.flow("hush_movie_hevc_1080_v1").unwrap();
        assert_eq!(
            hevc.steps,
            vec![
                "Input",
                "Check File Size (Skip tiny samples)",
                "HEVC + Audio Gate",
                "Verify",
                "Radarr Finalizer (SHH)",
                "Set Original File"
            ]
        );
        assert!(setup.flow("nothing").is_none());

        // A file belongs to the library whose folder it lies below, the longest one.
        let library = setup.library_of(KNOWN_FILE).unwrap();
        assert_eq!(library.id, "HushShowHEVC_1080");
        assert_eq!(
            setup
                .flow(library.flow_id.as_deref().unwrap())
                .map(|f| f.name.as_str()),
            Some("HUSH TV HEVC 1080p (SHH)")
        );
        assert_eq!(
            setup
                .library_of("/media/hush/queue/movies/hevc_1080p/A (2001)/a.mkv")
                .map(|l| l.name.as_str()),
            Some("Movies - HUSH HEVC 1080p")
        );
        for outside in [
            "/media/hush/queue/movies/hevc_1080p",
            "/media/hush/queue/movies/hevc_1080p_old/a.mkv",
            "/media/movies/A (2001)/a.mkv",
            "a.mkv",
        ] {
            assert!(setup.library_of(outside).is_none(), "{outside}");
        }
        let nested = WorkerSetup {
            libraries: libraries(&json!([
                {"_id": "a", "name": "All", "folder": "/queue/"},
                {"_id": "b", "name": "Movies", "folder": "/queue/movies"},
                {"_id": "c", "name": "No folder"},
                {"name": "No id", "folder": "/queue/x"}
            ])),
            flows: Vec::new(),
        };
        assert_eq!(nested.libraries.len(), 2);
        assert_eq!(nested.library_of("/queue/movies/a.mkv").unwrap().id, "b");
        assert_eq!(nested.library_of("/queue/shows/a.mkv").unwrap().id, "a");
        assert_eq!(nested.library_of("D:\\queue\\a.mkv"), None);

        // Two questions were sent, both with POST and both in the mode that reads all; the key
        // travels in a header.
        let sent = recorded.calls_sent();
        assert_eq!(sent.len(), 2);
        for (call, collection) in sent.iter().zip([LIBRARIES, FLOWS]) {
            assert_eq!(call.method, Method::Post);
            assert_eq!(call.url, format!("{BASE}{RECORDS_PATH}"));
            let Body::Json(text) = &call.body else {
                panic!("{:?}", call.body)
            };
            assert_eq!(
                serde_json::from_str::<Value>(text).unwrap(),
                json!({"data": {"collection": collection, "mode": "getAll"}})
            );
            assert!(
                call.headers
                    .iter()
                    .any(|(n, v)| n == "x-api-key" && v == "test-key")
            );
        }
        // What was read stays in memory when a later read fails.
        recorded.answer_body(
            Method::Post,
            &format!("{BASE}{RECORDS_PATH}"),
            LIBRARIES,
            503,
            "",
        );
        assert!(t.read_setup().await.is_err());
        assert_eq!(t.setup().unwrap().libraries.len(), 7);
        // A worker that does not answer, or answers something else, gives no configuration.
        let down = Recorded::new();
        down.answer(Method::Post, RECORDS_PATH, 503, "");
        let t = tdarr(down);
        assert!(t.read_setup().await.is_err() && t.setup().is_none());
        let odd = Recorded::new();
        odd.answer(Method::Post, RECORDS_PATH, 200, r#"{"error": "busy"}"#);
        let t = tdarr(odd);
        assert!(matches!(
            t.read_setup().await,
            Err(EngineError::Decode { .. })
        ));
        assert!(t.setup().is_none());
    }

    #[tokio::test]
    async fn its_totals_and_one_file_are_read_from_its_records() {
        let recorded = records();
        let t = tdarr(recorded.clone());
        assert_eq!(t.totals(), None);
        t.read_totals().await.unwrap();
        assert_eq!(
            t.totals(),
            Some(WorkerTotals {
                files: 15,
                transcodes: 8575,
                health_checks: 36_734,
                saved_bytes: 17_581_694_338_397,
                tables: WorkerTables {
                    queue: 0,
                    done: 15,
                    failed: 0,
                    health_queue: 0,
                    healthy: 15,
                    health_failed: 0,
                },
            })
        );

        // One file by its path: its library, where its transcode stands in the worker's own
        // words and in the product's, its sizes in bytes, when it was done.
        let known = t.file(KNOWN_FILE).await.unwrap();
        assert_eq!(
            known,
            FileAnswer::Known(WorkerFile {
                library: Some("HushShowHEVC_1080".to_string()),
                state: Some("done"),
                words: Some("Transcode success".to_string()),
                health: Some("Success".to_string()),
                old_size: Some(2_562_033_987),
                new_size: Some(2_021_176_200),
                transcoded_at: Some("2026-11-23T15:06:20.431Z".parse().unwrap()),
                hold_until: None,
                footprint: Some("Ep7KKkm".to_string()),
            })
        );
        // A file the worker has no record of: an empty body, which is an answer.
        let unknown = "/media/hush/queue/movies/audio_1080p/A (2001)/a.mkv";
        assert_eq!(t.file(unknown).await.unwrap(), FileAnswer::Absent);
        let sent = recorded.calls_sent();
        let Body::Json(text) = &sent[2].body else {
            panic!("{:?}", sent[2].body)
        };
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({"data": {"collection": "FileJSONDB", "mode": "getById", "docID": unknown}})
        );
        assert_eq!(recorded.methods_seen(), vec![Method::Post; 3]);
        // `null` and an empty document are no record either; anything else is not an answer.
        for (body, absent) in [
            ("null", true),
            ("{}", true),
            ("  \n", true),
            ("<html>", false),
        ] {
            let odd = Recorded::new();
            odd.answer(Method::Post, RECORDS_PATH, 200, body);
            let answer = tdarr(odd).file(unknown).await;
            assert_eq!(
                matches!(answer, Ok(FileAnswer::Absent)),
                absent,
                "{body}: {answer:?}"
            );
        }

        // Tdarr's words for where a transcode stands, in the product's, and what it holds.
        for (words, state) in [
            ("Queued", Some("queued")),
            ("Transcode success", Some("done")),
            ("Transcode error", Some("failed")),
            ("Transcode cancelled", Some("failed")),
            ("Not required", Some("notRequired")),
            (" queued ", Some("queued")),
            ("Something new", None),
        ] {
            assert_eq!(transcode_state(words), state, "{words}");
        }
        let held = file_record(&json!({
            "_id": "/queue/a.mkv", "DB": "lib", "TranscodeDecisionMaker": "Queued",
            "HealthCheck": "Queued", "oldSize": 0, "newSize": 0, "lastTranscodeDate": 0,
            "holdUntil": 1795446060000_i64
        }))
        .unwrap();
        assert_eq!(
            (held.state, held.old_size, held.new_size, held.transcoded_at),
            (Some("queued"), None, None, None)
        );
        assert_eq!(
            held.hold_until,
            Some("2026-11-23T15:01:00Z".parse().unwrap())
        );
        assert_eq!(file_record(&json!({"file": "/queue/a.mkv"})), None);
        assert_eq!(totals(&json!([])), None);
        assert_eq!(totals(&json!({"totalFileCount": "3"})).unwrap().files, 3);

        // The files of one of its tables: how many it holds, and the first of them.
        let tables = Recorded::new();
        tables.answer_body(
            Method::Post,
            TABLES_PATH,
            "table2",
            200,
            &fixture("tdarr_table_success.json"),
        );
        tables.answer_body(
            Method::Post,
            TABLES_PATH,
            "table1",
            200,
            &fixture("tdarr_table_queue_empty.json"),
        );
        let t = tdarr(tables.clone());
        let done = t.table(2, 5).await.unwrap();
        assert_eq!((done.total, done.files.len()), (15, 2));
        assert_eq!(done.files[0].0, KNOWN_FILE);
        assert_eq!(
            (done.files[1].1.library.as_deref(), done.files[1].1.state),
            (Some("HushShowHEVC_1080"), Some("done"))
        );
        let queue = t.table(1, 5).await.unwrap();
        assert_eq!((queue.total, queue.files.len()), (0, 0));
        let asked = tables.calls_sent();
        let Body::Json(text) = &asked[0].body else {
            panic!("no body")
        };
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            json!({"data": {"start": 0, "pageSize": 5, "filters": [], "sorts": [],
                            "opts": {"table": "table2"}}})
        );
    }

    /// A report's name says its job, its kind and its start: the name a busy worker's job gives
    /// itself (`job.fileId`) is the real sample of the form.
    #[test]
    fn a_report_s_name_says_its_job_kind_and_start() {
        let answer: Value = serde_json::from_str(&fixture("tdarr_nodes_busy_live.json")).unwrap();
        let busy = &workers(&answer)[0];
        let name = busy.raw["job"]["fileId"].as_str().unwrap();
        let read = report(name);
        assert_eq!(read.id, name);
        assert_eq!(
            (
                read.job.as_deref(),
                read.kind.as_deref(),
                read.version.as_deref()
            ),
            (Some("d99SYzi"), Some("transcode"), Some("3.09.00"))
        );
        assert_eq!(read.at, DateTime::from_timestamp_millis(1_795_471_357_941));
        // A name of another form is kept, with nothing read from it.
        let odd = report("  report.txt ");
        assert_eq!(
            (odd.id.as_str(), odd.job, odd.at),
            ("report.txt", None, None)
        );
        // A listing, newest first; what is no name is left out.
        let listed = reports(&json!([
            "QEdffjm()3.09.00()healthcheck()aaa()1795400000000.txt",
            name,
            {"name": "QEdffjm()3.09.00()transcode()bbb()1795300000000.txt"},
            7,
            "",
            null
        ]));
        let jobs: Vec<&str> = listed.iter().filter_map(|r| r.job.as_deref()).collect();
        assert_eq!(jobs, ["d99SYzi", "aaa", "bbb"]);
        assert!(reports(&json!({"error": "busy"})).is_empty());
    }

    /// The answers here are shaped after Tdarr's own API description (`tdarr_api_docs.json`):
    /// no real answer of these calls was recorded yet, except the table, whose rows are the
    /// recorded ones of another table.
    #[tokio::test]
    async fn its_reports_queue_history_and_resources_are_read_with_their_bodies() {
        let recorded = Recorded::new();
        let name = "QEdffjm()3.09.00()transcode()d99SYzi()1795471357941.txt";
        recorded.answer(Method::Post, REPORTS_PATH, 200, &json!([name]).to_string());
        recorded.answer(
            Method::Post,
            REPORT_PATH,
            200,
            r#"{"text": "line 1\nline 2", "isJobRunning": false, "jobReportExists": true}"#,
        );
        recorded.answer_body(
            Method::Post,
            TABLES_PATH,
            "table1",
            200,
            &fixture("tdarr_table_success.json"),
        );
        recorded.answer(
            Method::Post,
            RESOURCES_PATH,
            200,
            r#"{"process": {"uptime": 86400, "heapUsedMB": "512.5", "heapTotalMB": "1024"},
                "os": {"cpuPerc": "12.5", "memUsedGB": "8", "memTotalGB": "32"}}"#,
        );
        recorded.answer(
            Method::Post,
            SAVED_PATH,
            200,
            r#"{"buckets": [{"bucket": "2026-11-22", "space_saved": 1.5},
                            {"bucket": "2026-11-23", "space_saved": "2"}]}"#,
        );
        recorded.answer(
            Method::Post,
            VERDICTS_PATH,
            200,
            r#"{"buckets": [{"bucket": "2026-11-23", "tSuc": 5, "tErr": 1, "tNq": 2.0, "hSuc": 9, "hErr": 0},
                            {"tSuc": 1}]}"#,
        );
        let t = tdarr(recorded.clone());
        let listed = t.reports("QEdffjm").await.unwrap().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].job.as_deref(), Some("d99SYzi"));
        let text = t.report("QEdffjm", &listed[0]).await.unwrap().unwrap();
        assert_eq!(
            text,
            ReportText {
                text: "line 1\nline 2".to_string(),
                running: false,
                exists: true
            }
        );
        // A report whose name does not say its job is not asked for.
        assert_eq!(t.report("QEdffjm", &report("odd.txt")).await.unwrap(), None);

        // Its queue, in its own order, with how many it holds; kept until read again.
        assert!(t.queue().is_none());
        t.read_queue().await.unwrap();
        let queue = t.queue().unwrap();
        assert_eq!((queue.total, queue.files.len()), (15, 2));
        let first = &queue.files[0];
        assert_eq!(first.path, KNOWN_FILE);
        assert_eq!(
            (
                first.size,
                first.codec.as_deref(),
                first.resolution.as_deref(),
                first.bumped,
                first.hold_until,
                first.library.as_deref()
            ),
            (
                Some(2_021_176_200),
                Some("hevc"),
                Some("1080p"),
                false,
                None,
                Some("HushShowHEVC_1080")
            )
        );
        assert_eq!(
            first.added,
            DateTime::from_timestamp_millis(1_795_446_380_427)
        );

        // What its server uses now, in bytes and percent.
        let used = t.resources().await.unwrap().unwrap();
        assert_eq!(
            used,
            WorkerResources {
                cpu_percent: Some(12.5),
                memory_used: Some(8_589_934_592),
                memory_total: Some(34_359_738_368),
                process_memory: Some(537_395_200),
                process_memory_peak: Some(1_073_741_824),
                uptime_seconds: Some(86_400),
            }
        );
        // Its history, period by period; a period with no name is left out.
        let history = t.history().await.unwrap().unwrap();
        assert_eq!(
            history.saved,
            vec![
                SavedBucket {
                    bucket: "2026-11-22".to_string(),
                    saved_bytes: 1_610_612_736
                },
                SavedBucket {
                    bucket: "2026-11-23".to_string(),
                    saved_bytes: 2_147_483_648
                }
            ]
        );
        assert_eq!(
            history.verdicts,
            vec![VerdictBucket {
                bucket: "2026-11-23".to_string(),
                transcoded: 5,
                transcode_failed: 1,
                not_required: 2,
                healthy: 9,
                health_failed: 0
            }]
        );
        // Each question went with POST and the body its rule names, the key in a header.
        let sent: Vec<(String, Value)> = recorded
            .calls_sent()
            .iter()
            .map(|c| {
                assert_eq!(c.method, Method::Post);
                assert!(c.headers.iter().any(|(n, _)| n == "x-api-key"));
                let Body::Json(text) = &c.body else {
                    panic!("{:?}", c.body)
                };
                (
                    c.url.trim_start_matches(BASE).to_string(),
                    serde_json::from_str(text).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                (
                    REPORTS_PATH.to_string(),
                    json!({"data": {"footprintId": "QEdffjm"}})
                ),
                (
                    REPORT_PATH.to_string(),
                    json!({"data": {"footprintId": "QEdffjm", "jobId": "d99SYzi", "jobFileId": name}})
                ),
                (
                    TABLES_PATH.to_string(),
                    json!({"data": {"start": 0, "pageSize": QUEUE_SHOWN, "filters": [], "sorts": [], "opts": {"table": "table1"}}})
                ),
                (RESOURCES_PATH.to_string(), json!({"data": {}})),
                (SAVED_PATH.to_string(), json!({"data": {}})),
                (VERDICTS_PATH.to_string(), json!({"data": {}})),
            ]
        );

        // A report Tdarr does not have is answered with 400 and the same shape; any other
        // failure is an error, and so is an answer of another shape.
        let gone = Recorded::new();
        gone.answer(
            Method::Post,
            REPORT_PATH,
            400,
            r#"{"text": "", "isJobRunning": false, "jobReportExists": false}"#,
        );
        let missing = tdarr(gone).report("QEdffjm", &listed[0]).await.unwrap();
        assert!(missing.is_some_and(|m| !m.exists && m.text.is_empty()));
        let broken = Recorded::new();
        broken.answer(Method::Post, REPORT_PATH, 500, "{}");
        broken.answer(Method::Post, REPORTS_PATH, 200, r#"{"files": []}"#);
        broken.answer(Method::Post, SAVED_PATH, 200, r#"{"rows": []}"#);
        broken.answer(Method::Post, VERDICTS_PATH, 200, r#"{"buckets": []}"#);
        let t = tdarr(broken);
        assert!(t.report("QEdffjm", &listed[0]).await.is_err());
        assert!(t.reports("QEdffjm").await.is_err());
        assert!(t.history().await.is_err());
    }

    /// Every read Tdarr takes with POST passes with the body the adapter sends, and the same
    /// path with any other body is refused before anything leaves.
    #[tokio::test]
    async fn each_read_passes_with_its_own_body_and_no_other() {
        let recorded = Recorded::new();
        recorded.answer(Method::Post, BASE, 200, "{}");
        let out = out(recorded.clone());
        let ask = |path: &str, body: &Value| {
            OutRequest::post(format!("{BASE}{path}"), Body::Json(body.to_string()))
        };
        for (path, body) in [
            (
                TABLES_PATH,
                json!({"data": {"start": 0, "pageSize": 5, "filters": [], "sorts": [], "opts": {"table": "table4"}}}),
            ),
            (REPORTS_PATH, json!({"data": {"footprintId": "QEdffjm"}})),
            (
                REPORT_PATH,
                json!({"data": {"footprintId": "F", "jobId": "J", "jobFileId": "F()1()transcode()J()1.txt"}}),
            ),
            (SAVED_PATH, json!({"data": {}})),
            (VERDICTS_PATH, json!({"data": {}})),
            (RESOURCES_PATH, json!({"data": {}})),
        ] {
            assert!(out.send(ask(path, &body)).await.is_ok(), "{path} {body}");
        }
        let before = recorded.calls_sent().len();
        for (path, body) in [
            (TABLES_PATH, json!({"data": {"opts": {"table": "table7"}}})),
            (
                TABLES_PATH,
                json!({"data": {"opts": {"table": "table1", "updatedObj": {"bumped": true}}}}),
            ),
            (
                TABLES_PATH,
                json!({"data": {"opts": {"table": "table1", "setAll": true}}}),
            ),
            (TABLES_PATH, json!({"data": {"start": 0}})),
            (REPORTS_PATH, json!({"data": {}})),
            (
                REPORTS_PATH,
                json!({"data": {"footprintId": "F", "remove": true}}),
            ),
            (
                REPORT_PATH,
                json!({"data": {"footprintId": "F", "jobId": "J"}}),
            ),
            (SAVED_PATH, json!({"data": {"libraryId": "lib"}})),
            (VERDICTS_PATH, json!({})),
            (RESOURCES_PATH, json!({"data": {"reset": true}})),
        ] {
            let refused = out.send(ask(path, &body)).await;
            assert!(
                matches!(refused, Err(OutboundError::NotListed { .. })),
                "{path} {body}: {refused:?}"
            );
        }
        assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");
    }

    /// Every act goes in its one form, and only while the access is `act`: refused before
    /// anything leaves under `read`, sent and answered under `act`, and turned back again the
    /// moment the access is taken back.
    #[tokio::test]
    async fn each_act_goes_in_its_one_form_and_only_with_the_access() {
        let recorded = Recorded::new();
        recorded.answer(Method::Post, BASE, 200, r#""OK""#);
        recorded.answer(
            Method::Post,
            CANCEL_PATH,
            400,
            r#""Node missing, disconnected, or did not acknowledge""#,
        );
        let t = tdarr(recorded.clone());
        let every = [
            WorkerAct::Limit {
                node: "TNUn37R".into(),
                kind: WorkerKind::TranscodeGpu,
                up: true,
            },
            WorkerAct::Limit {
                node: "TNUn37R".into(),
                kind: WorkerKind::HealthCpu,
                up: false,
            },
            WorkerAct::Pause {
                node: "TNUn37R".into(),
                paused: true,
            },
            WorkerAct::Cancel {
                node: "TNUn37R".into(),
                worker: "yearly-yak".into(),
            },
            WorkerAct::Requeue {
                path: KNOWN_FILE.into(),
            },
            WorkerAct::Scan {
                library: "HushShowHEVC_1080".into(),
                folder: "/media/hush/queue/shows/hevc_1080p".into(),
            },
            WorkerAct::Watch {
                library: "HushShowHEVC_1080".into(),
                folder: "/media/hush/queue/shows/hevc_1080p".into(),
                on: false,
            },
            WorkerAct::Flow {
                library: "HushShowHEVC_1080".into(),
                flow: "hush_tv_hevc_1080_v1".into(),
            },
        ];
        for act in &every {
            let refused = t.act(act).await.unwrap_err();
            assert!(
                matches!(
                    refused,
                    EngineError::Outbound(OutboundError::NotAllowed { .. })
                ),
                "{act:?}: {refused:?}"
            );
        }
        assert!(recorded.calls_sent().is_empty(), "nothing left under read");
        let switch = t.out.allowlist().switch("tdarr").unwrap();
        switch.set(crate::core::outbound::Access::Act);
        let mut answers = Vec::new();
        for act in &every {
            answers.push(t.act(act).await.unwrap());
        }
        // The worker says no in its own words; every other act was taken.
        assert_eq!(
            answers[3],
            Acted::Refused("Node missing, disconnected, or did not acknowledge".to_string())
        );
        assert!(
            answers
                .iter()
                .enumerate()
                .all(|(i, a)| i == 3 || *a == Acted::Done)
        );
        let sent: Vec<(String, Value)> = recorded
            .calls_sent()
            .iter()
            .map(|c| {
                let Body::Json(text) = &c.body else {
                    panic!("{:?}", c.body)
                };
                (
                    c.url.trim_start_matches(BASE).to_string(),
                    serde_json::from_str(text).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            sent,
            vec![
                (
                    LIMIT_PATH.to_string(),
                    json!({"data": {"nodeID": "TNUn37R", "process": "increase", "workerType": "transcodegpu"}})
                ),
                (
                    LIMIT_PATH.to_string(),
                    json!({"data": {"nodeID": "TNUn37R", "process": "decrease", "workerType": "healthcheckcpu"}})
                ),
                (
                    NODE_PATH.to_string(),
                    json!({"data": {"nodeID": "TNUn37R", "nodeUpdates": {"nodePaused": true}}})
                ),
                (
                    CANCEL_PATH.to_string(),
                    json!({"data": {"nodeID": "TNUn37R", "workerID": "yearly-yak", "cause": "user"}})
                ),
                (
                    REQUEUE_PATH.to_string(),
                    json!({"data": {"fileIds": [KNOWN_FILE], "updatedObj": {"TranscodeDecisionMaker": "Queued"}}})
                ),
                (
                    SCAN_PATH.to_string(),
                    json!({"data": {"scanConfig": {"dbID": "HushShowHEVC_1080", "mode": "scanFindNew", "arrayOrPath": "/media/hush/queue/shows/hevc_1080p"}}})
                ),
                (
                    WATCH_PATH.to_string(),
                    json!({"data": {"auto": false, "folder": "/media/hush/queue/shows/hevc_1080p", "dbID": "HushShowHEVC_1080", "status": false}})
                ),
                (
                    RECORDS_PATH.to_string(),
                    json!({"data": {"collection": "LibrarySettingsJSONDB", "mode": "update", "docID": "HushShowHEVC_1080", "obj": {"flowId": "hush_tv_hevc_1080_v1"}}})
                ),
            ]
        );
        // A failure that is not the worker's no is an error.
        recorded.answer(Method::Post, &format!("{BASE}{SCAN_PATH}"), 500, "");
        assert!(t.act(&every[5]).await.is_err());
        // Taken back: refused again with the next call.
        switch.set(crate::core::outbound::Access::Read);
        let before = recorded.calls_sent().len();
        assert!(t.act(&every[0]).await.is_err());
        assert_eq!(recorded.calls_sent().len(), before);
    }

    /// Under `act` an act path passes in its form alone, and the paths that delete, restart,
    /// remove, upload or administer are never declared: refused with any method and any
    /// body, GET included where a GET changes something.
    #[tokio::test]
    async fn acting_never_reaches_what_deletes_restarts_or_administers() {
        let recorded = Recorded::new();
        recorded.answer(Method::Post, BASE, 200, r#""OK""#);
        recorded.answer(Method::Get, BASE, 200, "{}");
        let out = out(recorded.clone());
        out.allowlist()
            .switch("tdarr")
            .unwrap()
            .set(crate::core::outbound::Access::Act);
        let at = |path: &str| format!("{BASE}{path}");
        let post =
            |path: &str, body: Value| OutRequest::post(at(path), Body::Json(body.to_string()));
        // The same paths in other forms: more than the pause on a node, two files, other
        // fields set on a file, a fresh scan, a watch switched as if by the worker itself, the
        // record store's other collections and modes, a library update beyond its flow.
        for (path, body) in [
            (
                NODE_PATH,
                json!({"data": {"nodeID": "n", "nodeUpdates": {"nodePaused": true, "workerLimits": {}}}}),
            ),
            (
                NODE_PATH,
                json!({"data": {"nodeID": "n", "nodeUpdates": {"nodePaused": "yes"}}}),
            ),
            (
                REQUEUE_PATH,
                json!({"data": {"fileIds": ["/a", "/b"], "updatedObj": {"TranscodeDecisionMaker": "Queued"}}}),
            ),
            (
                REQUEUE_PATH,
                json!({"data": {"fileIds": ["/a"], "updatedObj": {"TranscodeDecisionMaker": "Transcode success"}}}),
            ),
            (
                REQUEUE_PATH,
                json!({"data": {"fileIds": ["/a"], "updatedObj": {"TranscodeDecisionMaker": "Queued", "file": "/b"}}}),
            ),
            (
                SCAN_PATH,
                json!({"data": {"scanConfig": {"dbID": "l", "mode": "scanFresh", "arrayOrPath": "/q"}}}),
            ),
            (
                WATCH_PATH,
                json!({"data": {"auto": true, "folder": "/q", "dbID": "l", "status": true}}),
            ),
            (
                LIMIT_PATH,
                json!({"data": {"nodeID": "n", "process": "set", "workerType": "transcodegpu"}}),
            ),
            (
                CANCEL_PATH,
                json!({"data": {"nodeID": "n", "workerID": "w", "cause": "other"}}),
            ),
            (
                RECORDS_PATH,
                json!({"data": {"collection": "LibrarySettingsJSONDB", "mode": "removeOne", "docID": "l"}}),
            ),
            (
                RECORDS_PATH,
                json!({"data": {"collection": "FileJSONDB", "mode": "update", "docID": "/a", "obj": {"flowId": "f"}}}),
            ),
            (
                RECORDS_PATH,
                json!({"data": {"collection": "LibrarySettingsJSONDB", "mode": "update", "docID": "l", "obj": {"flowId": "f", "folder": "/"}}}),
            ),
            (
                RECORDS_PATH,
                json!({"data": {"collection": "UsersJSONDB", "mode": "getAll"}}),
            ),
        ] {
            let refused = out.send(post(path, body.clone())).await;
            assert!(
                matches!(refused, Err(OutboundError::NotListed { .. })),
                "{path} {body}: {refused:?}"
            );
        }
        // What is never declared, whatever the method and the body.
        for path in [
            "/api/v2/delete-file",
            "/api/v2/bulk-delete-files",
            "/api/v2/delete-cache-file",
            "/api/v2/delete-unhealthy-files",
            "/api/v2/delete-job-report",
            "/api/v2/delete-job-reports",
            "/api/v2/delete-backup",
            "/api/v2/delete-plugin",
            "/api/v2/remove-library-files",
            "/api/v2/restart-server",
            "/api/v2/restart-node",
            "/api/v2/restart-ui",
            "/api/v2/stop-dedupe",
            "/api/v2/kill-worker",
            "/api/v2/disconnect-node",
            "/api/v2/set-all-status",
            "/api/v2/file/upload",
            "/api/v2/updater/relaunch",
            "/api/v2/stats/reset-job-statistics",
            "/api/v2/restore-config-history",
            "/api/v2/admin/register",
            "/api/v2/admin/reset-password",
            "/api/v2/admin/sso",
            "/api/v2/admin/sso/google",
            "/api/v2/admin/login-methods",
        ] {
            let bodies = [
                json!({"data": {}}),
                json!({"data": {"nodeID": "n", "nodeUpdates": {"nodePaused": true}}}),
                json!({"data": {"fileIds": ["/a"], "updatedObj": {"TranscodeDecisionMaker": "Queued"}}}),
            ];
            for body in bodies {
                for method in [Method::Post, Method::Put, Method::Delete] {
                    let req = OutRequest {
                        method,
                        url: at(path),
                        headers: Vec::new(),
                        body: Body::Json(body.to_string()),
                        timeout: None,
                    };
                    let refused = out.send(req).await;
                    assert!(
                        matches!(refused, Err(OutboundError::NotListed { .. })),
                        "{} {path}: {refused:?}",
                        method.as_str()
                    );
                }
            }
        }
        // A GET that changes something is no read: refused like any write.
        for path in [
            "/api/v2/restart-ui",
            "/api/v2/stop-dedupe",
            "/api/v2/admin/sso",
        ] {
            let refused = out.send(OutRequest::get(at(path))).await;
            assert!(
                matches!(refused, Err(OutboundError::NotListed { .. })),
                "GET {path}: {refused:?}"
            );
        }
        assert!(recorded.calls_sent().is_empty(), "nothing was sent");
        // A GET that reads still passes, also one that only shares a beginning with a barred
        // path.
        for path in ["/api/v2/get-nodes", "/api/v2/admin", "/api/v2/restart-uix"] {
            assert!(out.send(OutRequest::get(at(path))).await.is_ok(), "{path}");
        }
    }

    /// The record store has one path for reading and for writing: only its reading modes are
    /// listed, and nothing else that is not a GET reaches the worker.
    #[tokio::test]
    async fn nothing_but_its_two_reads_is_sent_with_post() {
        let recorded = records();
        let out = out(recorded.clone());
        let ask = |path: &str, body: Value| {
            OutRequest::post(format!("{BASE}{path}"), Body::Json(body.to_string()))
        };
        for mode in ["insert", "update", "removeOne", "removeAll", ""] {
            for collection in [FILES, LIBRARIES, FLOWS, STATISTICS] {
                let refused = out
                    .send(ask(
                        RECORDS_PATH,
                        json!({"data": {"collection": collection, "mode": mode, "docID": "/q/a.mkv"}}),
                    ))
                    .await;
                assert!(
                    matches!(refused, Err(OutboundError::NotListed { .. })),
                    "{mode} {collection}: {refused:?}"
                );
            }
        }
        for path in [
            "/api/v2/delete-file",
            "/api/v2/scan-files",
            "/api/v2/update-node",
            "/api/v2/cruddb/",
            "/api/v2/client/status-tables/1",
            STATUS_PATH,
        ] {
            let refused = out
                .send(ask(path, json!({"data": {"mode": "getAll"}})))
                .await;
            assert!(
                matches!(refused, Err(OutboundError::NotListed { .. })),
                "{path}: {refused:?}"
            );
        }
        for request in [
            OutRequest::put(format!("{BASE}{RECORDS_PATH}"), Body::Json("{}".into())),
            OutRequest::delete(format!("{BASE}{RECORDS_PATH}")),
        ] {
            assert!(out.send(request).await.is_err());
        }
        assert!(recorded.seen.lock().unwrap().is_empty(), "nothing was sent");
        // The worker's acts are listed, and its access is `read` until the owner allows more.
        assert_eq!(calls().acts.len(), ACTIONS.len());
        let switch = out
            .allowlist()
            .switch("tdarr")
            .expect("the worker has acts");
        assert_eq!(switch.get(), crate::core::outbound::Access::Read);
        // The reads pass.
        assert!(
            out.send(ask(
                RECORDS_PATH,
                json!({"data": {"collection": STATISTICS, "mode": "getAll"}})
            ))
            .await
            .is_ok()
        );
        assert_eq!(recorded.methods_seen(), vec![Method::Post]);
    }
}
