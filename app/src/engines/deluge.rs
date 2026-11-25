//! The Deluge adapter (the Web UI's JSON-RPC), read-only, behind [`DownloadClient`].
//!
//! Every call is a `POST /json` with a method in its body. Two are sent: `auth.login` with the
//! web password (its answer sets a session cookie) and `web.update_ui` with the fields read.
//! Both only read. Every other method on the same path, and every GET, is refused on the way
//! out ([`calls`]).
//!
//! The web interface answers for the daemon it is connected to. An answer without that
//! daemon's torrent list is an error: no daemon is connected, or the daemon's listing failed.
//! This adapter does not connect a daemon. Connecting one is a choice of the web interface's
//! owner.
//!
//! The category is the Label plugin's label, when that plugin is on.

use super::access::Action;
use super::clients::{count, tracker_host, unix};
use super::{ClientDownload, DownloadClient, DownloadStatus, EngineError, Protocol};
use crate::core::config::DownloadClientConfig;
use crate::core::outbound::{
    Body, BodyRule, Calls, Method, OutRequest, OutResponse, Outbound, expect_ok,
};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use tokio::sync::Mutex;

/// The type of the integration.
pub const KIND: &str = "deluge";
/// Where the web interface answers its JSON-RPC, below the address.
pub const JSON_PATH: &str = "/json";
pub const LOGIN: &str = "auth.login";
pub const UPDATE_UI: &str = "web.update_ui";

/// What Singularr can do to Deluge beyond reading it: nothing.
pub const ACTIONS: &[Action] = &[];

/// Deluge's error code for a call without a session.
const NOT_AUTHENTICATED: i64 = 1;

/// The fields `web.update_ui` asks for, each torrent's.
const KEYS: &[&str] = &[
    "hash",
    "name",
    "state",
    "progress",
    "total_size",
    "total_wanted",
    "total_done",
    "download_payload_rate",
    "upload_payload_rate",
    "eta",
    "download_location",
    "save_path",
    "time_added",
    "completed_time",
    "label",
    "ratio",
    "num_seeds",
    "num_peers",
    "seeding_time",
    "tracker_host",
    "is_finished",
];

/// The calls Deluge is sent: its JSON-RPC with `auth.login` or `web.update_ui`. Both only
/// read. Adding, removing, moving or reconfiguring through the same path is refused. No GET
/// passes.
pub fn calls() -> Calls {
    Calls::default()
        .read_with(
            Method::Post,
            JSON_PATH,
            BodyRule::new().one_of("/method", &[LOGIN, UPDATE_UI]),
        )
        .no_get()
}

pub struct Deluge {
    id: String,
    base: String,
    password: String,
    out: Outbound,
    /// The session cookie (`_session_id=...`) of the last sign-in.
    session: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct Answer {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: i64,
}

#[derive(Deserialize)]
struct UiResult {
    #[serde(default)]
    connected: bool,
    torrents: Option<BTreeMap<String, RawTorrent>>,
}

#[derive(Deserialize)]
struct RawTorrent {
    hash: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    state: String,
    /// 0 to 100.
    #[serde(default)]
    progress: f64,
    total_size: Option<i64>,
    total_wanted: Option<i64>,
    total_done: Option<i64>,
    download_payload_rate: Option<f64>,
    upload_payload_rate: Option<f64>,
    eta: Option<f64>,
    download_location: Option<String>,
    save_path: Option<String>,
    time_added: Option<f64>,
    completed_time: Option<f64>,
    label: Option<String>,
    ratio: Option<f64>,
    num_seeds: Option<i64>,
    num_peers: Option<i64>,
    seeding_time: Option<f64>,
    tracker_host: Option<String>,
    #[serde(default)]
    is_finished: bool,
}

/// The session cookie of a sign-in answer.
fn session_cookie(res: &OutResponse) -> Option<String> {
    res.headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|(_, value)| value.split(';').next())
        .map(str::trim)
        .find(|pair| pair.starts_with("_session_id="))
        .map(str::to_string)
}

/// Where a torrent stands, in the product's words, from Deluge's state.
pub fn status_of(state: &str, finished: bool) -> DownloadStatus {
    match state {
        "Downloading" => DownloadStatus::Downloading,
        "Seeding" => DownloadStatus::Seeding,
        "Paused" if finished => DownloadStatus::Completed,
        "Paused" => DownloadStatus::Paused,
        "Checking" => DownloadStatus::Checking,
        "Queued" | "Allocating" => DownloadStatus::Queued,
        "Moving" => DownloadStatus::Unpacking,
        "Error" => DownloadStatus::Failed,
        _ if finished => DownloadStatus::Completed,
        _ => DownloadStatus::Downloading,
    }
}

