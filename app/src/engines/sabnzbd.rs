//! The SABnzbd adapter (its API), read-only, behind [`DownloadClient`].
//!
//! Two calls, both a GET of `/api`: `mode=queue` (every job waiting or downloading) and
//! `mode=history` with a limit (the newest jobs that finished downloading: being verified,
//! unpacked or moved, completed or failed). The managers import from the history. A job is
//! followed from the queue into it, over the window of
//! [`HISTORY_DAYS`](super::clients::HISTORY_DAYS) days. A job still fetching its NZB is left
//! out. It holds no download yet. Its name is the link it is fetched from.
//!
//! SABnzbd takes its commands as GETs of the same path. A key beside `mode` turns a read into a
//! change (`mode=queue&name=delete`). Its host takes the two reads alone, each with no key but
//! `output`, `apikey` and `limit` ([`calls`]).
//!
//! The key travels in the query string, as SABnzbd wants it. It never reaches a log line or an
//! error. `core::outbound` logs the path without the query. Every error this adapter returns
//! has the key replaced.

use super::access::Action;
use super::clients::{history_window, number, safe_name, text, unix};
use super::{ClientDownload, ClientListing, DownloadClient, DownloadStatus, EngineError, Protocol};
use crate::core::config::DownloadClientConfig;
use crate::core::outbound::{Calls, OutRequest, Outbound, encode_component, expect_ok};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// The type of the integration.
pub const KIND: &str = "sabnzbd";
/// Where the API answers, below the address.
pub const API_PATH: &str = "/api";
/// History jobs asked for each pass, newest first. When the history holds more, the window's
/// edge is the oldest job read.
pub const HISTORY_LIMIT: u32 = 100;

/// What Singularr can do to SABnzbd beyond reading it: nothing.
pub const ACTIONS: &[Action] = &[];

/// The calls SABnzbd is sent: GETs of its API with `mode` `queue` or `history`, and no key
/// beside `mode` but the answer's format, the key and the limit. No other GET passes.
pub fn calls() -> Calls {
    Calls::default().get_only(
        API_PATH,
        "mode",
        &["queue", "history"],
        &["output", "apikey", "limit"],
    )
}

pub struct Sabnzbd {
    id: String,
    base: String,
    key: String,
    out: Outbound,
    /// The clock the history's window is measured by.
    now: fn() -> DateTime<Utc>,
}

/// What SABnzbd's "MB" is: a mebibyte.
const MB: f64 = 1_048_576.0;

/// Where a queue job stands, from SABnzbd's word for it.
pub fn queue_status(word: &str) -> DownloadStatus {
    match word {
        "Downloading" | "Fetching" => DownloadStatus::Downloading,
        "Paused" => DownloadStatus::Paused,
        "Checking" => DownloadStatus::Checking,
        // Waiting its turn, waiting to be propagated to the servers, fetching its NZB.
        _ => DownloadStatus::Queued,
    }
}

/// Where a history job stands, from SABnzbd's word for it.
pub fn history_status(word: &str) -> DownloadStatus {
    match word {
        "Completed" => DownloadStatus::Completed,
        "Failed" => DownloadStatus::Failed,
        "QuickCheck" | "Verifying" | "Repairing" => DownloadStatus::Checking,
        // Extra par2 files of a repair.
        "Fetching" => DownloadStatus::Downloading,
        // Waiting for post-processing, unpacking, moving, its script running.
        _ => DownloadStatus::Unpacking,
    }
}

/// `H:MM:SS`, or `D:HH:MM:SS`, as seconds.
fn seconds_left(text: &str) -> Option<i64> {
    let mut total: i64 = 0;
    let parts: Vec<&str> = text.trim().split(':').collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let scale: &[i64] = if parts.len() == 4 {
        &[86_400, 3_600, 60, 1]
    } else {
        &[3_600, 60, 1]
    };
    for (part, unit) in parts.iter().zip(scale) {
        total += part.parse::<i64>().ok()? * unit;
    }
    Some(total)
}

fn int(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(number).map(|n| n as i64)
}

fn category(v: &Value, key: &str) -> Option<String> {
    // `*` is SABnzbd's default category: no category of its own.
    text(v.get(key)).filter(|c| c != "*")
}

