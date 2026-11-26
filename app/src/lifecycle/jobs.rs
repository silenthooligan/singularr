//! The jobs of the job runtime (`core::runtime`): the three folder scans, the search index, the
//! notification dispatcher (B9a) and, with the sync, the passes of the managers and the download
//! clients (B9b), of the media servers and of the Seerr sites (B9c). Their keys, queues and
//! intervals are those of platform design section 4.8. The processing services, the processing
//! rules, the intents and the plays keep their loops until they move (B9c, B9d).
//!
//! The runtime runs on a pool of its own ([`connections`]). The handlers write on the app's pool.
//! The folder scans and the sync's passes write through a journal fenced by their run's lease: a
//! run whose lease is lost commits nothing. A sync pass ends its job in the transaction that
//! writes its pass row (`integration_health`). The index and the dispatcher write what their next
//! run writes again: the whole index, deliveries keyed by channel and event. They read no
//! integration and write no pass row.

use crate::core::config::Config;
use crate::core::journal::Journal;
use crate::core::runtime::{self, Running, Runtime, Schedule, Source, Timing};
use crate::engines::service::EngineSet;
use crate::homemovies::service::{self as homemovies, HomeMovies};
use crate::lifecycle::claims;
use crate::lifecycle::sync::{self, SyncJobs};
use crate::metadata::service::ProviderSet;
use crate::music::service::{self as music, MusicLibrary};
use crate::notify::service::{self as notify, Describe, Notifier};
use crate::search::Search;
use crate::search::index;
use crate::settings::Settings;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// The lease of each of these jobs. The heartbeat renews it every third of it.
const LEASE: Duration = Duration::from_secs(120);

/// The workers of each queue but the sync's. The scan queue has one for each folder scan: a scan
/// asked for from the API never waits behind another folder's scan. The others are the design's
/// defaults (section 4.2).
pub const WORKERS: [(&str, usize); 3] = [("scan", 3), ("index", 1), ("notify", 2)];

/// The passes of the sync, by the names their loops had: what each reads, its job's kind, the
/// setting of its interval and whether the Jobs page lists it. The accounts and the discover
/// lists of a Seerr site were read by its requests loop: their schedules are not listed.
const SYNC_PASSES: [(&str, &str, &str, bool); 8] = [
    ("titles", sync::TITLES, "jobs.titles", true),
    ("queue", sync::QUEUE, "jobs.queue", true),
    ("history", sync::HISTORY, "jobs.history", true),
    ("downloads", sync::DOWNLOADS, "jobs.downloads", true),
    ("server", sync::SERVER, "jobs.server", true),
    ("requests", sync::REQUESTS, "jobs.requests", true),
    ("accounts", sync::ACCOUNTS, "jobs.requests", false),
    ("discover", sync::DISCOVER, "", false),
];

/// The workers of every queue. The sync queue has one for each of its schedules, as each pass had a
/// loop of its own: a download client's pass never waits behind a manager's titles.
pub fn workers(sync: Option<&EngineSet>) -> Vec<(&'static str, usize)> {
    let mut out = WORKERS.to_vec();
    let passes = sync.map(|e| sync_keys(e).len());
    if let Some(n) = passes.filter(|n| *n > 0) {
        out.push(("sync", n));
    }
    out
}

/// The size of the runtime's own pool for these workers.
pub fn connections(sync: Option<&EngineSet>) -> u32 {
    runtime::connections(workers(sync).iter().map(|(_, n)| n).sum())
}

/// The folder scans the Jobs page lists, each with the pass its loop was named by.
const LISTED: [(&str, &str); 3] = [
    (sync::MOVIE_FOLDERS_JOB, "movie-folders"),
    (music::JOB, "music"),
    (homemovies::JOB, "home-movies"),
];

/// The keys of the schedules the Jobs page lists: the folder scans and, with the sync on, the
/// passes of each manager and download client. The search index and the dispatcher are not
/// listed: `GET /api/v1/jobs` keeps the ids of the loops.
pub fn listed_keys(config: &Config, engines: &EngineSet) -> Vec<String> {
    let mut keys: Vec<String> = LISTED.iter().map(|(key, _)| key.to_string()).collect();
    keys.extend(
        integration_keys(config, engines)
            .into_iter()
            .map(|(_, key)| key),
    );
    keys
}