/// A number of seconds Deluge writes as a float.
fn whole(v: Option<f64>) -> Option<i64> {
    v.filter(|n| n.is_finite()).map(|n| n.round() as i64)
}

fn download(key: String, t: RawTorrent) -> ClientDownload {
    let hash = t.hash.filter(|h| !h.trim().is_empty()).unwrap_or(key);
    let hash = hash.trim().to_string();
    let progress = (t.progress / 100.0).clamp(0.0, 1.0);
    ClientDownload {
        download_id: hash.to_ascii_uppercase(),
        id: hash,
        protocol: Protocol::Torrent,
        name: t.name,
        category: t
            .label
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty()),
        status: status_of(&t.state, t.is_finished),
        state: t.state,
        progress,
        size_bytes: t
            .total_wanted
            .filter(|s| *s > 0)
            .or(t.total_size)
            .unwrap_or(0)
            .max(0),
        downloaded_bytes: t.total_done.filter(|n| *n >= 0),
        added_at: unix(whole(t.time_added)),
        completed_at: unix(whole(t.completed_time)),
        save_path: t
            .download_location
            .or(t.save_path)
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty()),
        speed: count(whole(t.download_payload_rate)),
        upload_speed: count(whole(t.upload_payload_rate)),
        // Deluge writes 0 for a time left it cannot tell.
        eta: count(whole(t.eta).filter(|e| *e > 0)),
        tracker: t.tracker_host.as_deref().and_then(tracker_host),
        seeds: count(t.num_seeds),
        peers: count(t.num_peers),
        // -1: nothing downloaded to measure against.
        ratio: t.ratio.filter(|r| *r >= 0.0),
        seeding_seconds: whole(t.seeding_time).unwrap_or(0).max(0),
    }
}

impl Deluge {
    /// `None` when no URL or no password is configured.
    pub fn new(config: &DownloadClientConfig, out: Outbound) -> Option<Self> {
        Some(Deluge {
            id: config.id.clone(),
            base: config.url.clone()?,
            password: config.secret.clone()?,
            out,
            session: Mutex::new(None),
        })
    }

    fn url(&self) -> String {
        format!("{}{JSON_PATH}", self.base)
    }

    fn login_error(&self, message: &str) -> EngineError {
        EngineError::Login {
            engine: self.id.clone(),
            message: message.to_string(),
        }
    }

    fn decode_error(&self, message: String) -> EngineError {
        EngineError::Decode {
            engine: self.id.clone(),
            what: "torrents",
            message,
        }
    }

    async fn rpc(
        &self,
        method: &str,
        params: Value,
        cookie: Option<&str>,
    ) -> Result<(OutResponse, Answer), EngineError> {
        let body = json!({"method": method, "params": params, "id": 1});
        let mut req = OutRequest::post(self.url(), Body::Json(body.to_string()))
            .header("Accept", "application/json");
        if let Some(cookie) = cookie {
            req = req.header("Cookie", cookie);
        }
        let res = self.out.send(req).await?;
        expect_ok(&self.url(), &res)?;
        let answer: Answer = res.json().map_err(|e| self.decode_error(e.to_string()))?;
        Ok((res, answer))
    }

    /// Sign in with the web password and keep the session cookie.
    async fn login(&self) -> Result<String, EngineError> {
        let (res, answer) = self.rpc(LOGIN, json!([self.password]), None).await?;
        if answer.result.as_ref().and_then(Value::as_bool) != Some(true) {
            return Err(self.login_error("the password is wrong"));
        }
        session_cookie(&res)
            .ok_or_else(|| self.login_error("it signed in without a session cookie"))
    }

    async fn update_ui(&self, cookie: &str) -> Result<Answer, EngineError> {
        let (_, answer) = self.rpc(UPDATE_UI, json!([KEYS, {}]), Some(cookie)).await?;
        Ok(answer)
    }
}

#[async_trait]
impl DownloadClient for Deluge {
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
        let cookie = match session.clone() {
            Some(cookie) => cookie,
            None => {
                let fresh = self.login().await?;
                *session = Some(fresh.clone());
                fresh
            }
        };
        let mut answer = self.update_ui(&cookie).await?;
        if answer
            .error
            .as_ref()
            .is_some_and(|e| e.code == NOT_AUTHENTICATED)
        {
            // The session ended. Sign in once more and ask again.
            let fresh = self.login().await?;
            *session = Some(fresh.clone());
            answer = self.update_ui(&fresh).await?;
        }
        if let Some(e) = answer.error {
            return Err(self.decode_error(format!("its answer is an error: {}", e.message)));
        }
        let result: UiResult = serde_json::from_value(answer.result.unwrap_or(Value::Null))
            .map_err(|e| self.decode_error(e.to_string()))?;
        if !result.connected {
            return Err(self.decode_error(
                "its web interface is not connected to a Deluge daemon".to_string(),
            ));
        }
        // Deluge leaves the list null when its daemon's answer fails.
        let Some(torrents) = result.torrents else {
            return Err(self.decode_error("its answer holds no torrent list".to_string()));
        };
        Ok(torrents
            .into_iter()
            .map(|(key, t)| download(key, t))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClientKind;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, OutboundError};
    use std::sync::Arc;

