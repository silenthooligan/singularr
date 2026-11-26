//! The passes that collect plays, and the one way a play is written.
//!
//! Three sources tell of plays, and each play is one row whoever told of it:
//!
//! - **Sessions** ([`live_pass`]): each media server's sessions (Plex, Jellyfin, Emby, through
//!   the one port), read every few seconds while something plays and every minute while nothing
//!   does. A play is one item in one session. It starts at the reading that first shows the
//!   item in its session. It ends at the last reading that saw it: the session is gone or shows
//!   another item (the next episode, the next track). A session's id names its play while that
//!   play is open: the same id read after the play ended starts another. Sessions are the only
//!   source of how a new play is delivered (direct play, direct stream or transcode, the
//!   resolutions and bitrates). Every server has them: the statistics hold for any server
//!   without its history or Tautulli.
//! - **The media server's history** ([`server_history_pass`]), for a server that keeps one
//!   ([`MediaServer::keeps_history`]): what it marked as played, with who and on which device,
//!   newest first. The whole history at the first pass, then the entries newer than the newest
//!   read.
//! - **Tautulli** ([`tautulli_pass`]), when one is configured: years of plays with durations,
//!   devices and delivery on the Plex server. All of it at the first pass and every day, the
//!   newest days on every other pass.
//!
//! [`absorb`] writes what a source says: onto the stored play the source's own id names, else
//! onto the stored play it is another reading of (`model::closeness`), merged so that the
//! richer record wins field by field (`model::merge`), else as a new play. One writer at a time,
//! so two sources telling of one play at once still make one row.
//!
//! [`housekeeping`] links plays to the catalog titles the catalog has learned since, forgets
//! addresses unless the setting keeps them, and forgets plays older than the setting keeps.
//!
//! Each pass is a job of the runtime with the sync (`lifecycle::jobs`): a media server's
//! sessions ([`SessionsJob`], `<server>:sessions`), its history ([`ServerHistoryJob`],
//! `<server>:plays`) and Tautulli's plays ([`TautulliJob`], `<tautulli>:plays`). The housekeeping
//! follows each history pass. With no history to read it has a schedule of its own
//! ([`TidyJob`], `plays:tidy`).

use super::model::{
    DIRECT_PLAY, Follower, HISTORY_EVERY, HISTORY_EVERY_DEFAULT, KEEP_ADDRESSES, KEEP_FOR,
    PlayRecord, Reading, closeness, from_history_play, from_server_play, from_session, merge,
};
use super::store;
use crate::catalog::store as catalog_store;
use crate::core::journal::Journal;
use crate::core::runtime::{Cx, Done, Failed, Handler};
use crate::core::units;
use crate::engines::service::{EngineSet, PassOk};
use crate::engines::{ItemMedia, MediaServer, PlayHistory};
use crate::lifecycle::claims;
use crate::lifecycle::sync::{Last, Run as PassRun, end_run};
use crate::settings::Settings;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The pass names, in jobs, metrics and the journal.
pub const SESSIONS: &str = "sessions";
pub const HISTORY: &str = "plays";
/// The housekeeping's own pass, where no history pass runs it.
pub const TIDY: &str = "tidy";
/// How often the sessions are read while something plays, and while nothing does.
pub const BUSY: Duration = Duration::from_secs(10);
pub const IDLE: Duration = Duration::from_secs(60);
/// Entries of the media server's history asked for at once, and the most pages one pass reads.
pub const SERVER_PAGE: u32 = 500;
pub const SERVER_PAGES_MAX: u32 = 400;
/// Plays asked of Tautulli at once, and the most pages one pass reads.
pub const TAUTULLI_PAGE: u32 = 1000;
pub const TAUTULLI_PAGES_MAX: u32 = 500;
/// How long a read of everything Tautulli holds stands before the next one.
pub const WHOLE_READ_EVERY: Duration = Duration::from_secs(24 * 3600);
/// Days an ordinary Tautulli pass reads again before the newest play it told of.
const OVERLAP_DAYS: i64 = 2;

/// What the plays passes need.
#[derive(Clone)]
pub struct PlaysDeps {
    pub pool: PgPool,
    pub journal: Journal,
    pub engines: Arc<EngineSet>,
    pub settings: Arc<Settings>,
    /// One writer at a time: what one source writes is matched against what the others wrote.
    pub writer: Arc<tokio::sync::Mutex<()>>,
}

