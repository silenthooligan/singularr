//! The qBittorrent adapter (Web API v2), read-only.
//!
//! Two endpoints: `POST /api/v2/auth/login` (the one POST stage 1 makes toward an engine)
//! and `GET /api/v2/torrents/info`.

use super::arr::null_as_default;
use super::{Downloader, EngineError, Torrent};
use crate::core::config::QbitConfig;
use crate::core::outbound::{Body, OutRequest, OutResponse, Outbound, expect_ok};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use tokio::sync::Mutex;

/// The type of the download client integration, and the id of the environment form's.
pub const ENGINE_ID: &str = "qbittorrent";
pub const LOGIN_PATH: &str = "/api/v2/auth/login";

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
    added_on: Option<i64>,
    completion_on: Option<i64>,
    seeding_time: Option<i64>,
    save_path: Option<String>,
    dlspeed: Option<i64>,
    eta: Option<i64>,
    num_seeds: Option<i64>,
    num_leechs: Option<i64>,
}

/// qBittorrent writes 8640000 seconds (100 days) for a time left it cannot tell.
const ETA_UNKNOWN: i64 = 8_640_000;

/// A count or a rate as the client reports it: negative is none.
fn count(v: Option<i64>) -> Option<i32> {
    v.filter(|n| *n >= 0)
        .map(|n| i32::try_from(n).unwrap_or(i32::MAX))
}

/// A Unix time from qBittorrent. Zero, negative and the 32-bit "never" are no date.
fn unix(seconds: Option<i64>) -> Option<DateTime<Utc>> {
    let s = seconds.filter(|s| *s > 0 && *s < 4_000_000_000)?;
    DateTime::from_timestamp(s, 0)
}

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

impl Qbit {
    /// `None` when no URL is configured.
    pub fn new(config: &QbitConfig, out: Outbound) -> Option<Self> {
        Some(Qbit {
            id: config.id.clone(),
            base: config.url.clone()?,
            user: config.user.clone(),
            pass: config.pass.clone(),
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
        let mut req = OutRequest::get(format!("{}/api/v2/torrents/info", self.base));
        if let Some(cookie) = cookie {
            req = req.header("Cookie", cookie);
        }
        Ok(self.out.send(req).await?)
    }
}

#[async_trait]
impl Downloader for Qbit {
    fn id(&self) -> &str {
        &self.id
    }

    async fn torrents(&self) -> Result<Vec<Torrent>, EngineError> {
        let url = format!("{}/api/v2/torrents/info", self.base);
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
        Ok(raw
            .into_iter()
            .map(|t| Torrent {
                hash: t.hash.trim().to_ascii_uppercase(),
                name: t.name,
                category: t
                    .category
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty()),
                state: t.state,
                progress: t.progress.clamp(0.0, 1.0),
                size_bytes: t.size.unwrap_or(0),
                added_at: unix(t.added_on),
                completed_at: unix(t.completion_on),
                seeding_seconds: t.seeding_time.unwrap_or(0).max(0),
                save_path: t
                    .save_path
                    .map(|p| p.trim().to_string())
                    .filter(|p| !p.is_empty()),
                speed: count(t.dlspeed),
                eta: count(t.eta.filter(|e| *e < ETA_UNKNOWN)),
                seeds: count(t.num_seeds),
                peers: count(t.num_leechs),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method};
    use std::sync::Arc;

    const BASE: &str = "http://192.0.2.62:8080";

    fn qbit(recorded: Arc<Recorded>, with_login: bool) -> Qbit {
        let mut allow = Allowlist::default();
        allow.allow_engine(ENGINE_ID, BASE, &[LOGIN_PATH]).unwrap();
        let config = QbitConfig {
            id: ENGINE_ID.to_string(),
            url: Some(BASE.to_string()),
            user: with_login.then(|| "admin".to_string()),
            pass: with_login.then(|| "test-password".to_string()),
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
        let torrents = q.torrents().await.unwrap();
        assert_eq!(torrents.len(), 4);
        let first = &torrents[0];
        assert_eq!(first.hash, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1");
        assert_eq!(first.category.as_deref(), Some("radarr"));
        assert_eq!(first.state, "uploading");
        assert_eq!(first.progress, 1.0);
        assert!(first.completed_at.is_some() && first.added_at.is_some());
        assert_eq!(first.seeding_seconds, 86400);
        let downloading = &torrents[1];
        assert!(downloading.progress < 1.0);
        assert_eq!(
            downloading.completed_at, None,
            "a completion time of -1 is no date"
        );
        assert_eq!(torrents[3].category, None);

        q.torrents().await.unwrap();
        assert_eq!(
            recorded.methods_seen(),
            vec![Method::Post, Method::Get, Method::Get],
            "the session is reused"
        );
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
        let err = q.torrents().await.unwrap_err();
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
        let err = q.torrents().await.unwrap_err();
        assert!(err.to_string().contains("QBIT_USER"), "{err}");
    }

    #[tokio::test]
    async fn without_credentials_it_only_lists() {
        let recorded = Recorded::new();
        recorded.answer(Method::Get, "/api/v2/torrents/info", 200, "[]");
        let q = qbit(recorded.clone(), false);
        assert!(q.torrents().await.unwrap().is_empty());
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
}