/// The keys of the schedules that read an integration, each with that integration: the passes of
/// the managers and the download clients while the sync is on.
pub fn integration_keys(config: &Config, engines: &EngineSet) -> Vec<(String, String)> {
    if !config.sync.enabled {
        return Vec::new();
    }
    sync_keys(engines)
        .into_iter()
        .map(|(key, integration, _)| (integration, key))
        .collect()
}

/// The pass a listed schedule is named by: `music` for `folder:music`, `queue` for
/// `radarr:queue`. `None` for a schedule the Jobs page does not list.
pub fn listed(key: &str) -> Option<&'static str> {
    if let Some((_, pass)) = LISTED.iter().find(|(listed, _)| *listed == key) {
        return Some(*pass);
    }
    let (integration, pass) = key.rsplit_once(':')?;
    if integration.is_empty() || integration == "folder" || integration == "singularr" {
        return None;
    }
    SYNC_PASSES
        .iter()
        .find(|(name, _, _, listed)| *name == pass && *listed)
        .map(|(name, ..)| *name)
}

/// The sync's schedule keys of these engines, each with its integration and its pass.
fn sync_keys(engines: &EngineSet) -> Vec<(String, String, &'static str)> {
    let mut out = Vec::new();
    for arr in &engines.arrs {
        let id = &arr.engine().id;
        for pass in ["titles", "queue", "history"] {
            out.push((format!("{id}:{pass}"), id.clone(), pass));
        }
    }
    for client in &engines.download_clients {
        let id = client.id();
        out.push((format!("{id}:downloads"), id.to_string(), "downloads"));
    }
    for server in &engines.media_servers {
        let id = server.id();
        out.push((format!("{id}:server"), id.to_string(), "server"));
    }
    // The discover lists come from the first site (the Plex one when it is), as the owner's
    // Discover page asks it.
    for (n, site) in engines.request_sites.iter().enumerate() {
        let id = site.id();
        out.push((format!("{id}:requests"), id.to_string(), "requests"));
        out.push((format!("{id}:accounts"), id.to_string(), "accounts"));
        if n == 0 {
            out.push((format!("{id}:discover"), id.to_string(), "discover"));
        }
    }
    out
}

/// The schedules of this configuration. The movie folders have none without roots. The sync's
/// passes have one each when `sync` gives the engines. An interval's source is its setting's: the
/// variable when one is set.
pub fn schedules(config: &Config, settings: &Settings, sync: Option<&EngineSet>) -> Vec<Schedule> {
    let source = |setting: &str| {
        if settings.env_of(setting).is_some() {
            Source::Environment
        } else {
            Source::Default
        }
    };
    let scan = |key: &str, kind: &str, every: Duration, setting: &str, first: Duration| Schedule {
        key: key.to_string(),
        kind: kind.to_string(),
        integration: None,
        queue: "scan".to_string(),
        every,
        floor: Duration::from_secs(600),
        first,
        jitter: config.sync.jitter,
        on_start: false,
        lease: LEASE,
        source: source(setting),
    };
    let mut out = Vec::new();
    if !config.movie_folders.roots.is_empty() {
        // No scan comes sooner than a few minutes after a start: the titles of a fresh start are in
        // the catalog first.
        out.push(scan(
            sync::MOVIE_FOLDERS_JOB,
            "scan.movies",
            config.movie_folders.interval,
            "jobs.movieScan",
            Duration::from_secs(180),
        ));
    }
    out.push(scan(
        music::JOB,
        "scan.music",
        config.music.interval,
        "jobs.musicScan",
        Duration::ZERO,
    ));
    out.push(scan(
        homemovies::JOB,
        "scan.home-movies",
        config.home_movies.interval,
        "jobs.homeMoviesScan",
        Duration::ZERO,
    ));
    // In full shortly after each start, as the loop wrote it.
    out.push(Schedule {
        key: index::JOB.to_string(),
        kind: "search.index".to_string(),
        integration: None,
        queue: "index".to_string(),
        every: config.search.interval,
        floor: Duration::from_secs(60),
        first: index::START_DELAY,
        jitter: Duration::ZERO,
        on_start: true,
        lease: LEASE,
        source: source("system.searchIndexEvery"),
    });
    // At once after each start, as the loop ran: a delivery a stop cut short is sent.
    out.push(Schedule {
        key: notify::JOB.to_string(),
        kind: "notify.dispatch".to_string(),
        integration: None,
        queue: "notify".to_string(),
        every: notify::IDLE,
        floor: notify::IDLE,
        first: Duration::ZERO,
        jitter: Duration::ZERO,
        on_start: true,
        lease: LEASE,
        source: Source::Default,
    });
    // Each pass of the sync after each start within its jitter, as its loop ran. The floors are
    // those of their settings.
    for (key, integration, pass) in sync.map(sync_keys).unwrap_or_default() {
        let s = &config.sync;
        let (every, floor) = match pass {
            "titles" => (s.titles, 60),
            "queue" => (s.queue, 15),
            "history" => (s.history, 60),
            "server" => (s.server, 300),
            "requests" | "accounts" => (s.requests, 60),
            "discover" => (crate::requests::discover::TTL, 1800),
            _ => (s.downloads, 10),
        };
        let (_, kind, setting, _) = SYNC_PASSES
            .iter()
            .find(|(name, ..)| *name == pass)
            .copied()
            .unwrap_or(SYNC_PASSES[3]);
        out.push(Schedule {
            key,
            kind: kind.to_string(),
            integration: Some(integration),
            queue: "sync".to_string(),
            every,
            floor: Duration::from_secs(floor),
            first: Duration::ZERO,
            jitter: s.jitter,
            on_start: true,
            lease: LEASE,
            source: if setting.is_empty() {
                Source::Default
            } else {
                source(setting)
            },
        });
    }
    out
}