impl PlaysDeps {
    pub fn new(
        pool: PgPool,
        journal: Journal,
        engines: Arc<EngineSet>,
        settings: Arc<Settings>,
    ) -> PlaysDeps {
        PlaysDeps {
            pool,
            journal,
            engines,
            settings,
            writer: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}

/// Whether the players' addresses are kept: on by default, for the owner's statistics. Off,
/// only home network or outside is kept.
pub fn keep_addresses(settings: &Settings) -> bool {
    settings.value(KEEP_ADDRESSES).as_bool().unwrap_or(true)
}

/// How many days plays are kept. `None`: forever, the default.
pub fn keep_days(settings: &Settings) -> Option<i64> {
    settings
        .value(KEEP_FOR)
        .as_str()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|d| *d > 0)
}

/// How often the histories are read.
pub fn history_every(settings: &Settings) -> Duration {
    Duration::from_secs(
        settings
            .value(HISTORY_EVERY)
            .as_u64()
            .unwrap_or(HISTORY_EVERY_DEFAULT)
            .clamp(300, 86_400),
    )
}

/// How one record was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    /// A new play.
    New(i64),
    /// Merged onto a stored play, which changed.
    Changed(i64),
    /// The stored play said all of it already.
    Same(i64),
}

impl Written {
    pub fn id(self) -> i64 {
        match self {
            Written::New(id) | Written::Changed(id) | Written::Same(id) => id,
        }
    }
}

/// Write one record a source told of: onto the stored play its source's id names, else onto
/// the closest stored play it is another reading of, else as a new play.
pub async fn absorb(conn: &mut PgConnection, record: PlayRecord) -> Result<Written, sqlx::Error> {
    if let Some(stored) = store::by_source(conn, &record).await? {
        let merged = merge(&stored.record, &record);
        if merged == stored.record {
            return Ok(Written::Same(stored.id));
        }
        store::update(conn, stored.id, &merged).await?;
        return Ok(Written::Changed(stored.id));
    }
    let candidates = store::candidates(conn, &record).await?;
    let best = candidates
        .iter()
        .filter_map(|c| closeness(&c.record, &record).map(|d| (d, c)))
        .min_by_key(|(d, c)| (*d, c.id));
    if let Some((_, stored)) = best {
        let merged = merge(&stored.record, &record);
        if merged == stored.record {
            return Ok(Written::Same(stored.id));
        }
        store::update(conn, stored.id, &merged).await?;
        return Ok(Written::Changed(stored.id));
    }
    Ok(Written::New(store::insert(conn, &record).await?))
}

/// What writing a batch did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Absorbed {
    pub new: usize,
    pub changed: usize,
}

/// The media server whose items a play's keys are: the play's own server, except for a play
/// Tautulli told of while no Plex server was configured. Its server is Tautulli's id. Its keys
/// are those of the Plex server Tautulli reads, once one is configured.
fn items_server(deps: &PlaysDeps, server: &str) -> String {
    match (&deps.engines.play_history, deps.engines.plex_server()) {
        (Some(history), Some(plex)) if history.id() == server => plex.id().to_string(),
        _ => server.to_string(),
    }
}

/// Records made ready to store: linked to the catalog titles of their items (each item by its
/// server and its key: two servers can hold one key), without addresses unless the setting keeps
/// them, and none older than plays are kept.
async fn prepare(
    deps: &PlaysDeps,
    records: Vec<PlayRecord>,
) -> Result<Vec<PlayRecord>, sqlx::Error> {
    let items: Vec<(String, String)> = records
        .iter()
        .filter(|r| r.title_id.is_none())
        .filter_map(|r| Some((items_server(deps, &r.server), r.item_key()?.to_string())))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let titles: HashMap<(String, String), i64> =
        catalog_store::server_items_titles(&deps.pool, &items)
            .await?
            .into_iter()
            .map(|(server, key, title)| ((server, key), title))
            .collect();
    let keep = keep_addresses(&deps.settings);
    let cutoff = keep_days(&deps.settings).map(|d| Utc::now() - chrono::Duration::days(d));
    Ok(records
        .into_iter()
        .filter(|r| cutoff.is_none_or(|c| r.started_at >= c))
        .map(|mut r| {
            if r.title_id.is_none() {
                let server = items_server(deps, &r.server);
                let found = r
                    .item_key()
                    .and_then(|k| titles.get(&(server, k.to_string())).copied());
                r.title_id = found;
            }
            if keep { r } else { r.without_addresses() }
        })
        .collect())
}

