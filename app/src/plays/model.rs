//! A play as the sources tell it, how two records of one play are merged, how a record finds
//! the stored play it belongs to, and how a session is followed from start to end. Pure: no
//! SQL and no calls.

use crate::engines::{HistoryPlay, ServerPlay, ServerSession};
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};

/// The setting that keeps the players' addresses (off: only home network or outside is kept).
pub const KEEP_ADDRESSES: &str = "jobs.plays.keepAddresses";
/// The setting that says how long plays are kept: `forever`, or a number of days.
pub const KEEP_FOR: &str = "jobs.plays.keepFor";
/// The setting that says how often the histories are read, in seconds.
pub const HISTORY_EVERY: &str = "jobs.plays.historyEvery";
pub const HISTORY_EVERY_DEFAULT: u64 = 900;

/// The media server's own account: the owner. A play history that names the owner by another
/// id (Tautulli by the owner's plex.tv id) is read as this one.
pub const OWNER_ACCOUNT: i64 = 1;

/// The words of a delivery.
pub const DIRECT_PLAY: &str = "direct play";
pub const DIRECT_STREAM: &str = "direct stream";
pub const TRANSCODE: &str = "transcode";

/// How far apart, in seconds, a point in time may lie from a play's span and still be in it:
/// the media server marks an item played a little after the last reading saw it.
const NEAR: i64 = 300;
/// The longest time between two readings of a session that is counted as watched or paused.
/// A longer one means the readings stopped, and what happened in it is not known.
const GAP_MAX: i64 = 120;

/// One play: who, what, when, where and how, as one source or several told it. Each source's
/// own record is kept in `raw`, under `live`, `server` or `tautulli`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct PlayRecord {
    /// The media server's integration id.
    pub server: String,
    /// Its kind: `plex`, `jellyfin`, `emby`.
    pub server_kind: Option<String>,
    pub rating_key: Option<String>,
    pub show_key: Option<String>,
    /// The catalog title it belongs to: the movie, or an episode's show.
    pub title_id: Option<i64>,
    /// `movie`, `episode`, `track`, or what the source says.
    pub kind: String,
    /// The movie's, the episode's or the track's title.
    pub title: String,
    pub show_title: Option<String>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
    pub year: Option<i32>,
    /// The media server's section, by its key.
    pub library_key: Option<String>,
    pub account_id: Option<i64>,
    pub account_name: Option<String>,
    pub started_at: DateTime<Utc>,
    pub stopped_at: Option<DateTime<Utc>>,
    /// Seconds it played, pauses left out.
    pub watched_secs: Option<i32>,
    pub paused_secs: Option<i32>,
    /// The item's own length.
    pub duration_secs: Option<i32>,
    /// 0 to 100: how far it got.
    pub progress: Option<f64>,
    /// The player's name, its device, platform and app.
    pub player: Option<String>,
    pub device: Option<String>,
    pub platform: Option<String>,
    pub product: Option<String>,
    /// The app's version and the platform's.
    pub product_version: Option<String>,
    pub platform_version: Option<String>,
    /// The device's own id on the media server.
    pub device_id: Option<String>,
    /// On the home network (true) or outside (false).
    pub local: Option<bool>,
    /// The client's address, the owner's to see. Kept while the setting keeps addresses (the
    /// default), never in a log and never in Singularr Requests.
    pub address: Option<String>,
    /// The connection was encrypted.
    pub secure: Option<bool>,
    /// The server's vendor relayed the connection (Plex).
    pub relayed: Option<bool>,
    /// What the stream took, kbit/s.
    pub bandwidth: Option<i64>,
    /// `direct play`, `direct stream` or `transcode`: the play as a whole, then its video and
    /// its audio.
    pub decision: Option<String>,
    pub video_decision: Option<String>,
    pub audio_decision: Option<String>,
    pub source_resolution: Option<String>,
    /// kbit/s.
    pub source_bitrate: Option<i64>,
    pub stream_resolution: Option<String>,
    pub stream_bitrate: Option<i64>,
    /// The file's codecs, audio channels and container, and what was sent.
    pub source_video_codec: Option<String>,
    pub source_audio_codec: Option<String>,
    pub source_audio_channels: Option<i32>,
    pub source_container: Option<String>,
    pub stream_video_codec: Option<String>,
    pub stream_audio_codec: Option<String>,
    pub stream_audio_channels: Option<i32>,
    pub stream_container: Option<String>,
    /// Why the server transcoded, in its words, where it says.
    pub transcode_reasons: Vec<String>,
    /// The sources that know it, each by its own id: the media server's session, its history
    /// entry, Tautulli's row.
    pub session_id: Option<String>,
    pub history_id: Option<i64>,
    pub tautulli_id: Option<i64>,
    /// When the media server marked it played.
    pub viewed_at: Option<DateTime<Utc>>,
    pub raw: Value,
}

/// A stored play.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Play {
    pub id: i64,
    pub updated_at: DateTime<Utc>,
    #[sqlx(flatten)]
    pub record: PlayRecord,
}

/// Which sources know a play.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sources {
    pub live: bool,
    pub server: bool,
    pub tautulli: bool,
}

