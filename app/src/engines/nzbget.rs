//! The NZBGet adapter (its JSON-RPC), read-only, behind [`DownloadClient`].
//!
//! Three calls, each a `POST /jsonrpc` with a method in its body: `listgroups` (the queue),
//! `history` (what left the queue: completed, failed, deleted) and `status` (the download
//! rate: a job's speed and time left come from it). A user name and password, when set, go as
//! HTTP basic authentication.
//!
//! NZBGet takes every method on that one path. It takes them as a GET of `/jsonrpc/<method>`
//! too. Its host takes the three reads alone and no GET at all ([`calls`]).
//!
//! A manager that grabs a release with NZBGet gives the job a `drone` parameter and names the
//! download by it. A job's download id is its `drone`. A job added by hand has none: its
//! download id is its `NZBID`.
//!
//! The history is read over the window of [`HISTORY_DAYS`](super::clients::HISTORY_DAYS)
//! days. A job still fetching its NZB from a link is left out. It holds no download yet. Its
//! name is the link.

use super::access::Action;
use super::clients::{history_window, safe_name, text, unix};
use super::{ClientDownload, ClientListing, DownloadClient, DownloadStatus, EngineError, Protocol};
use crate::core::config::DownloadClientConfig;
use crate::core::outbound::{
    Body, BodyRule, Calls, Method, OutRequest, Outbound, basic_auth, expect_ok,
};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

/// The type of the integration.
pub const KIND: &str = "nzbget";
/// Where the JSON-RPC answers, below the address.
pub const RPC_PATH: &str = "/jsonrpc";
/// The methods sent. Each only reads.
pub const METHODS: &[&str] = &["listgroups", "history", "status"];
/// History jobs kept each pass, newest first. When the window holds more, its edge is the
/// oldest job kept.
pub const HISTORY_LIMIT: usize = 100;

/// What Singularr can do to NZBGet beyond reading it: nothing.
pub const ACTIONS: &[Action] = &[];

/// The parameter a manager names its downloads by.
const DRONE: &str = "drone";

/// The calls NZBGet is sent: its JSON-RPC with one of the three reading methods, and no GET.
/// Appending, editing the queue, pausing, saving the configuration or shutting down through
/// the same path is refused. So is a method named in a GET's path.
///
/// NZBGet finds the method by searching the body's text for the first `"method"`. A body
/// passes with `method`, `params` and `id` alone, its parameters plain values, `"method"`
/// written once, and no escape: the method NZBGet finds is then the one the rule read.
pub fn calls() -> Calls {
    Calls::default()
        .read_with(
            Method::Post,
            RPC_PATH,
            BodyRule::new()
                .one_of("/method", METHODS)
                .only("", &["method", "params", "id"])
                .scalars("/params", 2)
                .written_once("\"method\"")
                .plain(),
        )
        .no_get()
}

pub struct Nzbget {
    id: String,
    base: String,
    user: Option<String>,
    pass: Option<String>,
    out: Outbound,
    /// The clock the history's window is measured by.
    now: fn() -> DateTime<Utc>,
}