    const BASE: &str = "http://192.0.2.65:8112";
    const COOKIE: &str = "_session_id=9d4d25230a1b03c473c2811d25a5dab1; Expires=Wed, 25 Nov 2026 05:18:30 GMT; Path=/";

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/deluge/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn allow() -> Allowlist {
        let mut allow = Allowlist::default();
        allow.allow_integration(KIND, BASE, calls()).unwrap();
        allow
    }

    fn client(recorded: Arc<Recorded>) -> Deluge {
        let config = DownloadClientConfig {
            id: KIND.to_string(),
            kind: ClientKind::Deluge,
            url: Some(BASE.to_string()),
            user: None,
            secret: Some("deluge".to_string()),
        };
        Deluge::new(&config, Outbound::with_transport(allow(), recorded)).unwrap()
    }

    fn login(recorded: &Recorded) {
        recorded.answer_body_with(
            Method::Post,
            JSON_PATH,
            LOGIN,
            200,
            &fixture("login.json"),
            &[("set-cookie", COOKIE)],
        );
    }

    #[tokio::test]
    async fn it_signs_in_once_and_lists_the_torrents() {
        let recorded = Recorded::new();
        login(&recorded);
        recorded.answer_body(
            Method::Post,
            JSON_PATH,
            UPDATE_UI,
            200,
            &fixture("update_ui.json"),
        );
        let d = client(recorded.clone());
        let mut all = d.downloads().await.unwrap();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(all.len(), 4);
        let film = &all[0];
        assert_eq!(film.name, "Big.Buck.Bunny.2008.1080p.BluRay.x264-FX");
        assert_eq!(film.download_id, "CCF371C1D9835F9889212FDF033D29E605D91C22");
        assert_eq!(
            (film.status, film.state.as_str()),
            (DownloadStatus::Paused, "Paused")
        );
        assert_eq!(
            film.category.as_deref(),
            Some("radarr"),
            "the Label plugin's label"
        );
        assert_eq!(film.save_path.as_deref(), Some("/downloads/radarr"));
        assert_eq!((film.eta, film.ratio), (None, None));
        assert_eq!(film.tracker.as_deref(), Some("tracker.invalid"));
        let seeding = &all[1];
        assert_eq!(seeding.name, "Elephants.Dream.2006.720p.WEB-DL.x264-FX.mkv");
        assert_eq!(
            (seeding.status, seeding.progress),
            (DownloadStatus::Seeding, 1.0)
        );
        assert_eq!(seeding.category.as_deref(), Some("sonarr"));
        assert!(seeding.completed_at.is_some() && seeding.added_at.is_some());
        assert_eq!(seeding.downloaded_bytes, Some(3_145_728));
        let iso = &all[2];
        assert_eq!(
            (iso.category.as_deref(), iso.size_bytes),
            (None, 790_626_304)
        );
        let magnet = &all[3];
        assert_eq!(magnet.size_bytes, 0);
        d.downloads().await.unwrap();
        let sent = recorded.calls_sent();
        assert_eq!(sent.len(), 3, "one sign-in, then two listings");
        assert!(
            sent[1]
                .headers
                .iter()
                .any(|(n, v)| n == "Cookie" && v.starts_with("_session_id=9d4d")),
            "the session cookie, without its attributes"
        );
        assert!(sent[1].headers.iter().all(|(_, v)| !v.contains("Expires")));
        let body: Value = match &sent[1].body {
            Body::Json(text) => serde_json::from_str(text).unwrap(),
            other => panic!("{other:?}"),
        };
        assert_eq!(body["method"], UPDATE_UI);
        assert!(
            body["params"][0]
                .as_array()
                .unwrap()
                .iter()
                .any(|k| k == "label")
        );
    }