impl Sources {
    /// The words of the sources, for the API: `live`, `server`, `tautulli`.
    pub fn words(self) -> Vec<&'static str> {
        [
            (self.live, "live"),
            (self.server, "server"),
            (self.tautulli, "tautulli"),
        ]
        .into_iter()
        .filter_map(|(on, w)| on.then_some(w))
        .collect()
    }
}

impl PlayRecord {
    pub fn sources(&self) -> Sources {
        Sources {
            live: self.session_id.is_some(),
            server: self.history_id.is_some(),
            tautulli: self.tautulli_id.is_some(),
        }
    }

    /// The catalog's key of what was played: an episode's show, else the item.
    pub fn item_key(&self) -> Option<&str> {
        self.show_key.as_deref().or(self.rating_key.as_deref())
    }

    /// The play with no address kept: neither its own nor any in the sources' records (a Plex
    /// session's player, a Jellyfin or Emby session's remote end, Tautulli's row).
    pub fn without_addresses(mut self) -> PlayRecord {
        self.address = None;
        if let Some(live) = self.raw.get_mut("live").and_then(Value::as_object_mut) {
            live.remove("RemoteEndPoint");
        }
        if let Some(player) = self
            .raw
            .get_mut("live")
            .and_then(|l| l.get_mut("Player"))
            .and_then(Value::as_object_mut)
        {
            player.remove("address");
            player.remove("remotePublicAddress");
        }
        if let Some(row) = self.raw.get_mut("tautulli").and_then(Value::as_object_mut) {
            row.remove("ip_address");
        }
        self
    }
}

/// How far a record is trusted for one group of fields: the best of its sources' ranks.
fn rank(s: Sources, live: u8, tautulli: u8, server: u8) -> u8 {
    let mut best = 0;
    if s.live {
        best = best.max(live);
    }
    if s.tautulli {
        best = best.max(tautulli);
    }
    if s.server {
        best = best.max(server);
    }
    best
}

/// One field of two records: the one trusted more wins; on a tie the newer reading does.
fn pick<T: Clone>(mine: &Option<T>, my_rank: u8, theirs: &Option<T>, their_rank: u8) -> Option<T> {
    match (mine, theirs) {
        (Some(_), Some(t)) if their_rank >= my_rank => Some(t.clone()),
        (Some(m), _) => Some(m.clone()),
        (None, t) => t.clone(),
    }
}

/// Two records of one play as one. Each group of fields comes from the source that knows it
/// best: the times from Tautulli (it saw the whole play), then a followed session, then the
/// media server's history (it knows only when it marked the item played); how it was delivered
/// and where from a followed session, then Tautulli; the person's name from the media server.
/// What one record lacks the other fills. Both sources' own records are kept.
pub fn merge(old: &PlayRecord, new: &PlayRecord) -> PlayRecord {
    let (a, b) = (old.sources(), new.sources());
    let (ta, tb) = (rank(a, 2, 3, 1), rank(b, 2, 3, 1));
    let (da, db) = (rank(a, 3, 2, 0), rank(b, 3, 2, 0));
    let (wa, wb) = (rank(a, 3, 2, 1), rank(b, 3, 2, 1));
    let (na, nb) = (rank(a, 3, 1, 2), rank(b, 3, 1, 2));
    let time = |x: &Option<i32>, y: &Option<i32>| pick(x, ta, y, tb);
    let item = |x: &Option<String>, y: &Option<String>| pick(x, wa, y, wb);
    let way = |x: &Option<String>, y: &Option<String>| pick(x, da, y, db);
    let mut raw: Map<String, Value> = old.raw.as_object().cloned().unwrap_or_default();
    if let Some(theirs) = new.raw.as_object() {
        for (k, v) in theirs {
            raw.insert(k.clone(), v.clone());
        }
    }
    let codec = |x: &Option<String>, y: &Option<String>| pick(x, da, y, db);
    let count = |x: &Option<i32>, y: &Option<i32>| pick(x, da, y, db);
    PlayRecord {
        server: old.server.clone(),
        server_kind: old.server_kind.clone().or_else(|| new.server_kind.clone()),
        rating_key: item(&old.rating_key, &new.rating_key),
        show_key: item(&old.show_key, &new.show_key),
        title_id: new.title_id.or(old.title_id),
        kind: if wb >= wa && !new.kind.is_empty() {
            new.kind.clone()
        } else {
            old.kind.clone()
        },
        title: if wb >= wa && !new.title.is_empty() {
            new.title.clone()
        } else {
            old.title.clone()
        },
        show_title: item(&old.show_title, &new.show_title),
        season: pick(&old.season, wa, &new.season, wb),
        episode: pick(&old.episode, wa, &new.episode, wb),
        year: pick(&old.year, wa, &new.year, wb),
        library_key: item(&old.library_key, &new.library_key),
        account_id: new.account_id.or(old.account_id),
        account_name: pick(&old.account_name, na, &new.account_name, nb),
        started_at: if tb >= ta {
            new.started_at
        } else {
            old.started_at
        },
        stopped_at: pick(&old.stopped_at, ta, &new.stopped_at, tb),
        watched_secs: time(&old.watched_secs, &new.watched_secs),
        paused_secs: time(&old.paused_secs, &new.paused_secs),
        duration_secs: pick(&old.duration_secs, da, &new.duration_secs, db),
        progress: match (old.progress, new.progress) {
            (Some(x), Some(y)) => Some(x.max(y)),
            (x, y) => x.or(y),
        },
        player: item(&old.player, &new.player),
        device: item(&old.device, &new.device),
        platform: item(&old.platform, &new.platform),
        product: item(&old.product, &new.product),
        product_version: item(&old.product_version, &new.product_version),
        platform_version: item(&old.platform_version, &new.platform_version),
        device_id: item(&old.device_id, &new.device_id),
        local: pick(&old.local, wa, &new.local, wb),
        address: item(&old.address, &new.address),
        secure: pick(&old.secure, wa, &new.secure, wb),
        relayed: pick(&old.relayed, wa, &new.relayed, wb),
        bandwidth: pick(&old.bandwidth, da, &new.bandwidth, db),
        decision: way(&old.decision, &new.decision),
        video_decision: way(&old.video_decision, &new.video_decision),
        audio_decision: way(&old.audio_decision, &new.audio_decision),
        source_resolution: way(&old.source_resolution, &new.source_resolution),
        source_bitrate: pick(&old.source_bitrate, da, &new.source_bitrate, db),
        stream_resolution: way(&old.stream_resolution, &new.stream_resolution),
        stream_bitrate: pick(&old.stream_bitrate, da, &new.stream_bitrate, db),
        source_video_codec: codec(&old.source_video_codec, &new.source_video_codec),
        source_audio_codec: codec(&old.source_audio_codec, &new.source_audio_codec),
        source_audio_channels: count(&old.source_audio_channels, &new.source_audio_channels),
        source_container: codec(&old.source_container, &new.source_container),
        stream_video_codec: codec(&old.stream_video_codec, &new.stream_video_codec),
        stream_audio_codec: codec(&old.stream_audio_codec, &new.stream_audio_codec),
        stream_audio_channels: count(&old.stream_audio_channels, &new.stream_audio_channels),
        stream_container: codec(&old.stream_container, &new.stream_container),
        transcode_reasons: if new.transcode_reasons.is_empty() || db < da {
            old.transcode_reasons.clone()
        } else {
            new.transcode_reasons.clone()
        },
        session_id: old.session_id.clone().or_else(|| new.session_id.clone()),
        history_id: old.history_id.or(new.history_id),
        tautulli_id: old.tautulli_id.or(new.tautulli_id),
        viewed_at: old.viewed_at.or(new.viewed_at),
        raw: Value::Object(raw),
    }
}