/// What the jobs run on.
pub struct JobsDeps {
    /// The runtime's own pool, [`connections`] in size.
    pub runtime: PgPool,
    /// The app's pool: the handlers write on it.
    pub pool: PgPool,
    pub journal: Journal,
    pub settings: Arc<Settings>,
    pub editions: Arc<singularr_core::EditionList>,
    pub music: Arc<MusicLibrary>,
    pub home_movies: Arc<HomeMovies>,
    pub search: Arc<Search>,
    pub metadata: Arc<ProviderSet>,
    pub notify: Notifier,
    pub describe: Arc<dyn Describe>,
    /// The sync's passes, when the sync runs (`SINGULARR_SYNC_ENABLED`).
    pub sync: Option<Arc<SyncJobs>>,
}

/// The jobs of this process: the runtime and the triggers that ask it for runs after events.
pub struct Jobs {
    running: Running,
    triggers: Vec<JoinHandle<()>>,
}

impl Jobs {
    /// Stop. The triggers end. Running jobs have the runtime's grace period to finish. A stop
    /// takes the grace period and 3 s at most.
    pub async fn stop(self) {
        for t in &self.triggers {
            t.abort();
        }
        self.running.stop().await;
        tracing::info!(target: "singularr::runtime", "the job runtime stopped");
    }
}

/// Write the schedules, then start the runtime with their handlers and workers. The triggers ask
/// for a pass of the index 30 s after a library event. They ask for a round of the dispatcher
/// after each commit. With the sync, the claims and markers of an arr the configuration no longer
/// runs go first.
pub async fn start(config: &Config, deps: JobsDeps) -> Result<Jobs, sqlx::Error> {
    start_with(config, deps, Timing::default()).await
}

