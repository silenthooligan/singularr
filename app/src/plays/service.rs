//! The passes that collect plays, and the one way a play is written.
//!
//! Three sources tell of plays, and each play is one row whoever told of it:
//!
//! - **Sessions** ([`live_pass`]): the media server's sessions, read every few seconds while
//!   something plays and every minute while nothing does. A session is a play from its first
//!   reading; a session that is gone is a finished play. The only source of how a new play is
//!   delivered (direct play, direct stream or transcode, with the resolutions and bitrates).
//! - **The media server's history** ([`server_history_pass`]): what it marked as played, with
//!   who and on which device, newest first. The whole history at the first pass, then the
//!   entries newer than the newest read.
//! - **Tautulli** ([`tautulli_pass`]), when one is configured: years of plays with durations,
//!   devices and delivery. All of it at the first pass and every day, the newest days on every
//!   other pass.
//!
//! [`absorb`] writes what a source says: onto the stored play the source's own id names, else
//! onto the stored play it is another reading of (`model::closeness`), merged so that the
//! richer record wins field by field (`model::merge`), else as a new play. One writer at a time,
//! so two sources telling of one play at once still make one row.
//!
//! [`housekeeping`] links plays to the catalog titles the catalog has learned since, forgets
//! addresses unless the setting keeps them, and forgets plays older than the setting keeps.

use super::model::{
    Follower, HISTORY_EVERY, HISTORY_EVERY_DEFAULT, KEEP_ADDRESSES, KEEP_FOR, PlayRecord,
    TRANSCODE, closeness, from_history_play, from_server_play, from_session, merge,
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

/// Whether the players' addresses are kept. Off by default: only home network or outside is.
pub fn keep_addresses(settings: &Settings) -> bool {
    settings.value(KEEP_ADDRESSES).as_bool().unwrap_or(false)
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

/// Records made ready to store: linked to the catalog titles of their items, without
/// addresses unless the setting keeps them, and none older than plays are kept.
async fn prepare(
    deps: &PlaysDeps,
    records: Vec<PlayRecord>,
) -> Result<Vec<PlayRecord>, sqlx::Error> {
    let keys: Vec<String> = records
        .iter()
        .filter(|r| r.title_id.is_none())
        .filter_map(|r| r.item_key().map(str::to_string))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let titles: HashMap<String, i64> = catalog_store::server_item_titles(&deps.pool, &keys)
        .await?
        .into_iter()
        .collect();
    let keep = keep_addresses(&deps.settings);
    let cutoff = keep_days(&deps.settings).map(|d| Utc::now() - chrono::Duration::days(d));
    Ok(records
        .into_iter()
        .filter(|r| cutoff.is_none_or(|c| r.started_at >= c))
        .map(|mut r| {
            if r.title_id.is_none() {
                r.title_id = r.item_key().and_then(|k| titles.get(k).copied());
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
/// far, and the plays that ended are closed. The file of a transcoded video is asked of the
/// server once per play. Returns how many play now.
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
        if r.video_decision.as_deref() == Some(TRANSCODE)
            && r.source_resolution.is_none()
            && let (Some(session), Some(key)) = (r.session_id.clone(), r.rating_key.clone())
        {
            if !files.contains_key(&session) {
                let media = server.item_media(&key).await.ok().flatten();
                files.insert(session.clone(), media);
            }
            if let Some(Some(media)) = files.get(&session) {
                r.source_resolution = media.resolution.clone();
                r.source_bitrate = media.bitrate;
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
            .map(|p| from_server_play(id, p, &names, &places))
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
    let server = deps
        .engines
        .media_server
        .as_ref()
        .map(|s| s.id().to_string())
        .unwrap_or_else(|| history.id().to_string());
    let owner = deps
        .engines
        .media_server
        .as_ref()
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
    let keys = store::unlinked_keys(&deps.pool).await?;
    if !keys.is_empty() {
        let found = catalog_store::server_item_titles(&deps.pool, &keys).await?;
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

/// The loops: the sessions and the history of the media server, and Tautulli's plays.
pub fn spawn(deps: PlaysDeps) -> Vec<JoinHandle<()>> {
    let mut out = Vec::new();
    if let Some(server) = deps.engines.media_server.clone() {
        out.push(tokio::spawn(live_loop(deps.clone(), server.clone())));
        out.push(tokio::spawn(history_loop(deps.clone(), server)));
    }
    if let Some(history) = deps.engines.play_history.clone() {
        out.push(tokio::spawn(tautulli_loop(deps, history)));
    }
    out
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