/// The span a record says the play took, in seconds since the epoch: from a followed session or
/// from Tautulli. An open session reaches into the future. A record of the media server's
/// history alone has no span, only the moment it was marked played.
fn span(r: &PlayRecord) -> Option<(i64, i64)> {
    if r.session_id.is_none() && r.tautulli_id.is_none() {
        return None;
    }
    let start = r.started_at.timestamp();
    let end = match r.stopped_at {
        Some(s) => s.timestamp().max(start),
        None if r.session_id.is_some() => i64::MAX,
        None => start,
    };
    Some((start, end))
}

/// How near a moment lies to a span: `None` when it is outside it by more than [`NEAR`].
fn near((start, end): (i64, i64), at: Option<DateTime<Utc>>) -> Option<i64> {
    let at = at?.timestamp();
    if at < start - NEAR || (end != i64::MAX && at > end.saturating_add(NEAR)) {
        return None;
    }
    Some(if end == i64::MAX {
        (at - start).abs()
    } else {
        (at - end).abs()
    })
}

/// Whether two records tell of the same play, and how close they are (smaller is closer):
/// the same server, person and item, and spans that overlap, or a moment the media server
/// marked the item played inside the other's span. Two records of one source with different
/// ids are two plays.
pub fn closeness(a: &PlayRecord, b: &PlayRecord) -> Option<i64> {
    fn differ<T: PartialEq>(x: &Option<T>, y: &Option<T>) -> bool {
        x.is_some() && y.is_some() && x != y
    }
    if differ(&a.session_id, &b.session_id)
        || differ(&a.history_id, &b.history_id)
        || differ(&a.tautulli_id, &b.tautulli_id)
    {
        return None;
    }
    if a.server != b.server || a.account_id != b.account_id || a.rating_key.is_none() {
        return None;
    }
    if a.rating_key != b.rating_key {
        return None;
    }
    match (span(a), span(b)) {
        (Some((s1, e1)), Some((s2, e2))) => (s1.max(s2) <= e1.min(e2)).then(|| (s1 - s2).abs()),
        (Some(w), None) => near(w, b.viewed_at),
        (None, Some(w)) => near(w, a.viewed_at),
        (None, None) => None,
    }
}

