//! The Transmission adapter (its RPC), read-only, behind [`DownloadClient`].
//!
//! One call: `POST /transmission/rpc` with the method `torrent-get` and the fields it reads. The
//! RPC takes every method on that one path. The call passes the way out only with that method
//! in its body ([`calls`]). No GET passes.
//!
//! Transmission guards its RPC with a session id. A call without the right one is answered 409
//! with the id in `X-Transmission-Session-Id`. The call is then sent again with it. A user name
//! and password, when set, go as HTTP basic authentication.
//!
//! The request is the RPC form Transmission has always taken (the method `torrent-get`, the
//! fields in camel case). Transmission 4.1 also takes JSON-RPC 2.0 and keeps the older form.

use super::access::Action;
use super::clients::{count, tracker_host, unix};
use super::{ClientDownload, DownloadClient, DownloadStatus, EngineError, Protocol};
use crate::core::config::DownloadClientConfig;
use crate::core::outbound::{
    Body, BodyRule, Calls, Method, OutRequest, OutResponse, Outbound, basic_auth, expect_ok,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

/// The type of the integration.
pub const KIND: &str = "transmission";
/// Where the RPC answers, below the address.
pub const RPC_PATH: &str = "/transmission/rpc";
/// The header that carries the session id.
pub const SESSION_HEADER: &str = "X-Transmission-Session-Id";
/// The one method sent.
pub const TORRENT_GET: &str = "torrent-get";

/// What Singularr can do to Transmission beyond reading it: nothing.
pub const ACTIONS: &[Action] = &[];

/// The fields `torrent-get` asks for.
const FIELDS: &[&str] = &[
    "id",
    "hashString",
    "name",
    "status",
    "percentDone",
    "totalSize",
    "sizeWhenDone",
    "leftUntilDone",
    "rateDownload",
    "rateUpload",
    "eta",
    "downloadDir",
    "addedDate",
    "doneDate",
    "error",
    "isStalled",
    "labels",
    "peersSendingToUs",
    "peersGettingFromUs",
    "uploadRatio",
    "secondsSeeding",
    "trackers",
];

/// The calls Transmission is sent: its RPC with `torrent-get`. It only reads. Every other
/// method on the same path (adding, removing, moving a torrent, changing the session) is
/// refused on the way out. No GET passes: Transmission 3.00 runs a query string on the RPC
/// path as a call.
pub fn calls() -> Calls {
    Calls::default()
        .read_with(
            Method::Post,
            RPC_PATH,
            BodyRule::new().one_of("/method", &[TORRENT_GET]),
        )
        .no_get()
}

pub struct Transmission {
    id: String,
    base: String,
    user: Option<String>,
    pass: Option<String>,
    out: Outbound,
    /// The session id from the last 409.
    session: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    result: String,
    arguments: Option<Arguments>,
}

#[derive(Deserialize)]
struct Arguments {
    #[serde(default)]
    torrents: Vec<RawTorrent>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTorrent {
    hash_string: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    status: i64,
    #[serde(default)]
    percent_done: f64,
    total_size: Option<i64>,
    size_when_done: Option<i64>,
    left_until_done: Option<i64>,
    rate_download: Option<i64>,
    rate_upload: Option<i64>,
    eta: Option<i64>,
    download_dir: Option<String>,
    added_date: Option<i64>,
    done_date: Option<i64>,
    #[serde(default)]
    error: i64,
    #[serde(default)]
    is_stalled: bool,
    #[serde(default)]
    labels: Vec<String>,
    peers_sending_to_us: Option<i64>,
    peers_getting_from_us: Option<i64>,
    upload_ratio: Option<f64>,
    seconds_seeding: Option<i64>,
    #[serde(default)]
    trackers: Vec<RawTracker>,
}

#[derive(Deserialize)]
struct RawTracker {
    #[serde(default)]
    announce: String,
}

/// Transmission's error number for a problem with the torrent's own data. 1 and 2 are tracker
/// warnings and errors: the download stays as it is.
const LOCAL_ERROR: i64 = 3;

/// Transmission's own word for a status number (libtransmission's `TR_STATUS_*`).
pub fn state_word(status: i64) -> &'static str {
    match status {
        0 => "stopped",
        1 => "check_wait",
        2 => "check",
        3 => "download_wait",
        4 => "download",
        5 => "seed_wait",
        6 => "seed",
        _ => "unknown",
    }
}

/// Where a torrent stands, in the product's words.
pub fn status_of(status: i64, done: f64, stalled: bool, error: i64) -> DownloadStatus {
    if error == LOCAL_ERROR {
        return DownloadStatus::Failed;
    }
    match status {
        0 if done >= 1.0 => DownloadStatus::Completed,
        0 => DownloadStatus::Paused,
        1 | 2 => DownloadStatus::Checking,
        3 | 5 => DownloadStatus::Queued,
        4 if stalled => DownloadStatus::Stalled,
        4 => DownloadStatus::Downloading,
        6 => DownloadStatus::Seeding,
        _ if done >= 1.0 => DownloadStatus::Completed,
        _ => DownloadStatus::Downloading,
    }
}

fn download(t: RawTorrent) -> ClientDownload {
    let progress = t.percent_done.clamp(0.0, 1.0);
    let size = t
        .size_when_done
        .filter(|s| *s > 0)
        .or(t.total_size)
        .unwrap_or(0)
        .max(0);
    let hash = t.hash_string.trim().to_string();
    ClientDownload {
        download_id: hash.to_ascii_uppercase(),
        id: hash,
        protocol: Protocol::Torrent,
        name: t.name,
        category: t
            .labels
            .iter()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .map(str::to_string),
        status: status_of(t.status, progress, t.is_stalled, t.error),
        state: state_word(t.status).to_string(),
        progress,
        size_bytes: size,
        downloaded_bytes: t.left_until_done.map(|left| (size - left).max(0)),
        added_at: unix(t.added_date),
        completed_at: unix(t.done_date),
        save_path: t
            .download_dir
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty()),
        speed: count(t.rate_download),
        upload_speed: count(t.rate_upload),
        // -1: not known yet; -2: it cannot tell.
        eta: count(t.eta),
        tracker: t.trackers.first().and_then(|tr| tracker_host(&tr.announce)),
        seeds: count(t.peers_sending_to_us),
        peers: count(t.peers_getting_from_us),
        // -1: nothing downloaded to measure against.
        ratio: t.upload_ratio.filter(|r| *r >= 0.0),
        seeding_seconds: t.seconds_seeding.unwrap_or(0).max(0),
    }
}

