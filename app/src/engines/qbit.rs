//! The qBittorrent adapter (Web API v2), read-only, behind [`DownloadClient`].
//!
//! Two endpoints: `POST /api/v2/auth/login` (a sign-in, the one call that is not a GET) and
//! `GET /api/v2/torrents/info`.

use super::access::Action;
use super::arr::null_as_default;
use super::clients::{count, tracker_host, unix};
use super::{ClientDownload, DownloadClient, DownloadStatus, EngineError, Protocol};
use crate::core::config::DownloadClientConfig;
use crate::core::outbound::{Body, Calls, Method, OutRequest, OutResponse, Outbound, expect_ok};
use async_trait::async_trait;
use serde::Deserialize;
use tokio::sync::Mutex;

/// The type of the download client integration, and the id of the environment form's.
pub const ENGINE_ID: &str = "qbittorrent";
pub const LOGIN_PATH: &str = "/api/v2/auth/login";
pub const TORRENTS_PATH: &str = "/api/v2/torrents/info";

/// What Singularr can do to qBittorrent beyond reading it: nothing.
pub const ACTIONS: &[Action] = &[];

/// The calls qBittorrent is sent beyond GET: the sign-in. It only reads.
pub fn calls() -> Calls {
    Calls::default().read(Method::Post, LOGIN_PATH)
}

pub struct Qbit {
    id: String,
    base: String,
    user: Option<String>,
    pass: Option<String>,
    out: Outbound,
    /// The session cookie (`name=value`) from the last sign-in.
    session: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct RawTorrent {
    hash: String,
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
    category: Option<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    state: String,
    #[serde(default, deserialize_with = "null_as_default")]
    progress: f64,
    size: Option<i64>,
    completed: Option<i64>,
    added_on: Option<i64>,
    completion_on: Option<i64>,
    seeding_time: Option<i64>,
    save_path: Option<String>,
    dlspeed: Option<i64>,
    upspeed: Option<i64>,
    eta: Option<i64>,
    num_seeds: Option<i64>,
    num_leechs: Option<i64>,
    ratio: Option<f64>,
    tracker: Option<String>,
}

/// qBittorrent writes 8640000 seconds (100 days) for a time left it cannot tell.
const ETA_UNKNOWN: i64 = 8_640_000;

/// The session cookie of a sign-in answer: the first cookie whose name holds `SID`
/// (`SID` on qBittorrent 4, `QBT_SID_<port>` on newer ones).
fn session_cookie(res: &OutResponse) -> Option<String> {
    res.headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|(_, value)| value.split(';').next())
        .map(str::trim)
        .find(|pair| {
            pair.split('=')
                .next()
                .is_some_and(|n| n.to_ascii_uppercase().contains("SID"))
        })
        .map(str::to_string)
}

/// Where a torrent stands, in the product's words, from qBittorrent's state. A state this
/// adapter does not know reads by the progress. The title page reads these as it read
/// qBittorrent's states before ([`crate::api::metadata::title_queue_stage`]).
pub fn status_of(state: &str, progress: f64) -> DownloadStatus {
    match state {
        "downloading" | "forcedDL" | "forcedMetaDL" => DownloadStatus::Downloading,
        // A torrent waiting for its metadata, or checking the data of a download not complete
        // yet, waits as a queued one does.
        "metaDL" | "queuedDL" | "queuedUP" | "checkingDL" => DownloadStatus::Queued,
        "stalledDL" => DownloadStatus::Stalled,
        "pausedUP" | "stoppedUP" if progress >= 1.0 => DownloadStatus::Completed,
        "pausedDL" | "stoppedDL" | "pausedUP" | "stoppedUP" => DownloadStatus::Paused,
        "uploading" | "forcedUP" | "stalledUP" => DownloadStatus::Seeding,
        "checkingUP" | "checkingResumeData" => DownloadStatus::Checking,
        "moving" => DownloadStatus::Unpacking,
        "error" | "missingFiles" => DownloadStatus::Failed,
        _ if progress >= 1.0 => DownloadStatus::Completed,
        _ => DownloadStatus::Downloading,
    }
}

