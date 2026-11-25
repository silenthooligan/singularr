//! The configured engines and integrations, their recorded health, and the shadow parse with
//! an arr.

use super::access::{AccessBook, AccessState};
use super::arr::{self, ArrClient, Radarr, Sonarr};
use super::clients;
use super::hush::{self, Hush};
use super::jellyfin::{self, Dialect, Jellyfin};
use super::model::{EngineStatus, IntegrationView, ShadowParseRequest, ShadowParseResponse};
use super::plex::{self, Plex};
use super::seerr::{self, Seerr};
use super::store;
use super::tautulli::{self, Tautulli};
use super::tdarr::{self, Tdarr};
use super::{
    DownloadClient, FoundRelease, LibraryEngine, MediaServer, PlayHistory, Processor, RequestSite,
};
use crate::catalog::model::ArrReading;
use crate::core::config::{
    ArrKind, Config, Integration, IntegrationType, MediaServerConfig, MediaServerKind,
    ProcessorKind,
};
use crate::core::error::{AppError, AppResult};
use crate::core::journal::{Journal, event};
use crate::core::observe;
use crate::core::outbound::{Allowlist, Calls, Outbound, OutboundError};
use crate::core::units;
use crate::policy::{ActivePolicy, first_lane_of_arr};
use crate::releases::service::{differences, reading_of};
use serde_json::json;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// An engine that is listed in the configuration, reachable or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineSlot {
    pub id: String,
    /// The integration type: `radarr`, `sonarr`, a download client's (`qbittorrent`,
    /// `sabnzbd`, ...), `seerr`, `plex`, a processing service's, `tautulli`.
    pub kind: String,
    pub library: Option<String>,
    pub configured: bool,
}

/// True for the kinds of engine that are arrs.
fn is_arr(kind: &str) -> bool {
    kind == ArrKind::Radarr.as_str() || kind == ArrKind::Sonarr.as_str()
}

/// Every engine this run talks to.
pub struct EngineSet {
    pub arrs: Vec<Arc<dyn LibraryEngine>>,
    /// Every download client that is configured, torrents and usenet alike.
    pub download_clients: Vec<Arc<dyn DownloadClient>>,
    /// The Seerr integration requests are read from, when it is configured.
    pub request_sites: Vec<Arc<dyn RequestSite>>,
    /// The media servers that are configured (Plex, Jellyfin, Emby), in the configuration's
    /// order.
    pub media_servers: Vec<Arc<dyn MediaServer>>,
    /// The processing services that are configured: workers and bridges.
    pub processors: Vec<Arc<dyn Processor>>,
    /// The play history plays are imported from (Tautulli), when it is configured.
    pub play_history: Option<Arc<dyn PlayHistory>>,
    pub slots: Vec<EngineSlot>,
    /// What Singularr may do with each integration: read it, or read it and act on it.
    pub access: AccessBook,
    /// The titles a manager searches for now.
    pub searches: Searches,
    health: Mutex<HashMap<String, bool>>,
}

/// How long the releases a search found can be grabbed by their names here: the managers keep
/// what an interactive search found about as long.
pub const FOUND_KEPT: Duration = Duration::from_secs(30 * 60);

/// The titles a manager is searching for now, each by a key of the caller's (`title:12`), so a
/// second search of a title is refused while the first runs (a search asks every indexer); and
/// what the last search of each title found, so a grab names a release that search listed.
#[derive(Debug, Default)]
pub struct Searches {
    held: Mutex<HashSet<String>>,
    found: Mutex<HashMap<String, (Instant, Vec<FoundRelease>)>>,
}

/// The titles of one search, held until it is dropped.
#[derive(Debug)]
pub struct SearchHold<'a> {
    set: &'a Searches,
    keys: Vec<String>,
}

impl Searches {
    /// Hold these titles for a search. `None` when one of them is held by another search; then
    /// none is held.
    pub fn hold(&self, keys: &[String]) -> Option<SearchHold<'_>> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        if keys.iter().any(|k| held.contains(k)) {
            return None;
        }
        held.extend(keys.iter().cloned());
        Some(SearchHold {
            set: self,
            keys: keys.to_vec(),
        })
    }

    /// Whether a search holds this title now.
    pub fn running(&self, key: &str) -> bool {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(key)
    }

    /// Keep what a search of a title found, in place of what the one before found. Searches
    /// older than [`FOUND_KEPT`] are let go.
    pub fn keep(&self, key: &str, found: Vec<FoundRelease>) {
        let mut kept = self.found.lock().unwrap_or_else(PoisonError::into_inner);
        kept.retain(|_, (at, _)| at.elapsed() < FOUND_KEPT);
        kept.insert(key.to_string(), (Instant::now(), found));
    }

    /// The release the last search of a title found under this guid and indexer, while it is
    /// kept.
    pub fn found(&self, key: &str, guid: &str, indexer_id: i64) -> Option<FoundRelease> {
        let kept = self.found.lock().unwrap_or_else(PoisonError::into_inner);
        let (at, found) = kept.get(key)?;
        if at.elapsed() >= FOUND_KEPT {
            return None;
        }
        found
            .iter()
            .find(|r| r.guid == guid && r.indexer_id == indexer_id)
            .cloned()
    }
}

impl Drop for SearchHold<'_> {
    fn drop(&mut self) {
        let mut held = self.set.held.lock().unwrap_or_else(PoisonError::into_inner);
        for k in &self.keys {
            held.remove(k);
        }
    }
}

/// The calls a media server is sent, and the address they are below: the API's root.
fn media_server_calls(m: &MediaServerConfig, url: &str) -> (String, Calls) {
    match m.kind {
        MediaServerKind::Plex => (url.to_string(), plex::calls()),
        MediaServerKind::Jellyfin => (
            jellyfin::api_root(url, Dialect::Jellyfin),
            jellyfin::calls(Dialect::Jellyfin),
        ),
        MediaServerKind::Emby => (
            jellyfin::api_root(url, Dialect::Emby),
            jellyfin::calls(Dialect::Emby),
        ),
    }
}

/// The adapter of a media server, when it has an address and a secret.
fn media_server(
    m: &MediaServerConfig,
    config: &Config,
    out: &Outbound,
) -> Option<Arc<dyn MediaServer>> {
    match m.kind {
        MediaServerKind::Plex => Plex::new(m, out.clone())
            .map(|p| Arc::new(p.with_web(&config.plex_web_url)) as Arc<dyn MediaServer>),
        MediaServerKind::Jellyfin => Jellyfin::new(m, Dialect::Jellyfin, out.clone())
            .map(|j| Arc::new(j) as Arc<dyn MediaServer>),
        MediaServerKind::Emby => Jellyfin::new(m, Dialect::Emby, out.clone())
            .map(|e| Arc::new(e) as Arc<dyn MediaServer>),
    }
}

