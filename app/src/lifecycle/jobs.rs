//! The jobs of the job runtime (`core::runtime`): the three folder scans, the search index and the
//! notification dispatcher. Their keys, queues and intervals are those of platform design section
//! 4.8. The integrations' loops keep running on their own until they move (B9b, B9c).
//!
//! The runtime runs on a pool of its own ([`connections`]). The handlers write on the app's pool.
//! The folder scans write through a journal fenced by their run's lease: a scan whose lease is
//! lost commits nothing. The index and the dispatcher write what their next run writes again:
//! the whole index, deliveries keyed by channel and event. None of these jobs reads an
//! integration. They write no pass row (`integration_health`).

use crate::core::config::Config;
use crate::core::journal::Journal;
use crate::core::runtime::{self, Running, Runtime, Schedule, Source, Timing};
use crate::homemovies::service::{self as homemovies, HomeMovies};
use crate::lifecycle::sync;
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

/// The workers of each queue. The scan queue has one for each folder scan: a scan asked for from
/// the API never waits behind another folder's scan. The others are the design's defaults
/// (section 4.2).
pub const WORKERS: [(&str, usize); 3] = [("scan", 3), ("index", 1), ("notify", 2)];

/// The size of the runtime's own pool for these workers.
pub fn connections() -> u32 {
    runtime::connections(WORKERS.iter().map(|(_, n)| n).sum())
}

/// The schedules the Jobs page lists, each with the pass its loop was named by: the folder scans.
/// The search index and the dispatcher are not listed: `GET /api/v1/jobs` keeps its ids.
const LISTED: [(&str, &str); 3] = [
    (sync::MOVIE_FOLDERS_JOB, "movie-folders"),
    (music::JOB, "music"),
    (homemovies::JOB, "home-movies"),
];

/// The keys of the schedules the Jobs page lists.
pub fn listed_keys() -> Vec<&'static str> {
    LISTED.iter().map(|(key, _)| *key).collect()
}

/// The pass a listed schedule is named by: `music` for `folder:music`. `None` for a schedule the
/// Jobs page does not list.
pub fn listed(key: &str) -> Option<&'static str> {
    LISTED
        .iter()
        .find(|(listed, _)| *listed == key)
        .map(|(_, pass)| *pass)
}

/// The schedules of this configuration. The movie folders have none without roots. An interval's
/// source is its setting's: the variable when one is set.
pub fn schedules(config: &Config, settings: &Settings) -> Vec<Schedule> {
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
/// after each commit.
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
    Runtime::reconcile(&deps.pool, &schedules(config, &deps.settings)).await?;
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
    for (queue, n) in WORKERS {
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
        let all = schedules(&config, &settings);
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
        assert_eq!(connections(), 15);

        let without_roots = Config::from(&BTreeMap::from([(
            "DATABASE_URL",
            "postgres://x@127.0.0.1:1/x",
        )]))
        .unwrap();
        let settings = Settings::new(&without_roots, &env, None);
        assert!(
            schedules(&without_roots, &settings)
                .iter()
                .all(|s| s.key != "folder:movie-folders")
        );
    }
}