/// Write a batch of records, in one transaction, one writer at a time.
pub async fn absorb_all(
    deps: &PlaysDeps,
    records: Vec<PlayRecord>,
) -> Result<Absorbed, sqlx::Error> {
    let records = prepare(deps, records).await?;
    let mut out = Absorbed::default();
    if records.is_empty() {
        return Ok(out);
    }
    let _one = deps.writer.lock().await;
    let mut tx = deps.pool.begin().await?;
    for r in records {
        match absorb(&mut tx, r).await? {
            Written::New(_) => out.new += 1,
            Written::Changed(_) => out.changed += 1,
            Written::Same(_) => {}
        }
    }
    tx.commit().await?;
    Ok(out)
}

/// One reading of the sessions, written ([`read_sessions`], [`write_reading`]). Returns how many
/// play now.
pub async fn live_pass(
    deps: &PlaysDeps,
    server: &Arc<dyn MediaServer>,
    follower: &mut Follower,
    files: &mut Files,
) -> Result<usize, String> {
    let reading = read_sessions(server, follower, files).await?;
    write_reading(deps, reading).await
}

/// One reading of the sessions, before its write: each play that goes on with what it adds up to
/// so far, each play that ended with its end. The file of a play the server transcodes or sends
/// in another container is asked of the server once per play: the session describes what is
/// sent.
pub async fn read_sessions(
    server: &Arc<dyn MediaServer>,
    follower: &mut Follower,
    files: &mut Files,
) -> Result<Reading, String> {
    let sessions = server.sessions().await.map_err(|e| e.to_string())?;
    let now = Utc::now();
    let mut readings = Vec::new();
    for s in &sessions {
        let Some(mut r) = from_session(server.id(), s, now) else {
            continue;
        };
        r.server_kind = Some(server.kind().to_string());
        // Transcoded or sent in another container: the session describes what is sent. The
        // file's facts the session leaves out come from the item's record, each on its own.
        let changed = r.decision.as_deref().is_some_and(|d| d != DIRECT_PLAY);
        let unknown = r.source_resolution.is_none()
            || r.source_bitrate.is_none()
            || r.source_container.is_none()
            || (r.source_video_codec.is_none() && r.video_decision.is_some())
            || (r.source_audio_codec.is_none() && r.audio_decision.is_some());
        if changed
            && unknown
            && let (Some(session), Some(key)) = (r.session_id.clone(), r.rating_key.clone())
        {
            let play = (session, key);
            if !files.contains_key(&play) {
                let media = server.item_media(&play.1).await.ok().flatten();
                files.insert(play.clone(), media);
            }
            if let Some(Some(media)) = files.get(&play) {
                r.source_resolution = r
                    .source_resolution
                    .take()
                    .or_else(|| media.resolution.clone());
                r.source_bitrate = r.source_bitrate.or(media.bitrate);
                r.source_video_codec = r
                    .source_video_codec
                    .take()
                    .or_else(|| media.video_codec.clone());
                r.source_audio_codec = r
                    .source_audio_codec
                    .take()
                    .or_else(|| media.audio_codec.clone());
                r.source_audio_channels = r.source_audio_channels.or(media.audio_channels);
                r.source_container = r
                    .source_container
                    .take()
                    .or_else(|| media.container.clone());
            }
        }
        readings.push((r, s.state.clone()));
    }
    let reading = follower.observe(now, readings);
    // A file is kept while its play goes on.
    files.retain(|(session, key), _| {
        reading.open.iter().any(|r| {
            r.session_id.as_deref() == Some(session.as_str())
                && r.rating_key.as_deref() == Some(key.as_str())
        })
    });
    Ok(reading)
}

/// The file of each play its server transcodes or sends in another container, by the play's
/// session and item: asked of the server once per play.
pub type Files = HashMap<(String, String), Option<ItemMedia>>;