/// The outbound allowlist for a configuration: the metadata providers, plus the origin of
/// every configured integration Singularr reads, each with the calls beyond GET its adapter
/// declares. A download client takes the reads its adapter lists and nothing that acts (the
/// qBittorrent sign-in, Transmission's `torrent-get` and no GET, Deluge's sign-in and listing
/// and no GET, NZBGet's three reading methods and no GET, SABnzbd's queue and history GETs
/// alone); a Seerr integration takes GET alone; a media server takes the GETs its adapter
/// declares and nothing else; a manager takes GET and its acts (search, grab, import, remove);
/// a processing service takes what its adapter lists (a worker's record store in its reading
/// modes, its acts). Every act passes only while that integration's access is `act`.
pub fn allowlist(config: &Config) -> Result<Allowlist, OutboundError> {
    let mut allow = Allowlist::with_metadata_providers();
    for a in config.arrs.iter().filter(|a| a.configured()) {
        if let Some(url) = &a.url {
            allow.allow_integration(&a.id, url, arr::calls(a.kind))?;
        }
    }
    for c in config.downloads.iter().filter(|c| c.configured()) {
        if let Some(url) = &c.url {
            allow.allow_integration(&c.id, url, clients::calls(c.kind))?;
        }
    }
    for site in config.seerr.iter().filter(|s| s.configured()) {
        allow.allow_engine(&site.id, &site.url, &[])?;
    }
    for m in config.media_servers.iter().filter(|m| m.configured()) {
        if let Some(url) = &m.url {
            let (root, calls) = media_server_calls(m, url);
            allow.allow_integration(&m.id, &root, calls)?;
        }
    }
    // Tautulli takes GETs of its API with the commands that read, and nothing else: its API
    // takes the commands that change or delete things as GETs too.
    if let Some(t) = config.tautulli.as_ref().filter(|t| t.configured())
        && let Some(url) = &t.url
    {
        allow.allow_integration(&t.id, url, tautulli::calls())?;
    }
    for p in &config.processors {
        if let Some(url) = &p.url {
            let calls = match p.kind {
                ProcessorKind::Tdarr => tdarr::calls(),
                ProcessorKind::Hush => hush::calls(),
            };
            allow.allow_integration(&p.id, url, calls)?;
        }
    }
    // Singularr's own search index, when one is configured: not an engine, so it takes writes.
    if let Some((url, _)) = config.search.meilisearch() {
        allow.allow_service("meilisearch", url)?;
    }
    Ok(allow)
}

impl EngineSet {
    pub fn from_config(config: &Config, out: &Outbound) -> EngineSet {
        let mut arrs: Vec<Arc<dyn LibraryEngine>> = Vec::new();
        let mut slots = Vec::new();
        for arr in &config.arrs {
            let client = ArrClient::new(arr, out.clone());
            slots.push(EngineSlot {
                id: arr.id.clone(),
                kind: arr.kind.as_str().to_string(),
                library: Some(arr.library.clone()),
                configured: client.is_some(),
            });
            if let Some(client) = client {
                let engine: Arc<dyn LibraryEngine> = match arr.kind {
                    ArrKind::Radarr => Arc::new(Radarr::new(client)),
                    ArrKind::Sonarr => Arc::new(Sonarr(client)),
                };
                arrs.push(engine);
            }
        }
        let mut download_clients: Vec<Arc<dyn DownloadClient>> = Vec::new();
        for c in &config.downloads {
            let adapter = clients::adapter(c, out);
            slots.push(EngineSlot {
                id: c.id.clone(),
                kind: c.kind.as_str().to_string(),
                library: None,
                configured: adapter.is_some(),
            });
            download_clients.extend(adapter);
        }
        let mut request_sites: Vec<Arc<dyn RequestSite>> = Vec::new();
        for site in &config.seerr {
            let client = Seerr::new(site, out.clone());
            slots.push(EngineSlot {
                id: site.id.clone(),
                kind: seerr::KIND.to_string(),
                library: None,
                configured: client.is_some(),
            });
            if let Some(client) = client {
                request_sites.push(Arc::new(client));
            }
        }
        let mut media_servers: Vec<Arc<dyn MediaServer>> = Vec::new();
        for m in &config.media_servers {
            let server = media_server(m, config, out);
            slots.push(EngineSlot {
                id: m.id.clone(),
                kind: m.kind.as_str().to_string(),
                library: None,
                configured: server.is_some(),
            });
            media_servers.extend(server);
        }
        let mut processors: Vec<Arc<dyn Processor>> = Vec::new();
        for p in &config.processors {
            let adapter: Option<Arc<dyn Processor>> = match p.kind {
                ProcessorKind::Tdarr => {
                    Tdarr::new(p, out.clone()).map(|t| Arc::new(t) as Arc<dyn Processor>)
                }
                ProcessorKind::Hush => {
                    Hush::new(p, out.clone()).map(|h| Arc::new(h) as Arc<dyn Processor>)
                }
            };
            slots.push(EngineSlot {
                id: p.id.clone(),
                kind: p.kind.as_str().to_string(),
                library: None,
                configured: adapter.is_some(),
            });
            processors.extend(adapter);
        }
        let play_history: Option<Arc<dyn PlayHistory>> = config.tautulli.as_ref().and_then(|t| {
            let history =
                Tautulli::new(t, out.clone()).map(|h| Arc::new(h) as Arc<dyn PlayHistory>);
            slots.push(EngineSlot {
                id: t.id.clone(),
                kind: tautulli::KIND.to_string(),
                library: None,
                configured: history.is_some(),
            });
            history
        });
        let mut set = EngineSet::new(arrs, download_clients, slots)
            .with_request_sites(request_sites)
            .with_processors(processors);
        set.media_servers = media_servers;
        set.play_history = play_history;
        // The book shares the switches of the allowlist the adapters call through, so what it
        // says is what `core::outbound` enforces.
        set.access = AccessBook::new(config, out.allowlist());
        set
    }

    /// A set of these managers and download clients (a list, or `None` and `Some` for none
    /// and one), with the slots of the configuration.
    pub fn new(
        arrs: Vec<Arc<dyn LibraryEngine>>,
        download_clients: impl IntoIterator<Item = Arc<dyn DownloadClient>>,
        slots: Vec<EngineSlot>,
    ) -> EngineSet {
        EngineSet {
            arrs,
            download_clients: download_clients.into_iter().collect(),
            request_sites: Vec::new(),
            media_servers: Vec::new(),
            processors: Vec::new(),
            play_history: None,
            slots,
            access: AccessBook::default(),
            searches: Searches::default(),
            health: Mutex::new(HashMap::new()),
        }
    }