    #[tokio::test]
    async fn an_ended_session_signs_in_again_and_a_wrong_password_is_named() {
        let recorded = Recorded::new();
        login(&recorded);
        recorded.answer_body(
            Method::Post,
            JSON_PATH,
            UPDATE_UI,
            200,
            &fixture("not_authenticated.json"),
        );
        let err = client(recorded.clone()).downloads().await.unwrap_err();
        assert!(matches!(err, EngineError::Decode { .. }), "{err}");
        assert_eq!(
            recorded.calls_sent().len(),
            4,
            "sign-in, listing, sign-in, listing"
        );

        let recorded = Recorded::new();
        recorded.answer_body(
            Method::Post,
            JSON_PATH,
            LOGIN,
            200,
            &fixture("login_refused.json"),
        );
        let err = client(recorded.clone()).downloads().await.unwrap_err();
        assert!(matches!(err, EngineError::Login { .. }), "{err}");
        assert!(err.to_string().contains("password is wrong"), "{err}");
        assert_eq!(recorded.calls_sent().len(), 1);
    }

    /// A web interface connected to no daemon has no torrent list to give. Its listing is an
    /// error.
    #[tokio::test]
    async fn a_web_interface_without_its_daemon_is_an_error() {
        let recorded = Recorded::new();
        login(&recorded);
        recorded.answer_body(
            Method::Post,
            JSON_PATH,
            UPDATE_UI,
            200,
            &fixture("update_ui_disconnected.json"),
        );
        let err = client(recorded).downloads().await.unwrap_err();
        assert!(err.to_string().contains("not connected"), "{err}");
    }

    /// A connected daemon whose listing failed leaves the list null. The listing is an error:
    /// an empty list would read as every torrent gone.
    #[tokio::test]
    async fn a_connected_daemon_without_its_torrent_list_is_an_error() {
        let recorded = Recorded::new();
        login(&recorded);
        let mut answer: Value = serde_json::from_str(&fixture("update_ui.json")).unwrap();
        answer["result"]["torrents"] = Value::Null;
        recorded.answer_body(Method::Post, JSON_PATH, UPDATE_UI, 200, &answer.to_string());
        let err = client(recorded).downloads().await.unwrap_err();
        assert!(err.to_string().contains("no torrent list"), "{err}");
        // A connected daemon with no torrents lists none.
        answer["result"]["torrents"] = json!({});
        let recorded = Recorded::new();
        login(&recorded);
        recorded.answer_body(Method::Post, JSON_PATH, UPDATE_UI, 200, &answer.to_string());
        assert!(client(recorded).downloads().await.unwrap().is_empty());
    }

    #[test]
    fn every_state_has_a_status() {
        for (state, finished, status) in [
            ("Downloading", false, DownloadStatus::Downloading),
            ("Seeding", true, DownloadStatus::Seeding),
            ("Paused", false, DownloadStatus::Paused),
            ("Paused", true, DownloadStatus::Completed),
            ("Checking", false, DownloadStatus::Checking),
            ("Queued", false, DownloadStatus::Queued),
            ("Allocating", false, DownloadStatus::Queued),
            ("Moving", true, DownloadStatus::Unpacking),
            ("Error", false, DownloadStatus::Failed),
            ("Unknown", true, DownloadStatus::Completed),
        ] {
            assert_eq!(status_of(state, finished), status, "{state}");
        }
    }

    /// The sign-in and the listing pass. Every other method on the path is refused. So is every
    /// GET.
    #[test]
    fn the_json_rpc_passes_with_its_two_reads_alone() {
        let a = allow();
        let url = format!("{BASE}{JSON_PATH}");
        let body =
            |method: &str| Body::Json(json!({"method": method, "params": [], "id": 1}).to_string());
        for method in [LOGIN, UPDATE_UI] {
            assert!(
                a.check_call(Method::Post, &url, &body(method)).is_ok(),
                "{method}"
            );
        }
        for method in [
            "core.remove_torrent",
            "core.add_torrent_url",
            "core.add_torrent_magnet",
            "core.pause_torrent",
            "core.set_config",
            "core.move_storage",
            "web.connect",
            "web.disconnect",
            "web.set_config",
            "daemon.shutdown",
            "label.set_torrent",
        ] {
            assert!(
                matches!(
                    a.check_call(Method::Post, &url, &body(method)),
                    Err(OutboundError::NotListed { .. })
                ),
                "{method}"
            );
        }
        assert!(
            a.check_call(Method::Post, &format!("{BASE}/upload"), &body(LOGIN))
                .is_err()
        );
        // A method written twice is refused, whichever comes first.
        let twice = Body::Json(
            r#"{"method": "core.remove_torrent", "method": "web.update_ui", "params": [], "id": 1}"#
                .to_string(),
        );
        assert!(a.check_call(Method::Post, &url, &twice).is_err());
        for get in [
            url.clone(),
            format!("{url}?method=core.remove_torrent"),
            format!("{BASE}/"),
        ] {
            assert!(
                matches!(
                    a.check(Method::Get, &get),
                    Err(OutboundError::NotListed { .. })
                ),
                "{get}"
            );
        }
        assert!(ACTIONS.is_empty());
    }
}
