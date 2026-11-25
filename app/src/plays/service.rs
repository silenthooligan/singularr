//! The passes that collect plays, and the one way a play is written.
//!
//! Three sources tell of plays, and each play is one row whoever told of it:
//!
//! - **Sessions** ([`live_pass`]): each media server's sessions (Plex, Jellyfin, Emby, through
//!   the one port), read every few seconds while something plays and every minute while nothing
//!   does. A session is a play from its first reading; a session that is gone is a finished
//!   play. A session's id names its play while that play is open: the same id read after the
//!   play ended starts another. Sessions are the only source of how a new play is delivered
//!   (direct play, direct stream or transcode, with the resolutions and bitrates). Every server
//!   has them: the statistics hold for any server without its history or Tautulli.
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

use super::model::{
    DIRECT_PLAY, Follower, HISTORY_EVERY, HISTORY_EVERY_DEFAULT, KEEP_ADDRESSES, KEEP_FOR,
    PlayRecord, closeness, from_history_play, from_server_play, from_session, merge,
};
use super::store;
use crate::catalog::store as catalog_store;
use crate::core::jobs::{self, JobSpec};
use crate::core::journal::Journal;
use crate::core::observe;
use crate::core::units;
use crate::engines::service::{EngineSet, PassOk, pass_result};
use crate::engines::{ItemMedia, MediaServer, PlayHistory};
use crate::lifecycle::sync::jitter;
use crate::settings::Settings;
use chrono::Utc;
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// The pass names, in jobs, metrics and the journal.
pub const SESSIONS: &str = "sessions";
pub const HISTORY: &str = "plays";
/// The housekeeping's own loop, where no history pass runs it.
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
    pub jitter: Duration,
    /// One writer at a time: what one source writes is matched against what the others wrote.
    pub writer: Arc<tokio::sync::Mutex<()>>,
}