    /// The same set with these Seerr sites. Their slots are listed by the caller.
    pub fn with_request_sites(mut self, sites: Vec<Arc<dyn RequestSite>>) -> EngineSet {
        self.request_sites = sites;
        self
    }

    /// The same set with these processing services. Their slots are listed by the caller.
    pub fn with_processors(mut self, processors: Vec<Arc<dyn Processor>>) -> EngineSet {
        self.processors = processors;
        self
    }

    /// The processing service of an integration, when it is configured.
    pub fn processor(&self, id: &str) -> Option<&Arc<dyn Processor>> {
        self.processors.iter().find(|p| p.id() == id)
    }

    /// The first media server: the one read where a record names no server of its own (the
    /// workspace's server name, a recording asked for without one).
    pub fn media_server(&self) -> Option<&Arc<dyn MediaServer>> {
        self.media_servers.first()
    }

    /// The media server of an integration, when it is configured.
    pub fn media_server_of(&self, id: &str) -> Option<&Arc<dyn MediaServer>> {
        self.media_servers.iter().find(|s| s.id() == id)
    }

    /// The Plex server, when one is configured: the server a play history of Plex (Tautulli)
    /// tells of.
    pub fn plex_server(&self) -> Option<&Arc<dyn MediaServer>> {
        self.media_servers.iter().find(|s| s.kind() == plex::KIND)
    }

    /// The download client of an integration, when it is configured.
    pub fn download_client(&self, id: &str) -> Option<&Arc<dyn DownloadClient>> {
        self.download_clients.iter().find(|c| c.id() == id)
    }

    pub fn arr(&self, id: &str) -> Option<&Arc<dyn LibraryEngine>> {
        self.arrs
            .iter()
            .find(|a| a.engine().id.eq_ignore_ascii_case(id))
    }

    /// The Seerr integration that takes requests for this media server (`plex`), when it is
    /// configured.
    pub fn request_site(&self, site: &str) -> Option<&Arc<dyn RequestSite>> {
        self.request_sites
            .iter()
            .find(|s| s.site().eq_ignore_ascii_case(site))
    }

    /// Ids of the arrs in the configuration, configured or not.
    pub fn arr_ids(&self) -> Vec<String> {
        self.slots
            .iter()
            .filter(|s| is_arr(&s.kind))
            .map(|s| s.id.clone())
            .collect()
    }

    /// True when every arr integration of the configuration has an address and a key.
    pub fn every_arr_configured(&self) -> bool {
        let mut arrs = self.slots.iter().filter(|s| is_arr(&s.kind)).peekable();
        arrs.peek().is_some() && arrs.all(|s| s.configured)
    }

    pub fn slot(&self, id: &str) -> Option<&EngineSlot> {
        self.slots.iter().find(|s| s.id == id)
    }

    /// Record how a call to an engine went. When the engine changes between reachable and
    /// not, an `engine_up` or `engine_down` event is appended in the same transaction.
    pub async fn report(
        &self,
        pool: &PgPool,
        journal: &Journal,
        engine: &str,
        pass: &'static str,
        outcome: Result<PassOk, String>,
    ) -> Result<(), sqlx::Error> {
        let (kind, library) = self
            .slot(engine)
            .map(|s| (s.kind.clone(), s.library.clone()))
            .unwrap_or_else(|| (engine.to_string(), None));
        let now_ok = outcome.is_ok();
        observe::engine_up(engine, now_ok);
        let known = self.health.lock().unwrap().get(engine).copied();

        let mut tx = journal.begin(pool).await?;
        let before = match known {
            Some(v) => Some(v),
            None => store::was_ok(tx.conn(), engine).await?,
        };
        match &outcome {
            Ok(ok) => {
                store::record_ok(
                    tx.conn(),
                    engine,
                    &kind,
                    library.as_deref(),
                    ok.version.as_deref(),
                    i32::try_from(ok.took.as_millis()).unwrap_or(i32::MAX),
                    &ok.counts,
                )
                .await?
            }
            Err(error) => {
                store::record_error(tx.conn(), engine, &kind, library.as_deref(), error).await?
            }
        }
        if before != Some(now_ok) {
            let (kind_of_event, body) = match &outcome {
                Ok(ok) => (
                    "engine_up",
                    json!({ "engine": engine, "pass": pass, "version": ok.version, "first_contact": before.is_none() }),
                ),
                Err(error) => (
                    "engine_down",
                    json!({ "engine": engine, "pass": pass, "error": error, "first_contact": before.is_none() }),
                ),
            };
            tx.append_one(event(
                kind_of_event,
                "engine",
                engine,
                &format!("sync:{engine}"),
                body,
            ))
            .await?;
        }
        tx.commit().await?;
        self.health
            .lock()
            .unwrap()
            .insert(engine.to_string(), now_ok);
        Ok(())
    }
}

/// What a successful pass tells the state table.
#[derive(Debug, Clone)]
pub struct PassOk {
    pub took: Duration,
    pub version: Option<String>,
    /// Merged into the engine's counts, for example `{"titles": 4470}`.
    pub counts: serde_json::Value,
}

impl PassOk {
    pub fn new(took: Duration, counts: serde_json::Value) -> Self {
        PassOk {
            took,
            version: None,
            counts,
        }
    }

    /// What the pass read, counts only: `4,470 titles`, `3 in queue`.
    pub fn summary(&self) -> String {
        counts_text(&self.counts)
    }
}

/// How a pass ended, for its job: what it read, or why it failed.
pub fn pass_result(outcome: &Result<PassOk, String>) -> Result<String, String> {
    outcome.as_ref().map(PassOk::summary).map_err(Clone::clone)
}

/// Every engine in the configuration with its recorded state.
pub async fn statuses(pool: &PgPool, set: &EngineSet) -> AppResult<Vec<EngineStatus>> {
    let rows = store::list(pool).await?;
    Ok(set
        .slots
        .iter()
        .map(|slot| {
            let row = rows.iter().find(|r| r.engine == slot.id);
            EngineStatus {
                engine: slot.id.clone(),
                kind: slot.kind.clone(),
                library: slot.library.clone(),
                configured: slot.configured,
                ok: slot.configured && row.is_some_and(|r| r.ok),
                version: row.and_then(|r| r.version.clone()),
                last_sync_at: row.and_then(|r| r.last_sync_at),
                last_ok_at: row.and_then(|r| r.last_ok_at),
                last_error: row.and_then(|r| r.last_error.clone()),
                latency_ms: row.and_then(|r| r.latency_ms),
                latency_ms_history: row.map(|r| r.latency_history.clone()).unwrap_or_default(),
                counts: row.map(|r| r.counts.clone()).unwrap_or_else(|| json!({})),
                failures: row.map(|r| r.failures).unwrap_or(0),
            }
        })
        .collect())
}