impl Qbit {
    /// `None` when no URL is configured.
    pub fn new(config: &DownloadClientConfig, out: Outbound) -> Option<Self> {
        Some(Qbit {
            id: config.id.clone(),
            base: config.url.clone()?,
            user: config.user.clone(),
            pass: config.secret.clone(),
            out,
            session: Mutex::new(None),
        })
    }

    fn login_error(&self, message: impl Into<String>) -> EngineError {
        EngineError::Login {
            engine: self.id.clone(),
            message: message.into(),
        }
    }

    /// Sign in and keep the session cookie. Without a user name there is nothing to sign in
    /// with: qBittorrent can be set to trust its subnet, and then no cookie is needed.
    async fn login(&self) -> Result<Option<String>, EngineError> {
        let (Some(user), Some(pass)) = (&self.user, &self.pass) else {
            return Ok(None);
        };
        let url = format!("{}{LOGIN_PATH}", self.base);
        let req = OutRequest::post(
            &url,
            Body::Form(vec![
                ("username".to_string(), user.clone()),
                ("password".to_string(), pass.clone()),
            ]),
        )
        // qBittorrent checks that the Referer names its own address.
        .header("Referer", self.base.clone());
        let res = self.out.send(req).await?;
        if res.status == 403 {
            return Err(self.login_error(
                "too many failed attempts; qBittorrent has banned this address for a while",
            ));
        }
        expect_ok(&url, &res)?;
        match session_cookie(&res) {
            Some(cookie) => Ok(Some(cookie)),
            None => {
                Err(self.login_error("the user name or password is wrong (QBIT_USER, QBIT_PASS)"))
            }
        }
    }

    async fn list(&self, cookie: Option<&str>) -> Result<OutResponse, EngineError> {
        let mut req = OutRequest::get(format!("{}{TORRENTS_PATH}", self.base));
        if let Some(cookie) = cookie {
            req = req.header("Cookie", cookie);
        }
        Ok(self.out.send(req).await?)
    }
}

fn download(t: RawTorrent) -> ClientDownload {
    let progress = t.progress.clamp(0.0, 1.0);
    let hash = t.hash.trim().to_string();
    ClientDownload {
        download_id: hash.to_ascii_uppercase(),
        id: hash,
        protocol: Protocol::Torrent,
        name: t.name,
        category: t
            .category
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty()),
        status: status_of(&t.state, progress),
        state: t.state,
        progress,
        size_bytes: t.size.unwrap_or(0),
        downloaded_bytes: t.completed.filter(|n| *n >= 0),
        added_at: unix(t.added_on),
        completed_at: unix(t.completion_on),
        save_path: t
            .save_path
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty()),
        speed: count(t.dlspeed),
        upload_speed: count(t.upspeed),
        eta: count(t.eta.filter(|e| *e < ETA_UNKNOWN)),
        tracker: t.tracker.as_deref().and_then(tracker_host),
        seeds: count(t.num_seeds),
        peers: count(t.num_leechs),
        ratio: t.ratio.filter(|r| *r >= 0.0),
        seeding_seconds: t.seeding_time.unwrap_or(0).max(0),
    }
}