/// A playing session as a play. A session the server gives no id is not followed.
pub fn from_session(server: &str, s: &ServerSession, now: DateTime<Utc>) -> Option<PlayRecord> {
    let session_id = s.session_id.clone()?;
    let episode = s.kind == "episode";
    let d = &s.delivery;
    Some(PlayRecord {
        server: server.to_string(),
        server_kind: None,
        rating_key: s.rating_key.clone(),
        show_key: if episode { s.show_key.clone() } else { None },
        title_id: None,
        kind: if s.kind.is_empty() {
            "other".to_string()
        } else {
            s.kind.clone()
        },
        title: s.title.clone(),
        show_title: if episode { s.show.clone() } else { None },
        season: if episode { s.season } else { None },
        episode: if episode { s.episode } else { None },
        year: if s.kind == "movie" { s.year } else { None },
        library_key: s.library_key.clone(),
        account_id: s.account_id,
        account_name: s.person.clone(),
        started_at: now,
        stopped_at: None,
        watched_secs: Some(0),
        paused_secs: Some(0),
        duration_secs: s.duration_ms.and_then(|d| i32::try_from(d / 1000).ok()),
        progress: s.progress.map(|p| (p * 10.0).round() / 10.0),
        player: s.player.clone(),
        device: s.device.clone(),
        platform: s.platform.clone(),
        product: s.product.clone(),
        product_version: s.product_version.clone(),
        platform_version: s.platform_version.clone(),
        device_id: s.device_id.clone(),
        local: s.local,
        address: s.address.clone(),
        secure: s.secure,
        relayed: s.relayed,
        bandwidth: s.bandwidth,
        decision: Some(s.decision.clone()).filter(|d| !d.is_empty()),
        video_decision: d.video.clone(),
        audio_decision: d.audio.clone(),
        source_resolution: d.source_resolution.clone(),
        source_bitrate: d.source_bitrate,
        stream_resolution: d.stream_resolution.clone(),
        stream_bitrate: d.stream_bitrate,
        source_video_codec: d.source_video_codec.clone(),
        source_audio_codec: d.source_audio_codec.clone(),
        source_audio_channels: d.source_audio_channels,
        source_container: d.source_container.clone(),
        stream_video_codec: d.stream_video_codec.clone(),
        stream_audio_codec: d.stream_audio_codec.clone(),
        stream_audio_channels: d.stream_audio_channels,
        stream_container: d.stream_container.clone(),
        transcode_reasons: d.transcode_reasons.clone(),
        session_id: Some(session_id),
        history_id: None,
        tautulli_id: None,
        viewed_at: None,
        raw: json!({ "live": s.raw }),
    })
}

/// An entry of the media server's history as a play: who by the server's account list, the
/// device by its device list. It knows when the item was marked played and nothing of how
/// long or how.
pub fn from_server_play(
    server: &str,
    p: &ServerPlay,
    names: &HashMap<i64, String>,
    devices: &HashMap<i64, (Option<String>, Option<String>)>,
) -> PlayRecord {
    let device = p.device_id.and_then(|d| devices.get(&d));
    let episode = p.kind == "episode";
    PlayRecord {
        server: server.to_string(),
        server_kind: None,
        rating_key: p.rating_key.clone(),
        show_key: if episode { p.show_key.clone() } else { None },
        title_id: None,
        kind: if p.kind.is_empty() {
            "other".to_string()
        } else {
            p.kind.clone()
        },
        title: p.title.clone(),
        show_title: if episode { p.show.clone() } else { None },
        season: p.season,
        episode: p.episode,
        year: if p.kind == "movie" { p.year } else { None },
        library_key: p.library_key.clone(),
        account_id: p.account_id,
        account_name: p.account_id.and_then(|a| names.get(&a).cloned()),
        started_at: p.viewed_at,
        stopped_at: None,
        watched_secs: None,
        paused_secs: None,
        duration_secs: None,
        progress: None,
        player: device.and_then(|(name, _)| name.clone()),
        device: None,
        platform: device.and_then(|(_, platform)| platform.clone()),
        product: None,
        product_version: None,
        platform_version: None,
        device_id: None,
        local: None,
        address: None,
        secure: None,
        relayed: None,
        bandwidth: None,
        decision: None,
        video_decision: None,
        audio_decision: None,
        source_resolution: None,
        source_bitrate: None,
        stream_resolution: None,
        stream_bitrate: None,
        source_video_codec: None,
        source_audio_codec: None,
        source_audio_channels: None,
        source_container: None,
        stream_video_codec: None,
        stream_audio_codec: None,
        stream_audio_channels: None,
        stream_container: None,
        transcode_reasons: Vec::new(),
        session_id: None,
        history_id: Some(p.id),
        tautulli_id: None,
        viewed_at: Some(p.viewed_at),
        raw: json!({ "server": p.raw }),
    }
}