/// The integration types this version reads. The others are listed with their configuration
/// and marked as not read yet.
pub const READ_TYPES: [IntegrationType; 14] = [
    IntegrationType::Plex,
    IntegrationType::Jellyfin,
    IntegrationType::Emby,
    IntegrationType::Radarr,
    IntegrationType::Sonarr,
    IntegrationType::Qbittorrent,
    IntegrationType::Transmission,
    IntegrationType::Deluge,
    IntegrationType::Sabnzbd,
    IntegrationType::Nzbget,
    IntegrationType::Tdarr,
    IntegrationType::Hush,
    IntegrationType::Seerr,
    IntegrationType::Tautulli,
];

/// What a processing integration of this type is in the pipeline, configured or not: `bridge`
/// or `worker`. `None` for a type that processes nothing.
pub fn processing_role(kind: IntegrationType) -> Option<&'static str> {
    match kind {
        IntegrationType::Hush => Some(super::hush::ROLE),
        IntegrationType::Tdarr => Some(super::tdarr::ROLE),
        _ => None,
    }
}

/// Every integration of the configuration with its health and what Singularr may do with it:
/// what the workspace record lists.
pub async fn integrations(
    pool: &PgPool,
    config: &Config,
    access: &AccessBook,
) -> AppResult<Vec<IntegrationView>> {
    let rows = store::list(pool).await?;
    Ok(config
        .integrations
        .iter()
        .map(|i| integration_view(i, rows.iter().find(|r| r.engine == i.id), &access.of(&i.id)))
        .collect())
}

/// One integration's view, from its configuration, its recorded state and what Singularr may do
/// with it. Pure.
pub fn integration_view(
    i: &Integration,
    row: Option<&store::EngineStateRow>,
    access: &AccessState,
) -> IntegrationView {
    let (state, detail) = if let Some(why) = i.missing() {
        ("warn", format!("Not configured: {why}"))
    } else if !READ_TYPES.contains(&i.kind) {
        ("warn", "Not supported".to_string())
    } else {
        match row {
            None => ("warn", "Not contacted".to_string()),
            Some(r) if r.ok => ("ok", counts_text(&r.counts)),
            Some(r) => (
                "down",
                r.last_error
                    .clone()
                    .unwrap_or_else(|| "Last call failed".to_string()),
            ),
        }
    };
    IntegrationView {
        id: i.id.clone(),
        kind: i.kind.as_str().to_string(),
        name: i.name.clone(),
        role: i.role.clone(),
        state: state.to_string(),
        version: row.and_then(|r| r.version.clone()),
        detail,
        access: access.access.as_str().to_string(),
        access_source: access.source.to_string(),
        actions: access.action_views(),
    }
}

/// The counts an integration's line says, in the line's order, with their words for one and
/// for many. The passes record more (`counts` keeps every one); the line names these alone.
const COUNT_WORDS: &[(&str, &str, &str)] = &[
    ("titles", "title", "titles"),
    ("items", "item", "items"),
    ("albums", "album", "albums"),
    ("tracks", "track", "tracks"),
    ("queue", "in queue", "in queue"),
    ("grabs_known", "grab", "grabs"),
    ("torrents", "torrent", "torrents"),
    ("downloads", "download", "downloads"),
    ("requests", "request", "requests"),
    ("pending_on_site", "pending", "pending"),
    ("runs", "run", "runs"),
    ("runs_open", "open", "open"),
    ("nodes", "node", "nodes"),
    ("workers_busy", "busy", "busy"),
    ("plays", "play", "plays"),
];

/// What the last passes counted, counts only (`docs/copy.md`): `4,470 titles, 3 in queue`.
fn counts_text(counts: &serde_json::Value) -> String {
    let parts: Vec<String> = COUNT_WORDS
        .iter()
        .filter_map(|(key, one, many)| {
            counts
                .get(key)
                .and_then(serde_json::Value::as_i64)
                .map(|n| units::counted(n, one, many))
        })
        .collect();
    if parts.is_empty() {
        "Online".to_string()
    } else {
        parts.join(", ")
    }
}