/// Write one reading of the sessions in one transaction: the plays that ended, then the plays
/// that go on. A session gone on to another item closes its first play before it opens the next:
/// the open-play index of 0020 holds one open play per session. Returns how many play now.
pub async fn write_reading(deps: &PlaysDeps, reading: Reading) -> Result<usize, String> {
    let open = reading.open.len();
    let mut all = reading.ended;
    all.extend(reading.open);
    absorb_all(deps, all).await.map_err(|e| e.to_string())?;
    Ok(open)
}

/// The media server's history since the newest entry read, the whole of it the first time,
/// with the names of its accounts and devices. The names the server has for its accounts now
/// are given to every play of theirs.
pub async fn server_history_pass(
    deps: &PlaysDeps,
    server: &Arc<dyn MediaServer>,
) -> Result<String, String> {
    let id = server.id();
    let source = format!("{id}:history");
    let db = |e: sqlx::Error| e.to_string();
    let cursor = store::cursor(&deps.pool, &source).await.map_err(db)?;
    let accounts = server.accounts().await.map_err(|e| e.to_string())?;
    let devices = server.devices().await.map_err(|e| e.to_string())?;
    let names: HashMap<i64, String> = accounts
        .into_iter()
        .filter_map(|a| Some((a.id, a.name?)))
        .collect();
    let places: HashMap<i64, (Option<String>, Option<String>)> = devices
        .into_iter()
        .map(|d| (d.id, (d.name, d.platform)))
        .collect();
    let named: Vec<(i64, String)> = names.iter().map(|(k, v)| (*k, v.clone())).collect();
    store::rename(&deps.pool, id, &named).await.map_err(db)?;
    let mut newest = cursor;
    let (mut read, mut new) = (0usize, 0usize);
    for n in 0..SERVER_PAGES_MAX {
        let page = server
            .history(n * SERVER_PAGE, SERVER_PAGE)
            .await
            .map_err(|e| e.to_string())?;
        let size = page.plays.len();
        newest = page.plays.iter().map(|p| p.id).chain(newest).max();
        let fresh: Vec<PlayRecord> = page
            .plays
            .iter()
            .filter(|p| cursor.is_none_or(|c| p.id > c))
            .map(|p| PlayRecord {
                server_kind: Some(server.kind().to_string()),
                ..from_server_play(id, p, &names, &places)
            })
            .collect();
        let reached = fresh.len() < size;
        read += fresh.len();
        new += absorb_all(deps, fresh).await.map_err(db)?.new;
        if reached || size < SERVER_PAGE as usize {
            break;
        }
    }
    if let Some(c) = newest {
        store::set_cursor(&deps.pool, &source, c)
            .await
            .map_err(db)?;
    }
    Ok(if read == 0 {
        "No new entries".to_string()
    } else {
        format!(
            "{}, {}",
            units::counted(read as i64, "entry", "entries"),
            units::counted(new as i64, "new play", "new plays")
        )
    })
}

/// Tautulli's plays: all of them with `whole`, else the days since the newest one it told of
/// (and two before). The owner as Tautulli names them is the media server's owner account.
pub async fn tautulli_pass(
    deps: &PlaysDeps,
    history: &Arc<dyn PlayHistory>,
    whole: bool,
) -> Result<PassOk, String> {
    let db = |e: sqlx::Error| e.to_string();
    let started = Instant::now();
    let info = history.status().await.map_err(|e| e.to_string())?;
    let took = started.elapsed();
    let users = history.users().await.map_err(|e| e.to_string())?;
    // Tautulli tells of a Plex server's plays. Without a Plex server they are Tautulli's own.
    // They link to the catalog through the Plex server once one is configured.
    let server = deps
        .engines
        .plex_server()
        .map(|s| s.id().to_string())
        .unwrap_or_else(|| history.id().to_string());
    let owner = deps
        .engines
        .plex_server()
        .and_then(|_| users.iter().find(|u| u.admin).map(|u| u.id));
    let after = if whole {
        None
    } else {
        store::newest_tautulli(&deps.pool)
            .await
            .map_err(db)?
            .map(|t| (t - chrono::Duration::days(OVERLAP_DAYS)).date_naive())
    };
    let (mut read, mut new) = (0usize, 0usize);
    for n in 0..TAUTULLI_PAGES_MAX {
        let page = history
            .plays(n * TAUTULLI_PAGE, TAUTULLI_PAGE, after)
            .await
            .map_err(|e| e.to_string())?;
        let size = page.plays.len();
        let records: Vec<PlayRecord> = page
            .plays
            .iter()
            .map(|p| from_history_play(&server, p, owner))
            .collect();
        read += size;
        new += absorb_all(deps, records).await.map_err(db)?.new;
        if size < TAUTULLI_PAGE as usize {
            break;
        }
    }
    let kept = store::from_tautulli(&deps.pool).await.map_err(db)?;
    Ok(PassOk {
        took,
        version: info.version,
        counts: json!({
            "plays": kept,
            "read": read,
            "new": new,
            "whole": whole,
        }),
    })
}