/// A play of Tautulli as a play. `owner` is the id Tautulli knows the media server's owner by,
/// when that owner is the media server's own account ([`OWNER_ACCOUNT`]).
pub fn from_history_play(server: &str, p: &HistoryPlay, owner: Option<i64>) -> PlayRecord {
    let episode = p.kind == "episode";
    let secs = |n: Option<i64>| n.and_then(|n| i32::try_from(n.max(0)).ok());
    PlayRecord {
        server: server.to_string(),
        server_kind: Some("plex".to_string()),
        rating_key: p.rating_key.clone(),
        show_key: if episode { p.show_key.clone() } else { None },
        title_id: None,
        kind: p.kind.clone(),
        title: p.title.clone(),
        show_title: if episode { p.show.clone() } else { None },
        season: p.season,
        episode: p.episode,
        year: if p.kind == "movie" { p.year } else { None },
        library_key: None,
        account_id: p
            .user_id
            .map(|u| if owner == Some(u) { OWNER_ACCOUNT } else { u }),
        account_name: p.user.clone(),
        started_at: p.started,
        stopped_at: p.stopped,
        watched_secs: secs(p.played_secs),
        paused_secs: secs(p.paused_secs),
        duration_secs: None,
        progress: p.percent,
        player: p.player.clone(),
        device: None,
        platform: p.platform.clone(),
        product: p.product.clone(),
        product_version: None,
        platform_version: None,
        device_id: p.device_id.clone(),
        local: p.local,
        address: p.address.clone(),
        secure: p.secure,
        relayed: p.relayed,
        bandwidth: None,
        decision: p.decision.clone(),
        video_decision: None,
        audio_decision: None,
        source_resolution: None,
        source_bitrate: None,
        stream_resolution: None,
        stream_bitrate: None,
        source_video_codec: None,
        source_audio_codec: None,
        source_audio_channels: None,
        source_container: None,
        stream_video_codec: None,
        stream_audio_codec: None,
        stream_audio_channels: None,
        stream_container: None,
        transcode_reasons: Vec::new(),
        session_id: None,
        history_id: None,
        tautulli_id: Some(p.id),
        viewed_at: None,
        raw: json!({ "tautulli": p.raw }),
    }
}

/// A session being followed.
#[derive(Debug, Clone)]
struct Followed {
    record: PlayRecord,
    /// `playing`, `paused`, `buffering`, as the last reading had it. `None`: not known.
    state: Option<String>,
    seen: DateTime<Utc>,
}

/// What one reading of the sessions found: the plays that go on, with what they add up to so
/// far, and the plays that ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reading {
    pub open: Vec<PlayRecord>,
    pub ended: Vec<PlayRecord>,
}

/// The sessions followed from one reading to the next. A play starts when its session first
/// shows; the time from one reading to the next is counted as watched or paused by the state
/// the session had at the earlier one; a session that no longer shows ended at the last reading
/// that saw it.
#[derive(Debug, Default)]
pub struct Follower {
    open: BTreeMap<String, Followed>,
}

impl Follower {
    /// Take up the plays that were open when the process stopped, each as of the time it was
    /// last written: a session that still plays stays one play, one that ended meanwhile ends.
    pub fn resume(&mut self, plays: Vec<(PlayRecord, DateTime<Utc>)>) {
        for (record, seen) in plays {
            if let Some(id) = record.session_id.clone() {
                self.open.insert(
                    id,
                    Followed {
                        record,
                        state: None,
                        seen,
                    },
                );
            }
        }
    }

    /// Something plays.
    pub fn busy(&self) -> bool {
        !self.open.is_empty()
    }

    /// One reading at `now`: each session as a play with the state its player is in.
    pub fn observe(
        &mut self,
        now: DateTime<Utc>,
        sessions: Vec<(PlayRecord, Option<String>)>,
    ) -> Reading {
        let mut out = Reading::default();
        let mut here: HashSet<String> = HashSet::new();
        for (mut record, state) in sessions {
            let Some(id) = record.session_id.clone() else {
                continue;
            };
            if !here.insert(id.clone()) {
                continue;
            }
            match self.open.get_mut(&id) {
                Some(f) => {
                    let gap =
                        i32::try_from((now - f.seen).num_seconds().clamp(0, GAP_MAX)).unwrap_or(0);
                    let mut watched = f.record.watched_secs.unwrap_or(0);
                    let mut paused = f.record.paused_secs.unwrap_or(0);
                    match f.state.as_deref() {
                        Some("playing" | "buffering") => watched += gap,
                        Some("paused") => paused += gap,
                        _ => {}
                    }
                    record.started_at = f.record.started_at;
                    record.watched_secs = Some(watched);
                    record.paused_secs = Some(paused);
                    record.title_id = record.title_id.or(f.record.title_id);
                    if record.source_resolution.is_none() && record.source_bitrate.is_none() {
                        record.source_resolution = f.record.source_resolution.clone();
                        record.source_bitrate = f.record.source_bitrate;
                    }
                    // The file's codecs and container, where an earlier reading asked the server.
                    let old = &f.record;
                    if record.source_video_codec.is_none() {
                        record.source_video_codec = old.source_video_codec.clone();
                    }
                    if record.source_audio_codec.is_none() {
                        record.source_audio_codec = old.source_audio_codec.clone();
                    }
                    if record.source_audio_channels.is_none() {
                        record.source_audio_channels = old.source_audio_channels;
                    }
                    if record.source_container.is_none() {
                        record.source_container = old.source_container.clone();
                    }
                    if record.server_kind.is_none() {
                        record.server_kind = old.server_kind.clone();
                    }
                    f.record = record.clone();
                    f.state = state;
                    f.seen = now;
                    out.open.push(record);
                }
                None => {
                    record.started_at = now;
                    record.watched_secs = Some(0);
                    record.paused_secs = Some(0);
                    self.open.insert(
                        id,
                        Followed {
                            record: record.clone(),
                            state,
                            seen: now,
                        },
                    );
                    out.open.push(record);
                }
            }
        }
        let gone: Vec<String> = self
            .open
            .keys()
            .filter(|k| !here.contains(*k))
            .cloned()
            .collect();
        for id in gone {
            if let Some(f) = self.open.remove(&id) {
                let mut record = f.record;
                record.stopped_at = Some(f.seen);
                out.ended.push(record);
            }
        }
        out
    }
}