/// [`start`] with the runtime's waits given. Tests make them shorter.
pub async fn start_with(
    config: &Config,
    deps: JobsDeps,
    timing: Timing,
) -> Result<Jobs, sqlx::Error> {
    if config.movie_folders.roots.is_empty() {
        tracing::info!("SINGULARR_MOVIE_ROOTS is empty: the movie folders are not scanned");
    }
    let engines = deps.sync.as_ref().map(|s| s.deps.engines.clone());
    if let Some(engines) = &engines {
        let arrs: Vec<String> = engines.arrs.iter().map(|e| e.engine().id.clone()).collect();
        match claims::keep_managers(&deps.pool, &arrs).await? {
            0 => {}
            n => tracing::info!(
                rows = n,
                "the claims of managers this process does not run were removed"
            ),
        }
    }
    Runtime::reconcile(
        &deps.pool,
        &schedules(config, &deps.settings, engines.as_deref()),
    )
    .await?;
    let mut runtime = Runtime::new(deps.runtime.clone(), timing)
        .handle(
            "scan.movies",
            Arc::new(sync::FolderScan {
                pool: deps.pool.clone(),
                journal: deps.journal.clone(),
                editions: deps.editions.clone(),
                roots: config.movie_folders.roots.clone(),
            }),
        )
        .handle(
            "scan.music",
            Arc::new(music::ScanJob {
                pool: deps.pool.clone(),
                journal: deps.journal.clone(),
                lib: deps.music.clone(),
            }),
        )
        .handle(
            "scan.home-movies",
            Arc::new(homemovies::ScanJob {
                pool: deps.pool.clone(),
                journal: deps.journal.clone(),
                lib: deps.home_movies.clone(),
            }),
        )
        .handle(
            "search.index",
            Arc::new(index::IndexJob {
                pool: deps.pool.clone(),
                search: deps.search.clone(),
                metadata: deps.metadata.clone(),
            }),
        )
        .handle(
            "notify.dispatch",
            Arc::new(notify::DispatchJob {
                notifier: deps.notify.clone(),
                describe: deps.describe.clone(),
            }),
        );
    if let Some(jobs) = &deps.sync {
        runtime = runtime
            .handle(sync::TITLES, Arc::new(sync::TitlesJob(jobs.clone())))
            .handle(sync::QUEUE, Arc::new(sync::QueueJob(jobs.clone())))
            .handle(sync::HISTORY, Arc::new(sync::HistoryJob(jobs.clone())))
            .handle(sync::DOWNLOADS, Arc::new(sync::DownloadsJob(jobs.clone())))
            .handle(sync::SERVER, Arc::new(sync::ServerJob(jobs.clone())))
            .handle(sync::REQUESTS, Arc::new(sync::RequestsJob(jobs.clone())))
            .handle(sync::ACCOUNTS, Arc::new(sync::AccountsJob(jobs.clone())))
            .handle(sync::DISCOVER, Arc::new(sync::DiscoverJob(jobs.clone())));
    }
    for (queue, n) in workers(engines.as_deref()) {
        runtime = runtime.workers(queue, n);
    }
    let running = runtime.start();
    let triggers = vec![
        index::spawn_trigger(deps.pool.clone(), &deps.journal),
        notify::spawn_trigger(deps.pool.clone(), &deps.journal),
    ];
    Ok(Jobs { running, triggers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn the_jobs_keep_the_ids_and_intervals_of_their_loops() {
        let env = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("SINGULARR_MOVIE_ROOTS", "/movies"),
            ("SINGULARR_MUSIC_SCAN_SECS", "7200"),
        ]);
        let config = Config::from(&env).unwrap();
        let settings = Settings::new(&config, &env, None);
        let all = schedules(&config, &settings, None);
        let keys: Vec<(&str, &str, &str, u64)> = all
            .iter()
            .map(|s| {
                (
                    s.key.as_str(),
                    s.kind.as_str(),
                    s.queue.as_str(),
                    s.every.as_secs(),
                )
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                ("folder:movie-folders", "scan.movies", "scan", 21_600),
                ("folder:music", "scan.music", "scan", 7200),
                ("folder:home-movies", "scan.home-movies", "scan", 21_600),
                ("singularr:index", "search.index", "index", 900),
                ("singularr:notify-dispatch", "notify.dispatch", "notify", 60),
            ]
        );
        assert!(all.iter().all(|s| s.integration.is_none()));
        assert_eq!(all[0].first, Duration::from_secs(180));
        assert_eq!(
            (all[1].source, all[2].source),
            (Source::Environment, Source::Default)
        );
        assert!(all[3].on_start && all[4].on_start && !all[1].on_start);
        assert_eq!(
            (listed("folder:music"), listed("singularr:index")),
            (Some("music"), None)
        );
        assert_eq!(
            (
                listed("radarr-4k:history"),
                listed("qbittorrent:downloads"),
                listed("plex:server"),
                listed("seerr:requests"),
                listed("folder:titles")
            ),
            (
                Some("history"),
                Some("downloads"),
                Some("server"),
                Some("requests"),
                None
            )
        );
        // A Seerr site's accounts and discover lists are not listed: its requests loop read them.
        assert_eq!(
            (listed("seerr:accounts"), listed("seerr:discover")),
            (None, None)
        );
        assert_eq!(connections(None), 15);

        let without_roots = Config::from(&BTreeMap::from([(
            "DATABASE_URL",
            "postgres://x@127.0.0.1:1/x",
        )]))
        .unwrap();
        let settings = Settings::new(&without_roots, &env, None);
        assert!(
            schedules(&without_roots, &settings, None)
                .iter()
                .all(|s| s.key != "folder:movie-folders")
        );
    }
}