/// One job of the queue. `speed` is the queue's rate, given to the job that downloads when it
/// is the only one. A job still fetching its NZB (`Grabbing`) is none yet.
fn queue_job(v: &Value, speed: Option<i64>) -> Option<ClientDownload> {
    let id = text(v.get("nzo_id"))?;
    let word = text(v.get("status")).unwrap_or_default();
    if word == "Grabbing" {
        return None;
    }
    let status = queue_status(&word);
    let mb = v.get("mb").and_then(number).unwrap_or(0.0).max(0.0);
    let left = v
        .get("mbleft")
        .and_then(number)
        .unwrap_or(mb)
        .clamp(0.0, mb);
    let progress = if mb > 0.0 {
        (mb - left) / mb
    } else {
        v.get("percentage").and_then(number).unwrap_or(0.0) / 100.0
    };
    let downloading = status == DownloadStatus::Downloading;
    Some(ClientDownload {
        download_id: id.to_ascii_uppercase(),
        id,
        protocol: Protocol::Usenet,
        name: safe_name(&text(v.get("filename")).unwrap_or_default()),
        category: category(v, "cat"),
        status,
        state: word,
        progress: progress.clamp(0.0, 1.0),
        size_bytes: (mb * MB).round() as i64,
        downloaded_bytes: Some(((mb - left) * MB).round() as i64),
        added_at: unix(int(v, "time_added")),
        completed_at: None,
        save_path: None,
        speed: speed
            .filter(|_| downloading)
            .map(|s| i32::try_from(s).unwrap_or(i32::MAX)),
        upload_speed: None,
        eta: text(v.get("timeleft"))
            .and_then(|t| seconds_left(&t))
            .filter(|s| downloading && *s > 0)
            .map(|s| i32::try_from(s).unwrap_or(i32::MAX)),
        tracker: None,
        seeds: None,
        peers: None,
        ratio: None,
        seeding_seconds: 0,
    })
}