/// What a page of statistics is about. Empty: every play.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    /// Plays that started at or after this.
    pub since: Option<DateTime<Utc>>,
    /// Plays that started before this.
    pub until: Option<DateTime<Utc>>,
    /// Kinds as stored: `movie`, `episode`, `track`.
    pub kinds: Vec<String>,
    /// A library by its id (`plex-1`).
    pub library: Option<String>,
    pub account: Option<i64>,
    pub title_id: Option<i64>,
    /// As stored: `direct play`, `direct stream`, `transcode`.
    pub decision: Option<String>,
    pub platform: Option<String>,
    /// A media server by its integration id.
    pub server: Option<String>,
    /// A player by its name (the device's when the player has none).
    pub player: Option<String>,
    /// A player app.
    pub product: Option<String>,
    /// A client address.
    pub address: Option<String>,
}

/// How the history is ordered: a column, and the direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Date,
    Person,
    Title,
    Server,
    Player,
    Platform,
    Product,
    Delivery,
    Quality,
    Duration,
    Paused,
    Progress,
    Address,
}

impl SortKey {
    /// A column of the API by its word: `date`, `person`, `title`, `server`, `player`,
    /// `platform`, `product`, `delivery`, `quality`, `duration`, `paused`, `progress`,
    /// `address`.
    pub fn of(word: &str) -> Option<SortKey> {
        Some(match word.trim().to_ascii_lowercase().as_str() {
            "date" | "at" | "started" => SortKey::Date,
            "person" | "user" => SortKey::Person,
            "title" => SortKey::Title,
            "server" => SortKey::Server,
            "player" => SortKey::Player,
            "platform" => SortKey::Platform,
            "product" | "app" => SortKey::Product,
            "delivery" | "stream" | "decision" => SortKey::Delivery,
            "quality" => SortKey::Quality,
            "duration" | "watched" => SortKey::Duration,
            "paused" => SortKey::Paused,
            "progress" | "percent" => SortKey::Progress,
            "address" | "ip" => SortKey::Address,
            _ => return None,
        })
    }

    /// The expression the plays are ordered by.
    pub fn expression(self) -> &'static str {
        match self {
            SortKey::Date => "p.started_at",
            SortKey::Person => "lower(p.account_name)",
            SortKey::Title => "lower(COALESCE(p.show_title, p.title))",
            SortKey::Server => "p.server",
            SortKey::Player => "lower(COALESCE(p.player, p.device))",
            SortKey::Platform => "lower(p.platform)",
            SortKey::Product => "lower(p.product)",
            SortKey::Delivery => "p.decision",
            SortKey::Quality => "COALESCE(p.stream_bitrate, p.source_bitrate)",
            SortKey::Duration => "p.watched_secs",
            SortKey::Paused => "p.paused_secs",
            SortKey::Progress => "p.progress",
            SortKey::Address => "p.address",
        }
    }
}

/// The stored kinds of a kind word of the API: `movie`, `show` (its episodes), `music` (its
/// tracks). `None` for a word it does not know.
pub fn kinds_of(word: &str) -> Option<Vec<String>> {
    let kinds: &[&str] = match word.trim().to_ascii_lowercase().as_str() {
        "movie" | "movies" => &["movie"],
        "show" | "shows" | "episode" => &["episode"],
        "music" | "track" => &["track"],
        _ => return None,
    };
    Some(kinds.iter().map(|k| k.to_string()).collect())
}