/// Link plays to the catalog titles the catalog has learned since they were written, forget
/// addresses unless the setting keeps them, and forget plays older than the setting keeps.
pub async fn housekeeping(deps: &PlaysDeps) -> Result<(), sqlx::Error> {
    let unlinked = store::unlinked_items(&deps.pool).await?;
    if !unlinked.is_empty() {
        // Each item is asked of the server its keys are, and linked by the plays' own server.
        let asked: Vec<(String, String)> = unlinked
            .iter()
            .map(|(server, key)| (items_server(deps, server), key.clone()))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let titles: HashMap<(String, String), i64> =
            catalog_store::server_items_titles(&deps.pool, &asked)
                .await?
                .into_iter()
                .map(|(server, key, title)| ((server, key), title))
                .collect();
        let found: Vec<(String, String, i64)> = unlinked
            .into_iter()
            .filter_map(|(server, key)| {
                let title = *titles.get(&(items_server(deps, &server), key.clone()))?;
                Some((server, key, title))
            })
            .collect();
        store::link(&deps.pool, &found).await?;
    }
    if !keep_addresses(&deps.settings) {
        store::forget_addresses(&deps.pool).await?;
    }
    if let Some(days) = keep_days(&deps.settings) {
        store::forget_before(&deps.pool, Utc::now() - chrono::Duration::days(days)).await?;
    }
    Ok(())
}

/// The kinds of the plays jobs: their handlers in the runtime.
pub const SESSIONS_KIND: &str = "player.sessions";
pub const HISTORY_KIND: &str = "player.history";
pub const TAUTULLI_KIND: &str = "history.plays";
pub const TIDY_KIND: &str = "plays.tidy";

/// The key of the housekeeping's schedule, where no history pass runs it.
pub const TIDY_KEY: &str = "plays:tidy";

/// The marker of Tautulli's last whole read: when it ended well.
pub const WHOLE_AT: &str = "history.whole_at";

/// The key of a media server's sessions schedule: `plex:sessions`.
pub fn sessions_key(server: &str) -> String {
    format!("{server}:{SESSIONS}")
}

/// The housekeeping has a schedule of its own when media servers are read, none of them keeps a
/// history and no Tautulli is configured (a Jellyfin or Emby server alone).
pub fn tidies_alone(engines: &EngineSet) -> bool {
    !engines.media_servers.is_empty()
        && !engines.media_servers.iter().any(|s| s.keeps_history())
        && engines.play_history.is_none()
}

/// Failed readings in a row that are tried again after [`BUSY`] while a play is open. Later
/// failures wait [`IDLE`]. So does any failure while nothing plays.
pub const BUSY_RETRIES: u32 = 6;

/// When the sessions are read again after `failures` failed readings in a row. While a play is
/// open it is soon: a short outage costs the play neither its time nor its end. Never later than
/// [`IDLE`], the schedule's interval.
pub fn retry_after(busy: bool, failures: u32) -> Duration {
    if busy && failures <= BUSY_RETRIES {
        BUSY
    } else {
        IDLE
    }
}

impl PlaysDeps {
    /// The media server a job reads.
    fn server(&self, id: &str) -> Result<Arc<dyn MediaServer>, Failed> {
        self.engines
            .media_servers
            .iter()
            .find(|s| s.id() == id)
            .cloned()
            .ok_or_else(|| Failed::from(format!("no media server `{id}` runs in this process")))
    }
}