#[async_trait]
impl DownloadClient for Qbit {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        ENGINE_ID
    }

    fn protocol(&self) -> Protocol {
        Protocol::Torrent
    }

    async fn downloads(&self) -> Result<Vec<ClientDownload>, EngineError> {
        let url = format!("{}{TORRENTS_PATH}", self.base);
        let mut session = self.session.lock().await;
        if session.is_none() {
            *session = self.login().await?;
        }
        let mut res = self.list(session.as_deref()).await?;
        if res.status == 403 {
            // The session ended. Sign in once more and ask again.
            *session = self.login().await?;
            res = self.list(session.as_deref()).await?;
        }
        if res.status == 403 {
            return Err(
                self.login_error("it answers 403 after a sign-in; set QBIT_USER and QBIT_PASS")
            );
        }
        expect_ok(&url, &res)?;
        let raw: Vec<RawTorrent> =
            serde_json::from_slice(&res.body).map_err(|e| EngineError::Decode {
                engine: self.id.clone(),
                what: "torrents",
                message: e.to_string(),
            })?;
        Ok(raw.into_iter().map(download).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClientKind;
    use crate::core::outbound::Allowlist;
    use crate::core::outbound::recorded::Recorded;
    use std::sync::Arc;

    const BASE: &str = "http://192.0.2.62:8080";

    fn qbit(recorded: Arc<Recorded>, with_login: bool) -> Qbit {
        let mut allow = Allowlist::default();
        allow.allow_integration(ENGINE_ID, BASE, calls()).unwrap();
        let config = DownloadClientConfig {
            id: ENGINE_ID.to_string(),
            kind: ClientKind::Qbittorrent,
            url: Some(BASE.to_string()),
            user: with_login.then(|| "admin".to_string()),
            secret: with_login.then(|| "test-password".to_string()),
        };
        Qbit::new(&config, Outbound::with_transport(allow, recorded)).unwrap()
    }

    #[tokio::test]
    async fn it_signs_in_once_and_lists_torrents() {
        let recorded = Recorded::new();
        recorded.answer_with(
            Method::Post,
            LOGIN_PATH,
            200,
            "Ok.",
            &[("set-cookie", "SID=abc123; HttpOnly; path=/")],
        );
        recorded.answer(
            Method::Get,
            "/api/v2/torrents/info",
            200,
            include_str!("../../tests/fixtures/qbit_torrents.json"),
        );
        let q = qbit(recorded.clone(), true);
        let torrents = q.downloads().await.unwrap();
        assert_eq!(torrents.len(), 4);
        let first = &torrents[0];
        assert_eq!(
            first.download_id,
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1"
        );
        assert_eq!(first.id, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1");
        assert_eq!(first.category.as_deref(), Some("radarr"));
        assert_eq!(first.state, "uploading");
        assert_eq!(first.status, DownloadStatus::Seeding);
        assert_eq!(first.protocol, Protocol::Torrent);
        assert_eq!(first.progress, 1.0);
        assert!(first.completed_at.is_some() && first.added_at.is_some());
        assert_eq!(first.seeding_seconds, 86400);
        assert_eq!(first.ratio, Some(1.2));
        let downloading = &torrents[1];
        assert!(downloading.progress < 1.0);
        assert_eq!(downloading.status, DownloadStatus::Downloading);
        assert_eq!(
            downloading.completed_at, None,
            "a completion time of -1 is no date"
        );
        assert_eq!(torrents[2].status, DownloadStatus::Completed, "pausedUP");
        assert_eq!(torrents[3].category, None);
        assert_eq!(torrents[3].status, DownloadStatus::Seeding, "stalledUP");

        q.downloads().await.unwrap();
        assert_eq!(
            recorded.methods_seen(),
            vec![Method::Post, Method::Get, Method::Get],
            "the session is reused"
        );
        assert_eq!((q.kind(), q.protocol()), ("qbittorrent", Protocol::Torrent));
    }

    #[tokio::test]
    async fn an_ended_session_signs_in_again() {
        let recorded = Recorded::new();
        recorded.answer_with(
            Method::Post,
            LOGIN_PATH,
            200,
            "Ok.",
            &[("set-cookie", "QBT_SID_8080=fresh; path=/")],
        );
        recorded.answer(Method::Get, "/api/v2/torrents/info", 403, "Forbidden");
        let q = qbit(recorded.clone(), true);
        let err = q.downloads().await.unwrap_err();
        assert!(matches!(err, EngineError::Login { .. }));
        assert_eq!(
            recorded.methods_seen(),
            vec![Method::Post, Method::Get, Method::Post, Method::Get]
        );
    }

    #[tokio::test]
    async fn a_wrong_password_is_named() {
        let recorded = Recorded::new();
        recorded.answer(Method::Post, LOGIN_PATH, 200, "Fails.");
        let q = qbit(recorded, true);
        let err = q.downloads().await.unwrap_err();
        assert!(err.to_string().contains("QBIT_USER"), "{err}");
    }

    #[tokio::test]
    async fn without_credentials_it_only_lists() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/api/v2/torrents/info", 200, "[]");
        let q = qbit(recorded.clone(), false);
        assert!(q.downloads().await.unwrap().is_empty());
        assert_eq!(recorded.methods_seen(), vec![Method::Get]);
    }

    #[test]
    fn unix_times_and_cookies_are_read() {
        assert!(unix(Some(1_763_800_000)).is_some());
        assert_eq!(unix(Some(-1)), None);
        assert_eq!(unix(Some(0)), None);
        assert_eq!(unix(Some(4_294_967_295)), None);
        let res = OutResponse {
            status: 200,
            headers: vec![
                ("set-cookie".into(), "theme=dark; path=/".into()),
                ("Set-Cookie".into(), "QBT_SID_8080=xyz; HttpOnly".into()),
            ],
            body: vec![],
        };
        assert_eq!(session_cookie(&res).as_deref(), Some("QBT_SID_8080=xyz"));
    }

    /// Every state qBittorrent names, in the product's words. The stage a title's queue line
    /// shows follows from these (`api::metadata`).
    #[test]
    fn every_state_has_a_status() {
        for (state, status) in [
            ("downloading", DownloadStatus::Downloading),
            ("forcedDL", DownloadStatus::Downloading),
            ("forcedMetaDL", DownloadStatus::Downloading),
            ("metaDL", DownloadStatus::Queued),
            ("queuedDL", DownloadStatus::Queued),
            ("queuedUP", DownloadStatus::Queued),
            ("stalledDL", DownloadStatus::Stalled),
            ("pausedDL", DownloadStatus::Paused),
            ("stoppedDL", DownloadStatus::Paused),
            ("pausedUP", DownloadStatus::Paused),
            ("stoppedUP", DownloadStatus::Paused),
            ("uploading", DownloadStatus::Seeding),
            ("forcedUP", DownloadStatus::Seeding),
            ("stalledUP", DownloadStatus::Seeding),
            ("checkingDL", DownloadStatus::Queued),
            ("checkingUP", DownloadStatus::Checking),
            ("checkingResumeData", DownloadStatus::Checking),
            ("moving", DownloadStatus::Unpacking),
            ("error", DownloadStatus::Failed),
            ("missingFiles", DownloadStatus::Failed),
        ] {
            assert_eq!(status_of(state, 0.5), status, "{state}");
        }
        assert_eq!(status_of("pausedUP", 1.0), DownloadStatus::Completed);
        assert_eq!(status_of("stoppedUP", 1.0), DownloadStatus::Completed);
        assert_eq!(status_of("unknown", 0.5), DownloadStatus::Downloading);
        assert_eq!(status_of("unknown", 1.0), DownloadStatus::Completed);
    }

    /// The sign-in is the one call beyond GET. Nothing else that is not a GET passes.
    #[test]
    fn the_calls_are_the_sign_in_and_gets() {
        let mut allow = Allowlist::default();
        allow.allow_integration(ENGINE_ID, BASE, calls()).unwrap();
        assert!(
            allow
                .check(Method::Post, &format!("{BASE}{LOGIN_PATH}"))
                .is_ok()
        );
        assert!(
            allow
                .check(Method::Get, &format!("{BASE}{TORRENTS_PATH}"))
                .is_ok()
        );
        for path in [
            "/api/v2/torrents/delete",
            "/api/v2/torrents/pause",
            "/api/v2/torrents/add",
            "/api/v2/app/shutdown",
        ] {
            assert!(
                allow.check(Method::Post, &format!("{BASE}{path}")).is_err(),
                "{path}"
            );
        }
        assert!(ACTIONS.is_empty());
    }
}