/// The stored word of a delivery word of the API: `direct-play`, `direct-stream`, `transcode`.
pub fn decision_of(word: &str) -> Option<&'static str> {
    match word
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], " ")
        .as_str()
    {
        "direct play" => Some(DIRECT_PLAY),
        "direct stream" | "copy" => Some(DIRECT_STREAM),
        "transcode" => Some(TRANSCODE),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::Delivery;
    use chrono::TimeZone;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_795_000_000 + secs, 0).unwrap()
    }

    fn session(id: &str, state: &str, progress: f64) -> ServerSession {
        ServerSession {
            server: "plex".into(),
            kind: "movie".into(),
            title: "The 'Burbs".into(),
            show: None,
            season: None,
            episode: None,
            person: Some("owner".into()),
            player: Some("Living Room TV".into()),
            state: Some(state.into()),
            progress: Some(progress),
            decision: TRANSCODE.into(),
            rating_key: Some("109882".into()),
            show_key: None,
            session_id: Some(id.into()),
            account_id: Some(1),
            library_key: Some("1".into()),
            year: Some(1989),
            offset_ms: None,
            duration_ms: Some(6_000_000),
            device: Some("SHIELD".into()),
            platform: Some("Android".into()),
            product: Some("Plex for Android (TV)".into()),
            product_version: Some("8.26.2".into()),
            platform_version: Some("11".into()),
            device_id: Some("device-1".into()),
            local: Some(true),
            secure: Some(true),
            relayed: Some(false),
            address: Some("10.0.0.20".into()),
            delivery: Delivery {
                video: Some(TRANSCODE.into()),
                audio: Some(DIRECT_STREAM.into()),
                source_resolution: None,
                source_bitrate: None,
                stream_resolution: Some("720p".into()),
                stream_bitrate: Some(4000),
                stream_video_codec: Some("h264".into()),
                stream_audio_codec: Some("aac".into()),
                ..Delivery::default()
            },
            bandwidth: Some(4200),
            raw: json!({"Player": {"address": "10.0.0.20", "title": "Living Room TV"}}),
        }
    }

    fn reading(f: &mut Follower, secs: i64, sessions: &[ServerSession]) -> Reading {
        f.observe(
            at(secs),
            sessions
                .iter()
                .map(|s| (from_session("plex", s, at(secs)).unwrap(), s.state.clone()))
                .collect(),
        )
    }

    #[test]
    fn a_session_is_followed_from_start_through_a_pause_to_its_end() {
        let mut f = Follower::default();
        let r = reading(&mut f, 0, &[session("s1", "playing", 0.0)]);
        assert_eq!(r.open.len(), 1);
        assert_eq!(
            (r.open[0].started_at, r.open[0].watched_secs),
            (at(0), Some(0))
        );
        assert!(f.busy());
        reading(&mut f, 10, &[session("s1", "playing", 1.0)]);
        // Paused at the third reading: the ten seconds before it were played.
        reading(&mut f, 20, &[session("s1", "paused", 2.0)]);
        reading(&mut f, 30, &[session("s1", "paused", 2.0)]);
        let r = reading(&mut f, 40, &[session("s1", "playing", 2.0)]);
        assert_eq!(
            (r.open[0].watched_secs, r.open[0].paused_secs),
            (Some(20), Some(20))
        );
        assert_eq!(r.open[0].stopped_at, None);
        // The session is gone: it ended at the last reading that saw it.
        let r = reading(&mut f, 50, &[]);
        assert!(r.open.is_empty() && !f.busy());
        let end = &r.ended[0];
        assert_eq!(
            (
                end.started_at,
                end.stopped_at,
                end.watched_secs,
                end.paused_secs
            ),
            (at(0), Some(at(40)), Some(20), Some(20))
        );
        assert_eq!(
            (
                end.decision.as_deref(),
                end.video_decision.as_deref(),
                end.stream_resolution.as_deref()
            ),
            (Some(TRANSCODE), Some(TRANSCODE), Some("720p"))
        );
        assert_eq!(end.progress, Some(2.0));
        // A gap longer than readings ever leave counts for no more than the longest one.
        let mut g = Follower::default();
        reading(&mut g, 0, &[session("s2", "playing", 0.0)]);
        let r = reading(&mut g, 3600, &[session("s2", "playing", 50.0)]);
        assert_eq!(r.open[0].watched_secs, Some(GAP_MAX as i32));
    }

    #[test]
    fn a_play_open_at_a_restart_goes_on_as_one_play() {
        let mut f = Follower::default();
        let mut stored = from_session("plex", &session("s1", "playing", 10.0), at(0)).unwrap();
        stored.watched_secs = Some(600);
        f.resume(vec![(stored, at(600))]);
        // Its state before the restart is not known, so the gap counts as neither.
        let r = reading(&mut f, 660, &[session("s1", "playing", 12.0)]);
        assert_eq!(
            (r.open[0].started_at, r.open[0].watched_secs),
            (at(0), Some(600))
        );
        let r = reading(&mut f, 670, &[session("s1", "playing", 12.5)]);
        assert_eq!(r.open[0].watched_secs, Some(610));
        // One that ended while the process was down ends at its last reading.
        let mut g = Follower::default();
        let gone = from_session("plex", &session("s9", "playing", 10.0), at(0)).unwrap();
        g.resume(vec![(gone, at(300))]);
        let r = reading(&mut g, 900, &[]);
        assert_eq!(r.ended[0].stopped_at, Some(at(300)));
    }

    fn tautulli_row(id: i64, start: i64, stop: i64) -> PlayRecord {
        PlayRecord {
            started_at: at(start),
            stopped_at: Some(at(stop)),
            watched_secs: Some(i32::try_from(stop - start - 30).unwrap()),
            paused_secs: Some(30),
            decision: Some(DIRECT_PLAY.into()),
            player: Some("TV".into()),
            platform: Some("Roku".into()),
            local: Some(false),
            session_id: None,
            tautulli_id: Some(id),
            account_name: Some("friend in tautulli".into()),
            raw: json!({"tautulli": {"id": id, "ip_address": "198.51.100.5"}}),
            address: Some("198.51.100.5".into()),
            ..from_session("plex", &session("x", "playing", 0.0), at(0))
                .map(|mut r| {
                    r.session_id = None;
                    r.raw = json!({});
                    r
                })
                .unwrap()
        }
    }

    fn history_row(id: i64, viewed: i64) -> PlayRecord {
        PlayRecord {
            started_at: at(viewed),
            stopped_at: None,
            watched_secs: None,
            paused_secs: None,
            decision: None,
            video_decision: None,
            audio_decision: None,
            stream_resolution: None,
            stream_bitrate: None,
            history_id: Some(id),
            viewed_at: Some(at(viewed)),
            account_name: Some("friend".into()),
            raw: json!({"server": {"historyKey": id}}),
            ..tautulli_row(0, 0, 0)
        }
        .clean_ids()
    }

    impl PlayRecord {
        fn clean_ids(mut self) -> PlayRecord {
            self.tautulli_id = None;
            self.address = None;
            self.raw.as_object_mut().unwrap().remove("tautulli");
            self
        }
    }

    #[test]
    fn records_of_one_play_are_found_and_merged_and_two_plays_are_not() {
        let live = {
            let mut f = Follower::default();
            reading(&mut f, 0, &[session("s1", "playing", 0.0)]);
            reading(&mut f, 3000, &[session("s1", "playing", 95.0)]);
            reading(&mut f, 3010, &[]).ended.remove(0)
        };
        let history = history_row(8692, 2900);
        let tautulli = tautulli_row(6680, 5, 3020);
        // The media server marked it played while the session ran; Tautulli's span overlaps.
        assert!(closeness(&live, &history).is_some());
        assert!(closeness(&history, &tautulli).is_some());
        assert!(closeness(&live, &tautulli).is_some());
        // Another sitting, an hour later, is another play.
        let later = tautulli_row(6690, 7000, 9000);
        assert_eq!(closeness(&live, &later), None);
        assert_eq!(closeness(&history, &later), None);
        // Another person, another item, another row of the same source: never one play.
        let mut other = tautulli.clone();
        other.account_id = Some(7);
        assert_eq!(closeness(&live, &other), None);
        let mut item = tautulli.clone();
        item.rating_key = Some("1".into());
        assert_eq!(closeness(&live, &item), None);
        assert_eq!(closeness(&tautulli, &later.clone()), None);

        let one = merge(&merge(&live, &history), &tautulli);
        assert_eq!(one.sources().words(), vec!["live", "server", "tautulli"]);
        // Tautulli's times, the session's delivery and place, the server's name.
        assert_eq!(
            (one.started_at, one.stopped_at, one.watched_secs),
            (at(5), Some(at(3020)), Some(2985))
        );
        assert_eq!(
            (
                one.video_decision.as_deref(),
                one.stream_resolution.as_deref(),
                one.decision.as_deref()
            ),
            (Some(TRANSCODE), Some("720p"), Some(TRANSCODE))
        );
        assert_eq!(
            (one.player.as_deref(), one.local),
            (Some("Living Room TV"), Some(true))
        );
        assert_eq!(one.account_name.as_deref(), Some("owner"));
        assert_eq!(
            (one.history_id, one.tautulli_id, one.viewed_at),
            (Some(8692), Some(6680), Some(at(2900)))
        );
        assert!(one.raw.get("live").is_some() && one.raw.get("server").is_some());
        assert!(one.raw.get("tautulli").is_some());
        // The same play read in the other order is the same play.
        let other_way = merge(&merge(&tautulli, &history), &live);
        assert_eq!(
            (
                other_way.started_at,
                other_way.watched_secs,
                other_way.stream_resolution.clone()
            ),
            (
                one.started_at,
                one.watched_secs,
                one.stream_resolution.clone()
            )
        );
        // A play of the history alone takes Tautulli's times and keeps its own name.
        let two = merge(&history, &tautulli);
        assert_eq!(
            (two.started_at, two.account_name.as_deref()),
            (at(5), Some("friend"))
        );
        // Without addresses: none in the play, none in the sources' records.
        let bare = one.clone().without_addresses();
        assert_eq!(bare.address, None);
        assert!(bare.raw["tautulli"].get("ip_address").is_none());
        assert!(bare.raw["live"]["Player"].get("address").is_none());
        assert_eq!(bare.raw["live"]["Player"]["title"], "Living Room TV");
    }

    #[test]
    fn the_api_words_read_as_stored_words() {
        assert_eq!(kinds_of("show").unwrap(), vec!["episode"]);
        assert_eq!(kinds_of("movie").unwrap(), vec!["movie"]);
        assert_eq!(kinds_of("music").unwrap(), vec!["track"]);
        assert!(kinds_of("book").is_none());
        assert_eq!(decision_of("direct-play"), Some(DIRECT_PLAY));
        assert_eq!(decision_of("Direct stream"), Some(DIRECT_STREAM));
        assert_eq!(decision_of("transcode"), Some(TRANSCODE));
        assert_eq!(decision_of("x"), None);
    }
}