impl Transmission {
    /// `None` when no URL is configured.
    pub fn new(config: &DownloadClientConfig, out: Outbound) -> Option<Self> {
        Some(Transmission {
            id: config.id.clone(),
            base: config.url.clone()?,
            user: config.user.clone(),
            pass: config.secret.clone(),
            out,
            session: Mutex::new(None),
        })
    }

    fn url(&self) -> String {
        format!("{}{RPC_PATH}", self.base)
    }

    async fn ask(&self, session: Option<&str>) -> Result<OutResponse, EngineError> {
        let body = json!({"method": TORRENT_GET, "arguments": {"fields": FIELDS}});
        let mut req = OutRequest::post(self.url(), Body::Json(body.to_string()));
        if let Some(id) = session {
            req = req.header(SESSION_HEADER, id);
        }
        if self.user.is_some() || self.pass.is_some() {
            req = req.header(
                "Authorization",
                basic_auth(
                    self.user.as_deref().unwrap_or_default(),
                    self.pass.as_deref().unwrap_or_default(),
                ),
            );
        }
        Ok(self.out.send(req).await?)
    }
}

#[async_trait]
impl DownloadClient for Transmission {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        KIND
    }

    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    async fn downloads(&self) -> Result<Vec<ClientDownload>, EngineError> {
        let mut session = self.session.lock().await;
        let mut res = self.ask(session.as_deref()).await?;
        if res.status == 409 {
            // No session id yet, or an old one: the answer carries the one to send.
            *session = res.header(SESSION_HEADER).map(str::to_string);
            if session.is_none() {
                return Err(EngineError::Login {
                    engine: self.id.clone(),
                    message: "it answered 409 without a session id".to_string(),
                });
            }
            res = self.ask(session.as_deref()).await?;
        }
        if res.status == 401 {
            return Err(EngineError::Login {
                engine: self.id.clone(),
                message: "the user name or password is wrong".to_string(),
            });
        }
        expect_ok(&self.url(), &res)?;
        let decode = |message: String| EngineError::Decode {
            engine: self.id.clone(),
            what: "torrents",
            message,
        };
        let answer: Answer = res.json().map_err(|e| decode(e.to_string()))?;
        if answer.result != "success" {
            return Err(decode(format!("its answer says `{}`", answer.result)));
        }
        let torrents = answer
            .arguments
            .ok_or_else(|| decode("its answer has no arguments".to_string()))?
            .torrents;
        Ok(torrents.into_iter().map(download).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClientKind;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, OutboundError};
    use std::sync::Arc;

    const BASE: &str = "http://192.0.2.64:9091";
    const SESSION: &str = "YPl9V8d5htoOqvy6co1ifZEQI8VzBWx5M7yTVyEC3mDGHoE1";
    /// The answer of Transmission 4.1.4 to `torrent-get`: three torrents added paused (one a
    /// magnet whose metadata never came) and one that seeds (`tests/fixtures/transmission`).
    const TORRENTS: &str = include_str!("../../tests/fixtures/transmission/torrent_get.json");

    fn allow() -> Allowlist {
        let mut allow = Allowlist::default();
        allow.allow_integration(KIND, BASE, calls()).unwrap();
        allow
    }

    fn client(recorded: Arc<Recorded>, login: bool) -> Transmission {
        let config = DownloadClientConfig {
            id: KIND.to_string(),
            kind: ClientKind::Transmission,
            url: Some(BASE.to_string()),
            user: login.then(|| "fxuser".to_string()),
            secret: login.then(|| "fxpass".to_string()),
        };
        Transmission::new(&config, Outbound::with_transport(allow(), recorded)).unwrap()
    }

    /// Transmission as it guards its RPC: 409 with the session id for a call without it, the
    /// torrents for a call with it. Every call is kept.
    #[derive(Default)]
    struct Guarded {
        sent: std::sync::Mutex<Vec<OutRequest>>,
    }

    #[async_trait]
    impl crate::core::outbound::Transport for Guarded {
        async fn send(&self, _url: reqwest::Url, req: &OutRequest) -> Result<OutResponse, String> {
            self.sent.lock().unwrap().push(req.clone());
            let has_session = req
                .headers
                .iter()
                .any(|(n, v)| n == SESSION_HEADER && v == SESSION);
            Ok(if has_session {
                OutResponse {
                    status: 200,
                    headers: vec![],
                    body: TORRENTS.as_bytes().to_vec(),
                }
            } else {
                OutResponse {
                    status: 409,
                    headers: vec![("x-transmission-session-id".to_string(), SESSION.to_string())],
                    body: b"<h1>409: Conflict</h1>".to_vec(),
                }
            })
        }
    }

    #[tokio::test]
    async fn it_takes_the_session_id_from_a_409_and_lists_the_torrents() {
        let guarded = Arc::new(Guarded::default());
        let config = DownloadClientConfig {
            id: KIND.to_string(),
            kind: ClientKind::Transmission,
            url: Some(BASE.to_string()),
            user: Some("fxuser".to_string()),
            secret: Some("fxpass".to_string()),
        };
        let t =
            Transmission::new(&config, Outbound::with_transport(allow(), guarded.clone())).unwrap();
        let all = t.downloads().await.unwrap();
        assert_eq!(all.len(), 4);
        // Once known, the session id goes with the next pass's call at once.
        t.downloads().await.unwrap();
        let sent = guarded.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 3, "a 409, the call again, then one call");
        let with_session = |r: &OutRequest| {
            r.headers
                .iter()
                .any(|(n, v)| n == SESSION_HEADER && v == SESSION)
        };
        assert!(!with_session(&sent[0]) && with_session(&sent[1]) && with_session(&sent[2]));
        assert!(
            sent[1]
                .headers
                .iter()
                .any(|(n, v)| n == "Authorization" && v == "Basic Znh1c2VyOmZ4cGFzcw==")
        );
        let body: serde_json::Value = match &sent[1].body {
            Body::Json(text) => serde_json::from_str(text).unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(body["method"], TORRENT_GET);
        assert!(body["arguments"]["fields"].as_array().unwrap().len() > 10);

        let iso = &all[0];
        assert_eq!(iso.name, "debian-13.2.0-amd64-netinst.iso");
        assert_eq!(iso.id, "1218a374ecbefee1ce843ae15c9f6b06c4e1cdf5");
        assert_eq!(iso.download_id, "1218A374ECBEFEE1CE843AE15C9F6B06C4E1CDF5");
        assert_eq!(
            (iso.status, iso.state.as_str()),
            (DownloadStatus::Paused, "stopped")
        );
        assert_eq!(
            (iso.size_bytes, iso.downloaded_bytes),
            (790_626_304, Some(0))
        );
        assert_eq!(
            (iso.eta, iso.ratio, iso.category.as_deref()),
            (None, Some(0.0), None)
        );
        assert_eq!(iso.tracker.as_deref(), Some("tracker.invalid"));
        assert_eq!(iso.save_path.as_deref(), Some("/downloads/complete"));
        assert!(iso.added_at.is_some() && iso.completed_at.is_none());
        let film = &all[1];
        assert_eq!(film.category.as_deref(), Some("radarr"), "the first label");
        // A magnet whose metadata never came: no size, no ratio yet.
        let magnet = &all[2];
        assert_eq!((magnet.size_bytes, magnet.ratio), (0, None));
        assert_eq!(magnet.status, DownloadStatus::Paused);
        let seeding = &all[3];
        assert_eq!(
            (seeding.status, seeding.state.as_str(), seeding.progress),
            (DownloadStatus::Seeding, "seed", 1.0)
        );
        assert_eq!(seeding.category.as_deref(), Some("sonarr"));
        assert!(seeding.completed_at.is_some());
        assert_eq!(seeding.downloaded_bytes, Some(3_145_728));
        assert_eq!(seeding.protocol, Protocol::Torrent);
    }

    #[tokio::test]
    async fn a_refused_sign_in_and_an_unsuccessful_answer_are_errors() {
        let recorded = Recorded::new();
        recorded.answer(Method::Post, RPC_PATH, 401, "<h1>401: Unauthorized</h1>");
        let err = client(recorded.clone(), true)
            .downloads()
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Login { .. }), "{err}");
        assert!(err.to_string().contains("user name or password"), "{err}");
        let recorded = Recorded::new();
        recorded.answer(
            Method::Post,
            RPC_PATH,
            200,
            r#"{"result": "invalid or corrupt torrent file"}"#,
        );
        let err = client(recorded, false).downloads().await.unwrap_err();
        assert!(matches!(err, EngineError::Decode { .. }), "{err}");
        // A 409 without the header is no session to retry with.
        let recorded = Recorded::new();
        recorded.answer(Method::Post, RPC_PATH, 409, "");
        let err = client(recorded.clone(), false)
            .downloads()
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Login { .. }), "{err}");
        assert_eq!(recorded.methods_seen().len(), 1);
    }

    #[tokio::test]
    async fn without_a_user_no_credentials_are_sent() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Post,
            RPC_PATH,
            200,
            r#"{"arguments": {"torrents": []}, "result": "success"}"#,
        );
        assert!(
            client(recorded.clone(), false)
                .downloads()
                .await
                .unwrap()
                .is_empty()
        );
        let sent = recorded.calls_sent();
        assert!(sent[0].headers.iter().all(|(n, _)| n != "Authorization"));
    }

    #[test]
    fn every_status_number_has_a_status() {
        for (status, done, stalled, error, expected) in [
            (0, 0.3, false, 0, DownloadStatus::Paused),
            (0, 1.0, false, 0, DownloadStatus::Completed),
            (1, 0.3, false, 0, DownloadStatus::Checking),
            (2, 0.3, false, 0, DownloadStatus::Checking),
            (3, 0.0, false, 0, DownloadStatus::Queued),
            (4, 0.3, false, 0, DownloadStatus::Downloading),
            (4, 0.3, true, 0, DownloadStatus::Stalled),
            (4, 0.3, false, 2, DownloadStatus::Downloading),
            (5, 1.0, false, 0, DownloadStatus::Queued),
            (6, 1.0, false, 0, DownloadStatus::Seeding),
            (6, 1.0, false, 3, DownloadStatus::Failed),
        ] {
            assert_eq!(
                status_of(status, done, stalled, error),
                expected,
                "{status} {done} {stalled} {error}"
            );
        }
        assert_eq!(state_word(9), "unknown");
    }

    /// Only `torrent-get` passes: the same path with another method is refused, as is any
    /// other path and every GET.
    #[test]
    fn the_rpc_passes_with_torrent_get_alone() {
        let a = allow();
        let rpc = format!("{BASE}{RPC_PATH}");
        let body =
            |method: &str| Body::Json(json!({"method": method, "arguments": {}}).to_string());
        assert!(
            a.check_call(Method::Post, &rpc, &body("torrent-get"))
                .is_ok()
        );
        for method in [
            "torrent-remove",
            "torrent-add",
            "torrent-set",
            "torrent-start",
            "torrent-stop",
            "torrent-set-location",
            "session-set",
            "session-close",
            "torrent_get",
            "blocklist-update",
        ] {
            assert!(
                matches!(
                    a.check_call(Method::Post, &rpc, &body(method)),
                    Err(OutboundError::NotListed { .. })
                ),
                "{method}"
            );
        }
        // JSON-RPC 2.0's names are another form, refused as well; so is a batch.
        let batch =
            Body::Json(r#"[{"method": "torrent-get"}, {"method": "torrent-remove"}]"#.to_string());
        assert!(a.check_call(Method::Post, &rpc, &batch).is_err());
        // Transmission keeps the first of a member written twice: such a body is refused,
        // whichever comes first.
        for text in [
            r#"{"method": "torrent-remove", "method": "torrent-get", "arguments": {"ids": [1]}}"#,
            r#"{"method": "torrent-get", "method": "torrent-get", "arguments": {}}"#,
        ] {
            assert!(
                a.check_call(Method::Post, &rpc, &Body::Json(text.to_string()))
                    .is_err(),
                "{text}"
            );
        }
        assert!(
            a.check_call(Method::Post, &format!("{rpc}/x"), &body("torrent-get"))
                .is_err()
        );
        // Transmission 3.00 runs a GET's query string as a call.
        for url in [
            rpc.clone(),
            format!("{rpc}?method=torrent-remove&ids=1&delete-local-data=true"),
            format!("{rpc}?method=torrent-get"),
            format!("{BASE}/transmission/web/"),
        ] {
            assert!(
                matches!(
                    a.check(Method::Get, &url),
                    Err(OutboundError::NotListed { .. })
                ),
                "{url}"
            );
        }
        assert!(ACTIONS.is_empty());
    }
}
