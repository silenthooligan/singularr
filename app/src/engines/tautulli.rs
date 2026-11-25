//! The Tautulli adapter, read-only, behind [`PlayHistory`]: the media server's plays as
//! Tautulli kept them, for years, with durations, devices and delivery.
//!
//! Every call is a GET of `/api/v2` with one command from a closed list of reads
//! ([`Command`]): `get_history` (paged with `start` and `length`, one row per sitting with
//! `grouping=0`, `after` for the newest days), `get_users` (who is the owner) and
//! `get_tautulli_info` (its version). Tautulli's API also has commands that change or delete
//! things; this adapter has no way to name one.
//!
//! The key travels in the query string, as Tautulli wants it. It never reaches a log line, an
//! error or the journal: `core::outbound` logs the path without the query, and every error this
//! adapter returns has the key replaced ([`Tautulli::scrubbed`]).
//!
//! Answers are read leniently: Tautulli writes numbers as numbers or as text, and an empty
//! field as `""`.

use super::{EngineError, EngineInfo, HistoryPage, HistoryPlay, HistoryUser, PlayHistory};
use crate::core::config::TautulliConfig;
use crate::core::outbound::{
    Calls, OutRequest, Outbound, OutboundError, encode_component, expect_ok,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;

/// The kind of engine Tautulli is, in the engine list.
pub const KIND: &str = "tautulli";
/// Where Tautulli's API answers, below its address.
pub const API_PATH: &str = "/api/v2";
/// Plays asked for per page, at most.
pub const PAGE_MAX: u32 = 1000;

/// The commands this adapter sends. Every one only reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    History,
    Users,
    Info,
}

impl Command {
    pub const ALL: [Command; 3] = [Command::History, Command::Users, Command::Info];

    pub fn name(self) -> &'static str {
        match self {
            Command::History => "get_history",
            Command::Users => "get_users",
            Command::Info => "get_tautulli_info",
        }
    }
}

/// The calls Tautulli is sent: a GET of its API with one of the read commands, and nothing
/// else. Tautulli's API also takes, as GETs, commands that change or delete things (it ends
/// sessions, deletes history, restarts), so its host takes these GETs alone
/// (`core::outbound`).
pub fn calls() -> Calls {
    let commands: Vec<&str> = Command::ALL.iter().map(|c| c.name()).collect();
    Calls::default().get_when(API_PATH, "cmd", &commands)
}

pub struct Tautulli {
    id: String,
    base: String,
    key: String,
    out: Outbound,
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
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
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
}

fn unix(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    int(v, key)
        .filter(|s| *s > 0 && *s < 4_000_000_000)
        .and_then(|s| DateTime::from_timestamp(s, 0))
}

/// Tautulli's word for how a play was delivered, in the product's words.
pub fn decision_word(raw: &str) -> Option<String> {
    let word = match raw.trim().to_ascii_lowercase().as_str() {
        "direct play" | "directplay" => "direct play",
        "copy" | "direct stream" | "directstream" => "direct stream",
        "transcode" => "transcode",
        _ => return None,
    };
    Some(word.to_string())
}

/// One row of `get_history`. A row without an id or a start is left out.
pub fn history_play(v: &Value) -> Option<HistoryPlay> {
    let id = int(v, "id").or_else(|| int(v, "row_id"))?;
    let kind = text(v, "media_type").unwrap_or_else(|| "other".to_string());
    let episode = kind == "episode";
    let small = |key: &str| int(v, key).and_then(|n| i32::try_from(n).ok());
    let location = text(v, "location").map(|l| l.to_ascii_lowercase());
    let relayed = int(v, "relayed").is_some_and(|r| r != 0);
    Some(HistoryPlay {
        id,
        reference_id: int(v, "reference_id"),
        user_id: int(v, "user_id"),
        user: text(v, "friendly_name").or_else(|| text(v, "user")),
        rating_key: text(v, "rating_key"),
        show_key: text(v, "grandparent_rating_key"),
        title: text(v, "title").or_else(|| text(v, "full_title"))?,
        show: text(v, "grandparent_title"),
        season: if episode {
            small("parent_media_index")
        } else {
            None
        },
        episode: if episode { small("media_index") } else { None },
        year: small("year"),
        started: unix(v, "started").or_else(|| unix(v, "date"))?,
        stopped: unix(v, "stopped"),
        played_secs: int(v, "play_duration").or_else(|| int(v, "duration")),
        paused_secs: int(v, "paused_counter"),
        percent: float(v, "percent_complete"),
        player: text(v, "player"),
        platform: text(v, "platform"),
        product: text(v, "product"),
        address: text(v, "ip_address"),
        local: match location.as_deref() {
            Some("lan") => Some(!relayed),
            Some("wan" | "cellular") => Some(false),
            _ => None,
        },
        decision: text(v, "transcode_decision").and_then(|d| decision_word(&d)),
        kind,
        raw: v.clone(),
    })
}