/// Read one name with Singularr and with an arr's own parser (a GET to the arr).
pub async fn shadow_parse(
    set: &EngineSet,
    active: &ActivePolicy,
    req: &ShadowParseRequest,
) -> AppResult<ShadowParseResponse> {
    let name = req.name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("`name` is empty."));
    }
    if name.len() > 1000 {
        return Err(AppError::bad_request("`name` is too long."));
    }
    let arr_id = req.arr.trim();
    let Some(slot) = set.slot(arr_id).filter(|s| is_arr(&s.kind)) else {
        return Err(AppError::bad_request(format!(
            "`arr` is one of {}, not `{arr_id}`.",
            set.arr_ids().join(", ")
        )));
    };
    let engine = set
        .arr(arr_id)
        .ok_or_else(|| AppError::Upstream(format!("{} is not configured.", slot.id)))?;
    let lane = req
        .lane
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .or_else(|| first_lane_of_arr(&active.policy, arr_id));
    let arr_parse = engine
        .parse(name)
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    let (singularr, singularr_reading) = reading_of(active, name, lane.as_deref());
    let arr_reading = ArrReading {
        quality: arr_parse.quality.clone(),
        group: arr_parse.group.clone(),
        edition: arr_parse.edition.clone(),
        custom_formats: arr_parse.custom_formats.clone(),
        score: arr_parse.score,
    };
    Ok(ShadowParseResponse {
        name: name.to_string(),
        arr: arr_id.to_string(),
        lane,
        differs: differences(&arr_reading, &singularr_reading, &singularr),
        singularr,
        singularr_reading,
        arr_parse,
        arr_reading,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::{Access, Body, Method};
    use std::collections::BTreeMap;

    fn config() -> Config {
        Config::from(&BTreeMap::from([
            (
                "DATABASE_URL",
                "postgres://singularr:x@127.0.0.1:1/singularr",
            ),
            ("RADARR_URL", "http://10.0.0.61:7979"),
            ("RADARR_API_KEY", "k1"),
            ("SONARR_URL", "http://10.0.0.61:8989"),
            ("QBIT_URL", "http://10.0.0.62:8080"),
            ("SEERR_URL", "http://10.0.0.62:5055"),
            ("SEERR_API_KEY", "k3"),
        ]))
        .unwrap()
    }

    /// A second Radarr from the instance file: two integrations of one type.
    fn two_radarrs() -> Config {
        let file = "[[integration]]\nid = \"radarr-4k\"\ntype = \"radarr\"\nurl = \"http://10.0.0.61:7878\"\n";
        let env = BTreeMap::from([
            (
                "DATABASE_URL",
                "postgres://singularr:x@127.0.0.1:1/singularr",
            ),
            ("RADARR_URL", "http://10.0.0.61:7979"),
            ("RADARR_API_KEY", "k1"),
            ("RADARR_4K_API_KEY", "k2"),
        ]);
        Config::with_instance(&env, Some(("/x/instance.toml", file))).unwrap()
    }

    #[test]
    fn a_seerr_integration_is_listed_only_with_a_key_and_takes_no_write() {
        let allow = allowlist(&config()).unwrap();
        let rule = allow
            .hosts()
            .find(|(h, _)| h.as_str() == "http://10.0.0.62:5055")
            .map(|(_, r)| r.clone())
            .expect("the Seerr integration is listed");
        assert!(rule.read_only && rule.reads.is_empty() && rule.acts.is_empty());
        assert_eq!(rule.label, "seerr");
        for method in [Method::Post, Method::Put, Method::Delete] {
            assert!(
                allow
                    .check(method, "http://10.0.0.62:5055/api/v1/request/1/approve")
                    .is_err()
            );
        }
        let out = Outbound::new(allow, Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config(), &out);
        assert_eq!(set.request_sites.len(), 1);
        assert!(set.request_site(seerr::SITE).is_some());
        let slot = set.slot("seerr").unwrap();
        assert_eq!(
            (slot.kind.as_str(), slot.library.as_deref(), slot.configured),
            ("seerr", None, true)
        );
        assert!(
            !set.arr_ids().iter().any(|id| id.ends_with("seerr")),
            "a Seerr integration is not an arr"
        );
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("SEERR_URL", "http://10.0.0.62:5055"),
        ]);
        let without_key = Config::from(&e).unwrap();
        assert!(
            !allowlist(&without_key)
                .unwrap()
                .hosts()
                .any(|(h, _)| h.as_str() == "http://10.0.0.62:5055"),
            "a Seerr integration without a key is not listed"
        );
    }

    #[test]
    fn the_allowlist_holds_the_configured_engines_read_only() {
        let allow = allowlist(&two_radarrs()).unwrap();
        let hosts: Vec<&String> = allow.hosts().map(|(h, _)| h).collect();
        assert!(hosts.iter().any(|h| h.as_str() == "http://10.0.0.61:7979"));
        assert!(hosts.iter().any(|h| h.as_str() == "http://10.0.0.61:7878"));
        let allow = allowlist(&config()).unwrap();
        let hosts: Vec<&String> = allow.hosts().map(|(h, _)| h).collect();
        assert!(hosts.iter().any(|h| h.as_str() == "http://10.0.0.62:8080"));
        assert!(
            !hosts.iter().any(|h| h.as_str() == "http://10.0.0.61:8989"),
            "an engine without a key is not listed"
        );
        assert!(
            allow.hosts().all(|(_, rule)| rule.read_only),
            "no integration is a free host"
        );
        for (origin, rule) in allow.hosts() {
            // A manager has its acts and the paths it never takes; nothing else has an act.
            if rule.label.starts_with("radarr") {
                assert_eq!(
                    rule.acts
                        .iter()
                        .map(|c| (c.method, c.path.as_str()))
                        .collect::<Vec<_>>(),
                    vec![
                        (Method::Post, "/api/v3/release"),
                        (Method::Post, "/api/v3/command"),
                        (Method::Post, "/api/v3/command"),
                        (Method::Delete, "/api/v3/queue/{id}"),
                    ],
                    "{origin}"
                );
                assert!(rule.never.iter().any(|p| p == "/api/v3/movie/{id}"));
                assert_eq!(rule.access.get(), Access::Read, "{origin}");
            } else {
                assert!(rule.acts.is_empty(), "{origin} must take no act");
            }
            assert!(rule.gets.is_empty(), "{origin}");
            if rule.label != "qbittorrent" && rule.label != "tvdb" {
                assert!(
                    rule.reads.is_empty(),
                    "{origin} must take no POST that reads"
                );
            }
        }
        // The guarantee, end to end on the real list: under read access no write reaches a
        // manager, and the download client takes none at all.
        for method in [Method::Post, Method::Put, Method::Delete] {
            for url in [
                "http://10.0.0.61:7979/api/v3/movie/1",
                "http://10.0.0.61:7979/api/v3/command",
                "http://10.0.0.61:7979/api/v3/release",
                "http://10.0.0.61:7979/api/v3/queue/7",
            ] {
                assert!(
                    allow.check(method, url).is_err(),
                    "{} {url}",
                    method.as_str()
                );
            }
            assert!(
                allow
                    .check(method, "http://10.0.0.62:8080/api/v2/torrents/delete")
                    .is_err()
            );
        }
    }

    #[test]
    fn an_integration_says_why_it_is_not_ok() {
        let config = config();
        let view = |id: &str, row: Option<&store::EngineStateRow>| {
            integration_view(
                config.integration(id).unwrap(),
                row,
                &AccessState::read_only(),
            )
        };
        let sonarr = view("sonarr", None);
        assert_eq!(
            (
                sonarr.kind.as_str(),
                sonarr.state.as_str(),
                sonarr.role.as_str()
            ),
            ("sonarr", "warn", "shows")
        );
        assert!(sonarr.detail.contains("SONARR_API_KEY"));
        assert_eq!(view("radarr", None).detail, "Not contacted");
        let row = |ok: bool| store::EngineStateRow {
            engine: "radarr".to_string(),
            kind: "radarr".to_string(),
            library_id: Some("radarr".to_string()),
            ok,
            version: Some("6.0.4".to_string()),
            last_sync_at: None,
            last_ok_at: None,
            last_error: (!ok).then(|| "connection refused".to_string()),
            latency_ms: None,
            latency_history: vec![],
            counts: serde_json::json!({ "titles": 4470 }),
            failures: 0,
        };
        let up = view("radarr", Some(&row(true)));
        assert_eq!(
            (up.state.as_str(), up.version.as_deref(), up.detail.as_str()),
            ("ok", Some("6.0.4"), "4,470 titles")
        );
        let down = view("radarr", Some(&row(false)));
        assert_eq!(
            (down.state.as_str(), down.detail.as_str()),
            ("down", "connection refused")
        );
        let json = serde_json::to_value(&up).unwrap();
        assert_eq!(json["type"], "radarr");
        let mut keys: Vec<&String> = json.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "access",
                "accessSource",
                "actions",
                "detail",
                "id",
                "name",
                "role",
                "state",
                "type",
                "version"
            ]
        );
        // A manager has no action: it is read, by default.
        assert_eq!(
            (&json["access"], &json["accessSource"], &json["actions"]),
            (
                &serde_json::json!("read"),
                &serde_json::json!("default"),
                &serde_json::json!([])
            )
        );
    }

    #[test]
    fn a_line_is_counts_only_and_a_count_of_one_names_one_thing() {
        let text = |counts: serde_json::Value| counts_text(&counts);
        assert_eq!(
            text(serde_json::json!({"nodes": 1, "workers_busy": 0})),
            "1 node, 0 busy"
        );
        assert_eq!(
            text(serde_json::json!({"nodes": 2, "workers_busy": 1})),
            "2 nodes, 1 busy"
        );
        assert_eq!(
            text(serde_json::json!({"runs": 1, "runs_open": 1})),
            "1 run, 1 open"
        );
        assert_eq!(
            text(serde_json::json!({"runs": 1017, "runs_open": 2})),
            "1,017 runs, 2 open"
        );
        // In the line's order, whatever the order of the keys; counts it does not name stay
        // out of it.
        assert_eq!(
            text(serde_json::json!({"queue": 3, "titles": 4470, "titles_matched": 1})),
            "4,470 titles, 3 in queue"
        );
        // Only numbers are counts.
        assert_eq!(
            text(serde_json::json!({"name": "Home", "plex_pass": true, "pending_on_site": 1})),
            "1 pending"
        );
        assert_eq!(text(serde_json::json!({})), "Online");
    }

    #[test]
    fn the_plex_server_is_an_engine_read_with_get_alone() {
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("PLEX_URL", "http://10.0.0.30:32400/"),
            ("PLEX_TOKEN", "plex-token"),
        ]);
        let config = Config::from(&e).unwrap();
        let allow = allowlist(&config).unwrap();
        let rule = allow
            .hosts()
            .find(|(h, _)| h.as_str() == "http://10.0.0.30:32400")
            .map(|(_, r)| r.clone())
            .expect("the Plex server is listed");
        assert!(rule.read_only && rule.reads.is_empty() && rule.acts.is_empty());
        // Plex starts work on some GETs: the adapter's own paths are the GETs it is sent.
        assert_eq!(rule.gets.len(), plex::calls().gets.len());
        assert!(
            allow
                .check(Method::Get, "http://10.0.0.30:32400/library/sections")
                .is_ok()
        );
        assert!(
            allow
                .check(
                    Method::Get,
                    "http://10.0.0.30:32400/library/sections/1/refresh"
                )
                .is_err()
        );
        let out = Outbound::new(allow, Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        assert!(set.media_server().is_some());
        assert_eq!(set.slot("plex").unwrap().kind, "plex");
        assert_eq!(set.plex_server().map(|s| s.id()), Some("plex"));
        assert!(!config.to_string().contains("plex-token"));
        // Without a token it is listed and not asked.
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("PLEX_URL", "http://10.0.0.30:32400"),
        ]);
        let config = Config::from(&e).unwrap();
        let out = Outbound::new(allowlist(&config).unwrap(), Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        assert!(set.media_server().is_none() && !set.slot("plex").unwrap().configured);
    }

    #[test]
    fn jellyfin_and_emby_are_media_servers_read_with_their_declared_gets() {
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("PLEX_URL", "http://10.0.0.30:32400"),
            ("PLEX_TOKEN", "plex-token"),
            ("JELLYFIN_URL", "http://10.0.0.31:8096/"),
            ("JELLYFIN_API_KEY", "jellyfin-key"),
            ("EMBY_URL", "http://10.0.0.32:8096"),
            ("EMBY_API_KEY", "emby-key"),
            ("EMBY_ACCESS", "act"),
        ]);
        let config = Config::from(&e).unwrap();
        let allow = allowlist(&config).unwrap();
        // Each is listed at its API's root: Emby's is under /emby.
        for (address, label) in [
            ("http://10.0.0.31:8096", "jellyfin"),
            ("http://10.0.0.32:8096/emby", "emby"),
        ] {
            let rule = allow
                .hosts()
                .find(|(h, _)| h.as_str() == address)
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| panic!("{label} is listed"));
            assert!(rule.read_only && rule.reads.is_empty() && rule.acts.is_empty());
            assert_eq!((rule.label.as_str(), rule.gets.len()), (label, 6));
        }
        // Emby's API is under /emby; a GET that would start a stream is refused on both.
        assert!(
            allow
                .check(Method::Get, "http://10.0.0.32:8096/emby/Sessions")
                .is_ok()
        );
        assert!(
            allow
                .check(Method::Get, "http://10.0.0.32:8096/Sessions")
                .is_err()
        );
        assert!(
            allow
                .check(Method::Get, "http://10.0.0.31:8096/Library/VirtualFolders")
                .is_ok()
        );
        for url in [
            "http://10.0.0.31:8096/Videos/abc/stream",
            "http://10.0.0.32:8096/emby/Videos/abc/stream",
        ] {
            assert!(allow.check(Method::Get, url).is_err(), "{url}");
        }
        let out = Outbound::new(allow, Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        let servers: Vec<(&str, &str)> = set
            .media_servers
            .iter()
            .map(|s| (s.id(), s.kind()))
            .collect();
        assert_eq!(
            servers,
            vec![("plex", "plex"), ("jellyfin", "jellyfin"), ("emby", "emby")]
        );
        assert_eq!(set.media_server().map(|s| s.id()), Some("plex"));
        assert_eq!(set.media_server_of("emby").map(|s| s.kind()), Some("emby"));
        assert_eq!(set.slot("jellyfin").unwrap().kind, "jellyfin");
        // A media server is read: `act` gives it nothing to do.
        assert_eq!(set.access.of("emby").access, Access::Read);
        assert!(set.access.acting().is_empty());
        let view = integration_view(
            config.integration("jellyfin").unwrap(),
            None,
            &set.access.of("jellyfin"),
        );
        assert_eq!(
            (
                view.state.as_str(),
                view.role.as_str(),
                view.detail.as_str()
            ),
            ("warn", "media server", "Not contacted")
        );
        let text = config.to_string();
        assert!(!text.contains("jellyfin-key") && !text.contains("emby-key"));
    }

    #[test]
    fn processing_services_are_engines_with_the_calls_their_adapters_declare() {
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("TDARR_URL", "http://10.0.0.60:8265"),
            ("HUSH_URL", "http://10.0.0.60:8787/"),
            ("PROWLARR_URL", "http://10.0.0.62:9696"),
            ("PROWLARR_API_KEY", "k9"),
        ]);
        let config = Config::from(&e).unwrap();
        let allow = allowlist(&config).unwrap();
        for (origin, label) in [
            ("http://10.0.0.60:8265", "tdarr"),
            ("http://10.0.0.60:8787", "hush"),
        ] {
            let rule = allow
                .hosts()
                .find(|(h, _)| h.as_str() == origin)
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| panic!("{label} is listed"));
            assert!(rule.read_only);
            assert_eq!(rule.label, label);
            // Its access is `read` until the owner or the configuration says otherwise.
            assert_eq!(rule.access.get(), Access::Read);
            for method in [Method::Post, Method::Put, Method::Delete] {
                for path in ["/api/v2/cruddb", "/stage", "/strip-subs"] {
                    assert!(
                        allow.check(method, &format!("{origin}{path}")).is_err(),
                        "{label} takes no {} {path} while it is read",
                        method.as_str()
                    );
                }
            }
        }
        // The worker has the reads it takes with POST, each with the body that makes it one:
        // its record store in a reading mode, its tables, its reports, its history and its
        // server's resources.
        let worker = allow
            .hosts()
            .find(|(_, r)| r.label == "tdarr")
            .map(|(_, r)| r.clone())
            .unwrap();
        // Its acts, each with the body that makes it that act, and the paths it never takes.
        assert_eq!(
            worker
                .acts
                .iter()
                .map(|c| (c.method, c.path.as_str(), c.body.is_some()))
                .collect::<Vec<_>>(),
            vec![
                (Method::Post, "/api/v2/alter-worker-limit", true),
                (Method::Post, "/api/v2/update-node", true),
                (Method::Post, "/api/v2/cancel-worker-item", true),
                (Method::Post, "/api/v2/bulk-update-files", true),
                (Method::Post, "/api/v2/scan-files", true),
                (Method::Post, "/api/v2/toggle-folder-watch", true),
                (Method::Post, "/api/v2/cruddb", true),
            ]
        );
        assert_eq!(
            worker.never,
            [
                "/api/v2/restart-ui",
                "/api/v2/stop-dedupe",
                "/api/v2/admin/"
            ]
        );
        assert_eq!(
            worker
                .reads
                .iter()
                .map(|c| (c.method, c.path.as_str(), c.body.is_some()))
                .collect::<Vec<_>>(),
            vec![
                (Method::Post, "/api/v2/cruddb", true),
                (Method::Post, "/api/v2/client/status-tables", true),
                (Method::Post, "/api/v2/list-footprintId-reports", true),
                (Method::Post, "/api/v2/read-job-file", true),
                (Method::Post, "/api/v2/stats/get-space-saved", true),
                (Method::Post, "/api/v2/stats/get-worker-verdict-hist", true),
                (Method::Post, "/api/v2/get-res-stats", true),
            ]
        );
        let reading =
            Body::Json(r#"{"data": {"collection": "FlowsJSONDB", "mode": "getAll"}}"#.into());
        let writing =
            Body::Json(r#"{"data": {"collection": "FlowsJSONDB", "mode": "removeAll"}}"#.into());
        let records = "http://10.0.0.60:8265/api/v2/cruddb";
        assert!(allow.check_call(Method::Post, records, &reading).is_ok());
        assert!(allow.check_call(Method::Post, records, &writing).is_err());
        // The bridge has one act and no read beyond GET.
        let bridge = allow
            .hosts()
            .find(|(_, r)| r.label == "hush")
            .map(|(_, r)| r.clone())
            .unwrap();
        assert!(bridge.reads.is_empty());
        assert_eq!(
            bridge
                .acts
                .iter()
                .map(|c| (c.method, c.path.as_str()))
                .collect::<Vec<_>>(),
            vec![(Method::Post, "/stage")]
        );
        assert!(
            !allow
                .hosts()
                .any(|(h, _)| h.as_str() == "http://10.0.0.62:9696"),
            "a type this version does not read is not listed"
        );
        let out = Outbound::new(allow, Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        let ids: Vec<&str> = set.processors.iter().map(|p| p.id()).collect();
        assert_eq!(ids, vec!["tdarr", "hush"]);
        assert_eq!(
            (
                set.processor("hush").unwrap().kind(),
                set.processor("hush").unwrap().role(),
                set.processor("tdarr").unwrap().role()
            ),
            ("hush", "bridge", "worker")
        );
        let slot = set.slot("hush").unwrap();
        assert_eq!(
            (slot.kind.as_str(), slot.library.as_deref(), slot.configured),
            ("hush", None, true)
        );
        assert!(!set.arr_ids().iter().any(|id| id == "hush" || id == "tdarr"));
        // Both are read, so they report health like every other integration.
        let view = integration_view(
            config.integration("hush").unwrap(),
            None,
            &set.access.of("hush"),
        );
        assert_eq!(
            (
                view.state.as_str(),
                view.detail.as_str(),
                view.role.as_str()
            ),
            ("warn", "Not contacted", "processing")
        );
        // The bridge and the worker can be acted on, and are read until that is allowed.
        assert_eq!(
            (view.access.as_str(), view.access_source.as_str()),
            ("read", "default")
        );
        assert_eq!(
            serde_json::to_value(&view.actions).unwrap(),
            serde_json::json!([{"id": "send", "label": "Send a file to a flow"}])
        );
        assert!(set.access.of("hush").editable() && set.access.of("tdarr").editable());
        assert_eq!(
            set.access.of("tdarr").access,
            crate::core::outbound::Access::Read
        );
        assert!(set.access.acting().is_empty());
        let unread = integration_view(
            config.integration("prowlarr").unwrap(),
            None,
            &set.access.of("prowlarr"),
        );
        assert_eq!(unread.detail, "Not supported");
        // Listed without an address: a slot, and nothing to ask.
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("TDARR_API_KEY", "k"),
        ]);
        let config = Config::from(&e).unwrap();
        let out = Outbound::new(allowlist(&config).unwrap(), Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        assert!(set.processors.is_empty() && !set.slot("tdarr").unwrap().configured);

        // The environment allows acting on the bridge and on the worker: each rule lets its own
        // acts through, in their forms, and nothing else.
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("TDARR_URL", "http://10.0.0.60:8265"),
            ("HUSH_URL", "http://10.0.0.60:8787/"),
            ("HUSH_ACCESS", "act"),
            ("TDARR_ACCESS", "act"),
        ]);
        let config = Config::from(&e).unwrap();
        let out = Outbound::new(allowlist(&config).unwrap(), Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        assert_eq!(
            set.access.acting(),
            vec![
                ("tdarr".to_string(), "Tdarr".to_string()),
                ("hush".to_string(), "HUSH".to_string())
            ]
        );
        let allow = out.allowlist();
        assert!(
            allow
                .check(Method::Post, "http://10.0.0.60:8787/stage")
                .is_ok()
        );
        for (method, url) in [
            (Method::Post, "http://10.0.0.60:8787/strip-subs"),
            (Method::Delete, "http://10.0.0.60:8787/stage"),
            (Method::Post, "http://10.0.0.60:8265/stage"),
            (Method::Post, "http://10.0.0.60:8265/api/v2/cruddb"),
        ] {
            assert!(
                allow.check(method, url).is_err(),
                "{} {url}",
                method.as_str()
            );
        }
        let view = integration_view(
            config.integration("hush").unwrap(),
            None,
            &set.access.of("hush"),
        );
        assert_eq!(
            (view.access.as_str(), view.access_source.as_str()),
            ("act", "environment")
        );
        let worker = set.access.of("tdarr");
        assert_eq!((worker.access, worker.source), (Access::Act, "environment"));
        // The worker's act passes in its one form; the same path in another form does not.
        let pause = |body: &str| {
            allow.check_call(
                Method::Post,
                "http://10.0.0.60:8265/api/v2/update-node",
                &Body::Json(body.to_string()),
            )
        };
        assert!(pause(r#"{"data": {"nodeID": "n", "nodeUpdates": {"nodePaused": true}}}"#).is_ok());
        assert!(pause(r#"{"data": {"nodeID": "n", "nodeUpdates": {"nodeName": "x"}}}"#).is_err());
    }

    #[test]
    fn engines_come_from_the_configuration() {
        let config = config();
        let out = Outbound::new(allowlist(&config).unwrap(), Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        assert_eq!(set.arrs.len(), 1);
        assert!(set.arr("radarr").is_some() && set.arr("sonarr").is_none());
        assert_eq!(set.download_clients.len(), 1);
        assert!(set.download_client("qbittorrent").is_some());
        assert_eq!(set.arr_ids(), vec!["radarr", "sonarr"]);
        assert!(!set.slot("sonarr").unwrap().configured);
        assert_eq!(set.arr("radarr").unwrap().engine().library, "radarr");
        // Two Radarr integrations are two engines with a library each, no second code path.
        let config = two_radarrs();
        let out = Outbound::new(allowlist(&config).unwrap(), Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        assert_eq!(set.arr_ids(), vec!["radarr-4k", "radarr"]);
        assert_eq!(set.arr("radarr-4k").unwrap().engine().library, "radarr-4k");
        assert!(set.download_clients.is_empty() && set.slot("qbittorrent").is_none());
    }

    /// Every download client is an engine of its own: a torrent client and a usenet client side
    /// by side, each listed with the reads its adapter declares and no act, each read on its own.
    #[test]
    fn download_clients_of_every_type_are_engines_with_reads_alone() {
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("QBIT_URL", "http://10.0.0.62:8080"),
            ("TRANSMISSION_URL", "http://10.0.0.62:9091"),
            ("DELUGE_URL", "http://10.0.0.62:8112"),
            ("DELUGE_PASS", "deluge"),
            ("SABNZBD_URL", "http://10.0.0.63:8080"),
            ("SABNZBD_API_KEY", "k"),
            ("NZBGET_URL", "http://10.0.0.63:6789"),
            ("SABNZBD_ACCESS", "act"),
        ]);
        let config = Config::from(&e).unwrap();
        let allow = allowlist(&config).unwrap();
        let rule = |origin: &str| {
            allow
                .hosts()
                .find(|(h, _)| h.as_str() == origin)
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| panic!("{origin} is listed"))
        };
        for (origin, label) in [
            ("http://10.0.0.62:8080", "qbittorrent"),
            ("http://10.0.0.62:9091", "transmission"),
            ("http://10.0.0.62:8112", "deluge"),
            ("http://10.0.0.63:8080", "sabnzbd"),
            ("http://10.0.0.63:6789", "nzbget"),
        ] {
            let r = rule(origin);
            assert_eq!(r.label, label);
            assert!(r.read_only && r.acts.is_empty(), "{label} takes no act");
            assert!(
                allow.switch(label).is_none(),
                "{label} has nothing to switch"
            );
        }
        // The qBittorrent rule is the one it always was: the sign-in, and every GET.
        let q = rule("http://10.0.0.62:8080");
        assert_eq!(
            q.reads
                .iter()
                .map(|c| (c.method, c.path.as_str(), c.body.is_some()))
                .collect::<Vec<_>>(),
            vec![(Method::Post, "/api/v2/auth/login", false)]
        );
        assert!(q.gets.is_empty() && !q.no_get);
        assert!(rule("http://10.0.0.63:6789").no_get);
        assert_eq!(rule("http://10.0.0.63:8080").gets.len(), 1);
        let out = Outbound::new(allow, Duration::from_secs(5)).unwrap();
        let set = EngineSet::from_config(&config, &out);
        let ids: Vec<(&str, &str)> = set
            .download_clients
            .iter()
            .map(|c| (c.id(), c.protocol().as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("qbittorrent", "torrent"),
                ("transmission", "torrent"),
                ("deluge", "torrent"),
                ("sabnzbd", "usenet"),
                ("nzbget", "usenet"),
            ]
        );
        assert_eq!(set.slot("sabnzbd").unwrap().kind, "sabnzbd");
        // `act` changes nothing for a type without an action: it is read.
        let sab = set.access.of("sabnzbd");
        assert_eq!((sab.access, sab.source), (Access::Read, "default"));
        assert!(set.access.acting().is_empty());
        let view = integration_view(
            config.integration("nzbget").unwrap(),
            None,
            &set.access.of("nzbget"),
        );
        assert_eq!(
            (
                view.state.as_str(),
                view.detail.as_str(),
                view.role.as_str()
            ),
            ("warn", "Not contacted", "downloads")
        );
        assert_eq!(
            counts_text(&serde_json::json!({"downloads": 12, "ownership_settled": true})),
            "12 downloads"
        );
        // Without its password Deluge is listed and not asked. Its host is not listed.
        let e = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("DELUGE_URL", "http://10.0.0.62:8112"),
        ]);
        let config = Config::from(&e).unwrap();
        let allow = allowlist(&config).unwrap();
        assert!(
            !allow
                .hosts()
                .any(|(h, _)| h.as_str() == "http://10.0.0.62:8112")
        );
        let set = EngineSet::from_config(
            &config,
            &Outbound::new(allow, Duration::from_secs(5)).unwrap(),
        );
        assert!(set.download_clients.is_empty() && !set.slot("deluge").unwrap().configured);
    }
}