/// The housekeeping after a history pass. A failure is logged: the pass stands.
async fn tidy_after(deps: &PlaysDeps) {
    if let Err(e) = housekeeping(deps).await {
        tracing::warn!(error = %e, "the plays could not be tidied");
    }
}

/// One media server's sessions as they are followed from one run of its sessions job to the next
/// ([`SessionsJob`]).
#[derive(Clone)]
pub struct Following {
    /// The followed sessions may differ from the plays the table holds open: the next reading
    /// takes those up first. Set at a start. Set while a reading is written too: a run dropped
    /// then may have committed its write.
    doubt: bool,
    follower: Follower,
    /// The file of each play its server transcodes or sends in another container, asked once.
    files: Files,
    /// Failed readings in a row.
    failures: u32,
}

impl Default for Following {
    fn default() -> Following {
        Following {
            doubt: true,
            follower: Follower::default(),
            files: Files::new(),
            failures: 0,
        }
    }
}

impl Following {
    /// Something plays, as the sessions are followed.
    pub fn busy(&self) -> bool {
        self.follower.busy()
    }

    /// The followed sessions are in doubt, as a run dropped while its reading was written leaves
    /// them: the next reading takes up the plays the table holds open first.
    pub fn doubt(&mut self) {
        self.doubt = true;
    }

    /// One reading of a server's sessions, written ([`read_sessions`], [`write_reading`]). It
    /// works on a copy of the followed sessions. A reading or a write that fails leaves them as
    /// they were: the next reading ends a play at the last reading that saw it. Returns how many
    /// play now.
    pub async fn read(
        &mut self,
        deps: &PlaysDeps,
        server: &Arc<dyn MediaServer>,
    ) -> Result<usize, String> {
        if self.doubt {
            let open = store::open_sessions(&deps.pool, server.id())
                .await
                .map_err(|e| format!("the open plays could not be read: {e}"))?;
            let mut taken = Follower::default();
            taken.resume(open);
            self.follower = taken;
            self.doubt = false;
        }
        let mut follower = self.follower.clone();
        let reading = read_sessions(server, &mut follower, &mut self.files).await?;
        self.doubt = true;
        let written = write_reading(deps, reading).await;
        self.doubt = false;
        let open = written?;
        self.follower = follower;
        Ok(open)
    }
}

/// `player.sessions`: one reading of a media server's sessions ([`Following::read`]). The first
/// run of a start takes up the plays the table holds open: a session that still plays stays one
/// play, one that ended meanwhile ends at the last reading that saw it. A run that fails leaves
/// the followed sessions as they were. A run dropped while its reading is written leaves them in
/// doubt: the next run takes up the table first. While something plays the next run comes after
/// [`BUSY`], else after the schedule's interval, [`IDLE`]. A failed run while a play is open is
/// tried again after [`BUSY`], up to [`BUSY_RETRIES`] times in a row ([`retry_after`]). The job
/// writes no pass row and is no check of its server: a reading every few seconds would crowd out
/// the checks worth keeping.
pub struct SessionsJob {
    deps: PlaysDeps,
    servers: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Following>>>>,
}

impl SessionsJob {
    pub fn new(deps: PlaysDeps) -> SessionsJob {
        SessionsJob {
            deps,
            servers: Mutex::new(HashMap::new()),
        }
    }

    fn following(&self, server: &str) -> Arc<tokio::sync::Mutex<Following>> {
        self.servers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(server.to_string())
            .or_default()
            .clone()
    }
}

#[async_trait]
impl Handler for SessionsJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let key = cx.job.schedule.as_deref().unwrap_or_default();
        let id = key.strip_suffix(&format!(":{SESSIONS}")).unwrap_or(key);
        let server = self.deps.server(id)?;
        let following = self.following(id);
        let mut f = following.lock().await;
        let outcome = f.read(&self.deps, &server).await;
        match outcome {
            Ok(open) => {
                f.failures = 0;
                Ok(Done {
                    detail: match open {
                        0 => "Nothing playing".to_string(),
                        n => format!("{} playing", units::count(n as i64)),
                    },
                    again: f.busy().then_some(BUSY),
                    ..Done::default()
                })
            }
            Err(e) => {
                f.failures += 1;
                if f.failures == 1 {
                    tracing::warn!(server = %id, error = %e, "a reading of the sessions failed");
                }
                Err(Failed {
                    error: e,
                    again: Some(retry_after(f.busy(), f.failures)),
                    ..Failed::default()
                })
            }
        }
    }
}