impl PlaysDeps {
    pub fn new(
        pool: PgPool,
        journal: Journal,
        engines: Arc<EngineSet>,
        settings: Arc<Settings>,
        jitter: Duration,
    ) -> PlaysDeps {
        PlaysDeps {
            pool,
            journal,
            engines,
            settings,
            jitter,
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

/// One reading of the sessions: what plays now is written, with what each play adds up to so
/// far, and the plays that ended are closed. The file of a play the server transcodes or sends
/// in another container is asked of the server once per play: the session describes what is
/// sent. Returns how many play now.
pub async fn live_pass(
    deps: &PlaysDeps,
    server: &Arc<dyn MediaServer>,
    follower: &mut Follower,
    files: &mut HashMap<String, Option<ItemMedia>>,
) -> Result<usize, String> {
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
            if !files.contains_key(&session) {
                let media = server.item_media(&key).await.ok().flatten();
                files.insert(session.clone(), media);
            }
            if let Some(Some(media)) = files.get(&session) {
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
    if !follower.busy() {
        files.clear();
    }
    let open = reading.open.len();
    let mut all = reading.open;
    all.extend(reading.ended);
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

/// The loops: the sessions of each media server, the history of each that keeps one, and
/// Tautulli's plays. The housekeeping follows each history pass. With no history to read (a
/// Jellyfin or Emby server alone), it runs on a loop of its own.
pub fn spawn(deps: PlaysDeps) -> Vec<JoinHandle<()>> {
    let mut out = Vec::new();
    let mut tidied = false;
    for server in deps.engines.media_servers.iter().cloned() {
        out.push(tokio::spawn(live_loop(deps.clone(), server.clone())));
        if server.keeps_history() {
            out.push(tokio::spawn(history_loop(deps.clone(), server)));
            tidied = true;
        }
    }
    if let Some(history) = deps.engines.play_history.clone() {
        out.push(tokio::spawn(tautulli_loop(deps.clone(), history)));
        tidied = true;
    }
    if !tidied && !deps.engines.media_servers.is_empty() {
        out.push(tokio::spawn(housekeeping_loop(deps)));
    }
    out
}

/// The housekeeping on a loop of its own, as often as a history would be read.
async fn housekeeping_loop(deps: PlaysDeps) {
    let job = jobs::registry().register(JobSpec {
        id: format!("plays:{TIDY}"),
        pass: TIDY.to_string(),
        integration: None,
        every: history_every(&deps.settings),
    });
    job.wait(jitter(deps.jitter)).await;
    loop {
        let started = job.begin();
        let outcome = housekeeping(&deps)
            .await
            .map(|()| "Plays tidied".to_string())
            .map_err(|e| e.to_string());
        if let Err(e) = &outcome {
            tracing::warn!(error = %e, "the plays could not be tidied");
        }
        job.end(started, outcome);
        job.wait(history_every(&deps.settings)).await;
    }
}

async fn live_loop(deps: PlaysDeps, server: Arc<dyn MediaServer>) {
    let id = server.id().to_string();
    // A job of its own, not a check of the integration: a reading every few seconds would
    // crowd out the checks worth keeping.
    let job = jobs::registry().register(JobSpec {
        id: format!("{id}:{SESSIONS}"),
        pass: SESSIONS.to_string(),
        integration: None,
        every: IDLE,
    });
    let mut follower = Follower::default();
    match store::open_sessions(&deps.pool, &id).await {
        Ok(open) => follower.resume(open),
        Err(e) => tracing::warn!(server = %id, error = %e, "the open plays could not be read"),
    }
    let mut files = HashMap::new();
    let mut failing = false;
    job.wait(jitter(deps.jitter)).await;
    loop {
        let started = job.begin();
        let outcome = live_pass(&deps, &server, &mut follower, &mut files).await;
        job.end(
            started,
            outcome
                .as_ref()
                .map(|n| match n {
                    0 => "Nothing playing".to_string(),
                    n => format!("{} playing", units::count(*n as i64)),
                })
                .map_err(Clone::clone),
        );
        match &outcome {
            Err(e) if !failing => {
                tracing::warn!(server = %id, error = %e, "the sessions could not be read");
                failing = true;
            }
            Ok(_) => failing = false,
            Err(_) => {}
        }
        job.wait(if follower.busy() { BUSY } else { IDLE }).await;
    }
}

async fn history_loop(deps: PlaysDeps, server: Arc<dyn MediaServer>) {
    let id = server.id().to_string();
    let job = jobs::registry().register(JobSpec::of(&id, HISTORY, history_every(&deps.settings)));
    job.wait(jitter(deps.jitter)).await;
    loop {
        let started = job.begin();
        let outcome = server_history_pass(&deps, &server).await;
        if let Err(e) = housekeeping(&deps).await {
            tracing::warn!(error = %e, "the plays could not be tidied");
        }
        job.end(started, outcome.clone());
        observe::sync_pass(&id, HISTORY, started.elapsed(), outcome.is_ok());
        if let Err(e) = &outcome {
            tracing::warn!(server = %id, error = %e, "the history pass failed");
        }
        // The pass is the server's like its other passes: its failure shows on the server.
        let passed = outcome.map(|_| PassOk::new(started.elapsed(), json!({})));
        if let Err(e) = deps
            .engines
            .report(&deps.pool, &deps.journal, &id, HISTORY, passed)
            .await
        {
            tracing::error!(server = %id, error = %e, "the engine state could not be recorded");
        }
        job.wait(history_every(&deps.settings)).await;
    }
}

async fn tautulli_loop(deps: PlaysDeps, history: Arc<dyn PlayHistory>) {
    let id = history.id().to_string();
    let job = jobs::registry().register(JobSpec::of(&id, HISTORY, history_every(&deps.settings)));
    job.wait(jitter(deps.jitter)).await;
    let mut whole_at: Option<Instant> = None;
    loop {
        let started = job.begin();
        let whole = whole_at.is_none_or(|t| t.elapsed() >= WHOLE_READ_EVERY);
        let outcome = tautulli_pass(&deps, &history, whole).await;
        if outcome.is_ok() && whole {
            whole_at = Some(Instant::now());
        }
        if let Err(e) = housekeeping(&deps).await {
            tracing::warn!(error = %e, "the plays could not be tidied");
        }
        job.end(started, pass_result(&outcome));
        observe::sync_pass(&id, HISTORY, started.elapsed(), outcome.is_ok());
        if let Err(e) = &outcome {
            tracing::warn!(integration = %id, error = %e, "the Tautulli pass failed");
        }
        if let Err(e) = deps
            .engines
            .report(&deps.pool, &deps.journal, &id, HISTORY, outcome)
            .await
        {
            tracing::error!(integration = %id, error = %e, "the engine state could not be recorded");
        }
        job.wait(history_every(&deps.settings)).await;
    }
}
