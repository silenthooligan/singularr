//! The download client layer: the adapter of each type behind [`DownloadClient`], the calls
//! each declares, and the readings the adapters share.
//!
//! | Type | Protocol | Calls | Sign-in |
//! |---|---|---|---|
//! | qBittorrent | torrent | `GET /api/v2/torrents/info` | `POST /api/v2/auth/login`, a cookie |
//! | Transmission | torrent | `POST /transmission/rpc` with `torrent-get` | basic, and the `X-Transmission-Session-Id` handshake |
//! | Deluge | torrent | `POST /json` with `web.update_ui` | `auth.login` on the same path, a cookie |
//! | SABnzbd | usenet | `GET /api` with `mode` `queue` or `history` | the API key in the query |
//! | NZBGet | usenet | `POST /jsonrpc` with `listgroups`, `history`, `status` | basic |
//!
//! Every call reads. No adapter declares an act. Every download client is read-only whatever
//! its access says ([`super::access`]).
//!
//! A usenet client's history is read over a window: the jobs that finished in the last
//! [`HISTORY_DAYS`] days, at most a limit of them ([`history_window`]). The listing carries the
//! window's edge. A job past the edge is not listed. Its absence is no removal.

use super::access::Action;
use super::deluge::Deluge;
use super::nzbget::Nzbget;
use super::qbit::Qbit;
use super::sabnzbd::Sabnzbd;
use super::transmission::Transmission;
use super::{ClientDownload, DownloadClient};
use crate::core::config::{ClientKind, DownloadClientConfig};
use crate::core::outbound::{Calls, Outbound};
use chrono::{DateTime, TimeDelta, Utc};
use std::cmp::Reverse;
use std::sync::Arc;

/// How far back a usenet client's history is read, in days. A job that finished earlier is past
/// the window.
pub const HISTORY_DAYS: i64 = 7;

/// The calls the adapter of a type declares: what its host takes beyond GET, and which GETs
/// where the service takes only some.
pub fn calls(kind: ClientKind) -> Calls {
    match kind {
        ClientKind::Qbittorrent => super::qbit::calls(),
        ClientKind::Transmission => super::transmission::calls(),
        ClientKind::Deluge => super::deluge::calls(),
        ClientKind::Sabnzbd => super::sabnzbd::calls(),
        ClientKind::Nzbget => super::nzbget::calls(),
    }
}

/// The actions the adapter of a type declares. None for every download client.
pub fn actions(kind: ClientKind) -> &'static [Action] {
    match kind {
        ClientKind::Qbittorrent => super::qbit::ACTIONS,
        ClientKind::Transmission => super::transmission::ACTIONS,
        ClientKind::Deluge => super::deluge::ACTIONS,
        ClientKind::Sabnzbd => super::sabnzbd::ACTIONS,
        ClientKind::Nzbget => super::nzbget::ACTIONS,
    }
}

impl DownloadClientConfig {
    /// It is asked once it has an address and the secret its type's manifest needs.
    pub fn configured(&self) -> bool {
        self.url.is_some() && (self.secret.is_some() || self.kind.integration().secret_optional())
    }
}

/// The adapter of one download client integration. `None` while it is not configured: no
/// address, or no secret where its type needs one.
pub fn adapter(config: &DownloadClientConfig, out: &Outbound) -> Option<Arc<dyn DownloadClient>> {
    if !config.configured() {
        return None;
    }
    let out = out.clone();
    let made: Arc<dyn DownloadClient> = match config.kind {
        ClientKind::Qbittorrent => Arc::new(Qbit::new(config, out)?),
        ClientKind::Transmission => Arc::new(Transmission::new(config, out)?),
        ClientKind::Deluge => Arc::new(Deluge::new(config, out)?),
        ClientKind::Sabnzbd => Arc::new(Sabnzbd::new(config, out)?),
        ClientKind::Nzbget => Arc::new(Nzbget::new(config, out)?),
    };
    Some(made)
}

/// The jobs of a usenet client's history that its window holds, and the window's edge.
///
/// A job still being worked on (no completion time) is kept. Of the finished jobs, those that
/// completed in the last [`HISTORY_DAYS`] days are kept, newest first and at most `limit`. The
/// edge is the start of the window. When the client's answer held only part of its history
/// (`cut`) or `limit` left jobs out, the edge is the oldest finished job kept, if that is later.
pub fn history_window(
    jobs: Vec<ClientDownload>,
    now: DateTime<Utc>,
    limit: usize,
    cut: bool,
) -> (Vec<ClientDownload>, DateTime<Utc>) {
    let start = now - TimeDelta::days(HISTORY_DAYS);
    let (mut finished, mut kept): (Vec<ClientDownload>, Vec<ClientDownload>) =
        jobs.into_iter().partition(|j| j.completed_at.is_some());
    finished.retain(|j| j.completed_at.is_some_and(|t| t >= start));
    finished.sort_by_key(|j| Reverse(j.completed_at));
    let cut = cut || finished.len() > limit;
    finished.truncate(limit);
    let edge = match finished.last().and_then(|j| j.completed_at) {
        Some(oldest) if cut => oldest.max(start),
        _ => start,
    };
    kept.extend(finished);
    (kept, edge)
}