/// `player.history`: a media server's history since the newest entry read
/// ([`server_history_pass`]), then the housekeeping. The source's cursor stays on its row
/// (`play_sources`). The pass row and the end of the job are one commit, through a journal fenced
/// by the run's lease.
pub struct ServerHistoryJob(pub PlaysDeps);

#[async_trait]
impl Handler for ServerHistoryJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let deps = &self.0;
        let id = cx.job.integration.clone().unwrap_or_default();
        let server = deps.server(&id)?;
        let journal = deps.journal.fenced(Arc::new(cx.lease.clone()));
        let started = Instant::now();
        let outcome = server_history_pass(deps, &server).await;
        tidy_after(deps).await;
        let detail = outcome.as_ref().ok().cloned();
        let passed = outcome.map(|_| PassOk::new(started.elapsed(), json!({})));
        let run = PassRun {
            cx: &cx,
            journal: &journal,
            engine: &id,
            pass: HISTORY,
        };
        end_run(
            &deps.pool,
            &deps.engines,
            &run,
            started,
            passed,
            detail,
            Last::Nothing,
        )
        .await
        .1
    }
}

/// `history.plays`: Tautulli's plays ([`tautulli_pass`]): all of them when its marker
/// (`history.whole_at`) is [`WHOLE_READ_EVERY`] old or missing, else the newest days. Then the
/// housekeeping. The pass row, the marker of a whole read and the end of the job are one commit,
/// through a journal fenced by the run's lease.
pub struct TautulliJob(pub PlaysDeps);

#[async_trait]
impl Handler for TautulliJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let deps = &self.0;
        let asked = cx.job.integration.clone().unwrap_or_default();
        let history = deps
            .engines
            .play_history
            .clone()
            .filter(|h| h.id() == asked)
            .ok_or_else(|| Failed::from(format!("no Tautulli `{asked}` runs in this process")))?;
        let id = history.id().to_string();
        let whole_at = claims::marked_at(&deps.pool, &id, WHOLE_AT)
            .await
            .map_err(|e| {
                Failed::from(format!(
                    "the marker of the whole read could not be read: {e}"
                ))
            })?;
        let whole = whole_at.is_none_or(|at| {
            chrono::Duration::from_std(WHOLE_READ_EVERY).is_ok_and(|every| Utc::now() - at >= every)
        });
        let journal = deps.journal.fenced(Arc::new(cx.lease.clone()));
        let started = Instant::now();
        let outcome = tautulli_pass(deps, &history, whole).await;
        tidy_after(deps).await;
        let last = if whole {
            Last::Markers(vec![WHOLE_AT])
        } else {
            Last::Nothing
        };
        let run = PassRun {
            cx: &cx,
            journal: &journal,
            engine: &id,
            pass: HISTORY,
        };
        end_run(
            &deps.pool,
            &deps.engines,
            &run,
            started,
            outcome,
            None,
            last,
        )
        .await
        .1
    }
}

/// `plays.tidy`: the housekeeping on a schedule of its own, where no history pass runs it
/// ([`tidies_alone`]), as often as a history would be read.
pub struct TidyJob(pub PlaysDeps);

#[async_trait]
impl Handler for TidyJob {
    async fn run(&self, _cx: Cx) -> Result<Done, Failed> {
        match housekeeping(&self.0).await {
            Ok(()) => Ok(Done::from("Plays tidied")),
            Err(e) => {
                tracing::warn!(error = %e, "the plays could not be tidied");
                Err(Failed::from(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_reading_is_tried_again_soon_while_something_plays() {
        assert_eq!(
            (
                retry_after(true, 1),
                retry_after(true, BUSY_RETRIES),
                retry_after(true, BUSY_RETRIES + 1),
                retry_after(false, 1)
            ),
            (BUSY, BUSY, IDLE, IDLE)
        );
    }
}