/// One job of the history.
fn history_job(v: &Value) -> Option<ClientDownload> {
    let id = text(v.get("nzo_id"))?;
    let word = text(v.get("status")).unwrap_or_default();
    let status = history_status(&word);
    let size = int(v, "bytes").unwrap_or(0).max(0);
    let completed = unix(int(v, "completed"));
    let added = unix(int(v, "time_added")).or_else(|| {
        // Older versions do not say when a job was added: it was that long before it ended.
        let spent = int(v, "download_time").unwrap_or(0) + int(v, "postproc_time").unwrap_or(0);
        completed.map(|c| c - chrono::Duration::seconds(spent.max(0)))
    });
    Some(ClientDownload {
        download_id: id.to_ascii_uppercase(),
        id,
        protocol: Protocol::Usenet,
        name: safe_name(&text(v.get("name")).unwrap_or_default()),
        category: category(v, "category"),
        status,
        state: word,
        // A job in the history is past its download: completed, failed, or being worked on.
        // What arrived of a failed one is `downloaded_bytes`.
        progress: 1.0,
        size_bytes: size,
        downloaded_bytes: int(v, "downloaded").filter(|n| *n >= 0),
        added_at: added,
        completed_at: completed
            .filter(|_| matches!(status, DownloadStatus::Completed | DownloadStatus::Failed)),
        save_path: text(v.get("storage")).or_else(|| text(v.get("path"))),
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

/// The jobs of both answers: the queue's, then those of the history's window that the queue
/// does not hold. The history held only part of SABnzbd's when it counts more jobs
/// (`noofslots`) than it lists.
pub fn jobs(queue: &Value, history: &Value, now: DateTime<Utc>) -> ClientListing {
    let q = &queue["queue"];
    let slots = q["slots"].as_array().cloned().unwrap_or_default();
    let downloading = slots
        .iter()
        .filter(|s| s["status"].as_str() == Some("Downloading"))
        .count();
    // The queue's rate is in KB/s; it is one job's when one job downloads.
    let speed = q
        .get("kbpersec")
        .and_then(number)
        .filter(|_| downloading == 1)
        .map(|kb| (kb * 1024.0).round() as i64);
    let mut out: Vec<ClientDownload> = slots.iter().filter_map(|s| queue_job(s, speed)).collect();
    let listed = history["history"]["slots"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let cut = history["history"]
        .get("noofslots")
        .and_then(number)
        .is_some_and(|total| total > listed.len() as f64);
    let (done, edge) = history_window(
        listed.iter().filter_map(history_job).collect(),
        now,
        HISTORY_LIMIT as usize,
        cut,
    );
    for job in done {
        if !out.iter().any(|o| o.id == job.id) {
            out.push(job);
        }
    }
    ClientListing {
        downloads: out,
        finished_since: Some(edge),
    }
}

impl Sabnzbd {
    /// `None` when no URL or no key is configured.
    pub fn new(config: &DownloadClientConfig, out: Outbound) -> Option<Self> {
        Some(Sabnzbd {
            id: config.id.clone(),
            base: config.url.clone()?,
            key: config.secret.clone()?,
            out,
            now: Utc::now,
        })
    }

    /// An error with the key replaced, whatever produced it.
    fn scrubbed(&self, e: EngineError) -> EngineError {
        let text = e.to_string();
        if self.key.is_empty() || !text.contains(&self.key) {
            return e;
        }
        EngineError::Decode {
            engine: self.id.clone(),
            what: "answer",
            message: text.replace(&self.key, "(key)"),
        }
    }

    async fn get(&self, mode: &str, extra: &str) -> Result<Value, EngineError> {
        let url = format!(
            "{}{API_PATH}?mode={mode}&output=json{extra}&apikey={}",
            self.base,
            encode_component(&self.key)
        );
        let res = self
            .out
            .send(OutRequest::get(&url))
            .await
            .map_err(|e| self.scrubbed(e.into()))?;
        // SABnzbd 4 and later answer 403 for a key that is missing or wrong; older ones answer
        // 200 with an error.
        if res.status == 403 || res.status == 401 {
            return Err(EngineError::Login {
                engine: self.id.clone(),
                message: "the API key is wrong".to_string(),
            });
        }
        expect_ok(&url, &res).map_err(|e| self.scrubbed(e.into()))?;
        let value: Value = res.json().map_err(|e| self.scrubbed(e.into()))?;
        if let Some(error) = value.get("error").and_then(Value::as_str) {
            return Err(if error.to_ascii_lowercase().contains("api key") {
                EngineError::Login {
                    engine: self.id.clone(),
                    message: "the API key is wrong".to_string(),
                }
            } else {
                self.scrubbed(EngineError::Decode {
                    engine: self.id.clone(),
                    what: "answer",
                    message: error.to_string(),
                })
            });
        }
        Ok(value)
    }
}

#[async_trait]
impl DownloadClient for Sabnzbd {
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
        let queue = self.get("queue", "").await?;
        let history = self
            .get("history", &format!("&limit={HISTORY_LIMIT}"))
            .await?;
        if !queue["queue"]["slots"].is_array() || !history["history"]["slots"].is_array() {
            return Err(EngineError::Decode {
                engine: self.id.clone(),
                what: "queue",
                message: "the queue or the history has no list of jobs".to_string(),
            });
        }
        Ok(jobs(&queue, &history, (self.now)()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClientKind;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method, OutboundError};
    use std::sync::Arc;

    const BASE: &str = "http://192.0.2.66:8080";
    const KEY: &str = "1fb08498a8754c76b579a48a1e571e5e";

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/sabnzbd/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn allow() -> Allowlist {
        let mut allow = Allowlist::default();
        allow.allow_integration(KIND, BASE, calls()).unwrap();
        allow
    }

    /// An hour after the fixtures were captured.
    fn captured() -> DateTime<Utc> {
        DateTime::from_timestamp(1_795_584_420, 0).unwrap()
    }

    fn client(recorded: Arc<Recorded>) -> Sabnzbd {
        let config = DownloadClientConfig {
            id: KIND.to_string(),
            kind: ClientKind::Sabnzbd,
            url: Some(BASE.to_string()),
            user: None,
            secret: Some(KEY.to_string()),
        };
        let mut client =
            Sabnzbd::new(&config, Outbound::with_transport(allow(), recorded)).unwrap();
        client.now = captured;
        client
    }

    #[tokio::test]
    async fn it_lists_the_queue_then_the_history() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "mode=queue", 200, &fixture("queue.json"));
        recorded.answer(Method::Get, "mode=history", 200, &fixture("history.json"));
        let s = client(recorded.clone());
        let all = s.downloads().await.unwrap();
        let names: Vec<(&str, DownloadStatus)> =
            all.iter().map(|d| (d.name.as_str(), d.status)).collect();
        assert_eq!(
            names,
            vec![
                (
                    "Tears.of.Steel.S01E01.1080p.WEB-DL.x264-FX",
                    DownloadStatus::Paused
                ),
                ("Sintel.2010.1080p.BluRay.x264-FX", DownloadStatus::Paused),
                (
                    "Tears.of.Steel.S01E03.1080p.WEB-DL.x264-FX",
                    DownloadStatus::Downloading
                ),
                (
                    "Tears.of.Steel.S01E02.1080p.WEB-DL.x264-FX",
                    DownloadStatus::Completed
                ),
                (
                    "Cosmos.Laundromat.2015.720p.WEB-DL.x264-FX",
                    DownloadStatus::Failed
                ),
            ]
        );
        // The managers name a job by its nzo_id: the download id is it, upper case.
        let paused = &all[0];
        assert_eq!(paused.id, "ecee74c5-6619-4fb7-ba9a-2ee4cf76e5f2");
        assert_eq!(paused.download_id, "ECEE74C5-6619-4FB7-BA9A-2EE4CF76E5F2");
        assert_eq!(
            (paused.category.as_deref(), paused.protocol),
            (Some("tv"), Protocol::Usenet)
        );
        assert_eq!(
            (paused.progress, paused.speed, paused.eta),
            (0.0, None, None)
        );
        assert!(paused.added_at.is_some());
        let moving = &all[2];
        assert!(
            moving.progress > 0.07 && moving.progress < 0.09,
            "{}",
            moving.progress
        );
        assert_eq!(moving.speed, Some(102_625), "the queue's 100.22 KB/s");
        assert_eq!(moving.eta, Some(56));
        assert_eq!(moving.size_bytes, 6_291_456);
        let done = &all[3];
        assert_eq!((done.progress, done.size_bytes), (1.0, 3_145_728));
        assert!(done.completed_at.is_some());
        assert!(done.save_path.as_deref().unwrap().ends_with(".mkv"));
        // A failed job is past its download too; what arrived of it is its downloaded bytes.
        let failed = &all[4];
        assert_eq!(
            (failed.progress, failed.category.as_deref()),
            (1.0, Some("movies"))
        );
        assert_eq!(failed.downloaded_bytes, Some(0));
        assert_eq!(
            (failed.seeds, failed.ratio, failed.seeding_seconds),
            (None, None, 0)
        );
        // Two GETs, each in its declared form: the key and the format, the limit on history.
        let urls = recorded.urls_seen();
        assert_eq!(urls.len(), 2);
        assert!(
            urls[0].contains("mode=queue&output=json&apikey="),
            "{}",
            urls[0]
        );
        assert!(
            urls[1].contains("mode=history&output=json&limit=100&apikey="),
            "{}",
            urls[1]
        );
    }

    /// A job still fetching its NZB is left out: its name is the indexer's link, key and all.
    /// The history is read over its window. The edge is the window's start, or the oldest job
    /// read when SABnzbd holds more than it gave.
    #[test]
    fn a_fetching_job_is_left_out_and_the_history_has_an_edge() {
        let mut queue: Value = serde_json::from_str(&fixture("queue.json")).unwrap();
        queue["queue"]["slots"][0]["status"] = serde_json::json!("Grabbing");
        queue["queue"]["slots"][0]["filename"] = serde_json::json!(
            "Trying to fetch NZB from https://indexer.example/api?t=get&id=7&apikey=K3Y"
        );
        let mut history: Value = serde_json::from_str(&fixture("history.json")).unwrap();
        let listing = jobs(&queue, &history, captured());
        assert_eq!(listing.downloads.len(), 4);
        assert!(listing.downloads.iter().all(|d| !d.name.contains("K3Y")));
        assert_eq!(
            listing.finished_since,
            Some(captured() - chrono::TimeDelta::days(7))
        );
        history["history"]["noofslots"] = serde_json::json!(500);
        let listing = jobs(&queue, &history, captured());
        assert_eq!(
            listing.finished_since,
            DateTime::from_timestamp(1_795_580_820, 0)
        );
        // Eight days on, the finished jobs are past the window.
        let later = captured() + chrono::TimeDelta::days(8);
        assert_eq!(jobs(&queue, &history, later).downloads.len(), 2);
    }

    /// A wrong key is a refused sign-in. No error repeats the key.
    #[tokio::test]
    async fn a_wrong_key_is_named_and_never_shown() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "mode=queue", 403, "API Key Incorrect");
        let err = client(recorded).downloads().await.unwrap_err();
        assert!(matches!(err, EngineError::Login { .. }), "{err}");
        assert!(!err.to_string().contains(KEY));
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "mode=queue",
            200,
            r#"{"status": false, "error": "API Key Required"}"#,
        );
        let err = client(recorded).downloads().await.unwrap_err();
        assert!(matches!(err, EngineError::Login { .. }), "{err}");
        let recorded = Recorded::new();
        recorded.fail(
            Method::Get,
            "mode=queue",
            &format!("refused for apikey={KEY}"),
        );
        let err = client(recorded).downloads().await.unwrap_err();
        assert!(!err.to_string().contains(KEY), "{err}");
    }

    #[test]
    fn every_word_has_a_status_and_times_left_are_read() {
        for (word, status) in [
            ("Downloading", DownloadStatus::Downloading),
            ("Fetching", DownloadStatus::Downloading),
            ("Paused", DownloadStatus::Paused),
            ("Checking", DownloadStatus::Checking),
            ("Queued", DownloadStatus::Queued),
            ("Propagating", DownloadStatus::Queued),
            ("Grabbing", DownloadStatus::Queued),
        ] {
            assert_eq!(queue_status(word), status, "{word}");
        }
        for (word, status) in [
            ("Completed", DownloadStatus::Completed),
            ("Failed", DownloadStatus::Failed),
            ("QuickCheck", DownloadStatus::Checking),
            ("Verifying", DownloadStatus::Checking),
            ("Repairing", DownloadStatus::Checking),
            ("Fetching", DownloadStatus::Downloading),
            ("Queued", DownloadStatus::Unpacking),
            ("Extracting", DownloadStatus::Unpacking),
            ("Moving", DownloadStatus::Unpacking),
            ("Running", DownloadStatus::Unpacking),
        ] {
            assert_eq!(history_status(word), status, "{word}");
        }
        assert_eq!(seconds_left("0:00:56"), Some(56));
        assert_eq!(seconds_left("1:02:03:04"), Some(93_784));
        assert_eq!(seconds_left("soon"), None);
        // A job in post-processing, as SABnzbd lists it while it unpacks: the completed job of
        // the captured history with its status changed.
        let history: Value = serde_json::from_str(&fixture("history.json")).unwrap();
        let mut job = history["history"]["slots"][0].clone();
        job["status"] = Value::from("Extracting");
        job["completed"] = Value::from(0);
        let unpacking = history_job(&job).unwrap();
        assert_eq!(
            (unpacking.status, unpacking.progress, unpacking.completed_at),
            (DownloadStatus::Unpacking, 1.0, None)
        );
        // Without `time_added` the job was added as long before it ended as it took.
        let mut older = history["history"]["slots"][0].clone();
        older.as_object_mut().unwrap().remove("time_added");
        older["download_time"] = Value::from(60);
        older["postproc_time"] = Value::from(40);
        let read = history_job(&older).unwrap();
        assert_eq!(
            read.completed_at.unwrap() - read.added_at.unwrap(),
            chrono::Duration::seconds(100)
        );
    }

    /// Only the queue and the history pass, each with the keys it is sent with; a delete, a
    /// pause or any other command is refused, as is any other GET.
    #[test]
    fn the_api_passes_with_its_two_reads_alone() {
        let a = allow();
        let at = |q: &str| format!("{BASE}{API_PATH}?{q}&apikey={KEY}");
        assert!(a.check(Method::Get, &at("mode=queue&output=json")).is_ok());
        assert!(
            a.check(Method::Get, &at("mode=history&output=json&limit=100"))
                .is_ok()
        );
        for q in [
            "mode=queue&name=delete&value=all",
            "mode=queue&name=pause&value=SABnzbd_nzo_1",
            "mode=history&name=delete&value=all&del_files=1",
            "mode=addurl&name=http://example.invalid/x.nzb",
            "mode=pause",
            "mode=shutdown",
            "mode=restart",
            "mode=config&name=speedlimit&value=1",
            "mode=set_config&section=misc&keyword=api_key",
            "mode=retry_all",
            "mode=version",
        ] {
            let err = a.check(Method::Get, &at(q)).unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{q}: {err:?}"
            );
            assert!(!err.to_string().contains(KEY));
        }
        assert!(
            a.check(Method::Get, &format!("{BASE}/sabnzbd/shutdown"))
                .is_err()
        );
        assert!(a.check(Method::Post, &at("mode=queue")).is_err());
        assert!(ACTIONS.is_empty());
    }
}