/// The readers of a client's answers: the SDK's, at their old paths.
pub use singularr_sdk::read::{count, number, safe_name, text, tracker_host, unix};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::Allowlist;

    #[test]
    fn readings_are_lenient_and_say_nothing_for_nothing() {
        assert_eq!(
            tracker_host("http://Tracker.Example.org:6969/announce").as_deref(),
            Some("tracker.example.org")
        );
        assert_eq!(
            tracker_host("udp://open.example.net:1337").as_deref(),
            Some("open.example.net")
        );
        assert_eq!(
            tracker_host("tracker.invalid").as_deref(),
            Some("tracker.invalid")
        );
        assert_eq!(tracker_host(""), None);
        assert_eq!(tracker_host("not a host"), None);
        assert_eq!(number(&serde_json::json!("4.39")), Some(4.39));
        assert_eq!(number(&serde_json::json!(7)), Some(7.0));
        assert_eq!(number(&serde_json::json!(null)), None);
        assert_eq!(count(Some(-1)), None);
        assert_eq!(count(Some(5_000_000_000)), Some(i32::MAX));
        assert_eq!(text(Some(&serde_json::json!("  "))), None);
    }

    /// A name that is an address keeps no key: no query, no fragment, no user and password.
    #[test]
    fn a_name_that_is_an_address_keeps_no_key() {
        for (name, kept) in [
            (
                "Trying to fetch NZB from https://indexer.example/api?t=get&id=7&apikey=K3Y",
                "Trying to fetch NZB from https://indexer.example/api",
            ),
            (
                "https://user:K3Y@indexer.example/getnzb/7.nzb#x",
                "https://indexer.example/getnzb/7.nzb",
            ),
            (
                "magnet:?xt=urn:btih:ABC&tr=https://t.example/announce?passkey=K3Y",
                "magnet:",
            ),
            (
                "Tears.of.Steel.S01E02.1080p.WEB-DL.x264-FX",
                "Tears.of.Steel.S01E02.1080p.WEB-DL.x264-FX",
            ),
            ("What If...? S01E01", "What If...? S01E01"),
        ] {
            assert_eq!(safe_name(name), kept, "{name}");
        }
    }

    fn finished(id: &str, at: Option<DateTime<Utc>>) -> ClientDownload {
        ClientDownload {
            id: id.to_string(),
            download_id: id.to_ascii_uppercase(),
            protocol: super::super::Protocol::Usenet,
            name: id.to_string(),
            category: None,
            status: super::super::DownloadStatus::Completed,
            state: "Completed".to_string(),
            progress: 1.0,
            size_bytes: 1,
            downloaded_bytes: None,
            added_at: None,
            completed_at: at,
            save_path: None,
            speed: None,
            upload_speed: None,
            eta: None,
            tracker: None,
            seeds: None,
            peers: None,
            ratio: None,
            seeding_seconds: 0,
        }
    }

    /// The window keeps the jobs in motion and the newest finished ones of its days. Its edge
    /// is its start, or the oldest job kept when jobs were left out.
    #[test]
    fn a_usenet_history_is_read_over_its_window() {
        let now = DateTime::from_timestamp(1_795_600_000, 0).unwrap();
        let ago = |hours: i64| Some(now - TimeDelta::hours(hours));
        let start = now - TimeDelta::days(HISTORY_DAYS);
        let jobs = || {
            vec![
                finished("old", ago(24 * 8)),
                finished("b", ago(5)),
                finished("working", None),
                finished("a", ago(1)),
                finished("c", ago(30)),
            ]
        };
        let ids = |kept: &[ClientDownload]| -> Vec<String> {
            kept.iter().map(|j| j.id.clone()).collect()
        };
        let (kept, edge) = history_window(jobs(), now, 10, false);
        assert_eq!(ids(&kept), ["working", "a", "b", "c"]);
        assert_eq!(edge, start);
        // A limit leaves the oldest out: the edge moves to the oldest kept.
        let (kept, edge) = history_window(jobs(), now, 2, false);
        assert_eq!(ids(&kept), ["working", "a", "b"]);
        assert_eq!(Some(edge), ago(5));
        // An answer that held part of the history moves it the same way.
        let (_, edge) = history_window(jobs(), now, 10, true);
        assert_eq!(Some(edge), ago(30));
        // Never before the window's start.
        let (_, edge) = history_window(vec![finished("old", ago(24 * 8))], now, 10, true);
        assert_eq!(edge, start);
    }

    /// Each type's adapter is made only once it can be asked. Every adapter is read-only.
    #[test]
    fn an_adapter_is_made_per_configured_client_and_none_can_act() {
        let out = Outbound::with_transport(
            Allowlist::default(),
            crate::core::outbound::recorded::Recorded::new(),
        );
        for kind in ClientKind::ALL {
            let config = |url: Option<&str>, secret: Option<&str>| DownloadClientConfig {
                id: kind.as_str().to_string(),
                kind,
                url: url.map(str::to_string),
                user: None,
                secret: secret.map(str::to_string),
            };
            let made = adapter(&config(Some("http://10.0.0.62:9000"), Some("s")), &out)
                .unwrap_or_else(|| panic!("{}", kind.as_str()));
            assert_eq!((made.id(), made.kind()), (kind.as_str(), kind.as_str()));
            assert_eq!(
                made.protocol() == super::super::Protocol::Usenet,
                kind.usenet()
            );
            assert!(adapter(&config(None, Some("s")), &out).is_none());
            // Deluge and SABnzbd need their secret; the others can run without one.
            let bare = adapter(&config(Some("http://10.0.0.62:9000"), None), &out);
            assert_eq!(
                bare.is_some(),
                !matches!(kind, ClientKind::Deluge | ClientKind::Sabnzbd),
                "{}",
                kind.as_str()
            );
            assert!(actions(kind).is_empty(), "{}", kind.as_str());
            assert!(calls(kind).acts.is_empty(), "{}", kind.as_str());
        }
    }
}