/// A 64-bit size NZBGet gives as two 32-bit halves: `FileSizeHi` and `FileSizeLo`.
fn size(v: &Value, name: &str) -> i64 {
    let half = |part: &str| {
        v.get(format!("{name}{part}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    i64::try_from((half("Hi") << 32) | half("Lo")).unwrap_or(i64::MAX)
}

/// The download id of a job: its `drone` parameter, else its `NZBID`, upper case.
fn download_id(v: &Value, nzb_id: &str) -> String {
    v["Parameters"]
        .as_array()
        .and_then(|params| {
            params
                .iter()
                .find(|p| p["Name"].as_str() == Some(DRONE))
                .and_then(|p| text(p.get("Value")))
        })
        .unwrap_or_else(|| nzb_id.to_string())
        .to_ascii_uppercase()
}

/// Where a queue job stands, from NZBGet's word for it.
pub fn queue_status(word: &str) -> DownloadStatus {
    match word {
        "DOWNLOADING" => DownloadStatus::Downloading,
        "PAUSED" => DownloadStatus::Paused,
        "LOADING_PARS" | "VERIFYING_SOURCES" | "REPAIRING" | "VERIFYING_REPAIRED" => {
            DownloadStatus::Checking
        }
        "PP_QUEUED"
        | "RENAMING"
        | "UNPACKING"
        | "MOVING"
        | "EXECUTING_SCRIPT"
        | "POST_UNPACK_RENAMING"
        | "PP_FINISHED" => DownloadStatus::Unpacking,
        // Waiting its turn, or fetching its NZB from a URL.
        _ => DownloadStatus::Queued,
    }
}

/// Where a history job stands, from the first part of NZBGet's status: `SUCCESS/ALL`.
pub fn history_status(word: &str) -> DownloadStatus {
    match word.split('/').next().unwrap_or_default() {
        "SUCCESS" | "WARNING" => DownloadStatus::Completed,
        _ => DownloadStatus::Failed,
    }
}

/// The parts of a job both lists share.
fn job(v: &Value, status: DownloadStatus, word: String) -> Option<ClientDownload> {
    let nzb_id = v.get("NZBID").and_then(Value::as_i64)?.to_string();
    Some(ClientDownload {
        download_id: download_id(v, &nzb_id),
        id: nzb_id,
        protocol: Protocol::Usenet,
        name: safe_name(
            &text(v.get("NZBName"))
                .or_else(|| text(v.get("Name")))
                .unwrap_or_default(),
        ),
        category: text(v.get("Category")),
        status,
        state: word,
        progress: 0.0,
        size_bytes: size(v, "FileSize"),
        downloaded_bytes: None,
        added_at: None,
        completed_at: None,
        save_path: text(v.get("FinalDir")).or_else(|| text(v.get("DestDir"))),
        speed: None,
        upload_speed: None,
        eta: None,
        tracker: None,
        seeds: None,
        peers: None,
        ratio: None,
        seeding_seconds: 0,
    })
}

/// One job of the queue. `rate` is NZBGet's download rate, given to the job that downloads
/// when it is the only one. A job still fetching its NZB from a link (`Kind` `URL`) is none
/// yet.
fn queue_job(v: &Value, rate: Option<i64>) -> Option<ClientDownload> {
    if v.get("Kind").and_then(Value::as_str) == Some("URL") {
        return None;
    }
    let word = text(v.get("Status")).unwrap_or_default();
    let status = queue_status(&word);
    let mut d = job(v, status, word)?;
    let left = size(v, "RemainingSize").min(d.size_bytes);
    let done = (d.size_bytes - left).max(0);
    d.downloaded_bytes = Some(done);
    // Past the download, the job holds all it needs; what is left are par2 files kept for a
    // repair.
    d.progress = match status {
        DownloadStatus::Checking | DownloadStatus::Unpacking => 1.0,
        _ if d.size_bytes > 0 => done as f64 / d.size_bytes as f64,
        _ => 0.0,
    };
    if status == DownloadStatus::Downloading
        && let Some(rate) = rate.filter(|r| *r > 0)
    {
        d.speed = Some(i32::try_from(rate).unwrap_or(i32::MAX));
        d.eta = Some(i32::try_from(left / rate).unwrap_or(i32::MAX));
    }
    Some(d)
}

/// One job of the history. A job deleted by hand is gone for good, as the managers read it,
/// and is left out.
fn history_job(v: &Value) -> Option<ClientDownload> {
    let word = text(v.get("Status")).unwrap_or_default();
    if word == "DELETED/MANUAL"
        || v.get("Kind")
            .and_then(Value::as_str)
            .is_some_and(|k| k != "NZB")
    {
        return None;
    }
    let status = history_status(&word);
    let mut d = job(v, status, word)?;
    // A job in the history is past its download, completed or failed. What arrived of a
    // failed one is `downloaded_bytes`.
    d.progress = 1.0;
    d.downloaded_bytes = Some(size(v, "DownloadedSize").min(d.size_bytes));
    d.completed_at = unix(v.get("HistoryTime").and_then(Value::as_i64));
    Some(d)
}

/// The jobs of the three answers' results: the queue's, then those of the history's window.
/// NZBGet gives its whole history.
pub fn jobs(groups: &Value, history: &Value, status: &Value, now: DateTime<Utc>) -> ClientListing {
    let groups = groups.as_array().map(Vec::as_slice).unwrap_or_default();
    let downloading = groups
        .iter()
        .filter(|g| g["Status"].as_str() == Some("DOWNLOADING"))
        .count();
    let rate = status
        .get("DownloadRate")
        .and_then(Value::as_i64)
        .filter(|_| downloading == 1 && status["DownloadPaused"].as_bool() != Some(true));
    let mut out: Vec<ClientDownload> = groups.iter().filter_map(|g| queue_job(g, rate)).collect();
    let (done, edge) = history_window(
        history
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(history_job)
            .collect(),
        now,
        HISTORY_LIMIT,
        false,
    );
    for d in done {
        if !out.iter().any(|o| o.id == d.id) {
            out.push(d);
        }
    }
    ClientListing {
        downloads: out,
        finished_since: Some(edge),
    }
}

impl Nzbget {
    /// `None` when no URL is configured.
    pub fn new(config: &DownloadClientConfig, out: Outbound) -> Option<Self> {
        Some(Nzbget {
            id: config.id.clone(),
            base: config.url.clone()?,
            user: config.user.clone(),
            pass: config.secret.clone(),
            out,
            now: Utc::now,
        })
    }

    /// One method's result.
    async fn call(&self, method: &str, params: Value) -> Result<Value, EngineError> {
        let url = format!("{}{RPC_PATH}", self.base);
        let body = json!({"method": method, "params": params, "id": 1});
        let mut req = OutRequest::post(&url, Body::Json(body.to_string()));
        if self.user.is_some() || self.pass.is_some() {
            req = req.header(
                "Authorization",
                basic_auth(
                    self.user.as_deref().unwrap_or_default(),
                    self.pass.as_deref().unwrap_or_default(),
                ),
            );
        }
        let res = self.out.send(req).await?;
        if res.status == 401 || res.status == 403 {
            return Err(EngineError::Login {
                engine: self.id.clone(),
                message: "the user name or password is wrong".to_string(),
            });
        }
        expect_ok(&url, &res)?;
        let mut answer: Value = res.json()?;
        if let Some(error) = answer.get("error").filter(|e| !e.is_null()) {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("an error")
                .to_string();
            return Err(EngineError::Decode {
                engine: self.id.clone(),
                what: "answer",
                message: format!("{method} answered {message}"),
            });
        }
        Ok(answer
            .get_mut("result")
            .map(Value::take)
            .unwrap_or(Value::Null))
    }
}

#[async_trait]
impl DownloadClient for Nzbget {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        KIND
    }

    fn protocol(&self) -> Protocol {
        Protocol::Usenet
    }

    async fn downloads(&self) -> Result<Vec<ClientDownload>, EngineError> {
        Ok(self.listing().await?.downloads)
    }

    async fn listing(&self) -> Result<ClientListing, EngineError> {
        let groups = self.call("listgroups", json!([0])).await?;
        let history = self.call("history", json!([false])).await?;
        let status = self.call("status", json!([])).await?;
        if !groups.is_array() || !history.is_array() {
            return Err(EngineError::Decode {
                engine: self.id.clone(),
                what: "queue",
                message: "the queue or the history is no list".to_string(),
            });
        }
        Ok(jobs(&groups, &history, &status, (self.now)()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClientKind;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, OutboundError};
    use std::sync::Arc;

    const BASE: &str = "http://192.0.2.67:6789";

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/nzbget/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn allow() -> Allowlist {
        let mut allow = Allowlist::default();
        allow.allow_integration(KIND, BASE, calls()).unwrap();
        allow
    }

    fn client(recorded: Arc<Recorded>) -> Nzbget {
        let config = DownloadClientConfig {
            id: KIND.to_string(),
            kind: ClientKind::Nzbget,
            url: Some(BASE.to_string()),
            user: Some("fxuser".to_string()),
            secret: Some("fxpass".to_string()),
        };
        let mut client = Nzbget::new(&config, Outbound::with_transport(allow(), recorded)).unwrap();
        client.now = captured;
        client
    }

    /// An hour after the fixtures were captured.
    fn captured() -> DateTime<Utc> {
        DateTime::from_timestamp(1_795_584_274, 0).unwrap()
    }

    fn recorded() -> Arc<Recorded> {
        let recorded = Recorded::new();
        for (method, file) in [
            ("listgroups", "listgroups.json"),
            ("history", "history.json"),
            ("status", "status.json"),
        ] {
            recorded.answer_body(
                Method::Post,
                RPC_PATH,
                &format!("\"{method}\""),
                200,
                &fixture(file),
            );
        }
        recorded
    }

    #[tokio::test]
    async fn it_lists_the_queue_then_the_history() {
        let recorded = recorded();
        let all = client(recorded.clone()).downloads().await.unwrap();
        let names: Vec<(&str, DownloadStatus, &str)> = all
            .iter()
            .map(|d| (d.name.as_str(), d.status, d.state.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                (
                    "Tears.of.Steel.S01E01.1080p.WEB-DL.x264-FX",
                    DownloadStatus::Paused,
                    "PAUSED"
                ),
                (
                    "Sintel.2010.1080p.BluRay.x264-FX",
                    DownloadStatus::Paused,
                    "PAUSED"
                ),
                (
                    "Tears.of.Steel.S01E03.1080p.WEB-DL.x264-FX",
                    DownloadStatus::Downloading,
                    "DOWNLOADING"
                ),
                (
                    "Tears.of.Steel.S01E03.1080p.WEB-DL.x264-FX",
                    DownloadStatus::Failed,
                    "DELETED/COPY"
                ),
                (
                    "Sintel.2010.720p.BluRay.x264-FX",
                    DownloadStatus::Completed,
                    "SUCCESS/HEALTH"
                ),
                (
                    "Tears.of.Steel.S01E02.1080p.WEB-DL.x264-FX",
                    DownloadStatus::Completed,
                    "SUCCESS/HEALTH"
                ),
                (
                    "Cosmos.Laundromat.2015.720p.WEB-DL.x264-FX",
                    DownloadStatus::Failed,
                    "FAILURE/HEALTH"
                ),
            ]
        );
        // A manager's job is named by its drone parameter; a job added by hand by its NZBID.
        let show = &all[0];
        assert_eq!(
            (show.id.as_str(), show.download_id.as_str()),
            ("5", "3D5C0A8BE2F14C6E9B7A0D4F61E2C8A1")
        );
        assert_eq!(show.category.as_deref(), Some("Series"));
        assert_eq!(show.protocol, Protocol::Usenet);
        let film = &all[1];
        assert_eq!((film.id.as_str(), film.download_id.as_str()), ("6", "6"));
        let moving = &all[2];
        assert_eq!(moving.size_bytes, 6_291_456);
        assert_eq!(moving.downloaded_bytes, Some(6_000_000));
        assert!(
            moving.progress > 0.95 && moving.progress < 0.96,
            "{}",
            moving.progress
        );
        assert_eq!(
            moving.speed,
            Some(242_318),
            "NZBGet's rate, for its one download"
        );
        assert_eq!(moving.eta, Some(1));
        let done = &all[4];
        assert_eq!((done.progress, done.size_bytes), (1.0, 2_621_440));
        assert!(done.completed_at.is_some());
        assert_eq!(
            done.save_path.as_deref(),
            Some("/downloads/completed/Movies/Sintel.2010.720p.BluRay.x264-FX")
        );
        // A failed job is past its download too; what arrived of it is its downloaded bytes.
        let failed = &all[6];
        assert_eq!(failed.download_id, "C0FFEE11D2E34A5B9C8D7E6F5A4B3C21");
        assert_eq!((failed.progress, failed.downloaded_bytes), (1.0, Some(97)));
        // Three POSTs with basic authentication, one per method.
        let sent = recorded.calls_sent();
        assert_eq!(sent.len(), 3);
        for call in &sent {
            assert_eq!(call.method, Method::Post);
            assert!(
                call.headers
                    .iter()
                    .any(|(n, v)| n == "Authorization" && v == "Basic Znh1c2VyOmZ4cGFzcw==")
            );
        }
    }

    /// A job still fetching its NZB from a link is left out: its name is the link, key and all.
    /// The history is read over its window. The edge is the window's start, or the oldest job
    /// kept.
    #[test]
    fn a_fetching_job_is_left_out_and_the_history_has_an_edge() {
        let mut groups: Value = serde_json::from_str(&fixture("listgroups.json")).unwrap();
        groups["result"][0]["Kind"] = json!("URL");
        groups["result"][0]["Status"] = json!("FETCHING");
        groups["result"][0]["NZBName"] = json!("https://indexer.example/api?t=get&id=7&apikey=K3Y");
        let history: Value = serde_json::from_str(&fixture("history.json")).unwrap();
        let status: Value = serde_json::from_str(&fixture("status.json")).unwrap();
        let listing = jobs(
            &groups["result"],
            &history["result"],
            &status["result"],
            captured(),
        );
        assert_eq!(listing.downloads.len(), 6);
        assert!(listing.downloads.iter().all(|d| !d.name.contains("K3Y")));
        let start = captured() - chrono::TimeDelta::days(7);
        assert_eq!(listing.finished_since, Some(start));
        // Eight days on, the history's jobs are past the window.
        let later = captured() + chrono::TimeDelta::days(8);
        let listing = jobs(
            &groups["result"],
            &history["result"],
            &status["result"],
            later,
        );
        assert_eq!(listing.downloads.len(), 2);
    }

    #[tokio::test]
    async fn a_refused_sign_in_and_an_error_answer_are_errors() {
        let recorded = Recorded::new();
        recorded.answer(Method::Post, RPC_PATH, 401, "");
        let err = client(recorded).downloads().await.unwrap_err();
        assert!(matches!(err, EngineError::Login { .. }), "{err}");
        let recorded = Recorded::new();
        recorded.answer(
            Method::Post,
            RPC_PATH,
            200,
            r#"{"version": "1.1", "id": 1, "error": {"name": "JSONRPCError", "code": 1, "message": "Access denied"}}"#,
        );
        let err = client(recorded).downloads().await.unwrap_err();
        assert!(err.to_string().contains("Access denied"), "{err}");
    }

    #[test]
    fn every_word_has_a_status() {
        for (word, status) in [
            ("QUEUED", DownloadStatus::Queued),
            ("FETCHING", DownloadStatus::Queued),
            ("PAUSED", DownloadStatus::Paused),
            ("DOWNLOADING", DownloadStatus::Downloading),
            ("LOADING_PARS", DownloadStatus::Checking),
            ("VERIFYING_SOURCES", DownloadStatus::Checking),
            ("REPAIRING", DownloadStatus::Checking),
            ("VERIFYING_REPAIRED", DownloadStatus::Checking),
            ("PP_QUEUED", DownloadStatus::Unpacking),
            ("RENAMING", DownloadStatus::Unpacking),
            ("UNPACKING", DownloadStatus::Unpacking),
            ("MOVING", DownloadStatus::Unpacking),
            ("EXECUTING_SCRIPT", DownloadStatus::Unpacking),
            ("POST_UNPACK_RENAMING", DownloadStatus::Unpacking),
            ("PP_FINISHED", DownloadStatus::Unpacking),
        ] {
            assert_eq!(queue_status(word), status, "{word}");
        }
        for (word, status) in [
            ("SUCCESS/ALL", DownloadStatus::Completed),
            ("SUCCESS/HEALTH", DownloadStatus::Completed),
            ("WARNING/SCRIPT", DownloadStatus::Completed),
            ("FAILURE/PAR", DownloadStatus::Failed),
            ("FAILURE/HEALTH", DownloadStatus::Failed),
            ("DELETED/DUPE", DownloadStatus::Failed),
            ("DELETED/COPY", DownloadStatus::Failed),
        ] {
            assert_eq!(history_status(word), status, "{word}");
        }
        // A job deleted by hand, and anything but an NZB, is left out.
        let manual = json!({"NZBID": 9, "Name": "x", "Status": "DELETED/MANUAL", "Kind": "NZB"});
        assert!(history_job(&manual).is_none());
        let url = json!({"NZBID": 9, "Name": "x", "Status": "FAILURE/FETCH", "Kind": "URL"});
        assert!(history_job(&url).is_none());
        // Post-processing, as NZBGet lists it in its queue: the data is all there.
        let groups: Value =
            serde_json::from_str(&fixture("listgroups_post_processing.json")).unwrap();
        let moving: Vec<ClientDownload> = groups["result"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|g| queue_job(g, None))
            .filter(|d| d.status == DownloadStatus::Unpacking)
            .collect();
        assert_eq!(moving.len(), 2, "MOVING and PP_QUEUED");
        assert!(
            moving
                .iter()
                .all(|d| d.progress == 1.0 && d.speed.is_none())
        );
        assert_eq!(
            size(&json!({"FileSizeHi": 1, "FileSizeLo": 5}), "FileSize"),
            (1 << 32) + 5
        );
    }

    /// Only the three reading methods pass, and no GET at all.
    #[test]
    fn the_rpc_passes_with_its_three_reads_alone_and_no_get() {
        let a = allow();
        let rpc = format!("{BASE}{RPC_PATH}");
        let body = |method: &str| Body::Json(json!({"method": method, "params": []}).to_string());
        for method in METHODS {
            assert!(
                a.check_call(Method::Post, &rpc, &body(method)).is_ok(),
                "{method}"
            );
        }
        for method in [
            "append",
            "editqueue",
            "shutdown",
            "reload",
            "saveconfig",
            "pausedownload",
            "resumedownload",
            "scan",
            "rate",
            "writelog",
            "version",
        ] {
            assert!(
                matches!(
                    a.check_call(Method::Post, &rpc, &body(method)),
                    Err(OutboundError::NotListed { .. })
                ),
                "{method}"
            );
        }
        for url in [
            rpc.clone(),
            format!("{rpc}/shutdown"),
            format!("{rpc}/listgroups"),
            format!("{BASE}/jsonprpc/shutdown"),
            format!("{BASE}/user:pass/jsonrpc/shutdown"),
        ] {
            assert!(a.check(Method::Get, &url).is_err(), "{url}");
        }
        // The body the adapter sends passes. A body where NZBGet would find another method
        // than the rule reads is refused: a method written twice, a "method" among the
        // parameters, an escaped name, a nested object, a member beside the three.
        let raw = |text: &str| Body::Json(text.to_string());
        assert!(
            a.check_call(
                Method::Post,
                &rpc,
                &raw(r#"{"id":1,"method":"listgroups","params":[0]}"#)
            )
            .is_ok()
        );
        for text in [
            r#"{"method": "shutdown", "method": "listgroups", "params": []}"#,
            r#"{"params": ["method", "shutdown"], "method": "listgroups"}"#,
            r#"{"\u006dethod": "listgroups", "params": ["method", "shutdown"]}"#,
            r#"{"method": "listgroups", "params": [{"method": "shutdown"}]}"#,
            r#"{"method": "listgroups", "params": [[0]]}"#,
            r#"{"method": "listgroups", "params": [], "version": "1.1"}"#,
            r#"{"method": "listgroups"}"#,
        ] {
            assert!(
                matches!(
                    a.check_call(Method::Post, &rpc, &raw(text)),
                    Err(OutboundError::NotListed { .. })
                ),
                "{text}"
            );
        }
        assert!(
            a.check_call(Method::Post, &format!("{BASE}/xmlrpc"), &body("listgroups"))
                .is_err()
        );
        assert!(ACTIONS.is_empty());
    }
}