/// The answer of `get_history`: its rows and how many match.
pub fn history_page(v: &Value) -> HistoryPage {
    let data = v
        .get("response")
        .and_then(|r| r.get("data"))
        .unwrap_or(&Value::Null);
    let rows = data.get("data").and_then(Value::as_array);
    HistoryPage {
        plays: rows
            .map(|rows| rows.iter().filter_map(history_play).collect())
            .unwrap_or_default(),
        total: int(data, "recordsFiltered")
            .or_else(|| int(data, "recordsTotal"))
            .unwrap_or(0),
    }
}

/// The answer of `get_users`.
pub fn users(v: &Value) -> Vec<HistoryUser> {
    v.get("response")
        .and_then(|r| r.get("data"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|u| {
                    Some(HistoryUser {
                        id: int(u, "user_id")?,
                        name: text(u, "friendly_name").or_else(|| text(u, "username")),
                        admin: int(u, "is_admin").is_some_and(|a| a != 0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl Tautulli {
    /// `None` when the integration has no address or no key.
    pub fn new(config: &TautulliConfig, out: Outbound) -> Option<Self> {
        Some(Tautulli {
            id: config.id.clone(),
            base: config.url.clone()?,
            key: config.api_key.clone()?,
            out,
        })
    }

    /// The address of one command, its key and its parameters encoded.
    fn address(&self, command: Command, params: &[(&str, String)]) -> String {
        let mut url = format!(
            "{}{API_PATH}?apikey={}&cmd={}",
            self.base,
            encode_component(&self.key),
            command.name()
        );
        for (name, value) in params {
            url.push('&');
            url.push_str(name);
            url.push('=');
            url.push_str(&encode_component(value));
        }
        url
    }

    /// Text with the key replaced, as it is written and as it travels.
    pub fn scrubbed(&self, text: &str) -> String {
        if self.key.is_empty() {
            return text.to_string();
        }
        text.replace(&encode_component(&self.key), "***")
            .replace(&self.key, "***")
    }

    /// An error of a call with every text in it scrubbed.
    fn clean(&self, e: OutboundError) -> EngineError {
        let s = |t: String| self.scrubbed(&t);
        EngineError::Outbound(match e {
            OutboundError::BadUrl(u) => OutboundError::BadUrl(s(u)),
            OutboundError::HostNotAllowed { host } => {
                OutboundError::HostNotAllowed { host: s(host) }
            }
            OutboundError::NotListed {
                method,
                host,
                label,
                path,
            } => OutboundError::NotListed {
                method,
                host: s(host),
                label,
                path: s(path),
            },
            OutboundError::NotAllowed {
                method,
                host,
                label,
                path,
            } => OutboundError::NotAllowed {
                method,
                host: s(host),
                label,
                path: s(path),
            },
            OutboundError::Transport { host, message } => OutboundError::Transport {
                host: s(host),
                message: s(message),
            },
            OutboundError::Status { host, path, status } => OutboundError::Status {
                host: s(host),
                path: s(path),
                status,
            },
            OutboundError::Decode(m) => OutboundError::Decode(s(m)),
        })
    }

    /// One command, its answer read as JSON, `response.result` checked.
    async fn ask(
        &self,
        what: &'static str,
        command: Command,
        params: &[(&str, String)],
    ) -> Result<Value, EngineError> {
        let url = self.address(command, params);
        let res = self
            .out
            .send(OutRequest::get(&url))
            .await
            .map_err(|e| self.clean(e))?;
        expect_ok(&url, &res).map_err(|e| self.clean(e))?;
        let v: Value = serde_json::from_slice(&res.body).map_err(|e| EngineError::Decode {
            engine: self.id.clone(),
            what,
            message: self.scrubbed(&e.to_string()),
        })?;
        let response = v.get("response").unwrap_or(&Value::Null);
        match text(response, "result").as_deref() {
            Some("success") => Ok(v),
            _ => {
                let said = text(response, "message").unwrap_or_default();
                if said.to_ascii_lowercase().contains("apikey")
                    || said.to_ascii_lowercase().contains("api key")
                {
                    Err(EngineError::Login {
                        engine: self.id.clone(),
                        message: "invalid API key".to_string(),
                    })
                } else {
                    Err(EngineError::Decode {
                        engine: self.id.clone(),
                        what,
                        message: if said.is_empty() {
                            "result is not success".to_string()
                        } else {
                            self.scrubbed(&said)
                        },
                    })
                }
            }
        }
    }
}

#[async_trait]
impl PlayHistory for Tautulli {
    fn id(&self) -> &str {
        &self.id
    }

    async fn status(&self) -> Result<EngineInfo, EngineError> {
        let v = self.ask("info", Command::Info, &[]).await?;
        let data = v
            .get("response")
            .and_then(|r| r.get("data"))
            .unwrap_or(&Value::Null);
        Ok(EngineInfo {
            app: Some("Tautulli".to_string()),
            version: text(data, "tautulli_version"),
        })
    }

    async fn users(&self) -> Result<Vec<HistoryUser>, EngineError> {
        Ok(users(&self.ask("users", Command::Users, &[]).await?))
    }

    async fn plays(
        &self,
        start: u32,
        length: u32,
        after: Option<NaiveDate>,
    ) -> Result<HistoryPage, EngineError> {
        let mut params = vec![
            ("grouping", "0".to_string()),
            ("start", start.to_string()),
            ("length", length.clamp(1, PAGE_MAX).to_string()),
        ];
        if let Some(day) = after {
            params.push(("after", day.format("%Y-%m-%d").to_string()));
        }
        Ok(history_page(
            &self.ask("history", Command::History, &params).await?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method};
    use std::sync::{Arc, Mutex};

    const BASE: &str = "http://10.0.0.50:8181";
    const KEY: &str = "0123456789abcdef0123456789abcdef";

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn allowlist() -> Allowlist {
        let mut allow = Allowlist::default();
        allow.allow_integration("tautulli", BASE, calls()).unwrap();
        allow
    }

    fn tautulli(recorded: Arc<Recorded>) -> Tautulli {
        let allow = allowlist();
        Tautulli::new(
            &TautulliConfig {
                id: "tautulli".to_string(),
                url: Some(BASE.to_string()),
                api_key: Some(KEY.to_string()),
            },
            Outbound::with_transport(allow, recorded),
        )
        .unwrap()
    }

    /// Every log line written while the closure's future runs, as text.
    #[derive(Clone, Default)]
    struct Lines(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Lines {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
        type Writer = Lines;
        fn make_writer(&'a self) -> Lines {
            self.clone()
        }
    }

    #[tokio::test]
    async fn plays_and_users_are_read_with_get_and_read_commands_alone() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "cmd=get_history",
            200,
            &fixture("tautulli_history.json"),
        );
        recorded.answer(
            Method::Get,
            "cmd=get_users",
            200,
            &fixture("tautulli_users.json"),
        );
        let t = tautulli(recorded.clone());
        let page = t
            .plays(0, 40, NaiveDate::from_ymd_opt(2026, 11, 1))
            .await
            .unwrap();
        assert_eq!((page.plays.len(), page.total), (40, 4264));
        let first = &page.plays[0];
        assert_eq!(
            (
                first.id,
                first.reference_id,
                first.user_id,
                first.rating_key.as_deref()
            ),
            (6680, Some(6678), Some(881_961_598), Some("138963"))
        );
        assert_eq!(
            (
                first.started.timestamp(),
                first.stopped.map(|s| s.timestamp()),
                first.played_secs,
                first.paused_secs
            ),
            (1_795_487_128, Some(1_795_491_390), Some(4252), Some(9))
        );
        assert_eq!(
            (
                first.decision.as_deref(),
                first.local,
                first.platform.as_deref(),
                first.product.as_deref()
            ),
            (
                Some("transcode"),
                Some(false),
                Some("Tizen"),
                Some("Plex for Samsung")
            )
        );
        assert_eq!(first.percent, Some(54.0));
        let episode = page.plays.iter().find(|p| p.id == 6648).unwrap();
        assert_eq!(
            (
                episode.kind.as_str(),
                episode.show.as_deref(),
                episode.show_key.as_deref(),
                episode.season,
                episode.episode
            ),
            ("episode", Some("Friends"), Some("8565"), Some(8), Some(16))
        );
        // A play on the home network.
        let home = page.plays.iter().find(|p| p.id == 6637).unwrap();
        assert_eq!(home.local, Some(true));
        let users = t.users().await.unwrap();
        let owner: Vec<i64> = users.iter().filter(|u| u.admin).map(|u| u.id).collect();
        assert_eq!(owner, vec![16_289_551]);
        // GET alone, each with one of the read commands, the history in sittings.
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
        for url in recorded.urls_seen() {
            assert!(
                url.starts_with(&format!("{BASE}/api/v2?apikey={KEY}&cmd=")),
                "{url}"
            );
            assert!(
                Command::ALL
                    .iter()
                    .any(|c| url.contains(&format!("&cmd={}", c.name()))),
                "{url}"
            );
        }
        let history = recorded
            .urls_seen()
            .into_iter()
            .find(|u| u.contains("get_history"))
            .unwrap();
        assert!(
            history.contains("grouping=0") && history.contains("after=2026-11-01"),
            "{history}"
        );
    }

    #[tokio::test]
    async fn only_the_read_commands_reach_tautulli() {
        let lines = Lines::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(lines.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v2",
            200,
            &fixture("tautulli_history.json"),
        );
        let out = Outbound::with_transport(allowlist(), recorded.clone());
        let at = |rest: &str| format!("{BASE}/api/v2?apikey={KEY}{rest}");
        // A read command passes.
        out.send(OutRequest::get(at("&cmd=get_history&grouping=0")))
            .await
            .unwrap();
        assert_eq!(recorded.urls_seen().len(), 1);
        // Any other command, none at all, or the API elsewhere is refused before a byte is
        // sent, and the refusal names no key.
        for url in [
            at("&cmd=delete_history&row_ids=1"),
            at("&cmd=terminate_session&session_key=1"),
            at("&cmd=restart"),
            at("&cmd=delete_all_history"),
            at("&cmd=update"),
            at(""),
            at("&cmd=get_history&cmd=restart"),
            format!("{BASE}/api/v3?apikey={KEY}&cmd=get_history"),
        ] {
            let err = out.send(OutRequest::get(&url)).await.unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{url}: {err:?}"
            );
            assert!(!err.to_string().contains(KEY), "{err}");
        }
        assert_eq!(recorded.urls_seen().len(), 1, "a refused call went out");
        let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("refused"), "{logged}");
        assert!(
            !logged.contains(KEY) && !logged.contains("apikey"),
            "{logged}"
        );
        // Every command the adapter has is a read the host takes.
        let declared = calls();
        assert_eq!(declared.gets.len(), 1);
        assert!(declared.reads.is_empty() && declared.acts.is_empty());
        let query = declared.gets[0].query.as_ref().unwrap();
        for c in Command::ALL {
            assert!(query.values.iter().any(|v| v == c.name()), "{}", c.name());
        }
    }

    #[tokio::test]
    async fn the_key_is_never_in_a_log_line_or_an_error() {
        let lines = Lines::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(lines.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let recorded = Recorded::new();
        // A refused key, a failed answer, an unreadable answer, no answer at all.
        recorded.answer(
            Method::Get,
            "cmd=get_tautulli_info",
            200,
            &format!(
                "{{\"response\": {{\"result\": \"error\", \"message\": \"Invalid apikey {KEY}\", \"data\": {{}}}}}}"
            ),
        );
        recorded.answer(Method::Get, "cmd=get_users", 500, "oops");
        recorded.answer(Method::Get, "cmd=get_history", 200, "not json");
        let t = tautulli(recorded.clone());
        let mut errors = Vec::new();
        errors.push(t.status().await.unwrap_err().to_string());
        errors.push(t.users().await.unwrap_err().to_string());
        errors.push(t.plays(0, 10, None).await.unwrap_err().to_string());
        let silent = Recorded::new();
        silent.fail(
            Method::Get,
            "cmd=get_users",
            "connection refused or unreachable",
        );
        errors.push(tautulli(silent).users().await.unwrap_err().to_string());
        let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("singularr::outbound"), "{logged}");
        assert!(!logged.contains(KEY), "{logged}");
        assert!(!logged.contains("apikey"), "{logged}");
        for e in &errors {
            assert!(!e.contains(KEY), "{e}");
        }
        assert!(errors[0].contains("invalid API key"), "{}", errors[0]);
        assert!(errors[1].contains("500"), "{}", errors[1]);
        // The calls did carry it: that is where Tautulli wants it.
        assert!(recorded.urls_seen().iter().all(|u| u.contains(KEY)));
        assert_eq!(t.scrubbed(&format!("x {KEY} y")), "x *** y");
    }
}
