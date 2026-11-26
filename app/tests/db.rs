//! Tests that need a PostgreSQL: the migrations, every store, the sync and its journal, the
//! downloads reconcile, API keys, and the routes end to end.
//!
//! They run only when `SINGULARR_TEST_DATABASE_URL` names a server the tests may create
//! databases on; without it each test says so and passes. The gate (`tools/gate.sh`, the
//! Dockerfile's check stage) starts a throwaway PostgreSQL 18 and runs them with the other
//! tests. Never point the variable at a database that holds anything: every test creates a
//! database of its own and drops it afterwards.
//!
//! The engines are recorded answers (`tests/fixtures`), sent through the real outbound
//! allowlist. No test talks to a real arr, qBittorrent or metadata provider.
#![cfg(feature = "ssr")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use singularr::api::routes;
use singularr::api::state::{AppState, Inner};
use singularr::catalog::ingest::Assess;
use singularr::catalog::model::{CopyQuery, TitleQuery};
use singularr::catalog::{service as catalog, sync as catalog_sync};
use singularr::core::auth::{self, Caller};
use singularr::core::config::{ArrConfig, ArrKind, ClientKind, Config, DownloadClientConfig};
use singularr::core::config::{PortalRoleConfig, SeerrConfig};
use singularr::core::error::AppError;
use singularr::core::journal::Journal;
use singularr::core::model::{EventQuery, NewApiKey, Paging, Scope};
use singularr::core::outbound::recorded::Recorded;
use singularr::core::outbound::{Allowlist, Body as Sent, Method, Outbound};
use singularr::core::runtime;
use singularr::core::{db, store as core_store};
use singularr::engines::RequestSite;
use singularr::engines::arr::{ArrClient, Radarr, Sonarr};
use singularr::engines::qbit::Qbit;
use singularr::engines::seerr::Seerr;
use singularr::engines::service::{EngineSet, EngineSlot, PassOk, statuses};
use singularr::engines::{ClientDownload, DownloadClient, LibraryEngine};
use singularr::homemovies::model::{InboxQuery, TapePatch, TapeQuery};
use singularr::homemovies::service::{self as homemovies, HomeMovies};
use singularr::lifecycle::claims;
use singularr::lifecycle::downloads::{Claims, reconcile};
use singularr::lifecycle::jobs::{self as runtime_jobs, Jobs, JobsDeps};
use singularr::lifecycle::model::DownloadQuery;
use singularr::lifecycle::sync::{self as lifecycle_sync, SyncDeps, SyncHandle, titles_pass};
use singularr::lifecycle::{service as downloads, store as downloads_store};
use singularr::metadata::service::ProviderSet;
use singularr::music::model::{AlbumQuery, ArtistQuery};
use singularr::music::service::{self as music, MusicLibrary};
use singularr::policy::model::SimulateRequest;
use singularr::policy::{self, ActivePolicy, BUILTIN, PolicySource};
use singularr::processing::model::{Moment, RunFilter, StateFilter};
use singularr::processing::{service as processing, store as processing_store};
use singularr::releases::assess::Assessor;
use singularr::releases::model::{ScoreRequest, ShadowQuery};
use singularr::releases::service as releases;
use singularr::requests::discover::{DiscoverCache, discover};
use singularr::requests::model::{DiscoverQuery, RequestQuery};
use singularr::requests::role::{self as portal_role, Kept};
use singularr::requests::rules::Rules;
use singularr::requests::service::{self as requests_service, Requests};
use singularr::requests::sync::{PassConfig, requests_pass};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const RADARR: &str = "http://192.0.2.61:7979";
const RADARR4K: &str = "http://192.0.2.61:7978";
const SONARR: &str = "http://192.0.2.61:8989";
const QBIT: &str = "http://192.0.2.62:8080";
const SEERR: &str = "http://192.0.2.62:5055";
const PLEX: &str = "http://10.0.0.30:32400";
const MEILI: &str = "http://10.0.0.9:7700";
const HUSH: &str = "http://10.0.0.60:8787";
const TDARR: &str = "http://10.0.0.60:8265";

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A database of its own for one test, dropped when the test ends.
struct TestDb {
    pool: PgPool,
    admin: PgPool,
    name: String,
}

impl TestDb {
    /// `None` when no test server is configured.
    async fn new(test: &str) -> Option<TestDb> {
        let t = TestDb::bare(test).await?;
        db::migrate(&t.pool).await.expect("the migrations apply");
        Some(t)
    }

    /// A database with no migration applied. `None` when no test server is configured.
    async fn bare(test: &str) -> Option<TestDb> {
        let Ok(url) = std::env::var("SINGULARR_TEST_DATABASE_URL") else {
            eprintln!(
                "{test}: skipped, SINGULARR_TEST_DATABASE_URL is not set (see tools/gate.sh)"
            );
            return None;
        };
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await
            .expect("the test database server is reachable");
        let name = format!("mq_{}_{}", test, uuid_like());
        sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE \"{name}\"")))
            .execute(&admin)
            .await
            .expect("create the test database");
        let base = url.rsplit_once('/').map(|(b, _)| b).unwrap_or(&url);
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&format!("{base}/{name}"))
            .await
            .expect("connect to the test database");
        Some(TestDb { pool, admin, name })
    }

    async fn drop_db(self) {
        self.pool.close().await;
        let _ = sqlx::query(sqlx::AssertSqlSafe(format!(
            "DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)",
            self.name
        )))
        .execute(&self.admin)
        .await;
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", nanos ^ ((std::process::id() as u128) << 64))
}

fn outbound(recorded: Arc<Recorded>) -> Outbound {
    let mut allow = Allowlist::with_metadata_providers();
    allow.allow_engine("radarr", RADARR, &[]).unwrap();
    allow.allow_engine("radarr-4k", RADARR4K, &[]).unwrap();
    allow.allow_engine("sonarr", SONARR, &[]).unwrap();
    allow
        .allow_engine("qbittorrent", QBIT, &["/api/v2/auth/login"])
        .unwrap();
    allow.allow_engine("seerr", SEERR, &[]).unwrap();
    // The Plex server takes the GETs its adapter declares and no other, as in the program.
    allow
        .allow_integration("plex", PLEX, singularr::engines::plex::calls())
        .unwrap();
    // The processing services, with the calls beyond GET their adapters declare.
    allow
        .allow_integration("hush", HUSH, singularr::engines::hush::calls())
        .unwrap();
    allow
        .allow_integration("tdarr", TDARR, singularr::engines::tdarr::calls())
        .unwrap();
    allow.allow_service("meilisearch", MEILI).unwrap();
    Outbound::with_transport(allow, recorded)
}

fn arr(
    id: &'static str,
    kind: ArrKind,
    library: &'static str,
    url: &str,
    out: &Outbound,
) -> ArrClient {
    let config = ArrConfig {
        id: id.to_string(),
        kind,
        library: library.to_string(),
        url: Some(url.to_string()),
        api_key: Some("test-key".to_string()),
    };
    ArrClient::new(&config, out.clone()).unwrap()
}

/// Record the answers of one Radarr at `base`: the movie list, and the file records with
/// the custom formats the list does not carry.
fn record_radarr(recorded: &Recorded, base: &str, movies: &str, files: &str) {
    let at = |path: &str| format!("{base}/api/v3{path}");
    recorded.answer(
        Method::Get,
        &at("/system/status"),
        200,
        &fixture("radarr_system_status.json"),
    );
    recorded.answer(Method::Get, &at("/tag"), 200, &fixture("radarr_tag.json"));
    recorded.answer(
        Method::Get,
        &at("/qualityprofile"),
        200,
        &fixture("radarr_qualityprofile.json"),
    );
    recorded.answer(Method::Get, &at("/movie"), 200, movies);
    recorded.answer(Method::Get, &at("/moviefile"), 200, files);
    recorded.answer(
        Method::Get,
        &at("/queue"),
        200,
        &fixture("radarr_queue.json"),
    );
    recorded.answer(
        Method::Get,
        &at("/history"),
        200,
        &fixture("radarr_history.json"),
    );
}

fn record_sonarr(recorded: &Recorded, files: &str) {
    let at = |path: &str| format!("{SONARR}/api/v3{path}");
    recorded.answer(
        Method::Get,
        &at("/system/status"),
        200,
        &fixture("radarr_system_status.json"),
    );
    recorded.answer(Method::Get, &at("/tag"), 200, "[]");
    recorded.answer(
        Method::Get,
        &at("/qualityprofile"),
        200,
        &fixture("radarr_qualityprofile.json"),
    );
    recorded.answer(
        Method::Get,
        &at("/series"),
        200,
        &fixture("sonarr_series.json"),
    );
    recorded.answer(Method::Get, &at("/episodefile?seriesId=1"), 200, files);
    recorded.answer(Method::Get, &at("/episodefile?seriesId=2"), 200, "[]");
    recorded.answer(
        Method::Get,
        &at("/queue"),
        200,
        &fixture("sonarr_queue.json"),
    );
}

fn slots(ids: &[(&str, &str, Option<&str>)]) -> Vec<EngineSlot> {
    ids.iter()
        .map(|(id, kind, library)| EngineSlot {
            id: id.to_string(),
            kind: kind.to_string(),
            library: library.map(str::to_string),
            configured: true,
        })
        .collect()
}

/// The built-in policy's lanes, placed in the libraries of a Radarr and a Sonarr.
fn lanes() -> Vec<singularr::catalog::service::LaneSlot> {
    let config = Config::from(&BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("RADARR_URL", RADARR),
        ("SONARR_URL", SONARR),
    ]))
    .unwrap();
    singularr::catalog::service::lane_slots(&ActivePolicy::builtin().policy, &config.arrs)
}

fn sync_config() -> singularr::core::config::SyncConfig {
    Config::from(&BTreeMap::from([(
        "DATABASE_URL",
        "postgres://x@127.0.0.1:1/x",
    )]))
    .unwrap()
    .sync
}

fn deps(
    pool: &PgPool,
    journal: &Journal,
    engines: Arc<EngineSet>,
    policy: &Arc<ActivePolicy>,
) -> SyncDeps {
    SyncDeps {
        config: sync_config(),
        pool: pool.clone(),
        journal: journal.clone(),
        engines,
        assess: Arc::new(Assessor::new(policy.clone(), "test-build")),
        policy_event: None,
    }
}

async fn kinds(pool: &PgPool, after: i64) -> Vec<String> {
    core_store::events_after(pool, after, 10_000)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

fn count(kinds: &[String], kind: &str) -> usize {
    kinds.iter().filter(|k| k.as_str() == kind).count()
}

#[tokio::test]
async fn migrations_apply_and_the_database_reports_ready() {
    let Some(t) = TestDb::new("migrate").await else {
        return;
    };
    assert!(db::pending_migrations(&t.pool).await.unwrap().is_empty());
    assert!((db::ready_check(t.pool.clone()))().await.is_ok());
    // Running them again changes nothing.
    db::migrate(&t.pool).await.unwrap();
    // Libraries come from the configuration and the engines, never from a seed.
    let libraries = catalog::libraries(&t.pool).await.unwrap();
    assert!(libraries.is_empty());
    t.drop_db().await;
}

/// A database that took a higher migration before a lower one was written (0010 before 0009)
/// takes the lower one at the next start.
#[tokio::test]
async fn a_migration_numbered_below_an_applied_one_is_still_applied() {
    let Some(t) = TestDb::new("order").await else {
        return;
    };
    sqlx::query(
        "ALTER TABLE downloads DROP COLUMN speed_bytes, DROP COLUMN eta_seconds,
             DROP COLUMN seeds, DROP COLUMN peers, DROP COLUMN indexer",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 9")
        .execute(&t.pool)
        .await
        .unwrap();
    assert_eq!(db::pending_migrations(&t.pool).await.unwrap().len(), 1);
    db::migrate(&t.pool).await.unwrap();
    assert!(db::pending_migrations(&t.pool).await.unwrap().is_empty());
    let columns: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns
         WHERE table_name = 'downloads' AND column_name IN ('speed_bytes', 'indexer')",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(columns, 2);
    t.drop_db().await;
}

#[tokio::test]
async fn a_radarr_listing_becomes_titles_copies_and_journal_events() {
    let Some(t) = TestDb::new("radarr").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    let out = outbound(recorded.clone());
    let engine: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("radarr", "radarr", Some("radarr"))]),
    ));
    let journal = Journal::new();
    let mut live = journal.subscribe();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines.clone(), &policy);

    // First pass: everything is new.
    let (ok, report) = titles_pass(&deps, &engine, true).await.unwrap();
    assert_eq!(ok.version.as_deref(), Some("5.14.0.9383"));
    assert_eq!((report.titles_added, report.copies_added), (3, 2));
    let first = kinds(&t.pool, 0).await;
    assert_eq!(count(&first, "title_added"), 3);
    assert_eq!(count(&first, "copy_added"), 2);
    assert_eq!(first.len(), 5);
    // The same events went out live, in id order.
    let mut live_ids = Vec::new();
    while let Ok(e) = live.try_recv() {
        live_ids.push(e.id);
    }
    assert_eq!(live_ids.len(), 5);
    assert!(live_ids.windows(2).all(|w| w[0] < w[1]));

    // The catalog as the API reads it.
    let all = catalog::titles(&t.pool, &lanes(), &TitleQuery::default())
        .await
        .unwrap();
    assert_eq!(all.total, 3);
    assert_eq!(all.items[0].title, "10 Cloverfield Lane");
    assert_eq!(all.items[0].libraries, vec!["radarr"]);
    assert_eq!(all.items[0].copies, 1);
    let search = |q: TitleQuery| {
        let pool = t.pool.clone();
        async move { catalog::titles(&pool, &lanes(), &q).await.unwrap() }
    };
    assert_eq!(
        search(TitleQuery {
            q: Some("harry azkaban".into()),
            ..Default::default()
        })
        .await
        .total,
        1
    );
    assert_eq!(
        search(TitleQuery {
            q: Some("tmdb:333371".into()),
            ..Default::default()
        })
        .await
        .total,
        1
    );
    assert_eq!(
        search(TitleQuery {
            q: Some("tt0304141".into()),
            ..Default::default()
        })
        .await
        .total,
        1
    );
    assert_eq!(
        search(TitleQuery {
            q: Some("100%".into()),
            ..Default::default()
        })
        .await
        .total,
        0
    );
    assert_eq!(
        search(TitleQuery {
            kind: Some("show".into()),
            ..Default::default()
        })
        .await
        .total,
        0
    );
    assert_eq!(
        search(TitleQuery {
            library: Some("radarr-4k".into()),
            ..Default::default()
        })
        .await
        .total,
        0
    );
    assert_eq!(
        search(TitleQuery {
            lane: Some("movies-uhd".into()),
            ..Default::default()
        })
        .await
        .total,
        1
    );
    let protected = search(TitleQuery {
        protected: Some(true),
        ..Default::default()
    })
    .await;
    assert_eq!(protected.total, 1);
    assert_eq!(
        protected.items[0].title,
        "Harry Potter and the Prisoner of Azkaban"
    );
    assert_eq!(
        search(TitleQuery {
            protected: Some(false),
            ..Default::default()
        })
        .await
        .total,
        2
    );
    let by_size = search(TitleQuery {
        sort: Some("-size".into()),
        per_page: Some(1),
        ..Default::default()
    })
    .await;
    assert_eq!((by_size.items.len(), by_size.total), (1, 3));
    assert_eq!(
        by_size.items[0].title,
        "Harry Potter and the Prisoner of Azkaban"
    );
    assert!(
        catalog::titles(
            &t.pool,
            &lanes(),
            &TitleQuery {
                kind: Some("album".into()),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );

    // The filters and sorts the interface asks for.
    let uhd = search(TitleQuery {
        has_uhd: Some(true),
        ..Default::default()
    })
    .await;
    assert_eq!(uhd.total, 1);
    assert_eq!(
        uhd.items[0].title,
        "Harry Potter and the Prisoner of Azkaban"
    );
    assert_eq!(
        search(TitleQuery {
            has_uhd: Some(false),
            ..Default::default()
        })
        .await
        .total,
        2
    );
    assert_eq!(
        search(TitleQuery {
            edition: Some("open matte".into()),
            ..Default::default()
        })
        .await
        .total,
        1
    );
    assert_eq!(
        search(TitleQuery {
            edition: Some("IMAX".into()),
            ..Default::default()
        })
        .await
        .total,
        0
    );
    let newest = search(TitleQuery::default()).await;
    assert_eq!(
        newest.items[0].title, "10 Cloverfield Lane",
        "the default is newest copy first"
    );
    assert!(newest.items[0].newest_copy_at > newest.items[1].newest_copy_at);
    assert_eq!(
        newest.items[2].newest_copy_at, None,
        "a title with no copy sorts last"
    );
    let oldest = search(TitleQuery {
        sort: Some("added".into()),
        order: Some("asc".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(
        oldest.items[0].title,
        "Harry Potter and the Prisoner of Azkaban"
    );
    let by_title = search(TitleQuery {
        sort: Some("title".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(by_title.items[0].title, "10 Cloverfield Lane");
    let by_title_desc = search(TitleQuery {
        sort: Some("title".into()),
        order: Some("desc".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(
        by_title_desc.items[0].title, "A Movie Not Yet Held",
        "articles do not sort"
    );
    let by_score = search(TitleQuery {
        sort: Some("score".into()),
        ..Default::default()
    })
    .await;
    let best = by_score.items.iter().filter_map(|t| t.best_score).max();
    assert!(best.is_some());
    assert_eq!(by_score.items[0].best_score, best);
    let by_year = search(TitleQuery {
        sort: Some("year".into()),
        ..Default::default()
    })
    .await;
    assert_eq!(by_year.items[0].year, Some(2027));
    for bad in [
        TitleQuery {
            sort: Some("rating".into()),
            ..Default::default()
        },
        TitleQuery {
            order: Some("sideways".into()),
            ..Default::default()
        },
    ] {
        assert!(catalog::titles(&t.pool, &lanes(), &bad).await.is_err());
    }

    let cloverfield = catalog::title(&t.pool, &lanes(), all.items[0].id)
        .await
        .unwrap();
    // What a movie is, stored from the list call: the same on the title and in the list.
    let described = catalog::titles(
        &t.pool,
        &lanes(),
        &TitleQuery {
            q: Some("tmdb:333371".into()),
            kind: Some("movie".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let described = &described.items[0];
    assert_eq!(described.about.studio.as_deref(), Some("Bad Robot"));
    assert_eq!(described.about.genres, vec!["Thriller", "Science Fiction"]);
    assert_eq!(
        (described.about.rating, described.about.imdb_rating),
        (Some(7.0), Some(7.2))
    );
    assert_eq!(
        described.about.collection.as_deref(),
        Some("Cloverfield Collection")
    );
    assert_eq!(
        (
            described.about.runtime_minutes,
            described.about.certification.as_deref()
        ),
        (Some(104), Some("PG-13"))
    );
    assert!(described.about.in_cinemas.is_some() && described.about.digital_release.is_some());
    assert!(described.about.fanart_url.is_some() && described.about.next_airing.is_none());
    assert_eq!(
        (
            described.season_count,
            described.episode_count,
            described.episode_file_count
        ),
        (None, None, None)
    );
    let described_detail = catalog::title(&t.pool, &lanes(), described.id)
        .await
        .unwrap();
    assert_eq!(described_detail.about, described.about);
    assert!(described_detail.seasons.is_empty());
    assert!(described_detail.engines[0].seasons.is_empty());
    let held_before = cloverfield.copies[0].singularr.clone();
    let harry = protected.items[0].id;
    let detail = catalog::title(&t.pool, &lanes(), harry).await.unwrap();
    assert_eq!(detail.engines.len(), 1);
    assert_eq!(detail.engines[0].tags, vec!["open-matte"]);
    assert_eq!(detail.engines[0].lane.as_deref(), Some("movies-uhd"));
    assert_eq!(detail.protections, vec!["open-matte"]);
    assert_eq!(detail.copies.len(), 1);
    let copy = &detail.copies[0];
    // Harry Potter is held in 4K only: its other movie lane is empty.
    let empty: Vec<(&str, &str)> = detail
        .empty_lanes
        .iter()
        .map(|l| (l.lane.as_str(), l.library.as_str()))
        .collect();
    assert_eq!(empty, vec![("movies-hd", "radarr")]);
    assert!(detail.empty_lanes.iter().all(|l| !l.uhd));
    // The verdict carries the number of steps of its lane's ladder next to its rank.
    let stored = serde_json::to_value(copy).unwrap();
    assert_eq!(stored["verdict"]["ladder_steps"], 3);
    assert!(stored["verdict"]["rank"].is_u64());
    assert_eq!(copy.verdict.as_ref().unwrap().ladder_steps, Some(3));
    assert_eq!(copy.name_source, "scene_name");
    assert_eq!(copy.singularr.quality.as_deref(), Some("WEBDL-2160p"));
    assert_eq!(copy.arr.edition.as_deref(), Some("Open Matte"));
    assert!(copy.protected);
    assert_eq!(copy.arr.score, Some(300), "the score came from /moviefile");
    assert_eq!(copy.arr.custom_formats, vec!["Open Matte"]);
    assert!(copy.held.has_edition("Open Matte"));
    assert!(
        copy.arr_only_formats.is_empty(),
        "Open Matte lines up with its trait"
    );
    // The record marks Cloverfield as a proper and names its formats; neither format has
    // a trait in the built-in policy, so both are the arr's alone.
    let recorded_copy = &cloverfield.copies[0];
    assert!(recorded_copy.held.proper);
    assert_eq!(recorded_copy.arr_only_formats, vec!["DD+ ATMOS", "x265"]);
    assert_eq!(
        recorded_copy.held.quality().map(|q| q.label()).as_deref(),
        Some("Bluray-1080p")
    );
    let verdict = copy.verdict.as_ref().expect("scored in movies-uhd");
    assert_eq!(Some(verdict.score), copy.singularr.score);
    assert!(!verdict.trace.is_empty(), "the verdict carries its trace");
    assert!(
        !copy.parsed.notes.is_empty(),
        "the parse carries its evidence"
    );
    assert!(catalog::title(&t.pool, &lanes(), 999_999).await.is_err());
    let paged = catalog::title_copies(&t.pool, harry, &CopyQuery::default())
        .await
        .unwrap();
    assert_eq!(paged.total, 1);
    let libraries = catalog::libraries(&t.pool).await.unwrap();
    assert_eq!(libraries[0].root_folders, vec!["/media/movies"]);
    assert_eq!((libraries[0].titles, libraries[0].copies), (3, 2));
    assert_eq!(libraries[0].engines, vec!["radarr"]);

    // Shadow: the fixture's arr scores are not what the built-in policy gives.
    let shadow = releases::shadow(&t.pool, &["radarr".to_string()], &ShadowQuery::default())
        .await
        .unwrap();
    assert_eq!(shadow.counts.copies, 2);
    assert_eq!(shadow.counts.score, 2);
    assert_eq!(shadow.rows.total, 2);
    assert!(
        shadow
            .rows
            .items
            .iter()
            .all(|r| r.differs.iter().any(|d| d == "score"))
    );
    let none = releases::shadow(
        &t.pool,
        &["radarr".to_string()],
        &ShadowQuery {
            disagree: Some("edition".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(none.rows.total, 0);
    assert!(
        releases::shadow(
            &t.pool,
            &["radarr".to_string()],
            &ShadowQuery {
                arr: Some("lidarr".into()),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );

    // Score a candidate against the held open matte copy: the protection holds it.
    let plain = releases::score(
        &t.pool,
        &policy,
        &ScoreRequest {
            name: "Harry.Potter.and.the.Prisoner.of.Azkaban.2004.2160p.UHD.BluRay.REMUX.DV.HDR.TrueHD.Atmos-GRP".into(),
            held_copy: Some(copy.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(plain.lane, "movies-uhd");
    assert_eq!(format!("{:?}", plain.decision.action), "Hold");
    assert_eq!(plain.decision.blocked_by.as_deref(), Some("open-matte"));
    assert_eq!(plain.held.as_ref().unwrap().copy_id, copy.id);
    assert!(
        releases::score(
            &t.pool,
            &policy,
            &ScoreRequest {
                name: "X.2004.1080p.WEB-DL-G".into(),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert!(
        releases::score(
            &t.pool,
            &policy,
            &ScoreRequest {
                name: "X.2004.1080p.WEB-DL-G".into(),
                held_copy: Some(424242),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );

    // Simulation: a heavier HDR weight moves the UHD copy and nothing is stored.
    let proposed = BUILTIN.replacen("\"video.hdr\" = 200", "\"video.hdr\" = 900", 1);
    assert_ne!(proposed, BUILTIN);
    let sim = releases::simulate(
        &t.pool,
        &policy,
        &SimulateRequest {
            policy: proposed,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(sim.copies_evaluated, 2);
    assert_eq!((sim.changed, sim.raised), (1, 1));
    assert_eq!(sim.changes.items[0].copy_id, copy.id);
    assert_eq!(sim.changes.items[0].delta, 700);
    assert!(
        releases::simulate(
            &t.pool,
            &policy,
            &SimulateRequest {
                policy: "name = 1".into(),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );
    assert_eq!(
        catalog::copy(&t.pool, copy.id)
            .await
            .unwrap()
            .singularr
            .score,
        copy.singularr.score
    );

    // Second pass, same listing: nothing changes and nothing is journaled.
    let last = core_store::latest_event_id(&t.pool).await.unwrap();
    let (_, report) = titles_pass(&deps, &engine, true).await.unwrap();
    assert!(!report.changed(), "{report:?}");
    assert_eq!(core_store::latest_event_id(&t.pool).await.unwrap(), last);

    // Third pass: the first movie was upgraded to a remux, the third movie is gone.
    let recorded2 = Recorded::new();
    record_radarr(
        &recorded2,
        RADARR,
        &fixture("radarr_movie_upgraded.json"),
        &fixture("radarr_moviefile_upgraded.json"),
    );
    let out2 = outbound(recorded2.clone());
    let engine2: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out2,
    )));
    let (_, report) = titles_pass(&deps, &engine2, true).await.unwrap();
    assert_eq!((report.copies_replaced, report.titles_removed), (1, 1));
    assert_eq!(report.copies_added + report.copies_removed, 0);
    let after = core_store::events_after(&t.pool, last, 100).await.unwrap();
    let replaced = after
        .iter()
        .find(|e| e.kind == "copy_replaced")
        .expect("copy_replaced");
    assert_eq!(replaced.actor, "sync:radarr");
    assert_eq!(replaced.body["title"], "10 Cloverfield Lane");
    assert_eq!(
        replaced.body["old"]["quality"].as_str(),
        held_before.quality.as_deref()
    );
    assert_eq!(
        replaced.body["old"]["score"].as_i64(),
        held_before.score.map(i64::from)
    );
    assert_eq!(replaced.body["old"]["group"], "SM737");
    assert_eq!(replaced.body["new"]["group"], "FraMeSToR");
    assert_eq!(replaced.body["new"]["quality"], "Remux-1080p");
    assert!(
        replaced.body["old"]["verdict"]["trace"].is_array(),
        "the old verdict is kept in the event"
    );
    assert!(replaced.body["new"]["verdict"]["trace"].is_array());
    let delta = replaced.body["score_delta"]
        .as_i64()
        .expect("a score delta");
    assert_eq!(
        delta,
        replaced.body["new"]["score"].as_i64().unwrap()
            - replaced.body["old"]["score"].as_i64().unwrap()
    );
    let removed = after
        .iter()
        .find(|e| e.kind == "title_removed")
        .expect("title_removed");
    assert_eq!(removed.body["title"], "A Movie Not Yet Held");
    assert_eq!(removed.body["gone_from_catalog"], true);
    assert_eq!(
        catalog::titles(&t.pool, &lanes(), &TitleQuery::default())
            .await
            .unwrap()
            .total,
        2
    );

    // The title's history is one query.
    let title_id = replaced.body["title_id"].as_i64().unwrap();
    let (history, total) = core_store::list_events(
        &t.pool,
        &EventQuery {
            title: Some(title_id),
            ..Default::default()
        },
        Paging::new(None, None),
    )
    .await
    .unwrap();
    assert_eq!(total, 3, "title_added, copy_added, copy_replaced");
    assert_eq!(history[0].kind, "copy_replaced", "newest first");

    // Every call to the engine was a GET.
    for r in [&recorded, &recorded2] {
        assert!(r.methods_seen().iter().all(|m| *m == Method::Get));
    }
    t.drop_db().await;
}

#[tokio::test]
async fn a_short_listing_is_refused_and_removes_nothing() {
    let Some(t) = TestDb::new("short").await else {
        return;
    };
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let assess: Arc<dyn Assess> = Arc::new(Assessor::new(policy, "test-build"));
    let engine = singularr::catalog::ingest::EngineRef {
        id: "radarr".into(),
        library: "radarr".into(),
        kind: singularr::catalog::model::TitleKind::Movie,
    };
    let title = |id: i64| singularr::catalog::ingest::TitleFacts {
        engine_title_id: id,
        kind: singularr::catalog::model::TitleKind::Movie,
        title: format!("Movie {id}"),
        sort_title: None,
        year: Some(2000),
        tmdb_id: Some(1000 + id),
        tvdb_id: None,
        imdb_id: None,
        original_language: None,
        poster_url: None,
        status: None,
        path: Some(format!("/media/movies/Movie {id} (2000)")),
        monitored: true,
        profile: Some("HD-1080p".into()),
        lane: Some("movies-hd".into()),
        tags: vec![],
        added_at: None,
        file_count: 0,
        size_bytes: 0,
        seasons: vec![],
        episode_count: None,
        total_episode_count: None,
        about: Default::default(),
        files: Some(vec![]),
    };
    let all: Vec<_> = (1..=30).map(title).collect();
    let report = catalog_sync::apply_titles(&t.pool, &journal, &engine, all, assess.clone())
        .await
        .unwrap();
    assert_eq!(report.titles_added, 30);
    let short: Vec<_> = (1..=5).map(title).collect();
    let err = catalog_sync::apply_titles(&t.pool, &journal, &engine, short, assess.clone())
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        catalog_sync::SyncError::Implausible {
            known: 30,
            listed: 5,
            ..
        }
    ));
    assert_eq!(
        catalog::titles(&t.pool, &lanes(), &TitleQuery::default())
            .await
            .unwrap()
            .total,
        30
    );
    // A plausible shrink is applied.
    let most: Vec<_> = (1..=28).map(title).collect();
    let report = catalog_sync::apply_titles(&t.pool, &journal, &engine, most, assess)
        .await
        .unwrap();
    assert_eq!(report.titles_removed, 2);
    t.drop_db().await;
}

#[tokio::test]
async fn two_radarr_integrations_share_one_title_with_a_copy_in_each_library() {
    let Some(t) = TestDb::new("shared").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    record_radarr(
        &recorded,
        RADARR4K,
        &fixture("radarr_movie_upgraded.json"),
        &fixture("radarr_moviefile_upgraded.json"),
    );
    let out = outbound(recorded.clone());
    let hd: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let uhd: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr-4k",
        ArrKind::Radarr,
        "radarr-4k",
        RADARR4K,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![hd.clone(), uhd.clone()],
        None,
        slots(&[
            ("radarr", "radarr", Some("radarr")),
            ("radarr-4k", "radarr", Some("radarr-4k")),
        ]),
    ));
    let journal = Journal::new();
    // The second Radarr's profiles stand for the same lanes.
    let text = BUILTIN
        .replacen(
            "arr_profiles = [\"radarr:HD-1080p\"]",
            "arr_profiles = [\"radarr:HD-1080p\", \"radarr-4k:HD-1080p\"]",
            1,
        )
        .replacen(
            "arr_profiles = [\"radarr:UHD-4K\"]",
            "arr_profiles = [\"radarr:UHD-4K\", \"radarr-4k:UHD-4K\"]",
            1,
        );
    assert_ne!(text, BUILTIN);
    let policy = Arc::new(ActivePolicy::from_text(&text, PolicySource::Builtin).unwrap());
    let deps = deps(&t.pool, &journal, engines, &policy);
    titles_pass(&deps, &hd, true).await.unwrap();
    let (_, report) = titles_pass(&deps, &uhd, true).await.unwrap();
    assert_eq!((report.titles_added, report.copies_added), (2, 2));

    // Three titles in all: the two of the second Radarr are titles the first one holds.
    let all = catalog::titles(&t.pool, &lanes(), &TitleQuery::default())
        .await
        .unwrap();
    assert_eq!(all.total, 3);
    let both = catalog::titles(
        &t.pool,
        &lanes(),
        &TitleQuery {
            q: Some("cloverfield".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(both.items[0].libraries, vec!["radarr", "radarr-4k"]);
    assert_eq!(both.items[0].copies, 2);
    let detail = catalog::title(&t.pool, &lanes(), both.items[0].id)
        .await
        .unwrap();
    assert_eq!(detail.engines.len(), 2);
    let lanes: HashSet<Option<String>> = detail.copies.iter().map(|c| c.lane.clone()).collect();
    assert!(
        lanes.iter().all(|l| l.is_some()),
        "both copies have a lane: {lanes:?}"
    );
    let in_second = catalog::title_copies(
        &t.pool,
        both.items[0].id,
        &CopyQuery {
            library: Some("radarr-4k".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(in_second.total, 1);
    assert_eq!(in_second.items[0].engine, "radarr-4k");
    let added = core_store::list_events(
        &t.pool,
        &EventQuery {
            kind: Some("title_added".into()),
            actor: Some("sync:radarr-4k".into()),
            ..Default::default()
        },
        Paging::new(None, None),
    )
    .await
    .unwrap()
    .0;
    assert!(added.iter().all(|e| e.body["new_to_catalog"] == false));
    assert_eq!(
        catalog::libraries(&t.pool)
            .await
            .unwrap()
            .iter()
            .map(|l| l.copies)
            .sum::<i64>(),
        4
    );
    t.drop_db().await;
}

#[tokio::test]
async fn sonarr_episode_files_are_read_per_show_and_an_upgrade_replaces_the_episode() {
    let Some(t) = TestDb::new("sonarr").await else {
        return;
    };
    let recorded = Recorded::new();
    record_sonarr(&recorded, &fixture("sonarr_episodefile.json"));
    let out = outbound(recorded.clone());
    let engine: Arc<dyn LibraryEngine> = Arc::new(Sonarr(arr(
        "sonarr",
        ArrKind::Sonarr,
        "sonarr",
        SONARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("sonarr", "sonarr", Some("sonarr"))]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines, &policy);

    let (ok, report) = titles_pass(&deps, &engine, false).await.unwrap();
    assert_eq!((report.titles_added, report.copies_added), (2, 2));
    assert_eq!(ok.counts["shows_read"], 2);
    let shows = catalog::titles(
        &t.pool,
        &lanes(),
        &TitleQuery {
            kind: Some("show".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(shows.total, 2);
    let id = shows.items.iter().find(|s| s.title == "1883").unwrap().id;
    let detail = catalog::title(&t.pool, &lanes(), id).await.unwrap();
    assert_eq!(detail.seasons.len(), 1);
    assert_eq!(
        (detail.seasons[0].episode_count, detail.seasons[0].copies),
        (10, 2)
    );
    // The season's counts, over the engines and for the one engine that holds the show.
    assert_eq!(
        (
            detail.seasons[0].total_episode_count,
            detail.seasons[0].episode_file_count
        ),
        (10, 2)
    );
    let said = &detail.engines[0];
    assert_eq!(
        (
            said.episode_count,
            said.total_episode_count,
            said.file_count
        ),
        (Some(10), Some(10), 2)
    );
    assert_eq!(said.seasons.len(), 1);
    assert_eq!(
        (
            said.seasons[0].monitored,
            said.seasons[0].episode_file_count
        ),
        (true, 2)
    );
    // What the show is, stored from the same list call, on the title and in the list.
    assert_eq!(detail.about.network.as_deref(), Some("Paramount+"));
    assert_eq!(detail.about.genres, vec!["Drama", "Western"]);
    assert_eq!(
        (detail.about.rating, detail.about.imdb_rating),
        (Some(8.7), None)
    );
    assert!(detail.about.banner_url.is_some());
    assert_eq!(
        (
            detail.about.runtime_minutes,
            detail.about.certification.as_deref()
        ),
        (Some(60), Some("TV-MA"))
    );
    assert!(detail.about.overview.is_some() && detail.about.fanart_url.is_some());
    assert!(detail.about.first_aired.is_some() && detail.about.next_airing.is_none());
    let listed = shows.items.iter().find(|s| s.title == "1883").unwrap();
    assert_eq!(
        (
            listed.season_count,
            listed.episode_count,
            listed.episode_file_count
        ),
        (Some(1), Some(10), Some(2))
    );
    assert_eq!(listed.about, detail.about);
    let shoresy = shows.items.iter().find(|s| s.title == "Shoresy").unwrap();
    assert_eq!(
        shoresy.season_count,
        Some(1),
        "the specials are not a season"
    );
    assert_eq!(
        (shoresy.episode_count, shoresy.episode_file_count),
        (Some(6), Some(0))
    );
    assert!(shoresy.about.next_airing.is_some());
    let shoresy_detail = catalog::title(&t.pool, &lanes(), shoresy.id).await.unwrap();
    assert_eq!(
        shoresy_detail
            .seasons
            .iter()
            .map(|s| (s.season_number, s.total_episode_count))
            .collect::<Vec<_>>(),
        vec![(0, 1), (1, 6)]
    );
    // Movies only: this catalog holds none.
    let movies = catalog::titles(
        &t.pool,
        &lanes(),
        &TitleQuery {
            kind: Some("movie".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(movies.total, 0);
    assert_eq!(detail.copies[0].episode_numbers, vec![1]);
    assert_eq!(detail.copies[0].name_source, "library_name");
    assert_eq!(detail.copies[0].singularr.group.as_deref(), Some("Ghost"));
    assert_eq!(detail.copies[0].lane.as_deref(), Some("shows-hd"));
    let season_one = catalog::title_copies(
        &t.pool,
        id,
        &CopyQuery {
            season: Some(1),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(season_one.total, 2);
    assert_eq!(
        catalog::title_copies(
            &t.pool,
            id,
            &CopyQuery {
                season: Some(2),
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .total,
        0
    );

    // An unchanged file count means the shows' files are not asked for again.
    let asked = recorded
        .urls_seen()
        .iter()
        .filter(|u| u.contains("/episodefile"))
        .count();
    titles_pass(&deps, &engine, false).await.unwrap();
    assert_eq!(
        recorded
            .urls_seen()
            .iter()
            .filter(|u| u.contains("/episodefile"))
            .count(),
        asked
    );

    // A full pass reads them again: episode 2 now has another file, from another group.
    let recorded2 = Recorded::new();
    record_sonarr(&recorded2, &fixture("sonarr_episodefile_upgraded.json"));
    let out2 = outbound(recorded2.clone());
    let engine2: Arc<dyn LibraryEngine> = Arc::new(Sonarr(arr(
        "sonarr",
        ArrKind::Sonarr,
        "sonarr",
        SONARR,
        &out2,
    )));
    let last = core_store::latest_event_id(&t.pool).await.unwrap();
    let (_, report) = titles_pass(&deps, &engine2, true).await.unwrap();
    assert_eq!(report.copies_replaced, 1);
    let after = core_store::events_after(&t.pool, last, 100).await.unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].kind, "copy_replaced");
    assert_eq!(after[0].body["season"], 1);
    assert_eq!(after[0].body["episodes"], serde_json::json!([2]));
    assert_eq!(after[0].body["old"]["group"], "Ghost");
    assert_eq!(after[0].body["new"]["group"], "NTb");
    assert_eq!(after[0].body["new"]["quality"], "WEBDL-1080p");
    assert!(
        recorded
            .methods_seen()
            .iter()
            .chain(recorded2.methods_seen().iter())
            .all(|m| *m == Method::Get)
    );
    t.drop_db().await;
}

#[tokio::test]
async fn a_new_policy_scores_the_stored_copies_again() {
    let Some(t) = TestDb::new("rescore").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    let out = outbound(recorded);
    let engine: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("radarr", "radarr", Some("radarr"))]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let first = policy::store::activate(&t.pool, &journal, &policy)
        .await
        .unwrap();
    assert_eq!(
        first.as_ref().map(|e| e.kind.as_str()),
        Some("policy_loaded")
    );
    assert!(
        policy::store::activate(&t.pool, &journal, &policy)
            .await
            .unwrap()
            .is_none(),
        "the same policy again is no event"
    );
    let deps = deps(&t.pool, &journal, engines, &policy);
    titles_pass(&deps, &engine, true).await.unwrap();

    // The same stamp: nothing to do.
    let same = catalog_sync::reassess(&t.pool, &journal, deps.assess.clone(), None)
        .await
        .unwrap();
    assert_eq!(same.examined, 0);

    // Another build with the same rules: assessed again, nothing changes, no event.
    let rebuilt: Arc<dyn Assess> = Arc::new(Assessor::new(policy.clone(), "another-build"));
    let last = core_store::latest_event_id(&t.pool).await.unwrap();
    let r = catalog_sync::reassess(&t.pool, &journal, rebuilt, None)
        .await
        .unwrap();
    assert_eq!((r.examined, r.updated, r.score_changed), (2, 0, 0));
    assert_eq!(core_store::latest_event_id(&t.pool).await.unwrap(), last);

    // New rules: HEVC is worth more in movies-hd.
    let text = BUILTIN.replacen(
        "\"video.hevc\" = 100, \"audio.atmos\" = 50",
        "\"video.hevc\" = 400, \"audio.atmos\" = 50",
        1,
    );
    let next = Arc::new(ActivePolicy::from_text(&text, PolicySource::File("test".into())).unwrap());
    let loaded = policy::store::activate(&t.pool, &journal, &next)
        .await
        .unwrap()
        .expect("a new policy is an event");
    assert_eq!(loaded.body["previous_hash"], policy.hash.as_str());
    assert_eq!(policy::store::count_versions(&t.pool).await.unwrap(), 2);
    let assess: Arc<dyn Assess> = Arc::new(Assessor::new(next, "another-build"));
    let r = catalog_sync::reassess(&t.pool, &journal, assess, Some(loaded.id))
        .await
        .unwrap();
    assert_eq!((r.examined, r.updated, r.score_changed), (2, 1, 1));
    let changed = core_store::events_after(&t.pool, loaded.id, 10)
        .await
        .unwrap();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].kind, "score_changed");
    assert_eq!(changed[0].cause, Some(loaded.id));
    assert_eq!(changed[0].body["delta"], 300);
    assert_eq!(changed[0].body["traits"][0]["trait_id"], "video.hevc");
    assert_eq!(changed[0].body["traits"][0]["new_points"], 400);
    t.drop_db().await;
}

#[tokio::test]
async fn downloads_get_owners_and_stuck_ones_and_orphans_are_journaled() {
    let Some(t) = TestDb::new("downloads").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    recorded.answer_with(
        Method::Post,
        "/api/v2/auth/login",
        200,
        "Ok.",
        &[("set-cookie", "SID=test; path=/")],
    );
    recorded.answer(
        Method::Get,
        "/api/v2/torrents/info",
        200,
        &fixture("qbit_torrents.json"),
    );
    let out = outbound(recorded.clone());
    let engine: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let qbit = Qbit::new(
        &DownloadClientConfig {
            id: "qbittorrent".to_string(),
            kind: ClientKind::Qbittorrent,
            url: Some(QBIT.to_string()),
            user: Some("u".into()),
            secret: Some("p".into()),
        },
        out.clone(),
    )
    .unwrap();
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("radarr", "radarr", Some("radarr"))]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines, &policy);
    titles_pass(&deps, &engine, true).await.unwrap();
    let last = core_store::latest_event_id(&t.pool).await.unwrap();

    let torrents = qbit.downloads().await.unwrap();
    let queue = engine.queue().await.unwrap();
    let grabs = engine.grabs(1, 250).await.unwrap();

    // Before every arr was heard: owners are found, nobody is called an orphan for lack
    // of a claim. The queue item with no title behind it is one already.
    let mut claims = Claims::default();
    claims.queues.insert("radarr".into(), queue.clone());
    let report = reconcile(&t.pool, &journal, "qbittorrent", torrents.clone(), &claims)
        .await
        .unwrap();
    assert_eq!((report.listed, report.added), (4, 4));
    assert_eq!((report.stuck, report.orphaned), (2, 1));
    let unsettled = downloads::orphans(&t.pool, &DownloadQuery::default())
        .await
        .unwrap();
    assert_eq!(unsettled.total, 1);
    assert!(
        unsettled.items[0]
            .orphan_reason
            .as_ref()
            .unwrap()
            .contains("without a title")
    );

    // Once the picture is complete, the torrent no arr knows is an orphan too.
    claims
        .history
        .insert("radarr".into(), grabs.download_ids.into_iter().collect());
    claims.complete = true;
    let report = reconcile(&t.pool, &journal, "qbittorrent", torrents.clone(), &claims)
        .await
        .unwrap();
    assert_eq!((report.added, report.orphaned), (0, 1));
    let orphans = downloads::orphans(&t.pool, &DownloadQuery::default())
        .await
        .unwrap();
    assert_eq!(orphans.total, 2);
    assert!(
        orphans
            .items
            .iter()
            .any(|d| d.owner.is_none() && d.name.starts_with("A.Manual.Download"))
    );

    let all = downloads::downloads(
        &t.pool,
        &DownloadQuery {
            sort: Some("name".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(all.total, 4);
    let blocked = all
        .items
        .iter()
        .find(|d| d.name.starts_with("10.Cloverfield"))
        .unwrap();
    assert_eq!(blocked.owner.as_deref(), Some("radarr"));
    assert_eq!(blocked.owner_source.as_deref(), Some("queue"));
    assert!(blocked.stuck);
    assert_eq!(
        blocked.stuck_reason.as_deref(),
        Some("Not an upgrade for existing movie file(s)")
    );
    assert_eq!(blocked.queue_state.as_deref(), Some("importBlocked"));
    assert_eq!(
        blocked.title.as_deref(),
        Some("10 Cloverfield Lane"),
        "the queue item links the title"
    );
    let counts = downloads::counts(&t.pool).await.unwrap();
    assert_eq!((counts.total, counts.active, counts.complete), (4, 1, 3));
    assert_eq!((counts.stuck, counts.orphans, counts.unowned), (2, 2, 1));
    for (q, expected) in [
        (
            DownloadQuery {
                owner: Some("none".into()),
                ..Default::default()
            },
            1,
        ),
        (
            DownloadQuery {
                owner: Some("radarr".into()),
                ..Default::default()
            },
            3,
        ),
        (
            DownloadQuery {
                stuck: Some(true),
                ..Default::default()
            },
            2,
        ),
        (
            DownloadQuery {
                active: Some(true),
                ..Default::default()
            },
            1,
        ),
        (
            DownloadQuery {
                q: Some("harry potter".into()),
                ..Default::default()
            },
            1,
        ),
        (
            DownloadQuery {
                category: Some("radarr".into()),
                ..Default::default()
            },
            3,
        ),
        (
            DownloadQuery {
                state: Some("downloading".into()),
                ..Default::default()
            },
            1,
        ),
    ] {
        assert_eq!(
            downloads::downloads(&t.pool, &q).await.unwrap().total,
            expected,
            "{q:?}"
        );
    }

    let events = kinds(&t.pool, last).await;
    assert_eq!(count(&events, "download_added"), 4);
    assert_eq!(count(&events, "download_stuck"), 2);
    assert_eq!(count(&events, "download_orphaned"), 2);

    // The same picture again: no write, no event.
    let last = core_store::latest_event_id(&t.pool).await.unwrap();
    let report = reconcile(&t.pool, &journal, "qbittorrent", torrents.clone(), &claims)
        .await
        .unwrap();
    assert_eq!(
        report,
        singularr::lifecycle::downloads::DownloadsReport {
            listed: 4,
            ..Default::default()
        }
    );
    assert_eq!(core_store::latest_event_id(&t.pool).await.unwrap(), last);

    // The download finishes; another one leaves the client.
    let mut later = torrents.clone();
    let harry = later
        .iter_mut()
        .find(|d| d.name.starts_with("Harry"))
        .unwrap();
    harry.progress = 1.0;
    harry.state = "uploading".into();
    later.retain(|d| !d.name.starts_with("A.Manual.Download"));
    let report = reconcile(&t.pool, &journal, "qbittorrent", later, &claims)
        .await
        .unwrap();
    assert_eq!((report.completed, report.removed), (1, 1));
    let events = kinds(&t.pool, last).await;
    assert_eq!(
        events,
        vec![
            "download_completed".to_string(),
            "download_removed".to_string()
        ]
    );
    assert_eq!(downloads_store::all(&t.pool).await.unwrap().len(), 3);

    // Toward qBittorrent: the sign-in, then GET only.
    let seen = recorded.seen.lock().unwrap().clone();
    let qbit_calls: Vec<_> = seen.iter().filter(|(_, u)| u.starts_with(QBIT)).collect();
    assert_eq!(
        qbit_calls[0],
        &(Method::Post, format!("{QBIT}/api/v2/auth/login"))
    );
    assert!(qbit_calls[1..].iter().all(|(m, _)| *m == Method::Get));
    t.drop_db().await;
}

/// Two download clients side by side, a torrent client and a usenet client: each is read on
/// its own and its pass touches its own rows. A usenet job meets its manager by its job id as a
/// torrent does by its hash: Sonarr names SABnzbd's `nzo_id` as the download id. On Activity a
/// usenet download its client completed reads Completed.
#[tokio::test]
async fn a_usenet_client_lives_beside_a_torrent_client_and_meets_its_manager() {
    let Some(t) = TestDb::new("usenet").await else {
        return;
    };
    const SAB: &str = "http://192.0.2.63:8080";
    let recorded = Recorded::new();
    recorded.answer_with(
        Method::Post,
        "/api/v2/auth/login",
        200,
        "Ok.",
        &[("set-cookie", "SID=test; path=/")],
    );
    recorded.answer(
        Method::Get,
        "/api/v2/torrents/info",
        200,
        &fixture("qbit_torrents.json"),
    );
    recorded.answer(
        Method::Get,
        "mode=queue",
        200,
        &fixture("sabnzbd/queue.json"),
    );
    // The history's jobs finished an hour ago: inside the window the history is read over.
    let finished_at = (chrono::Utc::now().timestamp() - 3600).to_string();
    recorded.answer(
        Method::Get,
        "mode=history",
        200,
        &fixture("sabnzbd/history.json").replace("1795580820", &finished_at),
    );
    let mut allow = Allowlist::default();
    allow
        .allow_integration("qbittorrent", QBIT, singularr::engines::qbit::calls())
        .unwrap();
    allow
        .allow_integration("sabnzbd", SAB, singularr::engines::sabnzbd::calls())
        .unwrap();
    let out = Outbound::with_transport(allow, recorded.clone());
    let client = |id: &str, kind: ClientKind, url: &str| DownloadClientConfig {
        id: id.to_string(),
        kind,
        url: Some(url.to_string()),
        user: Some("u".into()),
        secret: Some("p".into()),
    };
    let qbit = singularr::engines::clients::adapter(
        &client("qbittorrent", ClientKind::Qbittorrent, QBIT),
        &out,
    )
    .unwrap();
    let sab =
        singularr::engines::clients::adapter(&client("sabnzbd", ClientKind::Sabnzbd, SAB), &out)
            .unwrap();
    let journal = Journal::new();

    // Sonarr's queue names the job SABnzbd downloads by its nzo_id; its grab history names the
    // one SABnzbd completed.
    const MOVING: &str = "9B79119C-23C6-4017-B101-4CE68E37A5EF";
    const DONE: &str = "93AC82C7-FFAF-41C7-A11A-06AF0E456887";
    let mut claims = Claims::default();
    claims.queues.insert(
        "sonarr".into(),
        vec![singularr::engines::QueueItem {
            queue_id: 7,
            engine_title_id: Some(4242),
            title: "Tears.of.Steel.S01E03.1080p.WEB-DL.x264-FX".into(),
            download_id: Some(MOVING.into()),
            status: Some("downloading".into()),
            tracked_state: Some("downloading".into()),
            tracked_status: Some("ok".into()),
            messages: vec![],
            indexer: Some("Usenet (Prowlarr)".into()),
        }],
    );
    claims
        .history
        .insert("sonarr".into(), HashSet::from([DONE.to_string()]));
    claims.complete = true;

    let last = core_store::latest_event_id(&t.pool).await.unwrap();
    let torrents = reconcile(
        &t.pool,
        &journal,
        qbit.id(),
        qbit.downloads().await.unwrap(),
        &claims,
    )
    .await
    .unwrap();
    let usenet = reconcile(
        &t.pool,
        &journal,
        sab.id(),
        sab.downloads().await.unwrap(),
        &claims,
    )
    .await
    .unwrap();
    assert_eq!((torrents.listed, usenet.listed, usenet.added), (4, 5, 5));
    let of = |client: &str| DownloadQuery {
        client: Some(client.into()),
        ..Default::default()
    };
    let rows = downloads::downloads(&t.pool, &of("sabnzbd")).await.unwrap();
    assert_eq!(rows.total, 5);
    let row = |id: &str| rows.items.iter().find(|d| d.hash == id).unwrap().clone();
    let moving = row(MOVING);
    assert_eq!(
        (moving.owner.as_deref(), moving.owner_source.as_deref()),
        (Some("sonarr"), Some("queue"))
    );
    assert_eq!(
        (
            moving.protocol.as_str(),
            moving.status.as_deref(),
            moving.state.as_str()
        ),
        ("usenet", Some("downloading"), "Downloading")
    );
    assert_eq!(
        moving.item_id.as_deref(),
        Some("9b79119c-23c6-4017-b101-4ce68e37a5ef")
    );
    assert_eq!(moving.indexer.as_deref(), Some("Usenet (Prowlarr)"));
    assert!(moving.speed_bytes.is_some() && moving.eta_seconds.is_some());
    assert_eq!(
        (moving.seeds, moving.peers, moving.orphan),
        (None, None, false)
    );
    let done = row(DONE);
    assert_eq!(
        (
            done.owner.as_deref(),
            done.owner_source.as_deref(),
            done.status.as_deref()
        ),
        (Some("sonarr"), Some("history"), Some("completed"))
    );
    // The failed job no manager claims is an orphan, as a torrent nobody claims is.
    let failed = rows
        .items
        .iter()
        .find(|d| d.status.as_deref() == Some("failed"))
        .unwrap();
    assert!(failed.orphan && failed.owner.is_none());
    let usenet_rows = downloads::downloads(
        &t.pool,
        &DownloadQuery {
            protocol: Some("usenet".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(usenet_rows.total, 5);
    assert_eq!(
        downloads::downloads(&t.pool, &of("qbittorrent"))
            .await
            .unwrap()
            .total,
        4
    );
    // The journal says which client saw what.
    let events = core_store::events_after(&t.pool, last, 1000).await.unwrap();
    let added_by = |actor: &str| {
        events
            .iter()
            .filter(|e| e.kind == "download_added" && e.actor == actor)
            .count()
    };
    assert_eq!(
        (added_by("sync:qbittorrent"), added_by("sync:sabnzbd")),
        (4, 5)
    );

    // On Activity each download names its own client. A usenet job is done once its client
    // reports it completed.
    let st = state_over(&t.pool, journal.clone(), &[("SABNZBD_URL", SAB)], &out);
    let app = routes::router(st);
    let queue = get_ok(&app, "/api/v1/activity/queue").await;
    let stage_of = |id: &str| {
        let row = queue["queue"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == format!("q-{}", id.to_ascii_lowercase()))
            .unwrap_or_else(|| panic!("{id} in {queue}"))
            .clone();
        (row["stage"].clone(), row["client"].clone())
    };
    assert_eq!(
        stage_of(DONE),
        (serde_json::json!("completed"), serde_json::json!("SABnzbd"))
    );
    assert_eq!(
        stage_of(MOVING),
        (
            serde_json::json!("downloading"),
            serde_json::json!("SABnzbd")
        )
    );
    assert_eq!(
        stage_of("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1").1,
        serde_json::json!("qBittorrent")
    );

    // One client's pass touches its own rows alone: qBittorrent listing nothing removes its own
    // four and none of SABnzbd's.
    let report = reconcile(&t.pool, &journal, "qbittorrent", Vec::new(), &claims)
        .await
        .unwrap();
    assert_eq!(report.removed, 4);
    assert_eq!(downloads_store::all(&t.pool).await.unwrap().len(), 5);
    // The same picture again from SABnzbd: no write, no event.
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let again = reconcile(
        &t.pool,
        &journal,
        sab.id(),
        sab.downloads().await.unwrap(),
        &claims,
    )
    .await
    .unwrap();
    assert_eq!((again.added, again.updated, again.removed), (0, 0, 0));
    assert_eq!(core_store::latest_event_id(&t.pool).await.unwrap(), before);
    // A download is found by the id its manager knows it by, as an act on a manager asks.
    let one = downloads_store::one(&t.pool, &MOVING.to_ascii_lowercase())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(one.client, "sabnzbd");
    // Toward SABnzbd: GETs alone, each in its declared form.
    let sab_calls: Vec<_> = recorded
        .seen
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, u)| u.starts_with(SAB))
        .cloned()
        .collect();
    assert!(!sab_calls.is_empty() && sab_calls.iter().all(|(m, _)| *m == Method::Get));
    t.drop_db().await;
}

/// One download as a test lists it: a torrent, or a usenet job.
fn listed(hash: &str, protocol: singularr::engines::Protocol, progress: f64) -> ClientDownload {
    let usenet = protocol == singularr::engines::Protocol::Usenet;
    ClientDownload {
        id: hash.to_ascii_lowercase(),
        download_id: hash.to_string(),
        protocol,
        name: format!("Release.{hash}"),
        category: Some("radarr".to_string()),
        status: match (usenet, progress >= 1.0) {
            (true, true) => singularr::engines::DownloadStatus::Completed,
            (false, true) => singularr::engines::DownloadStatus::Seeding,
            _ => singularr::engines::DownloadStatus::Downloading,
        },
        state: "state".to_string(),
        progress,
        size_bytes: 1_000,
        downloaded_bytes: None,
        added_at: None,
        completed_at: None,
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

/// A usenet job that finished `days` ago.
fn finished(hash: &str, days: i64) -> ClientDownload {
    ClientDownload {
        completed_at: Some(chrono::Utc::now() - chrono::TimeDelta::days(days)),
        ..listed(hash, singularr::engines::Protocol::Usenet, 1.0)
    }
}

/// A configuration slip (a client's variables lost) sets the client's rows aside: every read
/// leaves them out. Nothing is removed or raised. Its return finds them as they were, with
/// no `download_added` and no `download_orphaned`: the owner, the title, the indexer and when
/// each was first seen are kept. A client under a new id takes over the rows of the downloads
/// it lists, with the same.
#[tokio::test]
async fn a_client_the_configuration_stops_listing_keeps_its_downloads_for_its_return() {
    let Some(t) = TestDb::new("slip").await else {
        return;
    };
    use singularr::engines::Protocol::Torrent;
    const A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1";
    const B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB2";
    let journal = Journal::new();
    let title: i64 = sqlx::query_scalar(
        "INSERT INTO titles (ext_key, kind, tmdb_id, title, sort_title)
         VALUES ('tmdb:movie:763215', 'movie', 763215, 'Damsel', 'damsel') RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    // Radarr's queue holds A, its grab history B.
    let mut claims = Claims::default();
    claims.queues.insert(
        "radarr".into(),
        vec![singularr::engines::QueueItem {
            queue_id: 1,
            engine_title_id: Some(500),
            title: "Release".into(),
            download_id: Some(A.into()),
            status: Some("downloading".into()),
            tracked_state: Some("downloading".into()),
            tracked_status: Some("ok".into()),
            messages: vec![],
            indexer: Some("Indexer (Prowlarr)".into()),
        }],
    );
    claims
        .history
        .insert("radarr".into(), HashSet::from([B.to_string()]));
    claims.complete = true;
    let both = || vec![listed(A, Torrent, 0.5), listed(B, Torrent, 1.0)];
    reconcile(&t.pool, &journal, "qbittorrent", both(), &claims)
        .await
        .unwrap();
    // The catalog links A to its title; both were first seen weeks ago.
    sqlx::query("UPDATE downloads SET title_id = $1 WHERE hash = $2")
        .bind(title)
        .bind(A)
        .execute(&t.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE downloads SET first_seen_at = '2026-11-01T00:00:00Z'")
        .execute(&t.pool)
        .await
        .unwrap();
    let kept = |d: &singularr::lifecycle::model::Download| {
        (
            d.hash.clone(),
            d.owner.clone(),
            d.title_id,
            d.indexer.clone(),
            d.first_seen_at.to_rfc3339(),
            d.orphan,
        )
    };
    let mut first: Vec<_> = downloads_store::all(&t.pool)
        .await
        .unwrap()
        .iter()
        .map(kept)
        .collect();
    first.sort();
    assert_eq!(
        first[0],
        (
            A.to_string(),
            Some("radarr".to_string()),
            Some(title),
            Some("Indexer (Prowlarr)".to_string()),
            "2026-11-01T00:00:00+00:00".to_string(),
            false
        )
    );
    let config = |extra: &[(&'static str, &'static str)]| {
        let mut env = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("RADARR_URL", RADARR),
        ]);
        env.extend(extra.iter().copied());
        Config::from(&env).unwrap()
    };

    // The slip: QBIT_URL, QBIT_USER and QBIT_PASS come empty. Its rows are set aside with one
    // event. No read sees them.
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let report = singularr::api::state::prune_unconfigured(&t.pool, &journal, &config(&[]))
        .await
        .unwrap();
    assert_eq!((report.downloads_set_aside, report.downloads), (2, 0));
    assert_eq!(kinds(&t.pool, before).await, vec!["integrations_pruned"]);
    assert!(downloads_store::all(&t.pool).await.unwrap().is_empty());
    assert!(downloads_store::one(&t.pool, A).await.unwrap().is_none());
    assert!(
        downloads_store::title_queue(&t.pool, title)
            .await
            .unwrap()
            .is_empty()
    );
    let page = downloads::downloads(&t.pool, &DownloadQuery::default())
        .await
        .unwrap();
    assert_eq!(page.total, 0);
    assert_eq!(downloads::counts(&t.pool).await.unwrap().total, 0);
    // A second start in the same state changes nothing more.
    let again = singularr::api::state::prune_unconfigured(&t.pool, &journal, &config(&[]))
        .await
        .unwrap();
    assert!(again.is_empty(), "{again:?}");

    // The return: its rows are back as they were. Radarr imported A (it left the queue) and B
    // is past the grab history read: the stored owners stand. Nothing is raised.
    let with_qbit = [("QBIT_URL", QBIT), ("QBIT_USER", "u"), ("QBIT_PASS", "p")];
    let back = singularr::api::state::prune_unconfigured(&t.pool, &journal, &config(&with_qbit))
        .await
        .unwrap();
    assert!(back.is_empty(), "{back:?}");
    let mut gone = Claims::default();
    gone.history.insert("radarr".into(), HashSet::new());
    gone.complete = true;
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let report = reconcile(&t.pool, &journal, "qbittorrent", both(), &gone)
        .await
        .unwrap();
    assert_eq!(
        (report.added, report.orphaned, report.removed),
        (0, 0, 0),
        "{report:?}"
    );
    assert!(kinds(&t.pool, before).await.is_empty());
    let mut now: Vec<_> = downloads_store::all(&t.pool)
        .await
        .unwrap()
        .iter()
        .map(kept)
        .collect();
    now.sort();
    assert_eq!(now, first);

    // A new id for the same client: the rows of the downloads it lists are taken over with
    // what they held. None is added.
    let file =
        format!("[[integration]]\nid = \"seedbox\"\ntype = \"qbittorrent\"\nurl = \"{QBIT}\"\n");
    let renamed = Config::with_instance(
        &BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("RADARR_URL", RADARR),
        ]),
        Some(("/x/i.toml", file.as_str())),
    )
    .unwrap();
    let report = singularr::api::state::prune_unconfigured(&t.pool, &journal, &renamed)
        .await
        .unwrap();
    assert_eq!(report.downloads_set_aside, 2);
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let report = reconcile(
        &t.pool,
        &journal,
        "seedbox",
        vec![listed(A, Torrent, 0.6)],
        &gone,
    )
    .await
    .unwrap();
    assert_eq!((report.added, report.taken_over), (0, 1), "{report:?}");
    assert!(
        !kinds(&t.pool, before)
            .await
            .iter()
            .any(|k| k == "download_added")
    );
    let a = downloads_store::one(&t.pool, A).await.unwrap().unwrap();
    assert_eq!(a.client, "seedbox");
    assert_eq!(
        (kept(&a), a.progress),
        (first[0].clone(), 0.6),
        "the row of A, taken over"
    );
    // B stays set aside under the old id, out of every read.
    assert_eq!(downloads_store::all(&t.pool).await.unwrap().len(), 1);
    let set_aside: i64 =
        sqlx::query_scalar("SELECT count(*) FROM downloads WHERE absent_since IS NOT NULL")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(set_aside, 1);
    // Rows set aside for longer than the days kept are removed at start.
    sqlx::query("UPDATE downloads SET absent_since = now() - interval '31 days' WHERE absent_since IS NOT NULL")
        .execute(&t.pool)
        .await
        .unwrap();
    let report = singularr::api::state::prune_unconfigured(&t.pool, &journal, &renamed)
        .await
        .unwrap();
    assert_eq!((report.downloads, report.downloads_set_aside), (1, 0));
    t.drop_db().await;
}

/// A usenet client's history is read over a window. A job past the listing's edge is no
/// removal: its row stays while it is inside the window and goes without an event once it is
/// older. A job that comes back inside the window finds its row. A job inside the edge that
/// the listing leaves out was removed.
#[tokio::test]
async fn a_usenet_history_window_moves_without_false_events() {
    let Some(t) = TestDb::new("window").await else {
        return;
    };
    use singularr::engines::{ClientListing, Protocol::Usenet};
    let journal = Journal::new();
    let claims = Claims::default();
    let edge =
        |days: f64| Some(chrono::Utc::now() - chrono::TimeDelta::seconds((days * 86_400.0) as i64));
    // First pass: a job downloading, and four finished 1, 2, 3 and 9 days ago.
    let first = ClientListing {
        downloads: vec![
            listed("Q", Usenet, 0.4),
            finished("J1", 1),
            finished("J2", 2),
            finished("J3", 3),
            finished("J9", 9),
        ],
        finished_since: None,
    };
    let report = reconcile(&t.pool, &journal, "sabnzbd", first, &claims)
        .await
        .unwrap();
    assert_eq!(report.added, 5);
    // The history now holds more than the limit: the edge is the oldest job read, J2. J1 was
    // removed from SABnzbd (its manager took it). J3 is past the edge inside the window, J9
    // past the window.
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let cut = ClientListing {
        downloads: vec![listed("Q", Usenet, 0.5), finished("J2", 2)],
        finished_since: edge(2.0),
    };
    let report = reconcile(&t.pool, &journal, "sabnzbd", cut, &claims)
        .await
        .unwrap();
    assert_eq!(
        (report.removed, report.aged, report.added),
        (1, 1, 0),
        "{report:?}"
    );
    assert_eq!(kinds(&t.pool, before).await, vec!["download_removed"]);
    let mut stored: Vec<String> = downloads_store::all(&t.pool)
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.hash)
        .collect();
    stored.sort();
    assert_eq!(stored, ["J2", "J3", "Q"]);
    // The edge moves back: J3 comes back inside the window and finds its row.
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let whole = ClientListing {
        downloads: vec![
            listed("Q", Usenet, 0.6),
            finished("J2", 2),
            finished("J3", 3),
        ],
        finished_since: edge(7.0),
    };
    let report = reconcile(&t.pool, &journal, "sabnzbd", whole, &claims)
        .await
        .unwrap();
    assert_eq!((report.added, report.removed), (0, 0), "{report:?}");
    assert!(kinds(&t.pool, before).await.is_empty());
    t.drop_db().await;
}

/// Migration 0019 over rows the schema before it wrote: a qBittorrent row with every column
/// set takes the id of the qBittorrent engine last seen working, keeps every value it had, and
/// is keyed by its client and hash. The portal's view reads it. A database at version 18, as
/// the live one is, takes every later migration in order.
#[tokio::test]
async fn the_download_clients_migration_keeps_the_rows_before_it() {
    let Some(t) = TestDb::bare("m0019").await else {
        return;
    };
    db::MIGRATOR
        .run_to(18, &t.pool)
        .await
        .expect("the migrations before 0019 apply");
    let later: Vec<String> = db::MIGRATOR
        .iter()
        .filter(|m| m.version > 18)
        .map(|m| format!("{} {}", m.version, m.description))
        .collect();
    let pending = db::pending_migrations(&t.pool).await.unwrap();
    assert_eq!(pending, later);
    assert!(pending[0].starts_with("19 "), "{pending:?}");
    for (engine, ok_at) in [
        ("qbit-old", "2026-11-20T00:00:00Z"),
        ("qbit-main", "2026-11-24T00:00:00Z"),
    ] {
        sqlx::query(
            "INSERT INTO engine_state (engine, kind, last_ok_at) VALUES ($1, 'qbittorrent', $2::timestamptz)",
        )
        .bind(engine)
        .bind(ok_at)
        .execute(&t.pool)
        .await
        .unwrap();
    }
    let title: i64 = sqlx::query_scalar(
        "INSERT INTO titles (ext_key, kind, tmdb_id, title, sort_title)
         VALUES ('tmdb:movie:763215', 'movie', 763215, 'Damsel', 'damsel') RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO downloads (hash, name, category, state, progress, size_bytes, added_at,
             completed_at, seeding_seconds, save_path, owner, owner_source, title_id,
             queue_status, queue_state, queue_health, queue_messages, stuck, stuck_reason,
             orphan, orphan_reason, first_seen_at, updated_at, speed_bytes, eta_seconds, seeds,
             peers, indexer)
         VALUES ('147E6690379BCAC2718AAA6A0D4089BFED6737B2', 'Damsel.2024.1080p', 'radarr',
             'downloading', 0.5, 7911404354, '2026-11-14T11:50:14Z', '2026-11-14T13:00:00Z',
             60, '/downloads', 'radarr', 'queue', $1, 'downloading', 'importPending',
             'warning', '[\"Not an upgrade\"]', true, 'Not an upgrade', false, 'none',
             '2026-11-14T11:50:20Z', '2026-11-24T08:00:00Z', 1024, 600, 4, 9,
             'TorrentDownload (Prowlarr)')",
    )
    .bind(title)
    .execute(&t.pool)
    .await
    .unwrap();
    async fn row(pool: &PgPool) -> serde_json::Value {
        sqlx::query_scalar::<_, serde_json::Value>("SELECT to_jsonb(d) FROM downloads d")
            .fetch_one(pool)
            .await
            .unwrap()
    }
    let old = row(&t.pool).await;

    db::migrate(&t.pool).await.expect("0019 applies");
    assert!(db::pending_migrations(&t.pool).await.unwrap().is_empty());
    let mut new = row(&t.pool).await;
    let added: Vec<(String, serde_json::Value)> =
        ["client", "protocol", "item_id", "status", "absent_since"]
            .iter()
            .map(|c| {
                (
                    c.to_string(),
                    new.as_object_mut().unwrap().remove(*c).unwrap(),
                )
            })
            .collect();
    assert_eq!(new, old, "every value the row had is kept");
    assert_eq!(
        added,
        vec![
            ("client".to_string(), serde_json::json!("qbit-main")),
            ("protocol".to_string(), serde_json::json!("torrent")),
            ("item_id".to_string(), serde_json::Value::Null),
            ("status".to_string(), serde_json::Value::Null),
            ("absent_since".to_string(), serde_json::Value::Null),
        ]
    );
    // The key is the client and the hash: another client can hold the same torrent, the same
    // client cannot hold it twice.
    let insert = |client: &'static str| {
        sqlx::query(
            "INSERT INTO downloads (hash, client, name, state)
             VALUES ('147E6690379BCAC2718AAA6A0D4089BFED6737B2', $1, 'x', 'seed')",
        )
        .bind(client)
        .execute(&t.pool)
    };
    assert!(insert("transmission").await.is_ok());
    assert!(insert("qbit-main").await.is_err());
    // The row reads as before the branch. The portal sees the title on its way.
    let stored = downloads_store::one(&t.pool, "147e6690379bcac2718aaa6a0d4089bfed6737b2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            stored.client.as_str(),
            stored.protocol.as_str(),
            stored.owner.as_deref()
        ),
        ("qbit-main", "torrent", Some("radarr"))
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM face_availability WHERE tmdb_id = 763215")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(state, "processing");
    t.drop_db().await;
}

#[tokio::test]
async fn engine_state_follows_the_passes_and_journals_the_turns() {
    let Some(t) = TestDb::new("engines").await else {
        return;
    };
    let journal = Journal::new();
    let set = EngineSet::new(
        vec![],
        None,
        slots(&[
            ("radarr", "radarr", Some("radarr")),
            ("qbittorrent", "qbittorrent", None),
        ]),
    );
    let ok = || {
        Ok(PassOk {
            took: Duration::from_millis(42),
            version: Some("5.1".into()),
            counts: serde_json::json!({ "titles": 3 }),
        })
    };
    set.report(&t.pool, &journal, "radarr", "titles", ok())
        .await
        .unwrap();
    set.report(
        &t.pool,
        &journal,
        "radarr",
        "queue",
        Ok(PassOk::new(
            Duration::from_millis(7),
            serde_json::json!({ "queue": 1 }),
        )),
    )
    .await
    .unwrap();
    set.report(
        &t.pool,
        &journal,
        "radarr",
        "queue",
        Err("192.0.2.61 did not answer: timed out".into()),
    )
    .await
    .unwrap();
    set.report(
        &t.pool,
        &journal,
        "radarr",
        "queue",
        Err("192.0.2.61 did not answer: timed out".into()),
    )
    .await
    .unwrap();
    // The queue pass that failed recovers: the engine is up again.
    set.report(&t.pool, &journal, "radarr", "queue", ok())
        .await
        .unwrap();
    assert_eq!(
        kinds(&t.pool, 0).await,
        vec!["engine_up", "engine_down", "engine_up"]
    );

    let status = statuses(&t.pool, &set).await.unwrap();
    assert_eq!(status.len(), 2);
    let radarr = &status[0];
    assert!(radarr.ok && radarr.configured);
    assert_eq!(radarr.version.as_deref(), Some("5.1"));
    assert_eq!(radarr.latency_ms, Some(42));
    assert_eq!(
        radarr.counts,
        serde_json::json!({ "titles": 3, "queue": 1 }),
        "counts of the passes are merged"
    );
    assert_eq!((radarr.failures, radarr.last_error.clone()), (0, None));
    assert!(!status[1].ok, "an engine that was never asked is not up");

    // The round trips of the calls that succeeded, oldest first, and never more than 20.
    assert_eq!(radarr.latency_ms_history, vec![42, 7, 42]);
    for i in 0..25u64 {
        let pass = PassOk::new(Duration::from_millis(100 + i), serde_json::json!({}));
        set.report(&t.pool, &journal, "radarr", "queue", Ok(pass))
            .await
            .unwrap();
    }
    let status = statuses(&t.pool, &set).await.unwrap();
    assert_eq!(
        status[0].latency_ms_history,
        (105..125).collect::<Vec<i32>>()
    );
    assert_eq!(status[0].latency_ms, Some(124));
    assert!(status[1].latency_ms_history.is_empty());

    // A fresh process learns the last state from the table, so a restart is not a turn.
    let restarted = EngineSet::new(vec![], None, slots(&[("radarr", "radarr", Some("radarr"))]));
    let last = core_store::latest_event_id(&t.pool).await.unwrap();
    restarted
        .report(&t.pool, &journal, "radarr", "titles", ok())
        .await
        .unwrap();
    assert_eq!(core_store::latest_event_id(&t.pool).await.unwrap(), last);
    t.drop_db().await;
}

/// An integration is down while any pass of it fails, with that pass's error: a later pass that
/// succeeds hides nothing. One `engine_down`, and no `engine_up` until the failing pass recovers.
#[tokio::test]
async fn an_integration_is_down_while_any_pass_of_it_fails() {
    let Some(t) = TestDb::new("health_by_pass").await else {
        return;
    };
    let journal = Journal::new();
    let st = state_with(&t.pool, journal.clone(), &[("RADARR_API_KEY", "a-key")]);
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let token = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "reader".into(),
            scopes: vec![Scope::Read],
        },
    )
    .await
    .unwrap();
    let read = format!("Bearer {}", token.secret);
    let app = routes::router(st.clone());
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    const DOWN: &str = "192.0.2.61 answered 503 for /api/v3/movie";
    st.engines
        .report(&t.pool, &journal, "radarr", "titles", Err(DOWN.into()))
        .await
        .unwrap();
    let queue = || {
        Ok(PassOk::new(
            Duration::from_millis(9),
            serde_json::json!({ "queue": 2 }),
        ))
    };
    st.engines
        .report(&t.pool, &journal, "radarr", "queue", queue())
        .await
        .unwrap();

    let shown = listed_integration(&app, &read, "radarr").await;
    assert_eq!(
        (shown["state"].as_str(), shown["detail"].as_str()),
        (Some("down"), Some(DOWN))
    );
    let status = statuses(&t.pool, &st.engines).await.unwrap();
    let row = status.iter().find(|s| s.engine == "radarr").unwrap();
    assert!(!row.ok);
    assert_eq!((row.last_error.as_deref(), row.failures), (Some(DOWN), 1));
    assert_eq!(kinds(&t.pool, before).await, vec!["engine_down"]);

    // Another good queue pass changes nothing. The titles pass that recovers turns it up.
    st.engines
        .report(&t.pool, &journal, "radarr", "queue", queue())
        .await
        .unwrap();
    assert_eq!(kinds(&t.pool, before).await, vec!["engine_down"]);
    assert_eq!(
        listed_integration(&app, &read, "radarr").await["state"],
        "down"
    );
    st.engines
        .report(&t.pool, &journal, "radarr", "titles", queue())
        .await
        .unwrap();
    assert_eq!(
        kinds(&t.pool, before).await,
        vec!["engine_down", "engine_up"]
    );
    assert_eq!(
        listed_integration(&app, &read, "radarr").await["state"],
        "ok"
    );
    let status = statuses(&t.pool, &st.engines).await.unwrap();
    let row = status.iter().find(|s| s.engine == "radarr").unwrap();
    assert!(row.ok);
    assert_eq!((row.last_error.clone(), row.failures), (None, 0));
    t.drop_db().await;
}

/// A pass this process does not run is forgotten at start: its failing row would keep its
/// integration down for good. While a pass fails, the integration's last success is that pass's.
#[tokio::test]
async fn a_pass_this_process_does_not_run_is_forgotten_at_start() {
    let Some(t) = TestDb::new("stale_pass").await else {
        return;
    };
    let journal = Journal::new();
    let st = state_with(&t.pool, journal.clone(), &[("RADARR_API_KEY", "a-key")]);
    let passes = st.engines.passes(true);
    for pass in ["titles", "queue", "history"] {
        assert!(passes.contains(&("radarr".to_string(), pass)), "{passes:?}");
    }
    assert!(
        st.engines.passes(false).is_empty(),
        "with the sync off no loop runs"
    );
    let ok = || {
        Ok(PassOk::new(
            Duration::from_millis(5),
            serde_json::json!({ "titles": 1 }),
        ))
    };
    st.engines
        .report(&t.pool, &journal, "radarr", "titles", ok())
        .await
        .unwrap();
    st.engines
        .report(
            &t.pool,
            &journal,
            "radarr",
            "retired",
            Err("gone for good".into()),
        )
        .await
        .unwrap();
    let radarr = |all: Vec<_>| {
        all.into_iter()
            .find(|s: &singularr::engines::model::EngineStatus| s.engine == "radarr")
            .unwrap()
    };
    let down = radarr(statuses(&t.pool, &st.engines).await.unwrap());
    assert!(!down.ok);
    assert_eq!(down.last_error.as_deref(), Some("gone for good"));
    assert_eq!(down.last_ok_at, None, "the failing pass never succeeded");

    // At start the row of the pass no loop runs goes. The titles pass alone is left.
    assert_eq!(
        st.engines.forget_other_passes(&t.pool, true).await.unwrap(),
        1
    );
    let up = radarr(statuses(&t.pool, &st.engines).await.unwrap());
    assert!(up.ok && up.last_error.is_none(), "{up:?}");
    assert!(up.last_ok_at.is_some());
    t.drop_db().await;
}

/// The pass rows (B4): one per integration and pass.
async fn pass_rows(pool: &PgPool) -> Vec<(String, String, bool)> {
    sqlx::query_as("SELECT integration, pass, ok FROM integration_health ORDER BY 1, 2")
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The jobs of the runtime read no integration. Run once each, they leave the pass rows (B4) as
/// they were. The passes the loops report on stay the same. `forget_other_passes` keeps the same
/// rows. `GET /api/v1/jobs` lists the folder scans from their schedules with the ids and the
/// intervals of their loops. The index and the dispatcher are not listed. A listed scan runs now
/// with `POST /api/v1/jobs/{id}/run`.
#[tokio::test]
async fn the_runtimes_jobs_leave_the_pass_rows_alone() {
    let Some(t) = TestDb::new("jobs_passes").await else {
        return;
    };
    let journal = Journal::new();
    let st = state_with(&t.pool, journal.clone(), &[("RADARR_API_KEY", "a-key")]);
    let ok = PassOk::new(Duration::from_millis(5), serde_json::json!({ "titles": 1 }));
    st.engines
        .report(&t.pool, &journal, "radarr", "titles", Ok(ok))
        .await
        .unwrap();
    let before = pass_rows(&t.pool).await;
    assert_eq!(
        before,
        vec![("radarr".to_string(), "titles".to_string(), true)]
    );
    let passes = st.engines.passes(true);

    let schedules = runtime_jobs::schedules(&st.config, &st.settings, None);
    assert!(
        schedules.iter().all(|s| s.integration.is_none()),
        "{schedules:?}"
    );
    let jobs = start_jobs(&st).await;
    for s in &schedules {
        let ended = runtime::run_and_wait(&t.pool, &s.key, serde_json::json!({}))
            .await
            .unwrap();
        assert!(ended.is_some(), "{} ran", s.key);
    }

    let app = routes::router(st.clone());
    let owner: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, panel) = call(&app, "GET", "/api/v1/jobs", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{panel}");
    let listed: Vec<(String, u64)> = panel["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| {
            (
                j["id"].as_str().unwrap().to_string(),
                j["every"].as_u64().unwrap(),
            )
        })
        .filter(|(id, _)| id.starts_with("folder:") || id.starts_with("singularr:"))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("folder:home-movies".to_string(), 21_600),
            ("folder:music".to_string(), 21_600)
        ]
    );
    let music = panel["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == "folder:music")
        .unwrap();
    assert_eq!(music["label"], "Music folder scan");
    assert!(music["lastResult"].is_string(), "{music}");
    assert_eq!(music["runs"], 1);
    let (status, _, _) = call(&app, "POST", "/api/v1/jobs/folder:music/run", &owner, None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    for unknown in ["singularr:index", "folder:movie-folders"] {
        let (status, _, _) = call(
            &app,
            "POST",
            &format!("/api/v1/jobs/{unknown}/run"),
            &owner,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{unknown}");
    }
    jobs.stop().await;

    assert_eq!(pass_rows(&t.pool).await, before, "no job wrote a pass row");
    assert_eq!(st.engines.passes(true), passes);
    assert_eq!(
        st.engines.forget_other_passes(&t.pool, true).await.unwrap(),
        0
    );
    assert_eq!(pass_rows(&t.pool).await, before);
    t.drop_db().await;
}

/// The calls of one page of a grab history.
fn page_calls(recorded: &Recorded, page: u32) -> usize {
    let part = format!("/history?page={page}&pageSize=250");
    recorded
        .urls_seen()
        .iter()
        .filter(|u| u.contains(&part))
        .count()
}

/// The passes of the managers and the download clients are jobs of the runtime (B9b). Each run
/// writes its pass row, the claims and its markers. It ends its job in the same commit. The
/// downloads job judges each download by the claims of its id. A download nobody claims is an
/// orphan once every arr was heard. `GET /api/v1/jobs` lists the passes with the ids and the
/// intervals of their loops. A pass runs now from the Jobs page and from its integration.
/// `forget_other_passes` knows every pass the runtime runs.
#[tokio::test]
async fn the_manager_and_download_passes_run_as_jobs() {
    let Some(t) = TestDb::new("sync_jobs").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    record_sonarr(&recorded, &fixture("sonarr_episodefile.json"));
    recorded.answer(
        Method::Get,
        &format!("{SONARR}/api/v3/history"),
        200,
        r#"{"page": 1, "pageSize": 250, "totalRecords": 0, "records": []}"#,
    );
    recorded.answer_with(
        Method::Post,
        "/api/v2/auth/login",
        200,
        "Ok.",
        &[("set-cookie", "SID=test; path=/")],
    );
    recorded.answer(
        Method::Get,
        "/api/v2/torrents/info",
        200,
        &fixture("qbit_torrents.json"),
    );
    let journal = Journal::new();
    let st = state_on(
        &t.pool,
        journal.clone(),
        &[
            ("RADARR_API_KEY", "test-key"),
            ("SONARR_API_KEY", "test-key"),
            ("QBIT_URL", QBIT),
            ("QBIT_PASS", "p"),
            ("SINGULARR_SYNC_JITTER_SECS", "0"),
        ],
        recorded.clone(),
    );

    // Every pass the runtime runs is one `forget_other_passes` keeps. Every pass it keeps of a
    // manager or a download client has a schedule.
    let schedules = runtime_jobs::schedules(&st.config, &st.settings, Some(&st.engines));
    let run: Vec<(String, &str)> = schedules
        .iter()
        .filter_map(|s| Some((s.integration.clone()?, runtime_jobs::listed(&s.key)?)))
        .collect();
    let passes = st.engines.passes(true);
    for pass in &run {
        assert!(passes.contains(pass), "{pass:?} is not in {passes:?}");
    }
    for pass in passes
        .iter()
        .filter(|(_, p)| ["titles", "queue", "history", "downloads"].contains(p))
    {
        assert!(run.contains(pass), "{pass:?} has no schedule");
    }
    assert_eq!(run.len(), 7);

    let (jobs, sync) = start_sync_jobs(&st).await;
    for key in [
        "radarr:titles",
        "sonarr:titles",
        "radarr:queue",
        "sonarr:queue",
        "radarr:history",
        "sonarr:history",
        // After every arr was heard: the orphans are settled.
        "qbittorrent:downloads",
    ] {
        let ended = runtime::run_and_wait(&t.pool, key, serde_json::json!({}))
            .await
            .unwrap()
            .unwrap();
        assert!(ended.ok(), "{key}: {ended:?}");
    }

    // The claims: Radarr's queue and history, Sonarr's queue.
    let claimed: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT manager, source, count(*) FROM download_claims GROUP BY 1, 2 ORDER BY 1, 2",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert!(
        claimed.contains(&("radarr".into(), "queue".into(), 3))
            && claimed.contains(&("radarr".into(), "history".into(), 2)),
        "{claimed:?}"
    );
    // Sonarr's history holds no record: no marker says where a read stopped.
    assert_eq!(
        (
            claims::loaded_id(&t.pool, "radarr")
                .await
                .unwrap()
                .is_some(),
            claims::loaded_id(&t.pool, "sonarr").await.unwrap()
        ),
        (true, None)
    );
    for (arr, name) in [
        ("radarr", claims::HISTORY_LOADED),
        ("radarr", claims::FILES_FULL_AT),
        ("sonarr", claims::FILES_FULL_AT),
    ] {
        assert!(
            claims::marked_at(&t.pool, arr, name)
                .await
                .unwrap()
                .is_some(),
            "{arr} {name}"
        );
    }
    // Radarr's titles pass kept the custom formats of its two files, with the time it asked.
    let formats: Vec<(String, i64)> =
        sqlx::query_as("SELECT manager, count(*) FROM manager_file_formats GROUP BY 1 ORDER BY 1")
            .fetch_all(&t.pool)
            .await
            .unwrap();
    assert_eq!(formats, vec![("radarr".to_string(), 2)]);
    assert!(
        claims::marked_at(&t.pool, "radarr", "formats.refreshed_at")
            .await
            .unwrap()
            .is_some()
    );
    // Each pass wrote its row. Every one went well.
    let rows = pass_rows(&t.pool).await;
    assert_eq!(rows.len(), 7, "{rows:?}");
    assert!(rows.iter().all(|(_, _, ok)| *ok), "{rows:?}");
    // A torrent of Radarr's queue is Radarr's. The one nobody claims is an orphan: every arr was
    // heard.
    let owners: Vec<(String, Option<String>, bool)> =
        sqlx::query_as("SELECT hash, owner, orphan FROM downloads ORDER BY hash")
            .fetch_all(&t.pool)
            .await
            .unwrap();
    assert_eq!(
        (owners.len(), owners[0].1.as_deref(), owners[3].2),
        (4, Some("radarr"), true),
        "{owners:?}"
    );
    let settled: Option<String> = sqlx::query_scalar(
        "SELECT counts->>'ownership_settled' FROM engine_state WHERE engine = 'qbittorrent'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(settled.as_deref(), Some("true"));
    // Each run is a check of its integration.
    let checks = singularr::core::checks::recent().of("radarr");
    for via in ["titles", "queue", "history"] {
        assert!(checks.iter().any(|c| c.via == via && c.ok), "{via}");
    }

    let app = routes::router(st.clone());
    let owner: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, panel) = call(&app, "GET", "/api/v1/jobs", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{panel}");
    let listed: Vec<(String, u64, String, String)> = panel["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["integration"].is_string())
        .map(|j| {
            (
                j["id"].as_str().unwrap().to_string(),
                j["every"].as_u64().unwrap(),
                j["integration"].as_str().unwrap().to_string(),
                j["lastResult"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let ok =
        |id: &str, every: u64, of: &str| (id.to_string(), every, of.to_string(), "ok".to_string());
    assert_eq!(
        listed,
        vec![
            ok("qbittorrent:downloads", 30, "qbittorrent"),
            ok("radarr:history", 900, "radarr"),
            ok("radarr:queue", 60, "radarr"),
            ok("radarr:titles", 900, "radarr"),
            ok("sonarr:history", 900, "sonarr"),
            ok("sonarr:queue", 60, "sonarr"),
            ok("sonarr:titles", 900, "sonarr"),
        ]
    );
    let queue = panel["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == "radarr:queue")
        .unwrap();
    assert!(
        queue["label"].as_str().unwrap().ends_with("download queue"),
        "{queue}"
    );
    assert_eq!(queue["lastDetail"], "3 in queue", "{queue}");
    // The integration's panel names its jobs as it did with the loops.
    let (status, _, panels) = call(&app, "GET", "/api/v1/integrations", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{panels}");
    let radarr = panels["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "radarr")
        .unwrap();
    assert_eq!(
        radarr["jobs"],
        serde_json::json!(["radarr:history", "radarr:queue", "radarr:titles"]),
        "{radarr}"
    );
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/v1/jobs/qbittorrent:downloads/run",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let (status, _, started) = call(
        &app,
        "POST",
        "/api/v1/integrations/radarr/sync",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    assert_eq!(
        started["jobs"],
        serde_json::json!(["radarr:titles", "radarr:queue", "radarr:history"])
    );
    jobs.stop().await;
    sync.stop();

    // The rows the runtime wrote are the rows of the passes it runs.
    assert_eq!(
        st.engines.forget_other_passes(&t.pool, true).await.unwrap(),
        0
    );
    t.drop_db().await;
}

/// A history read takes pages newest first until a page holds a record the last read that ended
/// well saw (by the arr's record ids). A start reads the first page alone when nothing came since.
/// A re-grab of an old torrent at the top of a burst of 300 grabs reads on to those records. A read
/// cut short (here a page that fails, as a lost lease would) keeps its pages and moves no marker:
/// the next read goes on to the same records. Record ids that go backwards void the marker: the
/// read goes to its most pages. A start whose last full file pass is recent makes no full file
/// pass. Radarr's format book outlives a start.
#[tokio::test]
async fn a_history_read_stops_at_the_records_it_has() {
    let Some(t) = TestDb::new("sync_markers").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    // Each record: its id and the download id it grabbed.
    let hash = |n: i64| format!("{:040X}", n);
    let records = |from: i64, to: i64| -> Vec<(i64, String)> {
        (to..=from).rev().map(|id| (id, hash(id))).collect()
    };
    let answer = |pages: Vec<Vec<(i64, String)>>, total: i64| {
        for (n, page) in pages.into_iter().enumerate() {
            let records: Vec<serde_json::Value> = page
                .into_iter()
                .map(|(id, download)| serde_json::json!({ "id": id, "downloadId": download }))
                .collect();
            recorded.answer(
                Method::Get,
                &format!("{RADARR}/api/v3/history?page={}&pageSize=250", n + 1),
                200,
                &serde_json::json!({
                    "page": n + 1, "pageSize": 250, "totalRecords": total, "records": records
                })
                .to_string(),
            );
        }
    };
    answer(
        vec![records(1000, 751), records(750, 501), records(500, 401)],
        600,
    );
    let st = state_on(
        &t.pool,
        Journal::new(),
        &[
            ("RADARR_API_KEY", "test-key"),
            ("SINGULARR_SYNC_JITTER_SECS", "0"),
            ("SINGULARR_SYNC_HISTORY_MAX_PAGES", "3"),
        ],
        recorded.clone(),
    );
    let run = |key: &'static str| {
        let pool = t.pool.clone();
        async move {
            let ended = runtime::run_and_wait(&pool, key, serde_json::json!({}))
                .await
                .unwrap()
                .unwrap();
            assert!(ended.ok(), "{key}: {ended:?}");
        }
    };
    let count = || async { claims::history_count(&t.pool, "radarr").await.unwrap() };
    let loaded = || async { claims::loaded_id(&t.pool, "radarr").await.unwrap() };
    let calls = |n| page_calls(&recorded, n);
    let moviefile_calls = || {
        recorded
            .urls_seen()
            .iter()
            .filter(|u| u.contains("/moviefile"))
            .count()
    };
    let complete = || async {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT counts->>'grab_history_complete' FROM engine_state WHERE engine = 'radarr'",
        )
        .fetch_one(&t.pool)
        .await
        .unwrap()
    };

    // The first start reads the whole history. Its titles pass asks about the files' formats.
    let (jobs, sync) = start_sync_jobs(&st).await;
    run("radarr:history").await;
    run("radarr:titles").await;
    assert_eq!((calls(2), calls(3)), (1, 1));
    assert_eq!((count().await, loaded().await), (600, Some(1000)));
    assert_eq!(moviefile_calls(), 1);
    let full_at = claims::marked_at(&t.pool, "radarr", claims::FILES_FULL_AT)
        .await
        .unwrap();
    assert!(full_at.is_some());
    jobs.stop().await;
    sync.stop();

    // A start with nothing new: the first page of the history, no full file pass, no file asked
    // about again.
    let (jobs, sync) = start_sync_jobs(&st).await;
    run("radarr:history").await;
    run("radarr:titles").await;
    assert!(calls(1) >= 2);
    assert_eq!((calls(2), calls(3)), (1, 1));
    assert_eq!(moviefile_calls(), 1);
    assert_eq!(
        claims::marked_at(&t.pool, "radarr", claims::FILES_FULL_AT)
            .await
            .unwrap(),
        full_at
    );

    // A burst of 300 grabs whose newest re-grabs an old torrent: the read goes on to page 2.
    let mut burst = records(1300, 1051);
    burst[0].1 = hash(990);
    let mut second = records(1050, 1001);
    second.extend(records(1000, 801));
    answer(vec![burst.clone(), second], 900);
    let two = calls(2);
    run("radarr:history").await;
    assert_eq!(calls(2), two + 1);
    assert_eq!((count().await, loaded().await), (899, Some(1300)));

    // A read cut short at its second page: the first page stays, the marker stays. The next read
    // goes past the page the cut read stored, to the records the last good read saw.
    answer(vec![records(1550, 1301), burst], 1150);
    recorded.answer_once(
        Method::Get,
        "/api/v3/history?page=2&pageSize=250",
        400,
        "{}",
        &[],
    );
    let (one, two) = (calls(1), calls(2));
    let cut = runtime::run_and_wait(&t.pool, "radarr:history", serde_json::json!({}))
        .await
        .unwrap()
        .unwrap();
    assert!(!cut.ok(), "{cut:?}");
    assert_eq!((count().await, loaded().await), (1149, Some(1300)));
    run("radarr:history").await;
    assert_eq!((calls(1), calls(2)), (one + 2, two + 2));
    assert_eq!((count().await, loaded().await), (1149, Some(1550)));

    // The arr's database was rebuilt: its newest record is older than the marker's. The read goes
    // to its most pages (3 here). The marker takes the new newest record.
    answer(
        vec![records(750, 501), records(500, 251), records(250, 1)],
        5000,
    );
    let three = calls(3);
    run("radarr:history").await;
    assert_eq!(calls(3), three + 1);
    assert_eq!(loaded().await, Some(750));
    assert_eq!(complete().await.as_deref(), Some("false"));
    jobs.stop().await;
    sync.stop();
    t.drop_db().await;
}

/// The recorded answers of the sync's set: a Radarr, a Sonarr without grabs and a qBittorrent.
fn record_sync_set(recorded: &Recorded) {
    record_radarr(
        recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    record_sonarr(recorded, &fixture("sonarr_episodefile.json"));
    recorded.answer(
        Method::Get,
        &format!("{SONARR}/api/v3/history"),
        200,
        r#"{"page": 1, "pageSize": 250, "totalRecords": 0, "records": []}"#,
    );
    recorded.answer_with(
        Method::Post,
        "/api/v2/auth/login",
        200,
        "Ok.",
        &[("set-cookie", "SID=test; path=/")],
    );
    recorded.answer(
        Method::Get,
        "/api/v2/torrents/info",
        200,
        &fixture("qbit_torrents.json"),
    );
}

/// After a restart the claims are the ones the last start stored. A download nobody claims is no
/// orphan until every arr was heard again: a downloads pass before that leaves the question open.
#[tokio::test]
async fn a_restart_calls_no_new_orphan_before_every_arr_is_heard() {
    let Some(t) = TestDb::new("sync_unsettled").await else {
        return;
    };
    let recorded = Recorded::new();
    record_sync_set(&recorded);
    let st = state_on(
        &t.pool,
        Journal::new(),
        &[
            ("RADARR_API_KEY", "test-key"),
            ("SONARR_API_KEY", "test-key"),
            ("QBIT_URL", QBIT),
            ("QBIT_PASS", "p"),
            ("SINGULARR_SYNC_JITTER_SECS", "0"),
        ],
        recorded.clone(),
    );
    let run = |key: &'static str| {
        let pool = t.pool.clone();
        async move {
            let ended = runtime::run_and_wait(&pool, key, serde_json::json!({}))
                .await
                .unwrap()
                .unwrap();
            assert!(ended.ok(), "{key}: {ended:?}");
        }
    };
    let (jobs, sync) = start_sync_jobs(&st).await;
    for key in [
        "radarr:queue",
        "sonarr:queue",
        "radarr:history",
        "sonarr:history",
        "qbittorrent:downloads",
    ] {
        run(key).await;
    }
    jobs.stop().await;
    sync.stop();
    let orphans = || async {
        sqlx::query_as::<_, (String, bool)>("SELECT hash, orphan FROM downloads ORDER BY hash")
            .fetch_all(&t.pool)
            .await
            .unwrap()
    };
    let settled = || async {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT counts->>'ownership_settled' FROM engine_state WHERE engine = 'qbittorrent'",
        )
        .fetch_one(&t.pool)
        .await
        .unwrap()
    };
    assert_eq!(settled().await.as_deref(), Some("true"));
    assert!(orphans().await.iter().any(|(_, orphan)| *orphan));

    // A torrent nobody claims comes while Singularr is down.
    let mut torrents: serde_json::Value =
        serde_json::from_str(&fixture("qbit_torrents.json")).unwrap();
    torrents.as_array_mut().unwrap().push(serde_json::json!({
        "hash": "ffffffffffffffffffffffffffffffffffffff06",
        "name": "Some.Other.Download.2026.1080p-GRP",
        "category": "radarr", "state": "downloading", "progress": 0.2, "size": 1000000000,
        "added_on": 1763800000, "completion_on": -1, "seeding_time": 0,
        "save_path": "/downloads/radarr", "ratio": 0, "tags": "",
        "dlspeed": 1000000, "eta": 800, "num_seeds": 3, "num_leechs": 1
    }));
    recorded.answer(
        Method::Get,
        "/api/v2/torrents/info",
        200,
        &torrents.to_string(),
    );
    // The arrs' passes wait: only the downloads run after the restart.
    sqlx::query("UPDATE job_schedules SET enabled = false WHERE key LIKE '%arr:%'")
        .execute(&t.pool)
        .await
        .unwrap();
    let (jobs, sync) = start_sync_jobs(&st).await;
    run("qbittorrent:downloads").await;
    assert_eq!(settled().await.as_deref(), Some("false"));
    let now = orphans().await;
    let new = now
        .iter()
        .find(|(hash, _)| hash.starts_with("FFFF"))
        .unwrap();
    assert!(!new.1, "no orphan before every arr is heard: {now:?}");
    jobs.stop().await;
    sync.stop();
    t.drop_db().await;
}

/// A queue read replaces the arr's queue rows: an item that left the queue claims nothing more.
#[tokio::test]
async fn a_download_that_leaves_the_queue_loses_its_claim() {
    let Some(t) = TestDb::new("claims_queue").await else {
        return;
    };
    let item = |id: &str, queue_id: i64| singularr::engines::QueueItem {
        queue_id,
        engine_title_id: Some(7),
        title: "A Movie".to_string(),
        download_id: Some(id.to_string()),
        status: Some("downloading".to_string()),
        tracked_state: Some("downloading".to_string()),
        tracked_status: Some("ok".to_string()),
        messages: vec!["One".to_string(), "Two".to_string()],
        indexer: Some("An Indexer".to_string()),
    };
    let ids = ["AAAA".to_string(), "BBBB".to_string()];
    let mut conn = t.pool.acquire().await.unwrap();
    claims::put_queue(&mut conn, "radarr", &[item("AAAA", 1), item("BBBB", 2)])
        .await
        .unwrap();
    let both = claims::of_ids(&t.pool, &["radarr".to_string()], &ids)
        .await
        .unwrap();
    assert_eq!(both.queues["radarr"].len(), 2);
    assert_eq!(both.queues["radarr"][0].messages, vec!["One", "Two"]);
    claims::put_queue(&mut conn, "radarr", &[item("BBBB", 2)])
        .await
        .unwrap();
    let left = claims::of_ids(&t.pool, &["radarr".to_string()], &ids)
        .await
        .unwrap();
    let held: Vec<&str> = left.queues["radarr"]
        .iter()
        .filter_map(|i| i.download_id.as_deref())
        .collect();
    assert_eq!(held, vec!["BBBB"]);
    drop(conn);
    t.drop_db().await;
}

/// The passes of the media servers and the Seerr sites are jobs of the runtime (B9c). The media
/// server's pass and a site's requests are listed with the ids and intervals of their loops. A
/// site's accounts and its discover lists have schedules of their own that are not listed, as the
/// requests loop read them. A media server that does not answer fails its job and writes a
/// failed pass row. The discover lists of a fresh marker are not read again: the next run comes
/// as they expire.
#[tokio::test]
async fn the_server_and_seerr_passes_run_as_jobs() {
    let Some(t) = TestDb::new("sync_jobs_sites").await else {
        return;
    };
    let recorded = Recorded::new();
    let requests: serde_json::Value =
        serde_json::from_str(&fixture("seerr_requests.json")).unwrap();
    // The job reads the requests in pages of 100, as the loop did.
    record_seerr(&recorded, &requests, 100);
    recorded.answer(
        Method::Get,
        &format!("{SEERR}/api/v1/user?take=100&skip=0"),
        200,
        &fixture("seerr_users.json"),
    );
    let st = state_on(
        &t.pool,
        Journal::new(),
        &[
            ("SEERR_API_KEY", "test-key"),
            ("PLEX_URL", PLEX),
            ("PLEX_TOKEN", "plex-token"),
            ("SINGULARR_SYNC_JITTER_SECS", "0"),
        ],
        recorded.clone(),
    );
    let schedules = runtime_jobs::schedules(&st.config, &st.settings, Some(&st.engines));
    let keys: Vec<(&str, &str, u64, bool)> = schedules
        .iter()
        .filter(|s| s.integration.is_some())
        .map(|s| {
            (
                s.key.as_str(),
                s.kind.as_str(),
                s.every.as_secs(),
                runtime_jobs::listed(&s.key).is_some(),
            )
        })
        .collect();
    assert_eq!(
        keys,
        vec![
            ("plex:server", "server.pass", 3600, true),
            ("plex:plays", "player.history", 900, true),
            ("seerr:requests", "requests.pass", 300, true),
            ("seerr:accounts", "requests.accounts", 300, false),
            ("seerr:discover", "requests.discover", 1800, false),
        ]
    );
    let passes = st.engines.passes(true);
    for pass in [("plex", "server"), ("seerr", "requests")] {
        assert!(passes.contains(&(pass.0.to_string(), pass.1)), "{pass:?}");
    }

    let (jobs, sync) = start_sync_jobs(&st).await;
    let run = |key: &'static str| {
        let pool = t.pool.clone();
        async move {
            runtime::run_and_wait(&pool, key, serde_json::json!({}))
                .await
                .unwrap()
                .unwrap()
        }
    };
    for key in ["seerr:requests", "seerr:accounts", "seerr:discover"] {
        let ended = run(key).await;
        assert!(ended.ok(), "{key}: {ended:?}");
    }
    let server = run("plex:server").await;
    assert!(!server.ok(), "{server:?}");
    assert_eq!(
        pass_rows(&t.pool).await,
        vec![
            ("plex".to_string(), "server".to_string(), false),
            ("seerr".to_string(), "requests".to_string(), true)
        ]
    );
    let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM seerr_users")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert!(accounts > 0);
    assert!(
        claims::marked_at(&t.pool, "seerr", "discover.at")
            .await
            .unwrap()
            .is_some()
    );
    // A second run within the lists' life reads nothing.
    let discover_calls = || {
        recorded
            .urls_seen()
            .iter()
            .filter(|u| u.contains("/api/v1/discover"))
            .count()
    };
    let before = discover_calls();
    assert!(before > 0);
    let again = run("seerr:discover").await;
    assert_eq!(again.detail.as_deref(), Some("Lists fresh"));
    let next_in: f64 = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM next_run_at - now())::float8 FROM job_schedules
         WHERE key = 'seerr:discover'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert!(
        (1700.0..=1800.0).contains(&next_in),
        "the next run comes as the lists expire: {next_in}"
    );
    assert_eq!(discover_calls(), before);

    let app = routes::router(st.clone());
    let owner: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, panel) = call(&app, "GET", "/api/v1/jobs", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{panel}");
    let listed: Vec<(String, u64, String)> = panel["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["integration"].is_string())
        .map(|j| {
            (
                j["id"].as_str().unwrap().to_string(),
                j["every"].as_u64().unwrap(),
                j["lastResult"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        vec![
            ("plex:server".to_string(), 3600, "failed".to_string()),
            ("seerr:requests".to_string(), 300, "ok".to_string()),
        ]
    );
    let (status, _, started) = call(
        &app,
        "POST",
        "/api/v1/integrations/seerr/sync",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{started}");
    assert_eq!(
        started["jobs"],
        serde_json::json!(["seerr:requests", "seerr:accounts", "seerr:discover"])
    );
    jobs.stop().await;
    sync.stop();
    assert_eq!(
        st.engines.forget_other_passes(&t.pool, true).await.unwrap(),
        0
    );
    t.drop_db().await;
}

/// The passes of the processing services, the processing rules and the intents are jobs of the
/// runtime (B9c). A service's pass and the rules are listed with the ids and intervals of their
/// loops. The intents are not. A whole read writes `ledger.whole_at`. A pass looks for stuck runs
/// when `stuck.at` is 300 s old or missing. A process that holds no libraries and flows of a worker
/// reads them with its first pass, though a whole read stands. The rules' first pass marks where
/// the journal stands in `rules.cursor`.
#[tokio::test]
async fn the_processing_and_intents_passes_run_as_jobs() {
    let Some(t) = TestDb::new("sync_jobs_processing").await else {
        return;
    };
    let recorded = Recorded::new();
    record_processing(&recorded, "hush_health_live.json");
    let st = state_on(
        &t.pool,
        Journal::new(),
        &[
            ("HUSH_URL", HUSH),
            ("TDARR_URL", TDARR),
            ("SINGULARR_SYNC_JITTER_SECS", "0"),
        ],
        recorded.clone(),
    );
    // The worker's whole read stands from before this process. Its first pass reads the newest
    // jobs alone. It reads the worker's libraries and flows all the same.
    let whole_before = chrono::Utc::now() - chrono::Duration::minutes(5);
    let mut conn = t.pool.acquire().await.unwrap();
    claims::mark(&mut conn, "tdarr", "ledger.whole_at", whole_before)
        .await
        .unwrap();
    drop(conn);
    let worker = st.engines.processor("tdarr").unwrap().clone();
    assert!(worker.setup().is_none());
    let (jobs, sync) = start_sync_jobs(&st).await;
    let run = |key: &'static str| {
        let pool = t.pool.clone();
        async move {
            let ended = runtime::run_and_wait(&pool, key, serde_json::json!({}))
                .await
                .unwrap()
                .unwrap();
            assert!(ended.ok(), "{key}: {ended:?}");
            ended
        }
    };
    for key in [
        "hush:processing",
        "tdarr:processing",
        "singularr:processing-rules",
        "singularr:intents",
    ] {
        run(key).await;
    }
    assert!(pass_rows(&t.pool).await.contains(&(
        "hush".to_string(),
        "processing".to_string(),
        true
    )));
    assert!(
        claims::marked_at(&t.pool, "hush", "ledger.whole_at")
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        worker.setup().map(|s| s.flows.len()),
        Some(4),
        "the first pass of the process read the worker's libraries and flows"
    );
    assert_eq!(
        claims::marked_at(&t.pool, "tdarr", "ledger.whole_at")
            .await
            .unwrap(),
        Some(whole_before),
        "the whole read still stands: the pass read the newest jobs"
    );
    // The pass looked for stuck runs: the bridge's old staged runs, each journaled once.
    let looked = claims::marked_at(&t.pool, "hush", "stuck.at")
        .await
        .unwrap()
        .expect("the pass looked for stuck runs");
    let stuck = || {
        let pool = t.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM events WHERE kind = 'processing_stuck'",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let journaled = stuck().await;
    assert!(journaled > 0);
    // Within 300 s the next pass does not look again.
    run("hush:processing").await;
    assert_eq!(
        claims::marked_at(&t.pool, "hush", "stuck.at")
            .await
            .unwrap(),
        Some(looked)
    );
    assert_eq!(stuck().await, journaled);
    let cursor: Option<i64> = singularr::processing::store::rules_cursor(&t.pool)
        .await
        .unwrap();
    assert!(cursor.is_some());
    let marker: serde_json::Value =
        sqlx::query_scalar("SELECT value FROM markers WHERE name = 'rules.cursor'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(marker, serde_json::json!({"id": cursor.unwrap()}));

    let app = routes::router(st.clone());
    let owner: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, panel) = call(&app, "GET", "/api/v1/jobs", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{panel}");
    let listed: Vec<(String, u64)> = panel["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| {
            (
                j["id"].as_str().unwrap().to_string(),
                j["every"].as_u64().unwrap(),
            )
        })
        .filter(|(id, _)| !id.starts_with("folder:"))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("hush:processing".to_string(), 60),
            ("singularr:processing-rules".to_string(), 60),
            ("tdarr:processing".to_string(), 60),
        ]
    );
    jobs.stop().await;
    sync.stop();
    t.drop_db().await;
}

/// A rule claims a file and enqueues its send's act in one commit: a row in `sending` that names
/// its act and has no detail. One claim holds a file, by any rule. A claim counts as sent: a run
/// after a restart or a lost lease never sends the file again. The rule's log asks for a look
/// while the act is unknown. A run that the claim's send could have opened confirms the act and
/// settles the claim as sent: a run of the claim's flow, started through the bridge's route by
/// Singularr and made from 60 s before the act to a day after it.
#[tokio::test]
async fn the_processing_rules_claim_a_file_before_they_send_it() {
    let Some(t) = TestDb::new("rules_claims").await else {
        return;
    };
    use singularr::processing::rules::{NewLog, RuleBody, RuleConditions};
    use singularr::processing::sends::{self, RuleSend};
    let journal = Journal::new();
    let st = state_with(&t.pool, journal.clone(), &[("HUSH_URL", HUSH)]);
    let mut conn = t.pool.acquire().await.unwrap();
    let rule = processing_store::insert_rule(
        &mut conn,
        &RuleBody {
            name: "Opus audio".to_string(),
            enabled: true,
            when: "import".to_string(),
            conditions: RuleConditions {
                file: serde_json::from_value(serde_json::json!({"audio_codec": ["Opus"]})).unwrap(),
                ..RuleConditions::default()
            },
            service: "hush".to_string(),
            flow: "audio".to_string(),
        },
        "user:owner@example.com",
    )
    .await
    .unwrap();
    drop(conn);
    let path = "/movies/A Movie (2020)/A Movie.mkv".to_string();
    let entry = NewLog {
        claimed: false,
        rule_id: rule.id,
        rule_version: rule.version,
        event_id: None,
        title_id: None,
        copy_id: Some(1),
        path: path.clone(),
        service: "hush".to_string(),
        flow: "audio".to_string(),
        outcome: "sending",
        detail: None,
        evidence: Some("audio is Opus".to_string()),
        run_id: None,
    };
    let title = singularr::catalog::store::TitleRef {
        id: 1,
        kind: "movie".to_string(),
        tmdb_id: None,
        title: "A Movie".to_string(),
        year: Some(2020),
    };
    let outbox = st.outbox();
    let mut tx = journal.begin(&t.pool).await.unwrap();
    let claim = processing_store::claim_send(tx.conn(), &entry)
        .await
        .unwrap()
        .expect("the file is free");
    let act = outbox
        .enqueue(
            &mut tx,
            sends::by_rule(RuleSend {
                rule: rule.id,
                copy: 1,
                claim,
                service: "hush",
                title: &title,
                path: &path,
                flow: "audio",
                event: None,
            }),
        )
        .await
        .unwrap()
        .into_act();
    processing_store::name_act(tx.conn(), claim, act.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (act.state.as_str(), act.idempotency_key.clone()),
        ("pending", format!("rule:{}:1:{claim}", rule.id))
    );
    // A second claim of the file, by any rule, does not take it.
    let mut tx = journal.begin(&t.pool).await.unwrap();
    assert_eq!(
        processing_store::claim_send(tx.conn(), &entry)
            .await
            .unwrap(),
        None
    );
    drop(tx);
    // The claim counts as sent: a pass after a restart skips the file.
    assert_eq!(
        processing_store::sent_paths(&t.pool, std::slice::from_ref(&path))
            .await
            .unwrap(),
        vec![path.clone()]
    );
    let app = routes::router(st.clone());
    let owner: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let newest = || {
        let app = app.clone();
        let at = format!("/api/v1/processing/rules/{}/log", rule.id);
        async move {
            let (status, _, page) = call(&app, "GET", &at, &owner, None).await;
            assert_eq!(status, StatusCode::OK, "{page}");
            page["entries"][0].clone()
        }
    };
    // No look while the act is pending.
    let row = newest().await;
    assert_eq!(
        (
            &row["outcome"],
            &row["attention"],
            &row["detail"],
            &row["act"]
        ),
        (
            &serde_json::json!("sending"),
            &serde_json::json!(false),
            &serde_json::Value::Null,
            &serde_json::json!(act.id)
        ),
        "{row}"
    );
    // Unknown, made three minutes ago: a look.
    sqlx::query(
        "UPDATE acts SET state = 'unknown', created_at = now() - interval '3 minutes'
         WHERE id = $1",
    )
    .bind(act.id)
    .execute(&t.pool)
    .await
    .unwrap();
    assert_eq!(newest().await["attention"], true);
    // Runs of the service whose source is the file. Each that the act's send could not have
    // opened leaves it Unconfirmed: one made long before the act, one of another flow, one the
    // bridge started on a tag, one made two days later.
    let run = |id: &'static str, flow: &'static str, trigger: &'static str, ago: f64| {
        let pool = t.pool.clone();
        let path = path.clone();
        async move {
            sqlx::query(
                "INSERT INTO processing_runs
                     (id, integration, job_id, flow, trigger_text, state, created_at,
                      source_path, raw)
                 VALUES ('hush:' || $1, 'hush', $1, $2, $3, 'staged',
                         now() - make_interval(secs => $4), $5, '{}')",
            )
            .bind(id)
            .bind(flow)
            .bind(trigger)
            .bind(ago)
            .bind(&path)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    for (id, flow, trigger, ago) in [
        ("j-before", "audio", "api:singularr", 600.0),
        ("j-other-flow", "hevc", "api:singularr", 60.0),
        ("j-tag", "audio", "tag:hush-audio", 60.0),
        ("j-late", "audio", "api:singularr", -172_800.0),
    ] {
        run(id, flow, trigger, ago).await;
        assert_eq!(
            sends::confirm_by_runs(&outbox, &t.pool, "hush")
                .await
                .unwrap(),
            0,
            "{id}"
        );
        let row = newest().await;
        assert_eq!(
            (&row["outcome"], &row["attention"]),
            (&serde_json::json!("sending"), &serde_json::json!(true)),
            "{id}: {row}"
        );
    }
    // A run of the claim's flow that Singularr's send started, 30 s after the act: the act's.
    run("j-after", "audio", "api:singularr", 150.0).await;
    assert_eq!(
        sends::confirm_by_runs(&outbox, &t.pool, "hush")
            .await
            .unwrap(),
        1
    );
    let row = newest().await;
    assert_eq!(
        (&row["outcome"], &row["run"], &row["attention"]),
        (
            &serde_json::json!("sent"),
            &serde_json::json!("hush:j-after"),
            &serde_json::json!(false)
        ),
        "{row}"
    );
    let confirmed = singularr::core::outbox::act(&t.pool, act.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (confirmed.state.as_str(), sends::confirmed_run(&confirmed)),
        ("confirmed", Some("hush:j-after"))
    );
    // The confirmed send is journaled in the rule's name.
    let by_rule = core_store::find_events(
        &t.pool,
        &["processing_sent".to_string()],
        &[format!("rule:{}", rule.id)],
        None,
        None,
        10,
    )
    .await
    .unwrap();
    assert_eq!(by_rule.len(), 1);
    assert_eq!(by_rule[0].body["run"], "hush:j-after");
    t.drop_db().await;
}

/// A rule's send that gets no clear answer keeps its claim: a connection that broke after the
/// call went out, a 502 without the bridge's own words. The file is not sent again. The
/// processing pass confirms the act and settles the claim once the bridge shows the run the send
/// opened. A send by hand of the file and the flow is that unknown act until the owner marks it
/// not sent. That frees the file. A refusal in the bridge's own words frees the file too. A pass
/// after one that was cut short writes no second entry and no second act for the claim it made.
#[tokio::test]
async fn a_send_without_a_clear_answer_keeps_its_claim() {
    let Some(t) = TestDb::new("rules_unclear").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::journal::event;
    use singularr::core::outbound::Access;
    use singularr::processing::rules::{self, NewLog, RuleBody, RuleConditions};
    use singularr::processing::sends::{self, RuleSend};
    let journal = Journal::new();
    let recorded = Recorded::new();
    record_processing(&recorded, "hush_health_live.json");
    let stage = format!("{HUSH}/stage");
    recorded.answer(
        Method::Post,
        &stage,
        201,
        &fixture("hush_stage_staged.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids=20261123T160500Z-0a1b2c3d"),
        200,
        &fixture("hush_jobs_by_id.json"),
    );
    let out = outbound(recorded.clone());
    let st = state_over(&t.pool, journal.clone(), &[("HUSH_URL", HUSH)], &out);
    let policy = ActivePolicy::builtin().policy;
    ledger_catalog(&t.pool, &journal, &recorded).await;
    let bridge = st.engines.processor("hush").unwrap().clone();
    processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    let owner = "user:owner@example.com";
    st.engines
        .access
        .set(&t.pool, &journal, owner, "hush", Access::Act)
        .await
        .unwrap();
    let acts = acts_runtime(&st);
    let outbox = st.outbox();
    let body = |when: &str| RuleBody {
        name: format!("Atmos audio on {when}"),
        enabled: true,
        when: when.to_string(),
        conditions: RuleConditions {
            file: serde_json::from_value(json!({"audio_codec": ["Eac3"], "atmos": true})).unwrap(),
            ..RuleConditions::default()
        },
        service: "hush".to_string(),
        flow: "audio".to_string(),
    };
    let mut conn = t.pool.acquire().await.unwrap();
    let asked = processing_store::insert_rule(&mut conn, &body("request"), owner)
        .await
        .unwrap();
    drop(conn);
    let run = || {
        let (pool, journal, engines, policy, asked, outbox) = (
            t.pool.clone(),
            journal.clone(),
            st.engines.clone(),
            policy.clone(),
            asked.clone(),
            outbox.clone(),
        );
        async move {
            let ran = rules::request_run(&pool, &journal, &engines, &policy, &asked, true, &outbox)
                .await
                .unwrap();
            acts_rest(&pool).await;
            ran
        }
    };
    let newest = |rule: i64| {
        let pool = t.pool.clone();
        async move {
            processing_store::rule_log(&pool, rule, 1)
                .await
                .unwrap()
                .remove(0)
        }
    };
    let clear = || {
        let pool = t.pool.clone();
        async move {
            for sql in [
                "DELETE FROM processing_rule_log",
                "DELETE FROM jobs WHERE queue = 'acts'",
                "DELETE FROM acts",
            ] {
                sqlx::query(sql).execute(&pool).await.unwrap();
            }
        }
    };

    // The connection breaks after the call went out: no clear answer, the claim stays.
    recorded.fail_once(Method::Post, &stage, "connection reset by peer");
    let ran = run().await;
    assert_eq!((ran.matched, ran.logged, ran.sent), (1, 1, 1));
    assert_eq!(staged_calls(&recorded), 1);
    let row = newest(asked.id).await;
    assert_eq!(
        (row.outcome.as_str(), row.detail.as_deref(), row.attention),
        ("sending", Some(rules::NO_CLEAR_ANSWER), true)
    );
    // The claim holds the file: the next run sends nothing.
    let again = run().await;
    assert_eq!((again.matched, again.logged, again.sent), (1, 0, 0));
    assert_eq!(staged_calls(&recorded), 1);

    // A 502 without the bridge's own words: the same.
    clear().await;
    recorded.answer_once(Method::Post, &stage, 502, "<html>Bad Gateway</html>", &[]);
    let ran = run().await;
    assert_eq!((ran.logged, ran.sent), (1, 1));
    assert_eq!(staged_calls(&recorded), 2);
    let claimed = newest(asked.id).await;
    assert_eq!(
        (claimed.outcome.as_str(), claimed.detail.as_deref()),
        ("sending", Some(rules::NO_CLEAR_ANSWER))
    );
    let kingdom_path = claimed.path.clone();
    // The run the send opened, made after the act: the processing pass confirms it and settles
    // the claim as sent.
    sqlx::query(
        "INSERT INTO processing_runs
             (id, integration, job_id, flow, trigger_text, state, created_at, source_path, raw)
         VALUES ('hush:j-after', 'hush', 'j-after', 'audio', 'api:singularr', 'staged', now(),
                 $1, '{}')",
    )
    .bind(&kingdom_path)
    .execute(&t.pool)
    .await
    .unwrap();
    processing::pass(&t.pool, &bridge, processing::Read::Newest)
        .await
        .unwrap();
    assert_eq!(
        sends::confirm_by_runs(&outbox, &t.pool, "hush")
            .await
            .unwrap(),
        1
    );
    let settled = newest(asked.id).await;
    assert_eq!(
        (
            settled.outcome.as_str(),
            settled.run_id.as_deref(),
            settled.detail.as_deref()
        ),
        ("sent", Some("hush:j-after"), None)
    );
    sqlx::query("DELETE FROM processing_runs WHERE id = 'hush:j-after'")
        .execute(&t.pool)
        .await
        .unwrap();

    // A send by hand of the file and the flow while the rule's act is unknown is that act: no
    // second call. The owner marks it not sent: the claim frees the file. A send by hand then
    // goes through.
    clear().await;
    recorded.fail_once(Method::Post, &stage, "connection reset by peer");
    run().await;
    let unclear = newest(asked.id).await;
    assert_eq!(unclear.outcome, "sending");
    let app = routes::router(st.clone());
    let by_hand = json!({"title": "movie-1495", "file": KINGDOM_FILE, "flow": "audio"}).to_string();
    let (status, _, joined) = send_click(&app, &by_hand).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{joined}");
    assert_eq!(joined["act"], json!(unclear.act_id.unwrap()));
    assert_eq!(staged_calls(&recorded), 3);
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let not_sent = format!("/api/v1/acts/{}/not-sent", unclear.act_id.unwrap());
    let (status, _, marked) = call(&app, "POST", &not_sent, &session, None).await;
    assert_eq!(status, StatusCode::OK, "{marked}");
    assert_eq!(
        (&marked["state"], &marked["errorClass"]),
        (&json!("failed"), &json!("by_hand"))
    );
    let freed = newest(asked.id).await;
    assert_eq!(
        (
            freed.outcome.as_str(),
            freed.detail.as_deref(),
            freed.attention
        ),
        ("failed", Some("Not sent"), false)
    );
    let (status, _, sent) = send_click(&app, &by_hand).await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_eq!(sent["run"]["id"], "hush:20261123T160500Z-0a1b2c3d");
    assert_eq!(staged_calls(&recorded), 4);

    // A refusal in the bridge's own words frees the file: the next run sends it.
    clear().await;
    recorded.answer_once(
        Method::Post,
        &stage,
        422,
        r#"{"staged": false, "reason": "refused", "detail": "hushq: busy"}"#,
        &[],
    );
    let ran = run().await;
    assert_eq!((ran.logged, ran.sent), (1, 1));
    let refused = newest(asked.id).await;
    assert_eq!(
        (refused.outcome.as_str(), refused.detail.as_deref()),
        ("refused", Some("hushq: busy"))
    );
    assert_eq!(run().await.sent, 1);
    let resent = newest(asked.id).await;
    assert_eq!(
        (resent.outcome.as_str(), resent.run_id.as_deref()),
        ("sent", Some("hush:20261123T160500Z-0a1b2c3d"))
    );
    assert_eq!(staged_calls(&recorded), 6);

    // A pass after one that was cut short reads the same import again. The claim of the same
    // rule and import is that pass's entry: no second entry, no second act. Its act is sent once.
    clear().await;
    let mut conn = t.pool.acquire().await.unwrap();
    let on_import = processing_store::insert_rule(&mut conn, &body("import"), owner)
        .await
        .unwrap();
    drop(conn);
    let pass = || rules::import_pass(&t.pool, &journal, &st.engines, &policy, true, &outbox);
    pass().await.unwrap();
    let kingdom: i64 = sqlx::query_scalar("SELECT id FROM copies WHERE relative_path = $1")
        .bind(KINGDOM_FILE)
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let mut tx = journal.begin(&t.pool).await.unwrap();
    tx.append_one(event(
        "copy_added",
        "copy",
        kingdom,
        "sync:radarr",
        json!({}),
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let imported: i64 = sqlx::query_scalar("SELECT max(id) FROM events WHERE kind = 'copy_added'")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let cut_short = NewLog {
        claimed: false,
        rule_id: on_import.id,
        rule_version: on_import.version,
        event_id: Some(imported),
        title_id: None,
        copy_id: Some(kingdom),
        path: kingdom_path.clone(),
        service: "hush".to_string(),
        flow: "audio".to_string(),
        outcome: "sending",
        detail: None,
        evidence: None,
        run_id: None,
    };
    let title =
        singularr::catalog::store::title_refs(&t.pool, &[title_with_tmdb(&t.pool, 1495).await])
            .await
            .unwrap()
            .remove(0);
    let mut tx = journal.begin(&t.pool).await.unwrap();
    let claim = processing_store::claim_send(tx.conn(), &cut_short)
        .await
        .unwrap()
        .expect("the file is free");
    let act = outbox
        .enqueue(
            &mut tx,
            sends::by_rule(RuleSend {
                rule: on_import.id,
                copy: kingdom,
                claim,
                service: "hush",
                title: &title,
                path: &kingdom_path,
                flow: "audio",
                event: Some(imported),
            }),
        )
        .await
        .unwrap()
        .into_act();
    processing_store::name_act(tx.conn(), claim, act.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let passed = pass().await.unwrap();
    assert_eq!((passed.imports, passed.logged, passed.sent), (1, 0, 0));
    acts_rest(&t.pool).await;
    let log = processing_store::rule_log(&t.pool, on_import.id, 10)
        .await
        .unwrap();
    assert_eq!(
        log.iter().map(|e| e.outcome.as_str()).collect::<Vec<_>>(),
        vec!["sent"]
    );
    let acts_of_file: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acts WHERE request->>'path' = $1")
            .bind(&kingdom_path)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(acts_of_file, 1);
    assert_eq!(staged_calls(&recorded), 7);
    acts.stop().await;
    t.drop_db().await;
}

/// A notice that comes while the intents' job runs asks for one run after it: a notice late in a
/// run does not wait for the schedule. More notices in that run ask for nothing more.
#[tokio::test]
async fn a_notice_during_an_intents_run_asks_for_one_run_after_it() {
    let Some(t) = TestDb::new("intents_follow").await else {
        return;
    };
    use singularr::requests::intents;
    let st = state_with(&t.pool, Journal::new(), &[]);
    runtime::Runtime::reconcile(
        &t.pool,
        &runtime_jobs::schedules(&st.config, &st.settings, None),
    )
    .await
    .unwrap();
    // A run of the intents' job is on.
    let first = runtime::run_after(&t.pool, intents::JOB, serde_json::json!({}))
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "UPDATE jobs SET state = 'running', locked_by = 'test', started_at = now(),
                locked_until = now() + interval '1 minute'
         WHERE id = $1",
    )
    .bind(first)
    .execute(&t.pool)
    .await
    .unwrap();
    let queued = || {
        let pool = t.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "SELECT id FROM jobs WHERE schedule_key = $1 AND state = 'queued'",
            )
            .bind(intents::JOB)
            .fetch_all(&pool)
            .await
            .unwrap()
        }
    };
    let trigger = intents::Trigger::new(t.pool.clone());
    let follow = trigger
        .notice()
        .await
        .expect("a run is on: one run after it is asked for");
    assert!(
        trigger.notice().await.is_none(),
        "one run after it, whatever the notices"
    );
    assert!(
        queued().await.is_empty(),
        "nothing queued while the run is on"
    );
    // The run ends: the run after it is queued.
    sqlx::query(
        "UPDATE jobs SET state = 'done', locked_by = NULL, locked_until = NULL,
                finished_at = now()
         WHERE id = $1",
    )
    .bind(first)
    .execute(&t.pool)
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), follow)
        .await
        .expect("the run after it is asked for once the run ends")
        .unwrap();
    let next = queued().await;
    assert_eq!(next.len(), 1);
    assert!(next[0] > first);
    t.drop_db().await;
}

/// One integration as `GET /api/v1/integrations` lists it.
async fn listed_integration(
    app: &axum::Router,
    authorization: &str,
    id: &str,
) -> serde_json::Value {
    let (status, _, listed) = call(
        app,
        "GET",
        "/api/v1/integrations",
        &[("authorization", authorization)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    listed["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no integration {id} in {listed}"))
}

/// Access per action: on one manager grab is allowed and remove refused. Each action that changes
/// is one journal event. A value saved before actions had rows reads as before: every action of
/// the type acts, now as one row per action.
#[tokio::test]
async fn each_action_of_a_manager_has_its_own_access() {
    use singularr::core::outbound::{Class, OutboundError};
    let Some(t) = TestDb::new("access_per_action").await else {
        return;
    };
    let journal = Journal::new();
    sqlx::query(
        "INSERT INTO integration_access (integration, action, access, saved_by)
         VALUES ('sonarr', '*', 'act', 'user:owner@example.com')",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let mut allow = Allowlist::with_metadata_providers();
    for (id, base, kind) in [
        ("radarr", RADARR, ArrKind::Radarr),
        ("sonarr", SONARR, ArrKind::Sonarr),
    ] {
        allow
            .allow_integration(id, base, singularr::engines::arr::calls(kind))
            .unwrap();
    }
    let out = Outbound::with_transport(allow, Recorded::new());
    let st = state_over(
        &t.pool,
        journal.clone(),
        &[("RADARR_API_KEY", "a-key"), ("SONARR_API_KEY", "a-key")],
        &out,
    );
    st.engines.access.load(&t.pool).await.unwrap();
    let sonarr = st.engines.access.of("sonarr");
    assert_eq!(sonarr.acting, ["search", "grab", "import", "remove"]);
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT action, access FROM integration_access WHERE integration = 'sonarr' ORDER BY action",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 4, "{rows:?}");
    assert!(
        rows.iter()
            .all(|(action, access)| action != "*" && access == "act")
    );

    // Radarr through the route: grab acts, remove stays read.
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let (status, _, body) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/radarr",
        &session,
        Some(r#"{"actions": {"grab": "act", "remove": "read"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        kinds(&t.pool, before).await,
        vec!["integration_access_changed"],
        "remove was read already"
    );
    let release = format!("{RADARR}/api/v3/release");
    let grab = out
        .check(
            Method::Post,
            &release,
            &Sent::Json(r#"{"guid": "g", "indexerId": 1}"#.into()),
        )
        .unwrap();
    assert_eq!(grab.class, Class::Act("grab"));
    let remove = format!("{RADARR}/api/v3/queue/5?removeFromClient=true&blocklist=false");
    assert!(matches!(
        out.check(Method::Delete, &remove, &Sent::None),
        Err(OutboundError::NotAllowed { .. })
    ));
    let actions = body["actions"].as_array().unwrap().clone();
    let of = |id: &str| {
        actions
            .iter()
            .find(|a| a["id"] == id)
            .map(|a| a["access"].clone())
    };
    assert_eq!(
        (of("grab"), of("remove")),
        (Some("act".into()), Some("read".into()))
    );

    // The word of the whole integration: one event for each of the three that change.
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/radarr",
        &session,
        Some(r#"{"access": "act"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kinds(&t.pool, before).await.len(), 3);
    assert!(out.check(Method::Delete, &remove, &Sent::None).is_ok());
    // An action the type does not have is refused.
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/radarr",
        &session,
        Some(r#"{"actions": {"scan": "act"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    t.drop_db().await;
}

/// Consent: an `act` of the environment covers every action of its type. That includes an
/// action a later version adds. A start names each action it covers that the last start did not
/// record. The first start records the actions and names none. An access the owner saved covers
/// the actions it names alone. It is never named.
#[tokio::test]
async fn an_act_of_the_environment_names_the_actions_new_since_the_last_start() {
    let Some(t) = TestDb::new("access_declared").await else {
        return;
    };
    let out = outbound(Recorded::new());
    let st = state_over(
        &t.pool,
        Journal::new(),
        &[
            ("RADARR_API_KEY", "a-key"),
            ("RADARR_ACCESS", "act"),
            ("SONARR_API_KEY", "a-key"),
        ],
        &out,
    );
    let book = &st.engines.access;
    assert!(book.note_declared(&t.pool).await.unwrap().is_empty());
    assert!(
        book.note_declared(&t.pool).await.unwrap().is_empty(),
        "nothing is new"
    );
    let recorded: Vec<(String, String)> = sqlx::query_as(
        "SELECT integration, action FROM integration_actions ORDER BY integration, action",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(recorded.len(), 8, "{recorded:?}");

    // The last start knew three of Radarr's four actions and two of Sonarr's.
    sqlx::query(
        "DELETE FROM integration_actions
         WHERE (integration, action) IN (('radarr', 'remove'), ('sonarr', 'import'), ('sonarr', 'remove'))",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        book.note_declared(&t.pool).await.unwrap(),
        vec![
            "RADARR_ACCESS=act gives Radarr write access to an action the last start did not declare: Remove a download (`remove`)"
                .to_string()
        ],
        "Sonarr's access is nobody's act"
    );
    assert!(book.note_declared(&t.pool).await.unwrap().is_empty());
    t.drop_db().await;
}

fn state(pool: &PgPool, journal: Journal) -> AppState {
    state_with(pool, journal, &[])
}

/// The application state over a test database, with more variables set.
/// The jobs of the runtime over a test's state, started: the folder scans, the index and the
/// dispatcher. The runtime has a pool of its own, as at boot.
async fn start_jobs(st: &AppState) -> Jobs {
    start_jobs_with(st, None).await
}

/// The jobs of the runtime over a test's state with the sync's passes: those of its managers and
/// download clients. The stored copies are assessed under the built-in policy.
async fn start_sync_jobs(st: &AppState) -> (Jobs, SyncHandle) {
    let handle = lifecycle_sync::spawn(
        SyncDeps {
            config: st.config.sync.clone(),
            pool: st.pool.clone(),
            journal: st.journal.clone(),
            engines: st.engines.clone(),
            assess: Arc::new(Assessor::new(
                Arc::new(ActivePolicy::builtin()),
                "test-build",
            )),
            policy_event: None,
        },
        request_rules(),
    );
    let jobs = start_jobs_with(st, Some(handle.jobs())).await;
    (jobs, handle)
}

/// [`start_jobs`] with the sync's passes when `sync` gives them.
async fn start_jobs_with(st: &AppState, sync: Option<Arc<lifecycle_sync::SyncJobs>>) -> Jobs {
    let engines = sync.as_ref().map(|s| s.deps.engines.clone());
    let runtime = PgPoolOptions::new()
        .max_connections(runtime_jobs::connections(engines.as_deref()))
        .connect_lazy_with(st.pool.connect_options().as_ref().clone());
    runtime_jobs::start(
        &st.config,
        JobsDeps {
            runtime,
            pool: st.pool.clone(),
            journal: st.journal.clone(),
            settings: st.settings.clone(),
            editions: Arc::new(singularr_core::EditionList::shipped().clone()),
            music: st.music.clone(),
            home_movies: st.home_movies.clone(),
            search: st.search.clone(),
            metadata: st.metadata.clone(),
            notify: st.notify.clone(),
            describe: Arc::new(singularr::api::notifications::Describer::new(
                st.pool.clone(),
                &st.config,
                st.settings.clone(),
            )),
            engines: st.engines.clone(),
            // The plays jobs have their own tests (`plays_db.rs`): their schedules are left out.
            plays: None,
            locations: None,
            processing: sync
                .as_ref()
                .map(|_| singularr::processing::service::ProcessingDeps {
                    pool: st.pool.clone(),
                    journal: st.journal.clone(),
                    engines: st.engines.clone(),
                    interval: st.config.sync.processing,
                    jitter: st.config.sync.jitter,
                    settings: st.settings.clone(),
                    policy: st.policy.clone(),
                }),
            sync,
            intents: request_rules(),
        },
    )
    .await
    .unwrap()
}

fn state_with(pool: &PgPool, journal: Journal, extra: &[(&str, &str)]) -> AppState {
    state_on(pool, journal, extra, Recorded::new())
}

/// The application state with engines and providers that answer from `recorded`.
fn state_on(
    pool: &PgPool,
    journal: Journal,
    extra: &[(&str, &str)],
    recorded: Arc<Recorded>,
) -> AppState {
    state_over(pool, journal, extra, &outbound(recorded))
}

/// The application state with engines and providers that call through `out`: the test keeps
/// the way out, and with it the allowlist the state's engines are held to.
fn state_over(pool: &PgPool, journal: Journal, extra: &[(&str, &str)], out: &Outbound) -> AppState {
    let mut env = BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("SINGULARR_OWNER_EMAILS", "owner@example.com"),
        // Listed without keys, so none is asked.
        ("RADARR_URL", RADARR),
        ("SONARR_URL", SONARR),
        ("QBIT_USER", "u"),
        ("SEERR_URL", SEERR),
    ]);
    env.extend(extra.iter().copied());
    let config = Config::from(&env).unwrap();
    AppState::new(Inner {
        engines: Arc::new(EngineSet::from_config(&config, out)),
        metadata: Arc::new(ProviderSet::from_config(&config, out)),
        owners: singularr::core::identity::Allowlist::parse(&config.bootstrap.owner_emails),
        requests: Arc::new(Requests::new(&config.request_rules)),
        music: Arc::new(MusicLibrary::new(&config.music.root)),
        home_movies: Arc::new(HomeMovies::new(&config.home_movies.root)),
        settings: Arc::new(singularr::settings::Settings::new(&config, &env, None)),
        pages: Default::default(),
        search: Arc::new(singularr::search::Search::new(pool.clone(), &config, out)),
        notify: singularr::notify::service::Notifier::new(
            pool.clone(),
            journal.clone(),
            out.clone(),
            singularr::core::smtp::recorded::Recorded::new(),
        ),
        config,
        pool: pool.clone(),
        journal,
        policy: policy::PolicyCell::handle(ActivePolicy::builtin()),
    })
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> (StatusCode, String, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    for (n, v) in headers {
        req = req.header(*n, *v);
    }
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(res.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, content_type, json)
}

#[tokio::test]
async fn the_api_asks_for_a_caller_and_answers_in_problems() {
    let Some(t) = TestDb::new("api").await else {
        return;
    };
    let journal = Journal::new();
    let state = state(&t.pool, journal.clone());
    let app = routes::router(state);
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let reader = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "reader".into(),
            scopes: vec![Scope::Read],
        },
    )
    .await
    .unwrap();
    let writer = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "settings-only".into(),
            scopes: vec![Scope::Settings],
        },
    )
    .await
    .unwrap();
    let read = format!("Bearer {}", reader.secret);
    let no_read = format!("Bearer {}", writer.secret);

    // Health is open; everything else under /api/v1 is not.
    let (status, _, health) = call(&app, "GET", "/api/v1/health", &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(health["database"], true);
    // Radarr, Sonarr, qBittorrent and Seerr.
    assert_eq!(health["engines"].as_array().unwrap().len(), 4);
    let (status, _, listed) = call(
        &app,
        "GET",
        "/api/v1/integrations",
        &[("authorization", read.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{listed}");
    // The configured integrations, then the metadata providers.
    let listed = listed["integrations"].as_array().unwrap().clone();
    assert_eq!(
        listed.iter().filter(|i| i["group"] != "metadata").count(),
        4
    );
    assert_eq!(listed[0]["type"], "radarr");
    assert_eq!(listed[0]["state"], "warn");
    assert!(
        listed[0]["detail"]
            .as_str()
            .unwrap()
            .contains("RADARR_API_KEY")
    );
    let (status, _, _) = call(&app, "GET", "/api/v1/integrations", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    for path in [
        "/api/v1/titles",
        "/api/v1/libraries",
        "/api/v1/downloads",
        "/api/v1/policy",
        "/api/v1/events",
        "/api/v1/events/stream",
        "/api/v1/keys",
        "/api/v1/openapi.json",
        "/api/v1/whoami",
        "/api/v1/no-such-route",
    ] {
        let (status, content_type, problem) = call(&app, "GET", path, &[], None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(content_type, "application/problem+json", "{path}");
        assert_eq!(problem["code"], "singularr.unauthorized", "{path}");
    }
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/v1/releases/parse",
        &[],
        Some("{\"name\":\"x\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/titles",
        &[("authorization", "Bearer mq_wrongwrongwrongwrong")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // A key without `read` is known but not allowed; a signed-in stranger likewise.
    let (status, _, problem) = call(
        &app,
        "GET",
        "/api/v1/titles",
        &[("authorization", &no_read)],
        None,
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("singularr.forbidden"))
    );
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/titles",
        &[("x-auth-request-email", "guest@example.com")],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The owner's session and a read key both read.
    let (status, _, who) = call(
        &app,
        "GET",
        "/api/v1/whoami",
        &[("x-auth-request-email", "Owner@Example.com")],
        None,
    )
    .await;
    assert_eq!(
        (status, who["kind"].as_str()),
        (StatusCode::OK, Some("owner"))
    );
    let (status, _, who) = call(
        &app,
        "GET",
        "/api/v1/whoami",
        &[("authorization", &read)],
        None,
    )
    .await;
    assert_eq!(
        (status, who["name"].as_str()),
        (StatusCode::OK, Some("reader"))
    );
    let auth_read: [(&str, &str); 1] = [("authorization", &read)];
    let (status, _, titles) = call(
        &app,
        "GET",
        "/api/v1/titles?per_page=5000&page=0",
        &auth_read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (
            titles["page"].as_i64(),
            titles["per_page"].as_i64(),
            titles["total"].as_i64()
        ),
        (Some(1), Some(200), Some(0))
    );
    for path in [
        "/api/v1/libraries",
        "/api/v1/overview",
        "/api/v1/downloads",
        "/api/v1/downloads/orphans",
        "/api/v1/policy",
        "/api/v1/metadata/providers",
        "/api/v1/events",
        "/api/v1/releases/shadow",
        "/api/v1/openapi.json",
    ] {
        let (status, _, body) = call(&app, "GET", path, &auth_read, None).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
    }
    let (_, _, policy) = call(&app, "GET", "/api/v1/policy", &auth_read, None).await;
    assert_eq!(policy["source"], "builtin");
    assert_eq!(policy["policy"]["lanes"].as_array().unwrap().len(), 4);

    // What the interface design asked for beyond the first route list.
    assert!(health["engines"][0]["latency_ms_history"].is_array());
    for path in [
        "/api/v1/titles?has_uhd=true&edition=IMAX&sort=score&order=asc",
        "/api/v1/titles?sort=added&order=desc",
        "/api/v1/releases/shadow?disagree=true",
        "/api/v1/events?subject=title:1",
        "/api/v1/downloads",
        "/api/v1/downloads/orphans",
        "/api/v1/libraries",
        "/api/v1/metadata/providers",
    ] {
        let (status, _, body) = call(&app, "GET", path, &auth_read, None).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        assert!(
            body["total"].is_i64() || body["rows"]["total"].is_i64(),
            "{path} gives a total"
        );
    }
    let (_, _, about_key) = call(
        &app,
        "GET",
        &format!("/api/v1/events?subject=key:{}", reader.key.id),
        &auth_read,
        None,
    )
    .await;
    assert_eq!(about_key["total"], 1);
    assert_eq!(about_key["items"][0]["kind"], "key_created");
    let (status, _, _) = call(&app, "GET", "/api/v1/titles?sort=rating", &auth_read, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Parse and score.
    let (status, _, parsed) = call(
        &app,
        "POST",
        "/api/v1/releases/parse",
        &auth_read,
        Some("{\"name\":\"Movie.2004.1080p.BluRay.x265-QxR\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parsed["quality"], "Bluray-1080p");
    assert!(
        parsed["parsed"]["notes"]
            .as_array()
            .is_some_and(|n| !n.is_empty())
    );
    let (status, _, scored) = call(
        &app,
        "POST",
        "/api/v1/releases/score",
        &auth_read,
        Some("{\"name\":\"Movie.2004.1080p.BluRay.x265-QxR\",\"lane\":\"movies-hd\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(scored["verdict"]["score"], 100);
    assert_eq!(scored["decision"]["action"], "Grab");
    assert_eq!(
        scored["verdict"]["ladder_steps"], 5,
        "movies-hd has five steps"
    );
    assert_eq!(scored["verdict"]["rank"], 1);
    let (status, _, sim) = call(
        &app,
        "POST",
        "/api/v1/policy/simulate",
        &auth_read,
        Some(&serde_json::json!({ "policy": BUILTIN }).to_string()),
    )
    .await;
    assert_eq!((status, sim["changed"].as_i64()), (StatusCode::OK, Some(0)));

    // Malformed requests are problems too, not plain text.
    for (method, path, body) in [
        ("POST", "/api/v1/releases/parse", Some("{not json")),
        ("POST", "/api/v1/releases/parse", Some("{\"name\":\"\"}")),
        (
            "POST",
            "/api/v1/releases/score",
            Some("{\"name\":\"x\",\"lane\":\"no-such-lane\"}"),
        ),
        (
            "POST",
            "/api/v1/policy/simulate",
            Some("{\"policy\":\"this is not toml =\"}"),
        ),
        (
            "POST",
            "/api/v1/releases/shadow/parse",
            Some("{\"name\":\"x\",\"arr\":\"lidarr\"}"),
        ),
        ("GET", "/api/v1/titles?page=minus-one", None),
        ("GET", "/api/v1/titles?kind=album", None),
        ("GET", "/api/v1/releases/shadow?disagree=sometimes", None),
        ("GET", "/api/v1/metadata/compare", None),
        ("GET", "/api/v1/titles/abc", None),
    ] {
        let (status, content_type, problem) = call(&app, method, path, &auth_read, body).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{method} {path}: {problem}"
        );
        assert_eq!(content_type, "application/problem+json", "{method} {path}");
        assert!(problem["detail"].is_string(), "{method} {path}");
    }
    let (status, _, problem) = call(&app, "GET", "/api/v1/titles/424242", &auth_read, None).await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("singularr.not_found"))
    );
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/copies/424242/verdict",
        &auth_read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // An arr that is not configured cannot be asked: a gateway problem, not a crash.
    let (status, _, problem) = call(
        &app,
        "POST",
        "/api/v1/releases/shadow/parse",
        &auth_read,
        Some("{\"name\":\"x\",\"arr\":\"radarr\"}"),
    )
    .await;
    assert_eq!(
        (status, problem["code"].as_str()),
        (StatusCode::BAD_GATEWAY, Some("singularr.upstream"))
    );

    // Key management: not for a read key; the owner creates and revokes.
    let (status, _, _) = call(&app, "GET", "/api/v1/keys", &auth_read, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/v1/keys",
        &auth_read,
        Some("{\"name\":\"x\",\"scopes\":[\"admin\"]}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, created) = call(
        &app,
        "POST",
        "/api/v1/keys",
        &session,
        Some("{\"name\":\"ci\",\"scopes\":[\"read\",\"admin\"]}"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let secret = created["secret"].as_str().unwrap().to_string();
    let admin = format!("Bearer {secret}");
    let (status, _, keys) = call(
        &app,
        "GET",
        "/api/v1/keys",
        &[("authorization", &admin)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(keys["items"].as_array().unwrap().len(), 3);
    assert_eq!(keys["total"], 3);
    assert!(
        !keys.to_string().contains(&secret),
        "the list never shows a secret"
    );
    let id = created["key"]["id"].as_i64().unwrap();
    let (status, _, _) = call(
        &app,
        "DELETE",
        &format!("/api/v1/keys/{id}"),
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/keys",
        &[("authorization", &admin)],
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "the revoked key stops working at once"
    );

    // The journal holds the key events and filters by kind.
    let (_, _, events) = call(
        &app,
        "GET",
        "/api/v1/events?kind=key_created,key_revoked&order=asc",
        &auth_read,
        None,
    )
    .await;
    assert_eq!(events["total"], 4);
    assert_eq!(events["items"][0]["kind"], "key_created");
    assert_eq!(events["items"][3]["kind"], "key_revoked");
    assert_eq!(events["items"][3]["actor"], "user:owner@example.com");

    // The reference page is public and self-contained.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/docs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    t.drop_db().await;
}

/// One answer as it left the server: its status, its headers and its bytes.
async fn answer_of(
    app: &axum::Router,
    path: &str,
    headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(path);
    for (n, v) in headers {
        req = req.header(*n, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = axum::body::to_bytes(res.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap();
    (status, headers, bytes.to_vec())
}

#[tokio::test]
async fn a_long_answer_leaves_compressed_for_a_caller_that_takes_that() {
    use futures::StreamExt;
    use std::io::Read;
    let Some(t) = TestDb::new("compress").await else {
        return;
    };
    let app = routes::router(state(&t.pool, Journal::new()));
    let owner = ("x-auth-request-email", "owner@example.com");
    let long = "/api/v1/openapi.json";

    // The bytes as they are for a caller that does not ask.
    let (status, headers, plain) = answer_of(&app, long, &[owner]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("content-encoding").is_none(), "{headers:?}");
    assert!(plain.len() > 50_000, "{} bytes", plain.len());
    let document: serde_json::Value = serde_json::from_slice(&plain).unwrap();
    assert!(document["paths"].is_object());
    let (_, headers, same) = answer_of(&app, long, &[owner, ("accept-encoding", "identity")]).await;
    assert!(headers.get("content-encoding").is_none(), "{headers:?}");
    assert_eq!(same, plain);

    // gzip for a caller that takes it.
    let (status, headers, packed) =
        answer_of(&app, long, &[owner, ("accept-encoding", "gzip")]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["content-encoding"], "gzip", "{headers:?}");
    assert!(
        headers["vary"]
            .to_str()
            .unwrap()
            .eq_ignore_ascii_case("accept-encoding")
    );
    assert_eq!(headers["content-type"], "application/json");
    assert!(packed.len() * 4 < plain.len(), "{} bytes", packed.len());
    let mut back = Vec::new();
    flate2::read::GzDecoder::new(&packed[..])
        .read_to_end(&mut back)
        .unwrap();
    assert_eq!(back, plain);

    // Brotli for a browser, which takes both.
    let (_, headers, packed) = answer_of(
        &app,
        long,
        &[owner, ("accept-encoding", "gzip, deflate, br, zstd")],
    )
    .await;
    assert_eq!(headers["content-encoding"], "br", "{headers:?}");
    assert!(packed.len() * 4 < plain.len(), "{} bytes", packed.len());
    let mut back = Vec::new();
    brotli::Decompressor::new(&packed[..], 4096)
        .read_to_end(&mut back)
        .unwrap();
    assert_eq!(back, plain);

    // A short answer gains nothing and travels as it is; so does a problem.
    let takes_both = ("accept-encoding", "gzip, br");
    let (status, headers, short) = answer_of(&app, "/api/v1/whoami", &[owner, takes_both]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("content-encoding").is_none(), "{headers:?}");
    let who: serde_json::Value = serde_json::from_slice(&short).unwrap();
    assert_eq!(who["kind"], "owner", "{who}");
    let (status, headers, problem) = answer_of(&app, "/api/v1/whoami", &[takes_both]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(headers.get("content-encoding").is_none(), "{headers:?}");
    let problem: serde_json::Value = serde_json::from_slice(&problem).unwrap();
    assert_eq!(problem["code"], "singularr.unauthorized");

    // The event stream is never compressed: its frames leave as they are written.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/events/stream")
                .header(owner.0, owner.1)
                .header(takes_both.0, takes_both.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    assert!(
        res.headers().get("content-encoding").is_none(),
        "{:?}",
        res.headers()
    );
    let mut frames = res.into_body().into_data_stream();
    let first = tokio::time::timeout(Duration::from_secs(10), frames.next())
        .await
        .expect("the stream opens at once")
        .expect("a first frame")
        .unwrap();
    let first = String::from_utf8(first.to_vec()).expect("the frame is text, not an encoding");
    assert!(first.starts_with("event: connected"), "{first}");
    t.drop_db().await;
}

// Requests, discover, music and home movies -------------------------------------------------------

/// A folder of its own under the temporary folder, removed when the test ends.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new(tag: &str) -> Folder {
        let dir = std::env::temp_dir().join(format!("mq-db-{tag}-{}", uuid_like()));
        std::fs::create_dir_all(&dir).unwrap();
        Folder(dir)
    }

    fn file(&self, rel: &str, bytes: usize) {
        let path = self.0.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![b'x'; bytes]).unwrap();
    }

    fn root(&self) -> String {
        self.0.display().to_string()
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Record a Seerr site's answers: status, counts, the request list in pages of `page`, the
/// title lookups and the discover lists.
fn record_seerr(recorded: &Recorded, requests: &serde_json::Value, page: usize) {
    let at = |path: &str| format!("{SEERR}/api/v1{path}");
    recorded.answer(
        Method::Get,
        &at("/status"),
        200,
        &fixture("seerr_status.json"),
    );
    recorded.answer(
        Method::Get,
        &at("/request/count"),
        200,
        &fixture("seerr_request_count.json"),
    );
    let results = requests["results"].as_array().unwrap();
    for (i, chunk) in results.chunks(page).enumerate() {
        let body = serde_json::json!({
            "pageInfo": { "results": results.len(), "page": i + 1 },
            "results": chunk,
        });
        recorded.answer(
            Method::Get,
            &at(&format!("/request?take={page}&skip={}", i * page)),
            200,
            &body.to_string(),
        );
    }
    for name in ["seerr_media.json", "seerr_discover.json"] {
        let answers: serde_json::Value = serde_json::from_str(&fixture(name)).unwrap();
        for (path, body) in answers.as_object().unwrap() {
            recorded.answer(
                Method::Get,
                &at(&format!("/{path}")),
                200,
                &body.to_string(),
            );
        }
    }
}

fn seerr_site(out: &Outbound) -> Arc<dyn RequestSite> {
    let config = SeerrConfig {
        id: "seerr".to_string(),
        url: SEERR.to_string(),
        api_key: Some("test-key".to_string()),
    };
    Arc::new(Seerr::new(&config, out.clone()).unwrap())
}

/// The catalog of the recorded Radarr and Sonarr: 10 Cloverfield Lane and Prisoner of Azkaban
/// with a file, A Movie Not Yet Held without one, and the shows 1883 and Shoresy.
async fn recorded_catalog(pool: &PgPool, journal: &Journal, recorded: &Arc<Recorded>) {
    record_radarr(
        recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    record_sonarr(recorded, &fixture("sonarr_episodefile.json"));
    let out = outbound(recorded.clone());
    let radarr: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let sonarr: Arc<dyn LibraryEngine> = Arc::new(Sonarr(arr(
        "sonarr",
        ArrKind::Sonarr,
        "sonarr",
        SONARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![radarr.clone(), sonarr.clone()],
        None,
        slots(&[
            ("radarr", "radarr", Some("radarr")),
            ("sonarr", "sonarr", Some("sonarr")),
        ]),
    ));
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(pool, journal, engines, &policy);
    titles_pass(&deps, &radarr, true).await.unwrap();
    titles_pass(&deps, &sonarr, true).await.unwrap();
}

async fn title_with_tmdb(pool: &PgPool, tmdb_id: i64) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT id FROM titles WHERE tmdb_id = $1")
        .bind(tmdb_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn title_with_tvdb(pool: &PgPool, tvdb_id: i64) -> i64 {
    sqlx::query_scalar::<_, i64>("SELECT id FROM titles WHERE tvdb_id = $1")
        .bind(tvdb_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn request_rules() -> Rules {
    let config = Config::from(&BTreeMap::from([(
        "DATABASE_URL",
        "postgres://x@127.0.0.1:1/x",
    )]))
    .unwrap();
    Requests::new(&config.request_rules).rules
}

fn lookups(recorded: &Recorded) -> usize {
    recorded
        .urls_seen()
        .iter()
        .filter(|u| u.contains("/api/v1/movie/") || u.contains("/api/v1/tv/"))
        .count()
}

#[tokio::test]
async fn seerr_requests_become_rows_with_the_gate_and_the_cleanup() {
    let Some(t) = TestDb::new("requests").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    recorded_catalog(&t.pool, &journal, &recorded).await;
    let before = core_store::latest_event_id(&t.pool).await.unwrap();

    let requests: serde_json::Value =
        serde_json::from_str(&fixture("seerr_requests.json")).unwrap();
    record_seerr(&recorded, &requests, 3);
    let out = outbound(recorded.clone());
    let site = seerr_site(&out);
    let config = PassConfig {
        page_size: 3,
        lookups: 250,
    };
    let (ok, report) = requests_pass(&t.pool, &journal, &site, config)
        .await
        .unwrap();
    assert_eq!(ok.version.as_deref(), Some("2.7.3"));
    assert_eq!((report.listed, report.added, report.removed), (8, 8, 0));
    assert!(!report.removals_held, "the pass read every request");
    assert_eq!(report.lookups, 8);
    assert_eq!(ok.counts["requests_on_site"], 8);
    let first = kinds(&t.pool, before).await;
    assert_eq!(count(&first, "request_added"), 8);
    assert_eq!(first.len(), 8);
    assert!(
        recorded.methods_seen().iter().all(|m| *m == Method::Get),
        "only GET went to Seerr"
    );

    // No address is kept anywhere: not in the rows, not in the journal.
    let addresses: i64 =
        sqlx::query_scalar("SELECT count(*) FROM requests WHERE requested_by LIKE '%@%'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(addresses, 0);
    let events = core_store::events_after(&t.pool, before, 100)
        .await
        .unwrap();
    assert!(events.iter().all(|e| !e.body.to_string().contains('@')));

    let rules = request_rules();
    let page = requests_service::list(&t.pool, &rules, &RequestQuery::default())
        .await
        .unwrap();
    assert_eq!(page.total, 8);
    let c = &page.counts;
    assert_eq!(
        (
            c.all,
            c.pending,
            c.processing,
            c.available,
            c.declined,
            c.removed
        ),
        (8, 2, 2, 2, 1, 1)
    );
    assert_eq!(page.items[0].seerr_id, 2031, "newest first");
    let by = |id: i64| page.items.iter().find(|r| r.seerr_id == id).unwrap();

    let black_mirror = by(2031);
    assert_eq!(black_mirror.title.as_deref(), Some("Black Mirror"));
    assert_eq!(black_mirror.year, Some(2011));
    assert_eq!(
        black_mirror.poster_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w342/bm-poster.jpg")
    );
    assert_eq!(black_mirror.gate.result, "trim");
    assert_eq!(black_mirror.gate.approved, vec![1, 2]);
    assert_eq!(black_mirror.gate.declined, vec![3, 4]);
    assert!(!black_mirror.cleanup.applies, "nothing is in yet");
    assert!(!black_mirror.owner);

    let life = by(2019);
    assert!(life.owner);
    assert_eq!(life.gate.result, "exempt");
    assert_eq!(life.status, "processing");
    assert!(!life.cleanup.applies);

    assert_eq!(
        by(2010).title_id,
        Some(title_with_tmdb(&t.pool, 333371).await),
        "linked by TMDB id"
    );
    let shoresy = by(1990);
    assert_eq!(
        shoresy.title_id,
        Some(title_with_tvdb(&t.pool, 414443).await),
        "linked by TVDB id when the TMDB id is not in the catalog"
    );
    assert!(shoresy.cleanup.applies);
    assert_eq!(
        shoresy.cleanup.leaves_if_unplayed.unwrap().to_rfc3339(),
        "2027-01-17T19:04:00+00:00"
    );
    assert_eq!(by(1970).requested_by, "user 9");
    assert_eq!(by(1960).status, "removed");
    assert_eq!(
        by(1960).poster_url,
        None,
        "an image path that is not TMDB's is dropped"
    );
    assert!(by(1960).backdrop_url.is_some());
    assert_eq!(by(2030).gate.result, "not_gated", "4K is not gated");
    let mut people: Vec<&str> = page.people.iter().map(|p| p.name.as_str()).collect();
    people.sort_unstable();
    assert_eq!(
        people,
        vec!["homeowner", "jun.park", "maple.reed", "user 9"]
    );
    assert!(page.people.iter().any(|p| p.name == "homeowner" && p.owner));

    // Filters.
    let query = |status: Option<&str>, person: Option<&str>, kind: Option<&str>| RequestQuery {
        status: status.map(str::to_string),
        person: person.map(str::to_string),
        kind: kind.map(str::to_string),
        ..RequestQuery::default()
    };
    let list = |q: RequestQuery| {
        let pool = t.pool.clone();
        let rules = rules.clone();
        async move { requests_service::list(&pool, &rules, &q).await.unwrap() }
    };
    assert_eq!(list(query(Some("pending"), None, None)).await.total, 2);
    let some = list(query(Some("pending,declined"), None, None)).await;
    assert_eq!(some.total, 3);
    assert_eq!(some.counts.all, 8, "the counts leave the status filter out");
    assert_eq!(list(query(None, Some("Maple.Reed"), None)).await.total, 2);
    assert_eq!(list(query(None, None, Some("tv"))).await.total, 3);
    assert!(
        requests_service::list(&t.pool, &rules, &query(Some("waiting"), None, None))
            .await
            .is_err()
    );

    // The second pass: one request moved on, one is gone from the site, and no title is looked
    // up again.
    let looked_up = lookups(&recorded);
    let mut next = requests.clone();
    let results = next["results"].as_array_mut().unwrap();
    results[0]["status"] = serde_json::json!(2);
    results[0]["media"]["status"] = serde_json::json!(3);
    results.retain(|r| r["id"] != 1960);
    record_seerr(&recorded, &next, 3);
    let mark = core_store::latest_event_id(&t.pool).await.unwrap();
    let (_, report) = requests_pass(&t.pool, &journal, &site, config)
        .await
        .unwrap();
    assert_eq!(
        (report.status_changed, report.removed, report.added),
        (1, 1, 0)
    );
    assert_eq!(
        kinds(&t.pool, mark).await,
        vec!["request_status_changed", "request_removed"]
    );
    let moved = core_store::events_after(&t.pool, mark, 10).await.unwrap();
    assert_eq!(moved[0].body["from"], "pending");
    assert_eq!(moved[0].body["to"], "processing");
    assert_eq!(moved[0].body["title"], "Black Mirror");
    assert_eq!(
        lookups(&recorded),
        looked_up,
        "each title is looked up once"
    );

    // A pass that reads the same: nothing changes and nothing is journaled.
    let mark = core_store::latest_event_id(&t.pool).await.unwrap();
    let (_, report) = requests_pass(&t.pool, &journal, &site, config)
        .await
        .unwrap();
    assert!(!report.changed());
    assert_eq!(core_store::latest_event_id(&t.pool).await.unwrap(), mark);

    // The routes.
    let app = routes::router(state(&t.pool, journal.clone()));
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    // The list is the Requests page's (`api::pages`); its shape is proven in
    // `the_pages_answer_in_the_shapes_of_their_samples`.
    let (status, _, body) = call(&app, "GET", "/api/v1/requests", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed = body["requests"].as_array().unwrap();
    assert_eq!(
        listed.iter().filter(|r| r["status"] == "requested").count(),
        1,
        "the one pending request waits for a decision"
    );
    let id = by(2031).id;
    assert!(listed.iter().any(|r| r["id"] == format!("r-{id}")));
    let (status, _, detail) = call(
        &app,
        "GET",
        &format!("/api/v1/requests/{id}"),
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["seerr_id"], 2031);
    assert_eq!(detail["status"], "processing");
    assert!(detail["related"].is_array());
    let (status, _, _) = call(&app, "GET", "/api/v1/requests/424242", &session, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&app, "GET", "/api/v1/requests", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    t.drop_db().await;
}

#[tokio::test]
async fn discover_marks_what_the_library_holds() {
    let Some(t) = TestDb::new("discover").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    recorded_catalog(&t.pool, &journal, &recorded).await;
    let requests: serde_json::Value =
        serde_json::from_str(&fixture("seerr_requests.json")).unwrap();
    record_seerr(&recorded, &requests, 100);
    let out = outbound(recorded.clone());
    let site = seerr_site(&out);
    requests_pass(&t.pool, &journal, &site, PassConfig::default())
        .await
        .unwrap();
    // A download in progress for the title the catalog holds no file of.
    let wanted = title_with_tmdb(&t.pool, 999001).await;
    sqlx::query(
        "INSERT INTO downloads (client, hash, name, state, progress, title_id)
         VALUES ('qbittorrent', 'DDDD', 'A.Movie.Not.Yet.Held.2027.1080p.WEB-DL-GRP', 'downloading', 0.5, $1)",
    )
    .bind(wanted)
    .execute(&t.pool)
    .await
    .unwrap();

    let cache = DiscoverCache::default();
    let sites = vec![site.clone()];
    let answer = discover(&t.pool, &sites, &cache, &DiscoverQuery::default())
        .await
        .unwrap();
    assert_eq!(answer.site.as_deref(), Some("plex"));
    assert_eq!(answer.lists.len(), 4);
    let trending = &answer.lists[0];
    assert_eq!(trending.total, 10000);
    assert!(trending.error.is_none() && !trending.stale);
    let status = |title: &str| {
        trending
            .items
            .iter()
            .find(|i| i.title == title)
            .map(|i| i.library_status.clone())
            .unwrap()
    };
    assert_eq!(trending.items.len(), 4, "the person is left out");
    assert_eq!(status("10 Cloverfield Lane"), "in_library");
    assert_eq!(status("A Movie Not Yet Held"), "downloading");
    assert_eq!(status("Black Mirror"), "requested");
    assert_eq!(status("Nobody Asked For This"), "none");
    assert_eq!(
        trending.items[0].poster_url.as_deref(),
        Some("https://image.tmdb.org/t/p/w342/cloverfield-poster.jpg")
    );
    let shows = &answer.lists[2];
    assert_eq!(shows.items[0].title, "1883");
    assert!(shows.items[0].title_id.is_some(), "Sonarr holds 1883");

    // Within 30 minutes the lists come from the cache.
    let asked = |recorded: &Recorded| {
        recorded
            .urls_seen()
            .iter()
            .filter(|u| u.contains("/discover/"))
            .count()
    };
    let once = asked(&recorded);
    let again = discover(
        &t.pool,
        &sites,
        &cache,
        &DiscoverQuery {
            list: Some("trending".into()),
            page: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(again.lists.len(), 1);
    assert_eq!(asked(&recorded), once);
    assert!(
        discover(
            &t.pool,
            &sites,
            &cache,
            &DiscoverQuery {
                list: Some("charts".into()),
                page: None,
            },
        )
        .await
        .is_err()
    );
    // Without a site every list says why it is empty.
    let none = discover(
        &t.pool,
        &[],
        &DiscoverCache::default(),
        &DiscoverQuery::default(),
    )
    .await
    .unwrap();
    assert!(
        none.lists
            .iter()
            .all(|l| l.items.is_empty() && l.error.is_some())
    );
    t.drop_db().await;
}

#[tokio::test]
async fn the_music_folder_becomes_artists_albums_and_tracks() {
    let Some(t) = TestDb::new("music").await else {
        return;
    };
    let journal = Journal::new();
    let folder = Folder::new("music");
    folder.file("B.B. King/Riding with the King (2000)/cover.jpg", 10);
    folder.file(
        "B.B. King/Riding With the King (2000)/B.B. King - Riding With the King - 01 - Riding With the King.flac",
        300,
    );
    folder.file(
        "B.B. King/Riding With the King (2000)/B.B. King - Riding With the King - 02 - Ten Long Years.flac",
        200,
    );
    folder.file(
        "B.B. King/The Best Of B.B. King (1971)/01 - Hummingbird.mp3",
        50,
    );
    folder.file("Various/Mixed Bag (0000)/01 - One.mp3", 30);
    folder.file("Various/Mixed Bag (0000)/02 - Two.flac", 40);
    folder.file("Various/Mixed Bag (0000)/front.png", 4);
    let lib = MusicLibrary::new(&folder.root());

    let report = music::scan(&t.pool, &journal, &lib, "test").await.unwrap();
    assert_eq!((report.found, report.added, report.removed), (4, 4, 0));
    assert_eq!(count(&kinds(&t.pool, 0).await, "album_added"), 4);

    let all = music::albums(&t.pool, &AlbumQuery::default())
        .await
        .unwrap();
    assert_eq!(all.total, 4);
    let c = &all.counts;
    assert_eq!((c.artists, c.tracks, c.duplicates), (2, 5, 2));
    assert_eq!((c.no_year, c.mixed_formats, c.empty), (1, 1, 1));
    let albums = |q: AlbumQuery| {
        let pool = t.pool.clone();
        async move { music::albums(&pool, &q).await.unwrap() }
    };
    let duplicates = albums(AlbumQuery {
        duplicates: Some(true),
        ..AlbumQuery::default()
    })
    .await;
    assert_eq!(duplicates.total, 2);
    let flac = albums(AlbumQuery {
        format: Some("FLAC".into()),
        ..AlbumQuery::default()
    })
    .await;
    assert_eq!(flac.total, 2);
    let mixed = albums(AlbumQuery {
        format: Some("mixed".into()),
        ..AlbumQuery::default()
    })
    .await;
    assert_eq!(mixed.items[0].format.as_deref(), Some("mixed"));
    let no_cover = albums(AlbumQuery {
        missing_cover: Some(true),
        ..AlbumQuery::default()
    })
    .await;
    assert_eq!(no_cover.total, 2);
    let no_year = albums(AlbumQuery {
        no_year: Some(true),
        ..AlbumQuery::default()
    })
    .await;
    assert_eq!(no_year.items[0].name, "Mixed Bag");
    assert!(
        music::albums(
            &t.pool,
            &AlbumQuery {
                format: Some("vinyl".into()),
                ..AlbumQuery::default()
            }
        )
        .await
        .is_err()
    );

    let artists = music::artists(&t.pool, &ArtistQuery::default())
        .await
        .unwrap();
    assert_eq!(artists.total, 2);
    let king = &artists.items[0];
    assert_eq!(
        (king.name.as_str(), king.album_count, king.track_count),
        ("B.B. King", 3, 3)
    );
    assert_eq!(king.formats, vec!["flac", "mp3"]);
    let detail = music::artist(&t.pool, king.id).await.unwrap();
    assert_eq!(detail.albums.len(), 3);
    assert!(detail.problems.iter().any(|p| p.kind == "duplicate"));
    assert!(detail.problems.iter().any(|p| p.kind == "empty"));
    let riding = detail
        .albums
        .iter()
        .find(|a| a.album.folder == "Riding With the King (2000)")
        .unwrap();
    assert_eq!(riding.tracks.len(), 2);
    assert_eq!(riding.tracks[1].title.as_deref(), Some("Ten Long Years"));
    assert_eq!(riding.duplicates.len(), 1);
    let cover_only = riding.duplicates[0].id;
    let (bytes, kind) = music::cover(&t.pool, &lib, cover_only).await.unwrap();
    assert_eq!((bytes.len(), kind), (10, "image/jpeg"));
    assert!(music::cover(&t.pool, &lib, riding.album.id).await.is_err());

    // A path in the database that leads out of the folder is never served.
    sqlx::query(
        "UPDATE music_albums SET cover_path = '../../etc/passwd.jpg', facts_hash = '' WHERE id = $1",
    )
        .bind(cover_only)
        .execute(&t.pool)
        .await
        .unwrap();
    assert!(matches!(
        music::cover(&t.pool, &lib, cover_only).await,
        Err(AppError::NotFound(_))
    ));

    // The next scan sees the row differ from the folder and writes the folder's facts back,
    // without an event; a folder that is gone is one album removed.
    let mark = core_store::latest_event_id(&t.pool).await.unwrap();
    let report = music::scan(&t.pool, &journal, &lib, "test").await.unwrap();
    assert_eq!((report.added, report.removed, report.changed), (0, 0, 1));
    assert_eq!(core_store::latest_event_id(&t.pool).await.unwrap(), mark);
    assert!(music::cover(&t.pool, &lib, cover_only).await.is_ok());
    std::fs::remove_dir_all(folder.0.join("Various")).unwrap();
    let report = music::scan(&t.pool, &journal, &lib, "test").await.unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(kinds(&t.pool, mark).await, vec!["album_removed"]);
    let artists = music::artists(&t.pool, &ArtistQuery::default())
        .await
        .unwrap();
    assert_eq!(artists.total, 1, "an artist without albums is gone");

    // An empty folder (a mount that is down) removes nothing.
    let empty = Folder::new("music-empty");
    let unmounted = MusicLibrary::new(&empty.root());
    assert!(
        music::scan(&t.pool, &journal, &unmounted, "test")
            .await
            .is_err()
    );
    assert_eq!(
        music::albums(&t.pool, &AlbumQuery::default())
            .await
            .unwrap()
            .total,
        3
    );
    let scan_state = music::state(&t.pool, &unmounted).await.unwrap();
    assert!(!scan_state.ok && scan_state.error.is_some());

    // The routes. A scan from the API is a run of the folder's job: the runtime runs it.
    let root = folder.root();
    let st = state_with(
        &t.pool,
        journal.clone(),
        &[("SINGULARR_MUSIC_ROOT", root.as_str())],
    );
    let jobs = start_jobs(&st).await;
    let app = routes::router(st);
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let reader = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "reader".into(),
            scopes: vec![Scope::Read],
        },
    )
    .await
    .unwrap();
    let read = format!("Bearer {}", reader.secret);
    let auth_read: [(&str, &str); 1] = [("authorization", &read)];
    let (status, _, body) = call(
        &app,
        "GET",
        "/api/v1/music/albums?sort=-year",
        &auth_read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 3);
    assert!(body["counts"]["formats"].is_array());
    let cover_url = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == cover_only)
        .and_then(|a| a["cover_url"].as_str())
        .unwrap()
        .to_string();
    assert_eq!(
        cover_url,
        format!("/api/v1/music/albums/{cover_only}/cover")
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&cover_url)
                .header("authorization", &read)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "image/jpeg");
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    let (status, _, body) = call(
        &app,
        "GET",
        "/api/v1/music/artists?q=king",
        &auth_read,
        None,
    )
    .await;
    assert_eq!((status, body["total"].as_i64()), (StatusCode::OK, Some(1)));
    let (status, _, _) = call(&app, "GET", "/api/v1/music/albums/424242", &auth_read, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = call(&app, "POST", "/api/v1/music/scan", &auth_read, None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a scan needs the library scope"
    );
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, body) = call(&app, "POST", "/api/v1/music/scan", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["folder"], "music");
    assert_eq!(body["found"], 3, "the run's report");
    jobs.stop().await;
    t.drop_db().await;
}

#[tokio::test]
async fn home_movies_are_read_annotated_and_kept_through_a_rename() {
    let Some(t) = TestDb::new("homemovies").await else {
        return;
    };
    let journal = Journal::new();
    let folder = Folder::new("tapes");
    folder.file("1986 - montana - reptile gardens - bear country.mp4", 120);
    folder.file("1988-1991 - easter 88 - easter 91.mp4", 80);
    folder.file("montana - san jose - xmas.mp4", 60);
    let lib = HomeMovies::new(&folder.root());

    let report = homemovies::scan(&t.pool, &journal, &lib, "test")
        .await
        .unwrap();
    assert_eq!((report.found, report.added), (3, 3));
    assert_eq!(count(&kinds(&t.pool, 0).await, "tape_added"), 3);
    let page = homemovies::list(&t.pool, &TapeQuery::default())
        .await
        .unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(page.totals.inbox, 3, "nothing is annotated yet");
    assert_eq!(page.totals.size_bytes, 260);
    assert_eq!(
        (page.totals.first_year, page.totals.last_year),
        (Some(1986), Some(1991))
    );
    let montana = &page.items[0];
    assert_eq!(montana.year_from, Some(1986));
    assert_eq!(montana.chapters.len(), 3);
    assert_eq!(montana.title, "Montana, Reptile gardens, Bear country");
    assert_eq!(montana.still_url, None);
    assert!(page.items[1].uncertain, "1988 to 1991 is uncertain");
    assert!(page.items[2].uncertain && page.items[2].year_from.is_none());

    // Annotate one tape: it leaves the inbox and shows the owner's values.
    let patch: TapePatch = serde_json::from_value(serde_json::json!({
        "title": "Montana trip",
        "people": ["Opa", "Omi"],
        "places": ["Montana"],
        "chapters": [
            {"label": "Reptile Gardens", "start_seconds": 0},
            {"label": "Bear Country", "start_seconds": 1500}
        ],
        "private": true
    }))
    .unwrap();
    let id = montana.id;
    let tape = homemovies::patch(&t.pool, &journal, "user:owner@example.com", id, &patch)
        .await
        .unwrap();
    assert!(tape.annotated && tape.private);
    assert_eq!(tape.title, "Montana trip");
    assert_eq!(tape.read.title, "Montana, Reptile gardens, Bear country");
    assert_eq!(tape.chapters[1].start_seconds, Some(1500));
    let inbox = homemovies::inbox(&t.pool, &InboxQuery::default())
        .await
        .unwrap();
    assert_eq!(inbox.total, 2);
    let tapes = |q: TapeQuery| {
        let pool = t.pool.clone();
        async move { homemovies::list(&pool, &q).await.unwrap() }
    };
    let with_opa = tapes(TapeQuery {
        person: Some("opa".into()),
        ..TapeQuery::default()
    })
    .await;
    assert_eq!(with_opa.total, 1);
    assert_eq!(with_opa.people.len(), 2);
    let in_1990 = tapes(TapeQuery {
        year: Some(1990),
        ..TapeQuery::default()
    })
    .await;
    assert_eq!(in_1990.total, 1);
    let events = core_store::events_after(&t.pool, 0, 100).await.unwrap();
    let annotated = events.iter().find(|e| e.kind == "tape_annotated").unwrap();
    assert_eq!(annotated.actor, "user:owner@example.com");
    assert!(
        !annotated.body.to_string().contains("Opa"),
        "the journal names the fields, not the values"
    );

    // Renamed on disk: the tape and its annotation follow the file.
    std::fs::rename(
        folder
            .0
            .join("1986 - montana - reptile gardens - bear country.mp4"),
        folder.0.join("1986 - montana trip.mp4"),
    )
    .unwrap();
    let mark = core_store::latest_event_id(&t.pool).await.unwrap();
    let report = homemovies::scan(&t.pool, &journal, &lib, "test")
        .await
        .unwrap();
    assert_eq!((report.added, report.changed, report.removed), (0, 1, 0));
    assert_eq!(kinds(&t.pool, mark).await, vec!["tape_moved"]);
    let moved = homemovies::get(&t.pool, id).await.unwrap();
    assert_eq!(moved.path, "1986 - montana trip.mp4");
    assert_eq!(moved.title, "Montana trip", "the annotation stays");
    assert_eq!(moved.read.chapters[0].label, "Montana trip");

    // Gone, then back.
    std::fs::remove_file(folder.0.join("montana - san jose - xmas.mp4")).unwrap();
    let report = homemovies::scan(&t.pool, &journal, &lib, "test")
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    assert_eq!(
        homemovies::list(&t.pool, &TapeQuery::default())
            .await
            .unwrap()
            .total,
        2
    );
    let missing = tapes(TapeQuery {
        missing: Some(true),
        ..TapeQuery::default()
    })
    .await;
    assert_eq!(missing.total, 1);
    folder.file("montana - san jose - xmas.mp4", 60);
    let mark = core_store::latest_event_id(&t.pool).await.unwrap();
    homemovies::scan(&t.pool, &journal, &lib, "test")
        .await
        .unwrap();
    let back = core_store::events_after(&t.pool, mark, 10).await.unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].kind, "tape_added");
    assert_eq!(back[0].body["returned"], true);

    // The routes.
    let root = folder.root();
    let app = routes::router(state_with(
        &t.pool,
        journal.clone(),
        &[("SINGULARR_HOME_MOVIES_ROOT", root.as_str())],
    ));
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let reader = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "reader".into(),
            scopes: vec![Scope::Read],
        },
    )
    .await
    .unwrap();
    let librarian = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "librarian".into(),
            scopes: vec![Scope::Read, Scope::Library],
        },
    )
    .await
    .unwrap();
    let read = format!("Bearer {}", reader.secret);
    let library = format!("Bearer {}", librarian.secret);
    let auth_read: [(&str, &str); 1] = [("authorization", &read)];
    let auth_library: [(&str, &str); 1] = [("authorization", &library)];
    let (status, _, body) = call(
        &app,
        "GET",
        "/api/v1/home-movies?place=montana",
        &auth_read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], 1);
    assert_eq!(body["totals"]["tapes"], 3);
    assert!(body["items"][0]["still_url"].is_null());
    let (status, _, body) = call(&app, "GET", "/api/v1/home-movies/inbox", &auth_read, None).await;
    assert_eq!((status, body["total"].as_i64()), (StatusCode::OK, Some(2)));
    let other = body["items"][0]["id"].as_i64().unwrap();
    let path = format!("/api/v1/home-movies/{other}");
    let (status, _, _) = call(&app, "PATCH", &path, &auth_read, Some("{}")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a write needs the library scope"
    );
    let (status, _, problem) = call(
        &app,
        "PATCH",
        &path,
        &auth_library,
        Some("{\"titel\":\"x\"}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    let (status, _, problem) = call(
        &app,
        "PATCH",
        &path,
        &auth_library,
        Some("{\"year_from\":1991,\"year_to\":1988}"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    let (status, _, body) = call(&app, "PATCH", &path, &auth_library, Some("{}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["annotated"], true,
        "an empty patch accepts the reading"
    );
    let (status, _, body) = call(&app, "GET", &path, &auth_read, None).await;
    assert_eq!(
        (status, body["annotated"].as_bool()),
        (StatusCode::OK, Some(true))
    );
    let (status, _, _) = call(&app, "GET", "/api/v1/home-movies/424242", &auth_read, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, body) = call(
        &app,
        "POST",
        "/api/v1/home-movies/scan",
        &auth_library,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["found"], 3);

    // The sidebar's summary.
    let (status, _, summary) = call(&app, "GET", "/api/v1/summary", &auth_read, None).await;
    assert_eq!(status, StatusCode::OK, "{summary}");
    assert_eq!(summary["home_movies_inbox"], 1);
    assert_eq!(summary["requests_pending"], 0);
    assert_eq!(summary["folders"].as_array().unwrap().len(), 2);
    assert_eq!(summary["folders"][1]["folder"], "home_movies");
    assert_eq!(summary["folders"][1]["ok"], true);
    assert_eq!(summary["engines"].as_array().unwrap().len(), 4);
    t.drop_db().await;
}

#[tokio::test]
async fn the_pages_read_totals_leads_seasons_and_the_new_filters() {
    use singularr::catalog::model::TitleKind;
    let Some(t) = TestDb::new("pages").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    record_sonarr(&recorded, &fixture("sonarr_episodefile.json"));
    let out = outbound(recorded.clone());
    let radarr: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let sonarr: Arc<dyn LibraryEngine> = Arc::new(Sonarr(arr(
        "sonarr",
        ArrKind::Sonarr,
        "sonarr",
        SONARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![radarr.clone(), sonarr.clone()],
        None,
        slots(&[
            ("radarr", "radarr", Some("radarr")),
            ("sonarr", "sonarr", Some("sonarr")),
        ]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines, &policy);
    titles_pass(&deps, &radarr, true).await.unwrap();
    titles_pass(&deps, &sonarr, false).await.unwrap();

    // The heads of Movies and Shows.
    let movies = catalog::kind_totals(&t.pool, TitleKind::Movie)
        .await
        .unwrap();
    assert_eq!((movies.titles, movies.with_file), (3, 2));
    assert!(movies.size_bytes > 0);
    let shows = catalog::kind_totals(&t.pool, TitleKind::Show)
        .await
        .unwrap();
    assert_eq!((shows.titles, shows.episode_files), (2, 2));
    assert!(shows.episodes >= 10);

    let search = |q: TitleQuery| {
        let pool = t.pool.clone();
        async move { catalog::titles(&pool, &lanes(), &q).await.unwrap() }
    };
    let movie = |extra: TitleQuery| TitleQuery {
        kind: Some("movie".into()),
        ..extra
    };
    let show = |extra: TitleQuery| TitleQuery {
        kind: Some("show".into()),
        ..extra
    };

    // Each title in a list carries its lead copy.
    let all = search(movie(TitleQuery {
        sort: Some("title".into()),
        ..Default::default()
    }))
    .await;
    let lead_of = |name: &str| {
        all.items
            .iter()
            .find(|x| x.title == name)
            .unwrap()
            .lead
            .clone()
    };
    let cloverfield = lead_of("10 Cloverfield Lane").expect("a lead copy");
    assert_eq!(cloverfield.quality.as_deref(), Some("Bluray-1080p"));
    assert_eq!(cloverfield.group.as_deref(), Some("SM737"));
    assert_eq!(cloverfield.library, "radarr");
    assert_eq!(lead_of("A Movie Not Yet Held"), None);

    // The filters the pages add.
    let total =
        |page: singularr::core::model::Page<singularr::catalog::model::TitleSummary>| page.total;
    assert_eq!(
        total(
            search(movie(TitleQuery {
                quality: Some("bluray".into()),
                ..Default::default()
            }))
            .await
        ),
        1
    );
    assert_eq!(
        total(
            search(movie(TitleQuery {
                quality: Some("remux".into()),
                ..Default::default()
            }))
            .await
        ),
        0
    );
    let missing = search(movie(TitleQuery {
        missing: Some(true),
        ..Default::default()
    }))
    .await;
    assert_eq!(missing.total, 1);
    assert_eq!(missing.items[0].title, "A Movie Not Yet Held");
    assert_eq!(
        total(
            search(movie(TitleQuery {
                missing: Some(false),
                ..Default::default()
            }))
            .await
        ),
        2
    );
    assert_eq!(
        total(
            search(show(TitleQuery {
                missing: Some(true),
                ..Default::default()
            }))
            .await
        ),
        2,
        "1883 holds 2 of 10 episodes, Shoresy none"
    );
    let continuing = search(show(TitleQuery {
        status: Some("Continuing".into()),
        ..Default::default()
    }))
    .await;
    assert_eq!(continuing.total, 1);
    assert_eq!(continuing.items[0].title, "Shoresy");
    assert_eq!(
        total(
            search(show(TitleQuery {
                airing: Some(true),
                ..Default::default()
            }))
            .await
        ),
        1
    );
    let next = search(show(TitleQuery {
        sort: Some("next".into()),
        ..Default::default()
    }))
    .await;
    assert_eq!(next.items[0].title, "Shoresy", "the soonest airing first");
    assert!(
        catalog::titles(
            &t.pool,
            &lanes(),
            &TitleQuery {
                sort: Some("loudness".into()),
                ..Default::default()
            }
        )
        .await
        .is_err()
    );

    // A show's files per season, and its episode names.
    let id_1883 = next.items.iter().find(|x| x.title == "1883").unwrap().id;
    let seasons = catalog::season_files(&t.pool, id_1883).await.unwrap();
    assert_eq!(seasons.len(), 1);
    assert_eq!(seasons[0].season_number, 1);
    assert_eq!(seasons[0].qualities[0].name, "Bluray-1080p");
    assert_eq!(seasons[0].qualities[0].count, 2);
    assert_eq!(seasons[0].groups[0].count, 2);
    assert!(seasons[0].size_bytes > 0);
    let names = catalog::episode_names(&t.pool, id_1883, Some(1))
        .await
        .unwrap();
    assert_eq!(names.len(), 2);
    assert_eq!(names[1].name.as_deref(), Some("Behind Us a Cliff"));

    // The counts of the Rules page, and the kind behind an old address.
    let per_lane = catalog::lane_copy_counts(&t.pool).await.unwrap();
    let held = |lane: &str| per_lane.iter().find(|(l, _)| l == lane).map(|(_, n)| *n);
    assert_eq!(held("movies-hd"), Some(1));
    assert_eq!(held("shows-hd"), Some(2));
    let guarded = catalog::protection_title_counts(&t.pool).await.unwrap();
    assert!(!guarded.is_empty(), "Azkaban's copy is protected");
    assert!(guarded.iter().all(|(_, n)| *n == 1));
    assert_eq!(catalog::title_kind(&t.pool, id_1883).await.unwrap(), "show");
    assert!(catalog::title_kind(&t.pool, 424_242).await.is_err());
    t.drop_db().await;
}

// Editions ---------------------------------------------------------------------------------------

#[tokio::test]
async fn editions_come_from_the_record_the_name_and_the_folders() {
    let Some(t) = TestDb::new("editions").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    recorded_catalog(&t.pool, &journal, &recorded).await;
    let cloverfield = title_with_tmdb(&t.pool, 333_371).await;
    let potter = title_with_tmdb(&t.pool, 673).await;

    // Each copy belongs to one edition; a movie lists the editions it holds.
    let editions = catalog::title_editions(&t.pool, potter).await.unwrap();
    assert_eq!(editions.len(), 1);
    let om = &editions[0];
    assert_eq!(
        (om.key.as_str(), om.label.as_str(), om.wanted, om.is_default),
        ("open-matte", "Open Matte", true, true)
    );
    assert_eq!(om.held.len(), 1);
    assert!(om.held[0].tracked && om.held[0].copy_id.is_some());
    assert_eq!(om.held[0].library, "radarr");
    assert_eq!(om.held[0].resolution.as_deref(), Some("2160p"));
    assert_eq!(om.held[0].plays_as.as_deref(), Some("Open Matte"));
    let detail = catalog::title(&t.pool, &lanes(), potter).await.unwrap();
    assert_eq!(detail.editions, editions);
    assert_eq!(detail.copies[0].edition.key, "open-matte");
    assert!(detail.copies[0].edition.marked);
    let plain = catalog::title_editions(&t.pool, cloverfield).await.unwrap();
    assert_eq!(
        plain
            .iter()
            .map(|e| (e.key.as_str(), e.is_default))
            .collect::<Vec<_>>(),
        vec![("theatrical", true)]
    );
    assert_eq!(plain[0].held[0].plays_as, None);
    // A show has no editions yet.
    let shoresy = title_with_tvdb(&t.pool, 414_443).await;
    assert!(
        catalog::title_editions(&t.pool, shoresy)
            .await
            .unwrap()
            .is_empty()
    );

    // The edition filter takes a key, a label or an alias.
    let by = |edition: &str| TitleQuery {
        edition: Some(edition.to_string()),
        ..TitleQuery::default()
    };
    for text in ["Open Matte", "open-matte", "OM"] {
        let page = catalog::titles(&t.pool, &lanes(), &by(text)).await.unwrap();
        assert_eq!(
            page.items.iter().map(|x| x.id).collect::<Vec<_>>(),
            vec![potter],
            "{text}"
        );
    }
    let page = catalog::titles(&t.pool, &lanes(), &by("theatrical"))
        .await
        .unwrap();
    assert!(page.items.iter().any(|x| x.id == cloverfield));
    assert!(page.items.iter().all(|x| x.id != potter));

    // The folder scan lists what Radarr does not track. The movie folders are made under a
    // temporary root, and the stored paths point there.
    let root = Folder::new("movies");
    let relative: String =
        sqlx::query_scalar("SELECT relative_path FROM copies WHERE title_id = $1")
            .bind(cloverfield)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    let folder = "10 Cloverfield Lane (2016) {tmdb-333371}";
    root.file(&format!("{folder}/{relative}"), 10);
    let dc = "10 Cloverfield Lane (2016) {imdb-tt1179933} {edition-Director's Cut} [Bluray-2160p][DV HDR10][EAC3 5.1][x265]-GRP.mkv";
    root.file(&format!("{folder}/{dc}"), 20);
    root.file(
        &format!("{folder}/10 Cloverfield Lane (2016)-trailer.mkv"),
        5,
    );
    root.file(&format!("{folder}/10 Cloverfield Lane (2016).en.srt"), 5);
    sqlx::query("UPDATE title_engines SET path = replace(path, '/media/movies', $1)")
        .bind(root.root())
        .execute(&t.pool)
        .await
        .unwrap();
    let roots = vec![root.root()];
    let list = singularr_core::EditionList::shipped();
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let report = singularr::catalog::scan::scan(&t.pool, &journal, list, &roots)
        .await
        .unwrap();
    assert_eq!(
        (
            report.untracked,
            report.found,
            report.gone,
            report.unreadable
        ),
        (1, 1, 0, 0)
    );
    // Prisoner of Azkaban's folder is missing under this root: an empty folder, nothing found.
    assert_eq!(report.folders, 3);
    let after = kinds(&t.pool, before).await;
    assert_eq!(after, vec!["untracked_file_found".to_string()]);
    let both = catalog::title_editions(&t.pool, cloverfield).await.unwrap();
    assert_eq!(
        both.iter()
            .map(|e| (e.key.as_str(), e.is_default, e.wanted))
            .collect::<Vec<_>>(),
        vec![("directors-cut", false, true), ("theatrical", true, true)]
    );
    let untracked = &both[0].held[0];
    assert!(!untracked.tracked && untracked.copy_id.is_none());
    assert_eq!(untracked.file_name, dc);
    assert_eq!(untracked.resolution.as_deref(), Some("2160p"));
    assert_eq!(untracked.plays_as.as_deref(), Some("Director's Cut"));
    let page = catalog::titles(&t.pool, &lanes(), &by("Director's Cut"))
        .await
        .unwrap();
    assert_eq!(
        page.items.iter().map(|x| x.id).collect::<Vec<_>>(),
        vec![cloverfield]
    );
    let status = catalog::folder_scan_status(&t.pool).await.unwrap().unwrap();
    assert!(status.ok);
    assert_eq!(status.counts["untracked"], 1);

    // A second scan of the same folders changes nothing.
    let again = singularr::catalog::scan::scan(&t.pool, &journal, list, &roots)
        .await
        .unwrap();
    assert_eq!((again.found, again.gone), (0, 0));

    // A root that cannot be read leaves its rows as they are.
    let moved = format!("{}-away", root.root());
    std::fs::rename(root.root(), &moved).unwrap();
    let down = singularr::catalog::scan::scan(&t.pool, &journal, list, &roots)
        .await
        .unwrap();
    assert_eq!(down.roots_down, roots);
    assert_eq!(down.gone, 0);
    assert_eq!(
        catalog::title_editions(&t.pool, cloverfield)
            .await
            .unwrap()
            .len(),
        2
    );
    assert!(
        !catalog::folder_scan_status(&t.pool)
            .await
            .unwrap()
            .unwrap()
            .ok
    );

    // So does a root that is there and lists nothing: a mount point without its volume.
    std::fs::create_dir(root.root()).unwrap();
    let empty = singularr::catalog::scan::scan(&t.pool, &journal, list, &roots)
        .await
        .unwrap();
    assert_eq!(empty.roots_down, roots);
    assert_eq!(empty.gone, 0);
    std::fs::remove_dir(root.root()).unwrap();
    std::fs::rename(&moved, root.root()).unwrap();

    // A folder that cannot be read keeps its rows as well. Here a file stands in its place.
    let in_place = root.0.join(folder);
    let aside = format!("{}-aside", root.root());
    std::fs::rename(&in_place, &aside).unwrap();
    std::fs::write(&in_place, b"x").unwrap();
    let unread = singularr::catalog::scan::scan(&t.pool, &journal, list, &roots)
        .await
        .unwrap();
    assert_eq!((unread.unreadable, unread.gone), (1, 0));
    assert_eq!(
        catalog::title_editions(&t.pool, cloverfield)
            .await
            .unwrap()
            .len(),
        2
    );
    std::fs::remove_file(&in_place).unwrap();
    std::fs::rename(&aside, &in_place).unwrap();

    // The file leaves the folder: it is gone from the list, and so is its edition.
    std::fs::remove_file(root.0.join(folder).join(dc)).unwrap();
    let gone = singularr::catalog::scan::scan(&t.pool, &journal, list, &roots)
        .await
        .unwrap();
    assert_eq!((gone.found, gone.gone), (0, 1));
    let left = catalog::title_editions(&t.pool, cloverfield).await.unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].key, "theatrical");
    // Nothing in the folders was written.
    assert!(root.0.join(folder).join(&relative).exists());

    // The route answers the same list, and 404 for a title that does not exist.
    let app = routes::router(state(&t.pool, journal.clone()));
    let key = auth::create_key(
        &t.pool,
        &journal,
        &Caller::Owner {
            email: "o@example.com".into(),
        },
        &NewApiKey {
            name: "editions".into(),
            scopes: vec![Scope::Read],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", key.secret);
    let headers = [("authorization", bearer.as_str())];
    let (status, _, body) = call(
        &app,
        "GET",
        &format!("/api/v1/titles/{potter}/editions"),
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["key"], "open-matte");
    assert_eq!(body["items"][0]["held"][0]["plays_as"], "Open Matte");
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/titles/424242/editions",
        &headers,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    t.drop_db().await;
}

// Singularr Requests -------------------------------------------------------------------------------

/// The portal's role under a name of this test's own: roles are shared by every database of the
/// server. It is kept as the full process keeps it at its start (`requests::role` runs
/// deploy/requests-role.sql). The pool signs in as it to the test database over TCP with its
/// password, as the portal does.
async fn face_role(t: &TestDb) -> (String, PgPool) {
    let role = format!("mq_face_{}", uuid_like());
    let config = portal_config(&role);
    assert_eq!(
        portal_role::keep(&t.pool, &config).await,
        Kept::Created { role: role.clone() }
    );
    // The next start finds the role as it left it: the password signs in, nothing changes.
    assert_eq!(
        portal_role::keep(&t.pool, &config).await,
        Kept::Granted {
            role: role.clone(),
            password_set: false
        }
    );
    let pool = sign_in(&t.name, &role, &portal_password()).await;
    (role, pool)
}

/// The portal role's password in these tests. The gate sets a new one for each run. Its server
/// logs every DDL statement. The gate checks that this log never holds the password
/// (tools/gate.sh).
fn portal_password() -> String {
    std::env::var("SINGULARR_TEST_PORTAL_PASSWORD")
        .unwrap_or_else(|_| "portal-password-of-the-tests".to_string())
}

/// The password of the test logins that sign in over TCP: a database's owner, a plain login.
const LOGIN_PASSWORD: &str = "login-password-of-the-tests";

/// The full process's settings for the portal role `role`, with the tests' password.
fn portal_config(role: &str) -> PortalRoleConfig {
    PortalRoleConfig {
        role: Some(role.to_string()),
        password: Some(portal_password()),
    }
}

/// The test server for `role` with `password` in database `db`, over TCP: at 127.0.0.1 when
/// SINGULARR_TEST_DATABASE_URL names a socket. tools/gate.sh has the server ask every TCP login
/// for a SCRAM-SHA-256 password. A login over its socket needs none.
fn over_tcp(db: &str, role: &str, password: &str) -> PgConnectOptions {
    let server: PgConnectOptions = std::env::var("SINGULARR_TEST_DATABASE_URL")
        .unwrap()
        .parse()
        .unwrap();
    let host = match server.get_socket() {
        Some(_) => "127.0.0.1".to_string(),
        None => server.get_host().to_string(),
    };
    PgConnectOptions::new_without_pgpass()
        .host(&host)
        .port(server.get_port())
        .username(role)
        .password(password)
        .database(db)
}

/// A pool that signs in to the test server's database `db` as `role` with `password`, over TCP
/// with SCRAM-SHA-256.
async fn sign_in(db: &str, role: &str, password: &str) -> PgPool {
    PgPoolOptions::new()
        .max_connections(4)
        .connect_with(over_tcp(db, role, password))
        .await
        .unwrap_or_else(|e| panic!("{role} signs in: {e}"))
}

/// Whether a sign-in over TCP as `role` with `password` is refused for the password (28P01).
async fn refused(db: &str, role: &str, password: &str) -> bool {
    use sqlx::Connection;
    matches!(
        sqlx::PgConnection::connect_with(&over_tcp(db, role, password)).await,
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("28P01")
    )
}

/// Gives a role a password as a SCRAM-SHA-256 verifier, as psql's \password does: no statement
/// holds the password itself.
async fn give_password(admin: &PgPool, role: &str, password: &str) {
    let verifier = portal_role::verifier(password, b"salt-of-the-test", 4096).unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
        "ALTER ROLE {role} PASSWORD '{verifier}'"
    )))
    .execute(admin)
    .await
    .unwrap();
}

/// A database of a login of the test's own, as on a database server several programs share. The
/// login has a password and signs in over TCP. With `createrole` it may create roles. The
/// migrations are applied as that login. Also a pool of the server's superuser on that
/// database, for what only a superuser reads (pg_authid). `None` without a test server.
async fn owned_db(test: &str, createrole: bool) -> Option<(TestDb, String, PgPool)> {
    let Ok(url) = std::env::var("SINGULARR_TEST_DATABASE_URL") else {
        eprintln!("{test}: skipped, SINGULARR_TEST_DATABASE_URL is not set (see tools/gate.sh)");
        return None;
    };
    let admin = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("the test database server is reachable");
    let owner = format!("mq_{test}_{}", uuid_like());
    let name = format!("mq_{test}db_{}", uuid_like());
    let createrole = if createrole { " CREATEROLE" } else { "" };
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {owner} LOGIN{createrole}"
    )))
    .execute(&admin)
    .await
    .unwrap();
    give_password(&admin, &owner, LOGIN_PASSWORD).await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE \"{name}\" OWNER {owner}"
    )))
    .execute(&admin)
    .await
    .unwrap();
    let pool = sign_in(&name, &owner, LOGIN_PASSWORD).await;
    db::migrate(&pool)
        .await
        .expect("the migrations apply as the database's owner");
    let base = url.rsplit_once('/').map(|(b, _)| b).unwrap_or(&url);
    let superuser = PgPoolOptions::new()
        .max_connections(1)
        .connect(&format!("{base}/{name}"))
        .await
        .unwrap();
    Some((TestDb { pool, admin, name }, owner, superuser))
}

async fn role_exists(pool: &PgPool, role: &str) -> bool {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname::text = $1::text)")
        .bind(role)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// What a role holds, read as the server's superuser: its attributes and its password, and every
/// grant on a relation, a column, the schema and the database that names it (each list of grants
/// sorted).
async fn role_state(pool: &PgPool, role: &str) -> Vec<String> {
    sqlx::query_scalar(
        "WITH acl (kind, name, list) AS (
             SELECT 'rel', c.relname::text, c.relacl FROM pg_class c
                 JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public'
             UNION ALL
             SELECT 'col', c.relname::text || '.' || a.attname::text, a.attacl FROM pg_attribute a
                 JOIN pg_class c ON c.oid = a.attrelid
                 JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = 'public'
             UNION ALL
             SELECT 'schema', nspname::text, nspacl FROM pg_namespace WHERE nspname = 'public'
             UNION ALL
             SELECT 'db', '', datacl FROM pg_database WHERE datname = current_database())
         SELECT format('%s %s %s', kind, name,
                       (SELECT string_agg(item::text, ',' ORDER BY item::text) FROM unnest(list) item))
         FROM acl WHERE strpos(list::text, $1::text) > 0
         UNION ALL
         SELECT format('role %s %s %s %s %s %s %s %s %s %s', rolname, rolcanlogin, rolsuper,
                       rolinherit, rolcreaterole, rolcreatedb, rolreplication, rolbypassrls,
                       rolconnlimit, coalesce(rolpassword, '-'))
         FROM pg_authid WHERE rolname::text = $1::text
         ORDER BY 1",
    )
    .bind(role)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Base64 (RFC 4648) back to bytes: the salt of a stored verifier.
fn base64_decoded(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let (mut held, mut bits) = (0u32, 0);
    for c in text.bytes().take_while(|c| *c != b'=') {
        let value = ALPHABET
            .iter()
            .position(|a| *a == c)
            .unwrap_or_else(|| panic!("not base64: {text}"));
        held = (held << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((held >> bits) as u8);
        }
    }
    out
}

async fn drop_roles(admin: &PgPool, roles: &[&str]) {
    for role in roles {
        sqlx::query(sqlx::AssertSqlSafe(format!("DROP ROLE IF EXISTS {role}")))
            .execute(admin)
            .await
            .unwrap();
    }
}

async fn drop_role(t: TestDb, role: &str, face: PgPool) {
    face.close().await;
    let admin = t.admin.clone();
    t.drop_db().await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP ROLE IF EXISTS \"{role}\""
    )))
    .execute(&admin)
    .await
    .unwrap();
}

fn denied(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(d) if d.code().as_deref() == Some("42501"))
}

/// Seerr's accounts and requests as the full process would have synced them: a friend (Plex
/// account 1000007, Seerr user 7), another friend (1000009, user 9), and the owner (user 1).
async fn face_data(pool: &PgPool) {
    sqlx::raw_sql(
        "INSERT INTO seerr_users (site, seerr_user_id, display_name, plex_id, user_type, is_owner) VALUES
            ('plex', 1, 'Owner Account', 1000001, 1, true),
            ('plex', 7, 'Bright Friend', 1000007, 1, false),
            ('plex', 9, 'Other Friend', 1000009, 1, false);
         INSERT INTO requests (site, seerr_id, kind, tmdb_id, is_4k, request_status, media_status,
                               status, requested_by, requested_by_id, requested_at, seasons) VALUES
            ('plex', 501, 'movie', 999001, false, 'pending', 'pending', 'pending', 'Other Friend', 9, now(), '{}'),
            ('plex', 502, 'show', 1396, false, 'approved', 'processing', 'processing', 'Bright Friend', 7, now() - interval '30 days', '{1,2}'),
            ('plex', 503, 'movie', 777, true, 'pending', 'pending', 'pending', 'Other Friend', 9, now(), '{}');
         INSERT INTO request_media (kind, tmdb_id, title, year) VALUES ('show', 1396, 'Breaking Bad', 2008);
         INSERT INTO discover_items (list, position, kind, tmdb_id, title, year) VALUES
            ('trending', 0, 'movie', 603, 'The Matrix', 1999),
            ('trending', 1, 'movie', 999001, 'A Movie Not Yet Held', 2027);",
    )
    .execute(pool)
    .await
    .unwrap();
    singularr::requests::store::write_face_settings(
        pool,
        2,
        &["plex".to_string()],
        Some("https://singularr.example.org"),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn the_face_role_reads_its_views_and_nothing_else() {
    let Some(t) = TestDb::new("facerole").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    recorded_catalog(&t.pool, &journal, &recorded).await;
    face_data(&t.pool).await;
    let (role, face) = face_role(&t).await;
    use singularr::requests::store as rs;

    // No table is readable: not the catalog, the keys, the journal, the requests with their
    // requesters, the accounts, the sessions, the intents, music or home movies, nor the plays
    // with the players' addresses.
    for table in [
        "copies",
        "titles",
        "api_keys",
        "events",
        "requests",
        "seerr_users",
        "request_sessions",
        "request_session_ends",
        "request_intents",
        "home_movies",
        "home_movie_scan",
        "music_albums",
        "music_tracks",
        "title_editions",
        "untracked_files",
        "plays",
        "play_sources",
    ] {
        let e = sqlx::query(sqlx::AssertSqlSafe(format!(
            "SELECT 1 FROM {table} LIMIT 1"
        )))
        .execute(&face)
        .await
        .unwrap_err();
        assert!(denied(&e), "{table}: {e}");
    }
    let e = sqlx::query("SELECT requested_by, requested_by_id FROM requests")
        .execute(&face)
        .await
        .unwrap_err();
    assert!(denied(&e), "{e}");
    // The availability view has a state per title and nothing about who asked.
    let columns: Vec<String> = sqlx::query_scalar(
        "SELECT column_name::text FROM information_schema.columns
         WHERE table_name = 'face_availability' ORDER BY ordinal_position",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(columns, vec!["kind", "tmdb_id", "state"]);
    let marks = rs::face_availability(&face, &[603, 999_001, 1396, 777, 424_242])
        .await
        .unwrap();
    let state = |id: i64| {
        marks
            .iter()
            .find(|(_, t, _)| *t == id)
            .map(|(_, _, s)| s.as_str())
    };
    assert_eq!(state(999_001), Some("requested"));
    assert_eq!(state(1396), Some("processing"));
    assert_eq!(
        state(777),
        None,
        "a 4K request does not count on the HD face"
    );
    assert_eq!(state(424_242), None);
    let seasons = rs::face_seasons(&face, 1396).await.unwrap();
    assert_eq!(
        seasons,
        vec![(1, "processing".to_string()), (2, "processing".to_string())]
    );
    assert_eq!(rs::face_discover(&face).await.unwrap().len(), 2);
    let rules = rs::face_rules(&face).await.unwrap().unwrap();
    assert_eq!(rules.max_seasons, 2);

    // The sign-in check names one account at a time.
    let friend = rs::face_account(&face, 1_000_007).await.unwrap().unwrap();
    assert_eq!(
        (
            friend.seerr_user_id,
            friend.display_name.as_str(),
            friend.is_owner
        ),
        (7, "Bright Friend", false)
    );
    assert!(rs::face_account(&face, 1_000_404).await.unwrap().is_none());
    let none: Option<i64> = sqlx::query_scalar("SELECT seerr_user_id FROM face_account")
        .fetch_optional(&face)
        .await
        .unwrap();
    assert_eq!(none, None, "without a named account the view is empty");

    // A session: only for an account the Seerr integration knows; the Seerr account comes from the
    // database, whatever the face says.
    let token = singularr::face::session::new_token();
    let hash = singularr::face::session::token_sha256(&token);
    let in_a_month = chrono::Utc::now() + chrono::Duration::days(30);
    rs::insert_session(&face, &hash, 1_000_007, in_a_month)
        .await
        .unwrap();
    let stranger = singularr::face::session::new_token();
    assert!(
        rs::insert_session(
            &face,
            &singularr::face::session::token_sha256(&stranger),
            1_000_404,
            in_a_month
        )
        .await
        .is_err()
    );
    let e = sqlx::query("INSERT INTO request_sessions (token_sha256, plex_account_id, expires_at, seerr_user_id) VALUES ($1, 1000009, now(), 1)")
        .bind(singularr::face::session::token_sha256(&stranger))
        .execute(&face)
        .await
        .unwrap_err();
    assert!(denied(&e), "the face cannot name the Seerr account: {e}");
    let me = rs::face_session(&face, &token).await.unwrap().unwrap();
    assert_eq!(
        (
            me.plex_account_id,
            me.seerr_user_id,
            me.display_name.as_str()
        ),
        (1_000_007, 7, "Bright Friend")
    );
    assert!(rs::face_session(&face, &stranger).await.unwrap().is_none());
    assert!(rs::face_session(&face, "").await.unwrap().is_none());

    // An intent: who it is for comes from the session. HD only.
    let w = rs::IntentWrite {
        kind: "show",
        tmdb_id: 1396,
        seasons: &[3, 4, 5],
        title: Some("Breaking Bad"),
        year: Some(2008),
    };
    rs::insert_intent(&face, &token, &w).await.unwrap();
    assert!(
        rs::insert_intent(&face, &stranger, &w).await.is_err(),
        "no session, no intent"
    );
    let e = sqlx::query(
        "INSERT INTO request_intents (kind, tmdb_id, is_4k) VALUES ('movie', 603, true)",
    )
    .execute(&face)
    .await
    .unwrap_err();
    assert!(denied(&e), "the face cannot ask for 4K: {e}");
    let e = sqlx::query(
        "INSERT INTO request_intents (kind, tmdb_id, state) VALUES ('movie', 603, 'shadow')",
    )
    .execute(&face)
    .await
    .unwrap_err();
    assert!(denied(&e), "{e}");
    let e = sqlx::query("UPDATE request_intents SET seasons = '{1}'")
        .execute(&face)
        .await
        .unwrap_err();
    assert!(denied(&e), "{e}");
    let row: (i64, i64, String, bool) =
        sqlx::query_as("SELECT plex_account_id, seerr_user_id, state, is_4k FROM request_intents")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(row, (1_000_007, 7, "new".to_string(), false));
    // The database itself refuses a 4K intent, whoever writes it.
    assert!(
        sqlx::query("UPDATE request_intents SET is_4k = true")
            .execute(&t.pool)
            .await
            .is_err()
    );

    // My requests: the friend's own, here and on the Plex site; not the other friend's.
    let mine = rs::face_my_requests(&face, &token).await.unwrap();
    assert_eq!(mine.len(), 2, "{mine:?}");
    assert!(
        mine.iter()
            .any(|r| r.source == "intent" && r.seasons == vec![3, 4, 5])
    );
    assert!(
        mine.iter()
            .any(|r| r.source == "seerr" && r.title.as_deref() == Some("Breaking Bad"))
    );
    assert!(mine.iter().all(|r| r.tmdb_id != Some(999_001)));

    // The full process records what it would send, and never calls Seerr.
    let rules = request_rules();
    let handled = singularr::requests::intents::process_new(&t.pool, &journal, &rules)
        .await
        .unwrap();
    assert_eq!(handled, 1);
    let page = singularr::requests::intents::list(&t.pool, Some("shadow"), None, None)
        .await
        .unwrap();
    assert_eq!(page.total, 1);
    let outcome = page.items[0].outcome.clone().unwrap();
    assert_eq!(outcome["sent"], false);
    // The friend holds seasons 1 and 2 through Seerr: the gate has no room for more, so
    // nothing would be sent.
    assert_eq!(outcome["gate"]["result"], "decline");
    assert!(outcome["would_send"].is_null());
    assert!(recorded.urls_seen().iter().all(|u| !u.contains("5055")));
    let events = kinds(&t.pool, 0).await;
    assert_eq!(count(&events, "request_intent_shadowed"), 1);
    let mine = rs::face_my_requests(&face, &token).await.unwrap();
    let intent = mine.iter().find(|r| r.source == "intent").unwrap();
    assert_eq!(
        (intent.status.as_str(), intent.gate.as_deref()),
        ("shadow", Some("decline"))
    );
    assert!(intent.gate_reason.is_some());
    assert!(
        singularr::requests::intents::list(&t.pool, Some("nonsense"), None, None)
            .await
            .is_err()
    );

    // Sign-out ends the session.
    rs::end_session(&face, &hash).await.unwrap();
    rs::end_session(&face, &hash).await.unwrap();
    assert!(rs::face_session(&face, &token).await.unwrap().is_none());
    assert!(
        rs::face_my_requests(&face, &token)
            .await
            .unwrap()
            .is_empty()
    );
    drop_role(t, &role, face).await;
}

#[tokio::test]
async fn the_full_process_keeps_the_portal_role() {
    // Platform step A4: after the migrations, with a portal password set, the full process runs
    // deploy/requests-role.sql for the portal's role. The password reaches the database as a
    // SCRAM-SHA-256 verifier computed in process. A second start changes nothing. A grant the
    // role lost is back at the next start. A login that may not create roles gets one warning
    // naming the script. The checks of the role's views are the_face_role_reads_its_views_and_nothing_else,
    // on a role made the same way (face_role).
    let Some(t) = TestDb::new("portalrole").await else {
        return;
    };
    face_data(&t.pool).await;
    let role = format!("mq_face_{}", uuid_like());
    let password = portal_password();
    let config = portal_config(&role);
    let unset = PortalRoleConfig {
        role: Some(role.clone()),
        password: None,
    };
    assert_eq!(portal_role::keep(&t.pool, &unset).await, Kept::NotAsked);
    assert!(!role_exists(&t.pool, &role).await);

    assert_eq!(
        portal_role::keep(&t.pool, &config).await,
        Kept::Created { role: role.clone() }
    );
    let attributes: (bool, bool, bool, bool, bool, bool, bool, i32) = sqlx::query_as(
        "SELECT rolcanlogin, rolsuper, rolinherit, rolcreaterole, rolcreatedb, rolreplication,
                rolbypassrls, rolconnlimit
         FROM pg_roles WHERE rolname::text = $1::text",
    )
    .bind(&role)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        attributes,
        (true, false, false, false, false, false, false, 10),
        "a login and nothing more, at most 10 connections"
    );
    // The password is the verifier the process sent, kept as it was: computed again from its own
    // salt and iterations it is the same text. A password the server had hashed itself would
    // not be.
    let stored: String =
        sqlx::query_scalar("SELECT rolpassword FROM pg_authid WHERE rolname::text = $1::text")
            .bind(&role)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    let (iterations, salt) = stored
        .strip_prefix("SCRAM-SHA-256$")
        .and_then(|rest| rest.split_once('$'))
        .and_then(|(head, _)| head.split_once(':'))
        .map(|(i, s)| (i.parse::<u32>().unwrap(), base64_decoded(s)))
        .unwrap_or_else(|| panic!("not a SCRAM-SHA-256 verifier: {stored}"));
    assert_eq!((iterations, salt.len()), (4096, 16));
    assert_eq!(
        portal_role::verifier(&password, &salt, iterations).as_deref(),
        Some(stored.as_str())
    );
    assert!(!stored.contains(&password));

    // A second start changes nothing: the role, its password and its grants are as they were.
    let before = role_state(&t.pool, &role).await;
    for grant in [
        "rel face_rules ",
        "rel face_my_requests ",
        "col request_intents.kind ",
        "col request_sessions.token_sha256 ",
        "schema public ",
        "db  ",
        "role ",
    ] {
        assert!(
            before.iter().any(|line| line.starts_with(grant)),
            "{grant}: {before:#?}"
        );
    }
    assert!(
        !before.iter().any(|line| line.starts_with("rel requests ")),
        "{before:#?}"
    );
    let granted = Kept::Granted {
        role: role.clone(),
        password_set: false,
    };
    assert_eq!(portal_role::keep(&t.pool, &config).await, granted);
    assert_eq!(role_state(&t.pool, &role).await, before);

    // A grant the role does not hold, such as the view of a later migration, is there after the
    // next start.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE SELECT ON face_rules FROM {role}"
    )))
    .execute(&t.pool)
    .await
    .unwrap();
    assert_ne!(role_state(&t.pool, &role).await, before);
    assert_eq!(portal_role::keep(&t.pool, &config).await, granted);
    assert_eq!(role_state(&t.pool, &role).await, before);

    // The role signs in with the password over TCP. It reads its views. It reads no table.
    let face = sign_in(&t.name, &role, &password).await;
    let rules = singularr::requests::store::face_rules(&face)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rules.max_seasons, 2);
    let e = sqlx::query("SELECT 1 FROM requests LIMIT 1")
        .execute(&face)
        .await
        .unwrap_err();
    assert!(denied(&e), "{e}");

    // A role that is no portal role is left alone: this login itself, a role with more than a
    // login, a word SQL reserves.
    let me: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let strong = format!("mq_strong_{}", uuid_like());
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE ROLE {strong} LOGIN CREATEDB"
    )))
    .execute(&t.pool)
    .await
    .unwrap();
    for (other, why) in [
        (me.as_str(), "this database login itself"),
        (strong.as_str(), "more than a login"),
        ("user", "a word SQL reserves"),
    ] {
        let kept = portal_role::keep(&t.pool, &portal_config(other)).await;
        assert!(
            matches!(&kept, Kept::Warning(w) if w.contains(why) && w.contains("deploy/requests-role.sql")),
            "{other}: {kept:?}"
        );
    }
    assert!(
        role_state(&t.pool, &strong)
            .await
            .iter()
            .all(|line| line.starts_with("role ")),
        "no grant to a role that is no portal role"
    );

    // An existing role is taken only when it looks like a portal role. Each of these is one
    // warning, its grants left as they are.
    let id = uuid_like();
    let nologin = format!("mq_nologin_{id}");
    let group = format!("mq_group_{id}");
    let member = format!("mq_member_{id}");
    let owner = format!("mq_objowner_{id}");
    for statement in [
        format!("CREATE ROLE {nologin} NOLOGIN"),
        format!("CREATE ROLE {group} LOGIN"),
        format!("CREATE ROLE {member} LOGIN"),
        format!("GRANT {group} TO {member}"),
        format!("CREATE ROLE {owner} LOGIN"),
        "CREATE TABLE owned_by_another (id int)".to_string(),
        format!("ALTER TABLE owned_by_another OWNER TO {owner}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(statement))
            .execute(&t.pool)
            .await
            .unwrap();
    }
    for (other, why) in [
        (nologin.as_str(), "may not log in"),
        (group.as_str(), "members other than this login"),
        (member.as_str(), "memberships of its own"),
        (owner.as_str(), "the owner of objects in the schema public"),
        ("pg_monitor", "a name PostgreSQL keeps"),
    ] {
        let kept = portal_role::keep(&t.pool, &portal_config(other)).await;
        assert!(
            matches!(&kept, Kept::Warning(w) if w.contains(why)),
            "{other}: {kept:?}"
        );
        assert!(
            role_state(&t.pool, other)
                .await
                .iter()
                .all(|line| line.starts_with("role ")),
            "{other} got a grant"
        );
    }

    // A login that may not create roles: the missing role is one warning naming the script. The
    // start goes on without it.
    let plain = format!("mq_login_{}", uuid_like());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE ROLE {plain} LOGIN")))
        .execute(&t.pool)
        .await
        .unwrap();
    give_password(&t.pool, &plain, LOGIN_PASSWORD).await;
    let as_plain = sign_in(&t.name, &plain, LOGIN_PASSWORD).await;
    let missing = format!("mq_face_{}", uuid_like());
    let kept = portal_role::keep(&as_plain, &portal_config(&missing)).await;
    let Kept::Warning(warning) = &kept else {
        panic!("{kept:?}");
    };
    for part in ["deploy/requests-role.sql", "CREATEROLE", missing.as_str()] {
        assert!(warning.contains(part), "{part}: {warning}");
    }
    assert!(!warning.contains(&password));
    assert!(!role_exists(&t.pool, &missing).await);
    as_plain.close().await;

    face.close().await;
    let admin = t.admin.clone();
    t.drop_db().await;
    drop_roles(
        &admin,
        &[
            role.as_str(),
            strong.as_str(),
            plain.as_str(),
            nologin.as_str(),
            member.as_str(),
            group.as_str(),
            owner.as_str(),
        ],
    )
    .await;
}

#[tokio::test]
async fn an_existing_portal_role_needs_no_createrole() {
    // As on a shared database server. The database belongs to a login without CREATEROLE. That
    // login applies the migrations and signs in over TCP with a password. The portal's role was
    // made by hand with the script as the server's superuser, its password set as psql's
    // \password sets it. The login keeps the role: its grants again, no warning, its password
    // as it is.
    let Some((t, tenant, superuser)) = owned_db("tenant", false).await else {
        return;
    };
    let role = format!("mq_face_{}", uuid_like());
    sqlx::raw_sql(sqlx::AssertSqlSafe(portal_role::script_for(&role)))
        .execute(&superuser)
        .await
        .expect("deploy/requests-role.sql applies by hand");
    give_password(&superuser, &role, &portal_password()).await;
    let before = role_state(&superuser, &role).await;
    assert!(
        before
            .iter()
            .any(|line| line.starts_with("rel face_discover ")),
        "{before:#?}"
    );
    let config = portal_config(&role);
    let granted = Kept::Granted {
        role: role.clone(),
        password_set: false,
    };
    assert_eq!(portal_role::keep(&t.pool, &config).await, granted);
    assert_eq!(role_state(&superuser, &role).await, before);
    // The grants are the owner's to give: a lost one is back at the next start.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "REVOKE SELECT ON face_discover FROM {role}"
    )))
    .execute(&t.pool)
    .await
    .unwrap();
    assert_eq!(portal_role::keep(&t.pool, &config).await, granted);
    assert_eq!(role_state(&superuser, &role).await, before);
    let face = sign_in(&t.name, &role, &portal_password()).await;
    assert!(
        singularr::requests::store::face_discover(&face)
            .await
            .unwrap()
            .is_empty()
    );
    let e = sqlx::query("SELECT 1 FROM copies LIMIT 1")
        .execute(&face)
        .await
        .unwrap_err();
    assert!(denied(&e), "{e}");
    face.close().await;

    // Its password changed by hand: the configured one does not sign in, and this login may not
    // set it. One warning names psql's \password. Its grants stay as they were.
    give_password(&superuser, &role, "a-password-set-elsewhere").await;
    let kept = portal_role::keep(&t.pool, &config).await;
    assert!(
        matches!(&kept, Kept::Warning(w) if w.contains("\\password") && w.contains("may not change")),
        "{kept:?}"
    );
    let grants = |state: Vec<String>| -> Vec<String> {
        state
            .into_iter()
            .filter(|line| !line.starts_with("role "))
            .collect()
    };
    assert_eq!(
        grants(role_state(&superuser, &role).await),
        grants(before.clone())
    );

    // A missing role: one warning naming the script.
    let missing = format!("mq_face_{}", uuid_like());
    let kept = portal_role::keep(&t.pool, &portal_config(&missing)).await;
    assert!(
        matches!(&kept, Kept::Warning(w) if w.contains("deploy/requests-role.sql") && w.contains("CREATEROLE")),
        "{kept:?}"
    );
    assert!(!role_exists(&superuser, &missing).await);

    // The database's owner is no portal role.
    let kept = portal_role::keep(&superuser, &portal_config(&tenant)).await;
    assert!(
        matches!(&kept, Kept::Warning(w) if w.contains("the owner of this database")),
        "{kept:?}"
    );

    superuser.close().await;
    let admin = t.admin.clone();
    t.drop_db().await;
    drop_roles(&admin, &[role.as_str(), tenant.as_str()]).await;
}

#[tokio::test]
async fn a_portal_role_with_another_password_gets_the_configured_one() {
    // The database's owner may create roles. It made the portal's role at the first start. Then
    // the role's password changed outside Singularr. The next start sets the configured one
    // (password_set). The start after changes nothing. The portal signs in with SCRAM-SHA-256.
    let Some((t, owner, superuser)) = owned_db("creator", true).await else {
        return;
    };
    let role = format!("mq_face_{}", uuid_like());
    let config = portal_config(&role);
    assert_eq!(
        portal_role::keep(&t.pool, &config).await,
        Kept::Created { role: role.clone() }
    );
    give_password(&superuser, &role, "a-password-set-elsewhere").await;
    assert!(refused(&t.name, &role, &portal_password()).await);
    assert_eq!(
        portal_role::keep(&t.pool, &config).await,
        Kept::Granted {
            role: role.clone(),
            password_set: true
        }
    );
    let stored = role_state(&superuser, &role).await;
    assert_eq!(
        portal_role::keep(&t.pool, &config).await,
        Kept::Granted {
            role: role.clone(),
            password_set: false
        }
    );
    assert_eq!(role_state(&superuser, &role).await, stored);
    let face = sign_in(&t.name, &role, &portal_password()).await;
    assert!(
        singularr::requests::store::face_discover(&face)
            .await
            .unwrap()
            .is_empty()
    );
    face.close().await;
    assert!(refused(&t.name, &role, "a-password-set-elsewhere").await);

    superuser.close().await;
    let admin = t.admin.clone();
    t.drop_db().await;
    drop_roles(&admin, &[role.as_str(), owner.as_str()]).await;
}

#[tokio::test]
async fn a_friend_signs_in_with_plex_and_asks_for_a_movie() {
    // End to end as the face's own role: the sign-in and every signed-in page go through the
    // views of migration 0005, which read the transaction settings `marquee.session_token` and
    // `marquee.plex_account` by name. A rename on either side fails this test.
    let Some(t) = TestDb::new("facesignin").await else {
        return;
    };
    face_data(&t.pool).await;
    let (role, face) = face_role(&t).await;
    let recorded = Recorded::new();
    recorded.answer(
        Method::Post,
        "https://plex.tv/api/v2/pins",
        201,
        r#"{"id": 4242, "code": "abcd1234"}"#,
    );
    recorded.answer(
        Method::Get,
        "https://plex.tv/api/v2/pins/4242",
        200,
        r#"{"id": 4242, "code": "abcd1234", "authToken": "plex-token"}"#,
    );
    recorded.answer(
        Method::Get,
        "https://plex.tv/api/v2/user",
        200,
        r#"{"id": 1000007, "username": "brightfriend", "email": "friend@example.org"}"#,
    );
    recorded.answer(
        Method::Get,
        "https://api.themoviedb.org/3/movie/603",
        200,
        r#"{"id": 603, "title": "The Matrix", "release_date": "1999-03-31", "overview": "A hacker learns the truth.", "runtime": 136, "genres": [{"name": "Action"}], "poster_path": "/m.jpg"}"#,
    );
    let config = singularr::core::config::RequestsFaceConfig::from(&BTreeMap::from([
        (
            "SINGULARR_REQUESTS_DATABASE_URL",
            "postgres://x@127.0.0.1:1/x",
        ),
        (
            "SINGULARR_REQUESTS_SESSION_SECRET",
            "0123456789abcdef0123456789abcdef-test",
        ),
        ("SINGULARR_TMDB_TOKEN", "tmdb-test-token"),
        (
            "SINGULARR_REQUESTS_PUBLIC_URL",
            "https://requests.example.org",
        ),
    ]))
    .unwrap();
    let out = Outbound::with_transport(Allowlist::for_requests_face(), recorded.clone());
    let app = singularr::face::routes::router(singularr::face::routes::FaceState::new(
        config,
        face.clone(),
        out,
    ));
    let send = |method: &str, path: &str, cookie: Option<&str>, body: Option<&str>| {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("origin", "https://requests.example.org")
            .header("sec-fetch-site", "same-origin");
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        if body.is_some() {
            req = req.header("content-type", "application/x-www-form-urlencoded");
        }
        let req = req
            .body(Body::from(body.unwrap_or("").to_string()))
            .unwrap();
        let app = app.clone();
        async move {
            let res = app.oneshot(req).await.unwrap();
            let status = res.status();
            let headers = res.headers().clone();
            let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
                .await
                .unwrap();
            (
                status,
                headers,
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        }
    };
    let cookie_of = |headers: &axum::http::HeaderMap, name: &str| -> Option<String> {
        headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|v| v.starts_with(&format!("{name}=")) && !v.starts_with(&format!("{name};")))
            .and_then(|v| v.split(';').next())
            .map(str::to_string)
            .filter(|kv| !kv.ends_with('='))
    };

    // Not signed in: every page goes to the sign-in.
    let (status, headers, _) = send("GET", "/", None, None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/signin");
    let (status, headers, body) = send("GET", "/signin", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Sign in with Plex"));
    assert!(
        headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("default-src 'none'")
    );

    // Start: a PIN, a signed cookie, and off to Plex.
    let (status, headers, _) = send("POST", "/signin", None, Some("")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = headers["location"].to_str().unwrap();
    assert!(
        location.starts_with("https://app.plex.tv/auth#?clientID="),
        "{location}"
    );
    assert!(location.contains("forwardUrl=https%3A%2F%2Frequests.example.org%2Fsignin%2Fplex"));
    let pin = cookie_of(&headers, "__Host-mq_pin").expect("the PIN cookie");
    let raw = headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(raw.contains("Secure") && raw.contains("HttpOnly") && raw.contains("SameSite=Lax"));
    assert!(!raw.to_ascii_lowercase().contains("domain="));

    // Back from Plex: the account is named, the token dropped, the session starts.
    let (status, headers, _) = send(
        "GET",
        "/signin/plex",
        Some(&format!("_oauth2_proxy=gate; {pin}")),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/");
    let session = cookie_of(&headers, "__Host-mq_session").expect("the session cookie");
    let stored: Vec<String> = sqlx::query_scalar("SELECT token_sha256 FROM request_sessions")
        .fetch_all(&t.pool)
        .await
        .unwrap();
    let token = session.split_once('=').unwrap().1.to_string();
    assert_eq!(
        stored,
        vec![singularr::face::session::token_sha256(&token)],
        "only the hash is stored"
    );
    let everything: String =
        sqlx::query_scalar("SELECT string_agg(row_to_json(s)::text, ' ') FROM request_sessions s")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert!(!everything.contains("plex-token") && !everything.contains('@'));

    // Signed in: Discover, a title page, a request.
    let (status, _, body) = send("GET", "/", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("The Matrix") && body.contains("A Movie Not Yet Held"));
    assert!(
        body.contains(">Requested<"),
        "the other friend's request shows as a state"
    );
    assert!(!body.contains("Other Friend"), "never by whom");
    assert!(
        !body.contains("singularr.example.org"),
        "a friend sees no link to the owner face"
    );
    let (status, _, body) = send("GET", "/movie/603", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("A hacker learns the truth.") && body.contains(">Request this movie<"));
    // 4K is refused, and so is a request from another site.
    let (status, _, _) = send(
        "POST",
        "/movie/603/request",
        Some(&session),
        Some("is4k=true"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = send("POST", "/movie/603/request?4k=1", Some(&session), Some("")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let cross = Request::builder()
        .method("POST")
        .uri("/movie/603/request")
        .header("cookie", &session)
        .header("sec-fetch-site", "cross-site")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(cross).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    let (status, headers, _) = send("POST", "/movie/603/request", Some(&session), Some("")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/requests?asked=1");
    let (status, _, body) = send("GET", "/requests?asked=1", Some(&session), None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("The Matrix") && body.contains("Test run"));
    assert!(
        body.contains("You requested The Matrix. Test run: nothing is sent."),
        "the confirmation names what was asked for"
    );
    let asked: (i64, String, bool) =
        sqlx::query_as("SELECT tmdb_id, kind, is_4k FROM request_intents")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(asked, (603, "movie".to_string(), false));
    // The full process records what it would send: the movie, in HD, as the friend's account.
    let journal = Journal::new();
    singularr::requests::intents::process_new(&t.pool, &journal, &request_rules())
        .await
        .unwrap();
    let outcome: serde_json::Value =
        sqlx::query_scalar("SELECT outcome FROM request_intents WHERE tmdb_id = 603")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    let body = &outcome["would_send"]["body"];
    assert_eq!(
        (
            body["mediaType"].as_str(),
            body["mediaId"].as_i64(),
            body["userId"].as_i64(),
            body["is4k"].as_bool()
        ),
        (Some("movie"), Some(603), Some(7), Some(false))
    );
    assert_eq!(outcome["sent"], false);
    // Nothing reached Seerr or any engine.
    assert!(
        recorded
            .urls_seen()
            .iter()
            .all(|u| u.contains("plex.tv") || u.contains("themoviedb.org"))
    );

    // Sign-out ends it.
    let (status, headers, _) = send("POST", "/signout", Some(&session), Some("")).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(headers["location"], "/signin");
    let (status, _, _) = send("GET", "/", Some(&session), None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // A Plex account the Seerr integration does not know is refused.
    let unknown = Recorded::new();
    unknown.answer(
        Method::Get,
        "https://plex.tv/api/v2/pins/4243",
        200,
        r#"{"id": 4243, "authToken": "t"}"#,
    );
    unknown.answer(
        Method::Get,
        "https://plex.tv/api/v2/user",
        200,
        r#"{"id": 1000404, "username": "stranger"}"#,
    );
    let config = singularr::core::config::RequestsFaceConfig::from(&BTreeMap::from([
        (
            "SINGULARR_REQUESTS_DATABASE_URL",
            "postgres://x@127.0.0.1:1/x",
        ),
        (
            "SINGULARR_REQUESTS_SESSION_SECRET",
            "0123456789abcdef0123456789abcdef-test",
        ),
        (
            "SINGULARR_REQUESTS_PUBLIC_URL",
            "https://requests.example.org",
        ),
    ]))
    .unwrap();
    let pin = singularr::face::session::pin_cookie_value(
        &config.session_secret,
        4243,
        chrono::Utc::now().timestamp() + 600,
    );
    let other = singularr::face::routes::router(singularr::face::routes::FaceState::new(
        config,
        face.clone(),
        Outbound::with_transport(Allowlist::for_requests_face(), unknown),
    ));
    let req = Request::builder()
        .uri("/signin/plex")
        .header("cookie", format!("__Host-mq_pin={pin}"))
        .body(Body::empty())
        .unwrap();
    let res = other.oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
        res.headers()
            .get_all("set-cookie")
            .iter()
            .all(|v| !v.to_str().unwrap().starts_with("__Host-mq_session="))
    );
    drop_role(t, &role, face).await;
}

#[tokio::test]
async fn the_accounts_of_a_site_are_read_without_addresses() {
    let Some(t) = TestDb::new("seerrusers").await else {
        return;
    };
    let recorded = Recorded::new();
    recorded.answer(
        Method::Get,
        &format!("{SEERR}/api/v1/user?take=100&skip=0"),
        200,
        &fixture("seerr_users.json"),
    );
    let out = outbound(recorded.clone());
    let site = seerr_site(&out);
    let report = singularr::requests::sync::users_pass(&t.pool, &site, &request_rules())
        .await
        .unwrap();
    assert_eq!((report.read, report.unreadable), (3, 1));
    assert!(!report.complete, "an unreadable account keeps the others");
    let rows: Vec<(i64, String, Option<i64>, bool)> = sqlx::query_as(
        "SELECT seerr_user_id, display_name, plex_id, is_owner FROM seerr_users ORDER BY seerr_user_id",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "Owner Account".to_string(), Some(1_000_001), true),
            (7, "brightfriend".to_string(), Some(1_000_007), false),
            (9, "Local Only".to_string(), None, false),
        ]
    );
    let all: String =
        sqlx::query_scalar("SELECT string_agg(row_to_json(u)::text, ' ') FROM seerr_users u")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert!(!all.contains('@'), "no address is stored: {all}");
    assert_eq!(recorded.methods_seen(), vec![Method::Get]);
    t.drop_db().await;
}

#[tokio::test]
async fn rows_of_an_integration_the_configuration_no_longer_lists_are_removed() {
    let Some(t) = TestDb::new("prune").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    let out = outbound(recorded.clone());
    let engine: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("radarr", "radarr", Some("radarr"))]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines.clone(), &policy);
    titles_pass(&deps, &engine, true).await.unwrap();
    let before = core_store::latest_event_id(&t.pool).await.unwrap();
    let config = |extra: &[(&'static str, &'static str)]| {
        let mut env = BTreeMap::from([("DATABASE_URL", "postgres://x@127.0.0.1:1/x")]);
        env.extend(extra.iter().copied());
        Config::from(&env).unwrap()
    };

    // The configuration still lists Radarr, under a name: nothing goes, the library is named.
    let listed = config(&[("RADARR_URL", RADARR), ("RADARR_NAME", "Movies HD")]);
    let report = singularr::api::state::prune_unconfigured(&t.pool, &journal, &listed)
        .await
        .unwrap();
    assert!(report.is_empty(), "{report:?}");
    singularr::api::state::sync_libraries(&t.pool, &listed)
        .await
        .unwrap();
    let libraries = catalog::libraries(&t.pool).await.unwrap();
    assert_eq!(libraries.len(), 1);
    assert_eq!(
        (
            libraries[0].id.as_str(),
            libraries[0].name.as_str(),
            libraries[0].kind.as_deref(),
            libraries[0].integration.as_deref()
        ),
        ("radarr", "Movies HD", Some("movie"), Some("radarr"))
    );
    // An empty configuration removes nothing: it is more likely a missing .env.
    let empty = singularr::api::state::prune_unconfigured(&t.pool, &journal, &config(&[]))
        .await
        .unwrap();
    assert!(empty.is_empty());

    // Radarr is no longer listed: its rows go, with one event, and the journal keeps the rest.
    let report = singularr::api::state::prune_unconfigured(
        &t.pool,
        &journal,
        &config(&[("SONARR_URL", SONARR)]),
    )
    .await
    .unwrap();
    assert_eq!(report.catalog.engines, vec!["radarr"]);
    assert_eq!((report.catalog.titles, report.catalog.copies), (3, 2));
    assert_eq!(report.catalog.libraries, 1);
    let all = catalog::titles(&t.pool, &lanes(), &TitleQuery::default())
        .await
        .unwrap();
    assert_eq!(all.total, 0);
    assert!(catalog::libraries(&t.pool).await.unwrap().is_empty());
    let events = kinds(&t.pool, before).await;
    assert_eq!(events, vec!["integrations_pruned".to_string()]);
    assert_eq!(count(&kinds(&t.pool, 0).await, "title_added"), 3);
    t.drop_db().await;
}

#[tokio::test]
async fn the_metadata_routes_answer_from_tmdb_with_the_library_state() {
    let Some(t) = TestDb::new("metadata").await else {
        return;
    };
    // The catalog of the recorded Radarr: 10 Cloverfield Lane held, A Movie Not Yet Held
    // listed without a file. And a request for The Matrix Reloaded.
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    let tmdb = |path: &str| format!("https://api.themoviedb.org/3{path}");
    recorded.answer(
        Method::Get,
        &tmdb("/movie/603?append_to_response="),
        200,
        &fixture("tmdb_movie_603.json"),
    );
    let held = fixture("tmdb_movie_603.json").replacen("\"id\": 603", "\"id\": 333371", 1);
    recorded.answer(
        Method::Get,
        &tmdb("/movie/333371?append_to_response="),
        200,
        &held,
    );
    let reloaded = fixture("tmdb_movie_603.json").replacen("\"id\": 603", "\"id\": 604", 1);
    recorded.answer(
        Method::Get,
        &tmdb("/movie/604?append_to_response="),
        200,
        &reloaded,
    );
    recorded.answer(
        Method::Get,
        &tmdb("/tv/1396/season/1"),
        200,
        &fixture("tmdb_tv_1396_season_1.json"),
    );
    recorded.answer(
        Method::Get,
        &tmdb("/person/6384?append_to_response="),
        200,
        &fixture("tmdb_person_6384.json"),
    );
    recorded.answer(
        Method::Get,
        &tmdb("/collection/2344"),
        200,
        &fixture("tmdb_collection_2344.json"),
    );
    recorded.answer(
        Method::Get,
        &tmdb("/discover/movie?with_genres=28"),
        200,
        &fixture("tmdb_trending.json"),
    );
    recorded.answer(
        Method::Get,
        &tmdb("/genre/movie/list"),
        200,
        &fixture("tmdb_genre_movie_list.json"),
    );
    recorded.answer(
        Method::Get,
        &tmdb("/search/multi?query=matrix"),
        200,
        &fixture("tmdb_search_multi.json"),
    );
    recorded.answer(
        Method::Get,
        &tmdb("/search/collection?query=matrix"),
        200,
        &fixture("tmdb_search_collection.json"),
    );
    let out = outbound(recorded.clone());
    let engine: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("radarr", "radarr", Some("radarr"))]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines, &policy);
    titles_pass(&deps, &engine, true).await.unwrap();
    sqlx::raw_sql(
        "INSERT INTO requests (site, seerr_id, kind, tmdb_id, is_4k, request_status, media_status,
                               status, requested_by, requested_by_id, requested_at, seasons)
         VALUES ('plex', 900, 'movie', 604, false, 'pending', 'pending', 'pending', 'Friend', 7, now(), '{}')",
    )
    .execute(&t.pool)
    .await
    .unwrap();

    let st = state_on(
        &t.pool,
        journal.clone(),
        &[("SINGULARR_TMDB_TOKEN", "test-token")],
        recorded.clone(),
    );
    // The search index of what the library has, as the index loop writes it.
    let report = singularr::search::index::pass(&t.pool, &st.search, &st.metadata)
        .await
        .unwrap();
    assert!(
        report.builtin.is_ok() && report.meilisearch.is_none(),
        "{report:?}"
    );
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let get = |path: &'static str| {
        let app = app.clone();
        async move {
            let (status, _, body) = call(&app, "GET", path, &session, None).await;
            (status, body)
        }
    };

    // A title outside the library: every card says what the library holds of it.
    let (status, matrix) = get("/api/v1/metadata/titles/movie/603").await;
    assert_eq!(status, StatusCode::OK, "{matrix}");
    assert_eq!(matrix["id"], "movie-603");
    assert!(matrix["library"].is_null());
    let state_of = |id: &str| {
        matrix["recommendations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .map(|c| c["state"].clone())
            .unwrap()
    };
    assert_eq!(state_of("movie-604"), "requested");
    assert_eq!(state_of("movie-333371"), "available");
    assert_eq!(state_of("movie-999001"), "missing");
    assert!(
        matrix["similar"][0].get("state").is_none(),
        "a title nobody holds or asked for has no state"
    );
    assert_eq!(matrix["cast"][0]["id"], "person-6384");
    // The second answer comes from the cache: TMDB was asked once.
    let (status, _) = get("/api/v1/metadata/titles/movie/603").await;
    assert_eq!(status, StatusCode::OK);
    let asked = |part: &str| {
        recorded
            .urls_seen()
            .iter()
            .filter(|u| u.contains(part))
            .count()
    };
    assert_eq!(asked("/3/movie/603?"), 1);

    // A title the library holds carries its library facts.
    let (status, held) = get("/api/v1/metadata/titles/movie/333371").await;
    assert_eq!(status, StatusCode::OK, "{held}");
    assert_eq!(held["library"]["state"], "available");
    assert_eq!(held["state"], "available");
    // Without a Plex server the library is the manager's.
    assert_eq!(held["library"]["libraryId"], "radarr");
    assert!(held["library"]["library"].is_string());
    assert!(
        held["library"]["plays"].is_null(),
        "plays is null, never a number"
    );
    assert!(held["library"].get("plex").is_none());
    let editions = held["library"]["editions"].as_array().unwrap();
    assert!(!editions.is_empty());
    let file = &editions[0]["files"][0];
    assert!(
        file["fileName"].as_str().unwrap().ends_with(".mkv"),
        "{file}"
    );
    assert!(file["size"].as_i64().unwrap() > 0);
    // A file reads here as it reads in the library table: the product's words, never the
    // names of the parser's values (`H265`, `Eac3`, `WEBDL`).
    assert_eq!(
        (
            file["source"].as_str(),
            file["codec"].as_str(),
            file["hdr"].as_str(),
            file["audio"].as_str()
        ),
        (
            Some("BluRay"),
            Some("HEVC"),
            Some("DV HDR10"),
            Some("EAC3 Atmos 5.1")
        ),
        "{file}"
    );
    assert!(file.get("cutoffMet").is_some() && file["trace"].is_array());
    assert!(
        file["resolution"].is_number() || file["resolution"].is_null(),
        "{file}"
    );
    assert!(held["library"]["protections"].is_array());
    // The history of a held title: the journal's events about it, newest first.
    let history = held["history"].as_array().unwrap();
    assert!(
        history.iter().any(|h| h["event"] == "imported"),
        "{history:?}"
    );
    assert!(history.iter().any(|h| h["event"] == "added"), "{history:?}");
    // Cast and crew with their totals, and everything of them on the credits route.
    assert_eq!(held["castTotal"], held["cast"].as_array().unwrap().len());
    let (status, credits) = get("/api/v1/metadata/titles/movie/333371/credits").await;
    assert_eq!(status, StatusCode::OK, "{credits}");
    assert_eq!(credits["id"], "movie-333371");
    assert!(credits["crew"][0]["dept"].is_string());
    assert_eq!(
        asked("/3/movie/333371?"),
        1,
        "the credits come from the same answer"
    );
    // A requested title outside the library says so, with who asked.
    let (status, asked_for) = get("/api/v1/metadata/titles/movie/604").await;
    assert_eq!(status, StatusCode::OK, "{asked_for}");
    assert_eq!(asked_for["state"], "requested");
    assert_eq!(asked_for["requests"][0]["by"]["name"], "Friend");
    assert_eq!(asked_for["requests"][0]["what"]["type"], "movie");
    assert_eq!(asked_for["requests"][0]["target"], "HD");

    // A season of a show the library does not hold.
    let (status, season) = get("/api/v1/metadata/shows/1396/seasons/1").await;
    assert_eq!(status, StatusCode::OK, "{season}");
    assert_eq!(season["show"], "show-1396");
    assert_eq!(season["episodes"][0]["state"], "missing");
    assert_eq!(season["episodes"][2]["state"], "unaired");
    assert_eq!(season["have"], 0);

    // A person and a collection.
    let (status, person) = get("/api/v1/metadata/people/person-6384").await;
    assert_eq!(status, StatusCode::OK, "{person}");
    assert_eq!(person["credits"]["cast"][0]["role"], "Neo");
    let (status, collection) = get("/api/v1/metadata/collections/2344").await;
    assert_eq!(status, StatusCode::OK, "{collection}");
    assert_eq!(collection["parts"].as_array().unwrap().len(), 3);
    assert_eq!(collection["rich"], true);

    // The collections the library has a part of: 10 Cloverfield Lane's, from the stored
    // record when there is one, else the library's own parts.
    sqlx::query("UPDATE titles SET collection_tmdb_id = 2344, collection = 'The Matrix Collection' WHERE tmdb_id = 333371")
        .execute(&t.pool)
        .await
        .unwrap();
    let (status, all) = get("/api/v1/collections").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    let matrix_parts = &all["collection-2344"];
    assert_eq!(matrix_parts["rich"], true, "{all}");
    assert_eq!(matrix_parts["parts"].as_array().unwrap().len(), 3);
    sqlx::query("UPDATE titles SET collection_tmdb_id = 777, collection = 'Lane Collection' WHERE tmdb_id = 333371")
        .execute(&t.pool)
        .await
        .unwrap();
    recorded.answer(Method::Get, &tmdb("/collection/777"), 404, "{}");
    let (status, all) = get("/api/v1/collections").await;
    assert_eq!(status, StatusCode::OK, "{all}");
    let lane = &all["collection-777"];
    assert_eq!(
        (lane["name"].as_str(), lane["rich"].as_bool()),
        (Some("Lane Collection"), Some(false))
    );
    assert_eq!(lane["parts"][0]["id"], "movie-333371");
    assert_eq!(lane["parts"][0]["state"], "available");

    // A browse list by genre, named by TMDB's genre list.
    let (status, action) = get("/api/v1/discover/genre-movie-28").await;
    assert_eq!(status, StatusCode::OK, "{action}");
    assert_eq!(
        (action["title"].as_str(), action["kind"].as_str()),
        (Some("Action"), Some("movie"))
    );
    assert_eq!(action["totalPages"], 500);
    let (status, _) = get("/api/v1/discover/popular").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Search: the library's titles, the outside ones, people and collections.
    let (status, found) = get("/api/v1/search?q=matrix").await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["titles"][0]["id"], "movie-603");
    assert_eq!(found["people"][0]["id"], "person-6384");
    assert_eq!(found["collections"][0]["id"], "collection-2344");
    let (status, clover) = get("/api/v1/search?q=cloverfield").await;
    assert_eq!(status, StatusCode::OK, "{clover}");
    assert_eq!(clover["library"][0]["id"], "movie-333371");
    assert_eq!(clover["library"][0]["state"], "available");
    assert!(
        clover["collections"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "Cloverfield Collection"),
        "the library's collections are found too: {clover}"
    );
    assert!(clover["albums"].is_array() && clover["artists"].is_array());
    let (status, _) = get("/api/v1/search?q=").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Built-in search: its status, typo tolerance where the database has the trigram
    // extension, and a kind that narrows the answer.
    let (status, search_status) = get("/api/v1/search/status").await;
    assert_eq!(status, StatusCode::OK, "{search_status}");
    assert_eq!(search_status["provider"], "postgresql");
    assert_eq!(search_status["configured"], "postgresql");
    assert_eq!(search_status["state"], "ok", "{search_status}");
    assert!(
        search_status["documents"].as_i64().unwrap() >= 3,
        "{search_status}"
    );
    assert!(search_status["lastIndexed"].is_string());
    // The gate's PostgreSQL has the trigram extension (it ships with the server), so the
    // typo-tolerant query is the one tested here.
    assert_eq!(search_status["typoTolerant"], true, "{search_status}");
    let (status, typo) = get("/api/v1/search?q=cloverfeild").await;
    assert_eq!(status, StatusCode::OK, "{typo}");
    assert_eq!(typo["library"][0]["id"], "movie-333371", "{typo}");
    // The query a database without the extension gets: words and prefixes only.
    let kinds = vec!["movie".to_string()];
    let plain = singularr::search::store::search(
        &t.pool,
        &singularr::search::store::Query {
            phrase: "cloverf",
            tsquery: "cloverf:*",
            prefix: "cloverf%",
            contains: "%cloverf%",
            kinds: &kinds,
            limit: 5,
        },
        false,
    )
    .await
    .unwrap();
    assert_eq!(plain[0].0.id, "movie-333371", "{plain:?}");
    let (status, shows) = get("/api/v1/search?q=cloverfield&kind=show").await;
    assert_eq!(status, StatusCode::OK, "{shows}");
    assert!(shows["library"].as_array().unwrap().is_empty(), "{shows}");
    assert!(shows["people"].as_array().unwrap().is_empty());
    let (status, _) = get("/api/v1/search?q=x&kind=everything").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Discover in the client's shape: the billboard, the rows, the genres each with a
    // backdrop, the studios the library has titles of, and the recent requests.
    for list in [
        "/trending/all/week",
        "/movie/popular",
        "/movie/upcoming",
        "/tv/popular",
        "/tv/on_the_air",
    ] {
        recorded.answer(
            Method::Get,
            &tmdb(list),
            200,
            &fixture("tmdb_trending.json"),
        );
    }
    recorded.answer(
        Method::Get,
        &tmdb("/genre/tv/list"),
        200,
        r#"{"genres": [{"id": 18, "name": "Drama"}]}"#,
    );
    recorded.answer(
        Method::Get,
        &tmdb("/tv/1396?append_to_response="),
        200,
        &fixture("tmdb_tv_1396.json"),
    );
    sqlx::query("UPDATE titles SET studio = 'Warner Bros. Pictures' WHERE tmdb_id = 333371")
        .execute(&t.pool)
        .await
        .unwrap();
    let (status, disc) = get("/api/v1/discover").await;
    assert_eq!(status, StatusCode::OK, "{disc}");
    let rows: Vec<&str> = disc["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        rows,
        vec![
            "trending",
            "popular-movies",
            "upcoming-movies",
            "popular-shows",
            "on-the-air",
            "recent-requests"
        ]
    );
    assert_eq!(disc["billboard"][0]["id"], "movie-603", "{disc}");
    assert_eq!(disc["billboard"][0]["backdrop"], "/matrix-bd.jpg");
    assert_eq!(disc["billboard"][0]["certification"], "R");
    assert_eq!(disc["billboard"][1]["id"], "show-1396");
    let action = disc["genres"]["movie"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["key"] == "genre-movie-28")
        .unwrap();
    assert_eq!(action["backdrop"], "/matrix-bd.jpg");
    assert_eq!(disc["genres"]["show"][0]["key"], "genre-show-18");
    assert_eq!(disc["genres"]["show"][0]["backdrop"], "/bb-bd.jpg");
    assert_eq!(disc["studios"][0]["key"], "studio-174", "{disc}");
    assert_eq!(disc["studios"][0]["logo"], "/wb.png");
    assert!(disc["networks"].as_array().unwrap().is_empty());
    let recent = &disc["rows"][5]["items"][0];
    assert_eq!(
        (recent["id"].as_str(), recent["state"].as_str()),
        (Some("movie-604"), Some("requested"))
    );
    let (status, list) = get("/api/v1/discover/recent-requests").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["items"][0]["id"], "movie-604");

    // Nothing reached TMDB but GET.
    assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));

    // Without a TMDB token a record is a 502 that says why, and a stored one is still served.
    let bare = routes::router(state_on(&t.pool, journal.clone(), &[], Recorded::new()));
    let (status, _, body) = call(
        &bare,
        "GET",
        "/api/v1/metadata/titles/movie/605",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body.to_string().contains("SINGULARR_TMDB_TOKEN"));
    t.drop_db().await;
}

#[tokio::test]
async fn the_plex_server_gives_libraries_ratings_plays_music_and_sessions() {
    let Some(t) = TestDb::new("plex").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    let at = |path: &str| format!("{PLEX}{path}");
    recorded.answer(Method::Get, &at("/"), 200, &fixture("plex_root.json"));
    recorded.answer(
        Method::Get,
        &at("/library/sections"),
        200,
        &fixture("plex_library_sections.json"),
    );
    // The first movie of the section is 10 Cloverfield Lane, which the catalog holds.
    let movies = fixture("plex_section_movies.json").replacen("tmdb://11974", "tmdb://333371", 1);
    recorded.answer(
        Method::Get,
        &at("/library/sections/1/all?includeGuids=1&type=1"),
        200,
        &movies,
    );
    recorded.answer(
        Method::Get,
        &at("/library/sections/2/all?includeGuids=1&type=2"),
        200,
        "{\"MediaContainer\": {\"size\": 0}}",
    );
    recorded.answer(
        Method::Get,
        &at("/library/sections/5/all?includeGuids=1&type=1"),
        200,
        "{\"MediaContainer\": {\"size\": 0}}",
    );
    for (kind, file) in [
        (8, "plex_music_artists.json"),
        (9, "plex_music_albums.json"),
        (10, "plex_music_tracks.json"),
    ] {
        recorded.answer(
            Method::Get,
            &at(&format!(
                "/library/sections/6/all?includeGuids=1&type={kind}"
            )),
            200,
            &fixture(file),
        );
    }
    recorded.answer(
        Method::Get,
        &at("/status/sessions"),
        200,
        &fixture("plex_sessions.json"),
    );
    recorded.answer_with(
        Method::Get,
        &at("/photo/:/transcode"),
        200,
        "not really a jpeg",
        &[("content-type", "image/jpeg")],
    );
    let out = outbound(recorded.clone());
    let engine: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("radarr", "radarr", Some("radarr"))]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines, &policy);
    titles_pass(&deps, &engine, true).await.unwrap();
    let server: Arc<dyn singularr::engines::MediaServer> = Arc::new(
        singularr::engines::plex::Plex::new(
            &singularr::core::config::MediaServerConfig {
                id: "plex".to_string(),
                kind: singularr::core::config::MediaServerKind::Plex,
                url: Some(PLEX.to_string()),
                secret: Some("plex-token".to_string()),
                web_url: None,
            },
            out.clone(),
        )
        .unwrap(),
    );
    let ok = singularr::lifecycle::server::server_pass(&deps, &server)
        .await
        .unwrap();
    assert_eq!(ok.version.as_deref(), Some("1.43.5.11079-b3b13e506"));
    assert_eq!(ok.counts["sections"], 4);
    assert_eq!(ok.counts["titles_matched"], 1);
    assert_eq!(ok.counts["albums"], 2);

    // The sections are libraries of the server, beside the Radarr library that holds copies.
    let libraries = catalog::libraries(&t.pool).await.unwrap();
    let plex: Vec<(&str, Option<&str>)> = libraries
        .iter()
        .filter(|l| l.integration.as_deref() == Some("plex"))
        .map(|l| (l.id.as_str(), l.kind.as_deref()))
        .collect();
    assert_eq!(plex.len(), 4, "{libraries:?}");
    assert!(plex.contains(&("plex-6", Some("music"))));
    assert!(plex.contains(&("plex-5", Some("homevideo"))));
    // A second pass changes nothing.
    singularr::lifecycle::server::server_pass(&deps, &server)
        .await
        .unwrap();
    assert_eq!(
        catalog::libraries(&t.pool).await.unwrap().len(),
        libraries.len()
    );
    // A server that answers with no section at all (it is starting, its answer changed shape)
    // is refused: the pass fails and the libraries stay.
    recorded.answer(
        Method::Get,
        &at("/library/sections"),
        200,
        r#"{"MediaContainer": {"size": 0}}"#,
    );
    let refused = singularr::lifecycle::server::server_pass(&deps, &server)
        .await
        .unwrap_err();
    assert!(refused.contains("no section"), "{refused}");
    assert_eq!(
        catalog::libraries(&t.pool).await.unwrap().len(),
        libraries.len(),
        "a refused listing removes nothing"
    );
    recorded.answer(
        Method::Get,
        &at("/library/sections"),
        200,
        &fixture("plex_library_sections.json"),
    );

    let st = state_on(
        &t.pool,
        journal.clone(),
        &[
            ("PLEX_URL", PLEX),
            ("PLEX_TOKEN", "plex-token"),
            ("SINGULARR_TMDB_TOKEN", "test-token"),
        ],
        recorded.clone(),
    );
    singularr::search::index::pass(&t.pool, &st.search, &st.metadata)
        .await
        .unwrap();
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let held = fixture("tmdb_movie_603.json").replacen("\"id\": 603", "\"id\": 333371", 1);
    recorded.answer(
        Method::Get,
        "https://api.themoviedb.org/3/movie/333371?append_to_response=",
        200,
        &held,
    );
    let (status, _, record) = call(
        &app,
        "GET",
        "/api/v1/metadata/titles/movie/333371",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{record}");
    assert_eq!(record["ratings"]["rt"]["critics"], 57);
    assert_eq!(record["ratings"]["rt"]["audience"], 71);
    assert_eq!(record["library"]["plays"]["count"], 0);
    // The title is in the server's Movies section, and its Plex link names the server's
    // machine identifier and the item.
    assert_eq!(record["library"]["libraryId"], "plex-1", "{record}");
    assert_eq!(record["library"]["library"], "Movies");
    assert_eq!(record["library"]["plex"]["key"], "/library/metadata/109882");
    let url = record["library"]["plex"]["url"].as_str().unwrap();
    assert!(
        url.starts_with("https://app.plex.tv/desktop/#!/server/0123456789abcdef/details?key="),
        "{url}"
    );

    let (status, _, playing) = call(&app, "GET", "/api/v1/server/sessions", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{playing}");
    assert_eq!(playing["configured"], true);
    assert_eq!(playing["sessions"][0]["decision"], "transcode");
    assert_eq!(playing["sessions"][1]["show"], "Breaking Bad");

    let (status, _, music) = call(&app, "GET", "/api/v1/music", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{music}");
    let albums = music["albums"].as_array().unwrap();
    assert_eq!(albums.len(), 2);
    let first = albums.iter().find(|a| a["id"] == "album-120210").unwrap();
    assert_eq!(first["label"], "Interscope Records");
    assert_eq!(first["format"], "MP3");
    // A cover is an address the browser can load.
    assert_eq!(first["cover"], "/api/v1/music/albums/album-120210/cover");
    assert!(albums.iter().any(|a| {
        a["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p == "no-cover")
    }));
    assert_eq!(music["problemLabels"]["no-cover"], "No cover");
    assert_eq!(music["artists"][0]["id"], "artist-120209");
    assert!(music["artists"][0]["photo"].is_string() || music["artists"][0]["photo"].is_null());

    // An album with its tracks, and an artist with their albums, from the server.
    let (status, _, record) = call(
        &app,
        "GET",
        "/api/v1/music/albums/album-120210",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{record}");
    let tracks = record["tracks"].as_array().unwrap();
    assert!(!tracks.is_empty(), "{record}");
    assert_eq!(tracks[0]["format"], "MP3");
    assert!(tracks[0]["duration"].is_number() && tracks[0]["disc"].is_number());
    assert!(
        record["path"]
            .as_str()
            .unwrap()
            .contains("When Your Heart Stops Beating")
    );
    assert_eq!(record["trackCount"], tracks.len());
    let (status, _, artist) = call(
        &app,
        "GET",
        "/api/v1/music/artists/artist-120209",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{artist}");
    assert_eq!(artist["name"], "+44");
    assert_eq!(artist["discography"][0]["id"], "album-120210");
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/music/albums/album-404",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A plain video library: the server's files with what the owner noted on the tapes.
    recorded.answer(
        Method::Get,
        &at("/library/sections/5/all?includeGuids=1&type=1"),
        200,
        r#"{"MediaContainer": {"size": 1, "totalSize": 1, "Metadata": [{"ratingKey": "29499",
            "title": "1986 - montana", "year": 1986, "duration": 3720000, "addedAt": 1695674537,
            "thumb": "/library/metadata/29499/thumb/1",
            "Media": [{"videoCodec": "h264", "height": 480,
                       "Part": [{"file": "/homemovies/1986 - montana.mp4", "size": 1197602196}]}]}]}}"#,
    );
    let (status, _, videos) = call(&app, "GET", "/api/v1/library/videos", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{videos}");
    assert_eq!(videos["library"]["id"], "plex-5");
    assert_eq!(videos["library"]["kind"], "homevideo");
    assert_eq!(videos["library"]["titles"], 1);
    let rec = &videos["recordings"][0];
    assert_eq!(rec["id"], "video-29499");
    assert_eq!(
        (rec["codec"].as_str(), rec["duration"].as_i64()),
        (Some("H.264"), Some(62))
    );
    assert_eq!(rec["file"], "1986 - montana.mp4");
    assert_eq!(rec["still"], "/api/v1/library/videos/video-29499/still");
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/library/videos?id=plex-1",
        &session,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a movie library is not a plain video library"
    );
    let still = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/library/videos/video-29499/still?size=200")
                .header("x-auth-request-email", "owner@example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(still.status(), StatusCode::OK);
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/library/videos/video-1%2F..%2Fx/still",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The cover comes from the server's transcoder, with cache headers; the token stays in the
    // request header.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/music/albums/album-120210/cover?size=300")
                .header("x-auth-request-email", "owner@example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()["content-type"], "image/jpeg");
    assert!(
        res.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("max-age")
    );
    let photos: Vec<String> = recorded
        .urls_seen()
        .into_iter()
        .filter(|u| u.contains("/photo/:/transcode"))
        .collect();
    assert!(photos.iter().any(|u| u.contains("width=300")), "{photos:?}");
    assert!(photos.iter().all(|u| !u.contains("plex-token")));
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/music/artists/artist-999/photo",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Search finds the server's music: albums with their artist and cover, and artists.
    let (status, _, found) = call(
        &app,
        "GET",
        "/api/v1/search?q=heart&kind=music",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["albums"][0]["id"], "album-120210", "{found}");
    assert_eq!(found["albums"][0]["artistId"], "artist-120209");
    assert_eq!(
        found["albums"][0]["cover"],
        "/api/v1/music/albums/album-120210/cover"
    );
    assert!(found["library"].as_array().unwrap().is_empty());
    let (status, _, found) = call(
        &app,
        "GET",
        "/api/v1/search?q=44&kind=artist",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["artists"][0]["id"], "artist-120209", "{found}");
    // The server is asked with GET alone.
    assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
    t.drop_db().await;
}

/// The keys of `answer` that the example does not have at the same place: an answer may leave a
/// field out, never add one the client's sample does not know. Each element of an array is
/// compared with the example's elements merged into one.
fn keys_beyond(answer: &serde_json::Value, example: &serde_json::Value, at: &str) -> Vec<String> {
    use serde_json::Value;
    match (answer, example) {
        (Value::Object(a), Value::Object(e)) => a
            .iter()
            .flat_map(|(k, v)| match e.get(k) {
                None => vec![format!("{at}.{k}")],
                Some(ev) => keys_beyond(v, ev, &format!("{at}.{k}")),
            })
            .collect(),
        (Value::Array(a), Value::Array(e)) if !e.is_empty() => {
            let model = merged(e);
            a.iter()
                .enumerate()
                .flat_map(|(i, v)| keys_beyond(v, &model, &format!("{at}[{i}]")))
                .collect()
        }
        _ => Vec::new(),
    }
}

/// The elements of an example array as one: every field any of them has, objects merged.
fn merged(items: &[serde_json::Value]) -> serde_json::Value {
    use serde_json::Value;
    let mut out = Value::Null;
    for item in items {
        out = match (out, item) {
            (Value::Object(mut m), Value::Object(n)) => {
                for (k, v) in n {
                    let next = match m.remove(k) {
                        Some(old) => merged(&[old, v.clone()]),
                        None => v.clone(),
                    };
                    m.insert(k.clone(), next);
                }
                Value::Object(m)
            }
            (Value::Null, v) => v.clone(),
            (Value::Array(mut a), Value::Array(b)) => {
                a.extend(b.iter().cloned());
                Value::Array(a)
            }
            (old, Value::Null) => old,
            (_, v) => v.clone(),
        };
    }
    out
}

/// A `{columns, rows}` table as objects.
fn table_rows(table: &serde_json::Value) -> Vec<serde_json::Map<String, serde_json::Value>> {
    let columns: Vec<String> = table["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_string())
        .collect();
    table["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            columns
                .iter()
                .cloned()
                .zip(r.as_array().unwrap().iter().cloned())
                .collect()
        })
        .collect()
}

#[tokio::test]
async fn the_pages_answer_in_the_shapes_of_their_samples() {
    let Some(t) = TestDb::new("pages").await else {
        return;
    };
    let examples: serde_json::Value =
        serde_json::from_str(&fixture("pages_examples.json")).unwrap();
    let journal = Journal::new();
    let recorded = Recorded::new();
    recorded_catalog(&t.pool, &journal, &recorded).await;
    let out = outbound(recorded.clone());

    // The downloads: the recorded torrents and one episode of 1883 that Sonarr's queue claims.
    let mut torrents: serde_json::Value =
        serde_json::from_str(&fixture("qbit_torrents.json")).unwrap();
    torrents.as_array_mut().unwrap().push(serde_json::json!({
        "hash": "ddddddddddddddddddddddddddddddddddddddd4",
        "name": "1883.S01E03.1080p.WEB-DL-GRP",
        "category": "sonarr", "state": "downloading", "progress": 0.5, "size": 2000000000,
        "added_on": 1763800000, "completion_on": -1, "seeding_time": 0,
        "save_path": "/downloads/sonarr", "ratio": 0, "tags": "",
        "dlspeed": 4000000, "eta": 250, "num_seeds": 12, "num_leechs": 3
    }));
    recorded.answer_with(
        Method::Post,
        "/api/v2/auth/login",
        200,
        "Ok.",
        &[("set-cookie", "SID=test; path=/")],
    );
    recorded.answer(
        Method::Get,
        "/api/v2/torrents/info",
        200,
        &torrents.to_string(),
    );
    let qbit = Qbit::new(
        &DownloadClientConfig {
            id: "qbittorrent".to_string(),
            kind: ClientKind::Qbittorrent,
            url: Some(QBIT.to_string()),
            user: Some("u".into()),
            secret: Some("p".into()),
        },
        out.clone(),
    )
    .unwrap();
    let radarr: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let sonarr: Arc<dyn LibraryEngine> = Arc::new(Sonarr(arr(
        "sonarr",
        ArrKind::Sonarr,
        "sonarr",
        SONARR,
        &out,
    )));
    // What the managers claim goes through its table, as the queue and history jobs write it and
    // the downloads job reads it.
    let mut conn = t.pool.acquire().await.unwrap();
    claims::put_queue(&mut conn, "radarr", &radarr.queue().await.unwrap())
        .await
        .unwrap();
    claims::put_queue(&mut conn, "sonarr", &sonarr.queue().await.unwrap())
        .await
        .unwrap();
    let grabs = radarr.grabs(1, 250).await.unwrap().download_ids;
    claims::add_history(&mut conn, "radarr", &grabs)
        .await
        .unwrap();
    drop(conn);
    let listing = qbit.listing().await.unwrap();
    let ids: Vec<String> = listing
        .downloads
        .iter()
        .map(|d| d.download_id.clone())
        .collect();
    let managers = ["radarr".to_string(), "sonarr".to_string()];
    let mut claims = claims::of_ids(&t.pool, &managers, &ids).await.unwrap();
    claims.complete = true;
    reconcile(&t.pool, &journal, "qbittorrent", listing, &claims)
        .await
        .unwrap();

    // The requests of the Seerr site.
    let requests: serde_json::Value =
        serde_json::from_str(&fixture("seerr_requests.json")).unwrap();
    record_seerr(&recorded, &requests, 3);
    let pages_of_three = PassConfig {
        page_size: 3,
        lookups: 250,
    };
    requests_pass(&t.pool, &journal, &seerr_site(&out), pages_of_three)
        .await
        .unwrap();

    // The Plex server: its sections (the show section holds the shows' folder), plays and
    // ratings, what plays now.
    let at = |path: &str| format!("{PLEX}{path}");
    recorded.answer(Method::Get, &at("/"), 200, &fixture("plex_root.json"));
    recorded.answer(
        Method::Get,
        &at("/library/sections"),
        200,
        &fixture("plex_library_sections.json").replace("\"/tv\"", "\"/shows\""),
    );
    recorded.answer(
        Method::Get,
        &at("/library/sections/1/all?includeGuids=1&type=1"),
        200,
        &fixture("plex_section_movies.json").replacen("tmdb://11974", "tmdb://333371", 1),
    );
    for key in ["2/all?includeGuids=1&type=2", "5/all?includeGuids=1&type=1"] {
        recorded.answer(
            Method::Get,
            &at(&format!("/library/sections/{key}")),
            200,
            "{\"MediaContainer\": {\"size\": 0}}",
        );
    }
    for (kind, file) in [
        (8, "plex_music_artists.json"),
        (9, "plex_music_albums.json"),
        (10, "plex_music_tracks.json"),
    ] {
        recorded.answer(
            Method::Get,
            &at(&format!(
                "/library/sections/6/all?includeGuids=1&type={kind}"
            )),
            200,
            &fixture(file),
        );
    }
    recorded.answer(
        Method::Get,
        &at("/status/sessions"),
        200,
        &fixture("plex_sessions.json"),
    );
    let engines = Arc::new(EngineSet::new(
        vec![radarr.clone(), sonarr.clone()],
        None,
        slots(&[
            ("radarr", "radarr", Some("radarr")),
            ("sonarr", "sonarr", Some("sonarr")),
        ]),
    ));
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines.clone(), &policy);
    let server: Arc<dyn singularr::engines::MediaServer> = Arc::new(
        singularr::engines::plex::Plex::new(
            &singularr::core::config::MediaServerConfig {
                id: "plex".to_string(),
                kind: singularr::core::config::MediaServerKind::Plex,
                url: Some(PLEX.to_string()),
                secret: Some("plex-token".to_string()),
                web_url: None,
            },
            out.clone(),
        )
        .unwrap(),
    );
    let ok = singularr::lifecycle::server::server_pass(&deps, &server)
        .await
        .unwrap();
    engines
        .report(&t.pool, &journal, "plex", "server", Ok(ok))
        .await
        .unwrap();

    // What only the managers know, asked when a page is.
    for (base, kind) in [(RADARR, "radarr"), (SONARR, "sonarr")] {
        for (part, file) in [
            ("/calendar", format!("{kind}_calendar.json")),
            (
                "/history?page=1&pageSize=500&sortKey=date",
                format!("{kind}_history_all.json"),
            ),
            ("/wanted/missing", format!("{kind}_wanted_missing.json")),
            ("/wanted/cutoff", format!("{kind}_wanted_cutoff.json")),
        ] {
            recorded.answer(
                Method::Get,
                &format!("{base}/api/v3{part}"),
                200,
                &fixture(&file),
            );
        }
    }

    let env = [
        ("RADARR_API_KEY", "test-key"),
        ("SONARR_API_KEY", "test-key"),
        ("PLEX_URL", PLEX),
        ("PLEX_TOKEN", "plex-token"),
        ("SINGULARR_SIGN_OUT_URL", "/oauth2/sign_out"),
        (
            "SINGULARR_REQUESTS_PUBLIC_URL",
            "https://requests.example.org",
        ),
    ];
    let app = routes::router(state_on(&t.pool, journal.clone(), &env, recorded.clone()));
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    async fn get(app: &axum::Router, path: &str) -> serde_json::Value {
        let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
        let (status, _, body) = call(app, "GET", path, &session, None).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        body
    }
    let within = |answer: &serde_json::Value, name: &str| {
        let beyond = keys_beyond(answer, &examples[name], name);
        assert!(
            beyond.is_empty(),
            "fields the sample does not know: {beyond:?}"
        );
    };

    // The workspace.
    let ws = get(&app, "/api/v1/workspace").await;
    within(&ws, "workspace");
    assert_eq!(ws["name"], "Singularr");
    assert_eq!(ws["mode"], "observe");
    assert_eq!(ws["user"]["role"], "owner");
    assert_eq!(ws["server"]["name"], "Home Server");
    assert_eq!(ws["signOut"], "/oauth2/sign_out");
    assert_eq!(ws["portal"]["url"], "https://requests.example.org");
    let libraries = ws["libraries"].as_array().unwrap();
    let kinds: Vec<&str> = libraries
        .iter()
        .map(|l| l["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec!["movie", "show", "music", "homevideo"],
        "{libraries:?}"
    );
    let movies = &libraries[0];
    assert_eq!(movies["manager"], "radarr");
    assert_eq!(movies["plexKey"], "1");
    assert_eq!(
        (movies["titles"].as_i64(), movies["have"].as_i64()),
        (Some(3), Some(2))
    );
    assert_eq!(libraries[1]["episodes"], 16);
    assert_eq!(libraries[2]["albums"], 2);
    assert!(libraries[3].get("titles").is_none(), "no count is made up");
    assert_eq!(ws["counts"]["queue"], 5, "no download seeds yet");
    assert_eq!(
        ws["counts"]["wanted"], 8,
        "every entry of the wanted lists counts"
    );
    let bell = ws["attention"].as_array().unwrap();
    assert!(
        bell.iter().any(|a| a["kind"] == "queue"
            && a["tone"] == "bad"
            && a["title"] == "10 Cloverfield Lane"),
        "{bell:?}"
    );
    assert_eq!(
        ws["counts"]["attention"].as_u64().unwrap() as usize,
        bell.len()
    );
    assert!(ws["profiles"].as_array().unwrap().len() >= 2);
    // How the interface writes dates and times: the settings in force, the defaults first.
    assert_eq!(
        (&ws["formats"]["date"], &ws["formats"]["time"]),
        (&serde_json::json!("D MMM YYYY"), &serde_json::json!("24h"))
    );
    assert!(ws["formats"]["timeZone"].is_string(), "{ws}");
    let formats = serde_json::json!({"values": {
        "general.timeZone": "Europe/Berlin",
        "general.dateFormat": "YYYY-MM-DD",
        "general.timeFormat": "12h"
    }});
    let (status, _, saved) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &session,
        Some(&formats.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let after = get(&app, "/api/v1/workspace").await;
    within(&after, "workspace");
    assert_eq!(
        after["formats"],
        serde_json::json!({"timeZone": "Europe/Berlin", "date": "YYYY-MM-DD", "time": "12h"})
    );

    // The library tables.
    let table = get(&app, "/api/v1/library/movies").await;
    assert_eq!(table["columns"], examples["library-movies"]["columns"]);
    let rows = table_rows(&table);
    assert_eq!(rows.len(), 3);
    let lane = rows.iter().find(|r| r["id"] == "movie-333371").unwrap();
    assert_eq!(lane["state"], "attention", "its download is stuck");
    assert_eq!(lane["resolution"], 1080);
    assert_eq!(lane["source"], "BluRay");
    assert_eq!(lane["codec"], "HEVC");
    assert_eq!(lane["hdr"], "DV HDR10");
    assert_eq!(lane["audio"], "EAC3 Atmos 5.1");
    assert_eq!(lane["library"], "Movies");
    assert_eq!(lane["files"], 1);
    assert_eq!(lane["poster"], "/poster1.jpg");
    let later = rows.iter().find(|r| r["id"] == "movie-999001").unwrap();
    assert_eq!(later["state"], "unreleased");
    assert_eq!(later["files"], 0);
    // The table is cacheable: the tag sent back answers 304.
    let ask = |tag: Option<&str>| {
        let mut req = Request::builder()
            .uri("/api/v1/library/movies")
            .header("x-auth-request-email", "owner@example.com");
        if let Some(tag) = tag {
            req = req.header("if-none-match", tag);
        }
        app.clone().oneshot(req.body(Body::empty()).unwrap())
    };
    let res = ask(None).await.unwrap();
    let etag = res.headers()["etag"].to_str().unwrap().to_string();
    assert!(
        res.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("max-age")
    );
    assert_eq!(
        ask(Some(&etag)).await.unwrap().status(),
        StatusCode::NOT_MODIFIED
    );
    let table = get(&app, "/api/v1/library/shows").await;
    assert_eq!(table["columns"], examples["library-shows"]["columns"]);
    let rows = table_rows(&table);
    let show = rows.iter().find(|r| r["title"] == "1883").unwrap();
    assert_eq!(
        (show["have"].as_i64(), show["episodes"].as_i64()),
        (Some(2), Some(10))
    );
    assert_eq!(show["state"], "partial");
    assert_eq!(show["library"], "Shows");

    // Activity: three addresses, merged here into the record the sample holds.
    let activity = serde_json::json!({
        "queue": get(&app, "/api/v1/activity/queue").await["queue"],
        "history": get(&app, "/api/v1/activity/history").await["history"],
        "wanted": get(&app, "/api/v1/activity/wanted").await["wanted"],
    });
    within(&activity, "activity");
    let queue = activity["queue"].as_array().unwrap();
    assert_eq!(queue.len(), 5);
    let episode = queue.iter().find(|q| q["kind"] == "show").unwrap();
    assert_eq!(episode["episode"]["season"], 1);
    assert_eq!(episode["episode"]["number"], 3);
    assert_eq!(episode["stage"], "downloading");
    assert_eq!(episode["manager"], "Sonarr");
    assert_eq!(episode["indexer"], "Indexer Two");
    assert_eq!(
        (
            &episode["speed"],
            &episode["eta"],
            &episode["seeds"],
            &episode["peers"]
        ),
        (
            &serde_json::json!(4000000),
            &serde_json::json!(250),
            &serde_json::json!(12),
            &serde_json::json!(3)
        )
    );
    let stuck = queue
        .iter()
        .find(|q| q["id"] == "q-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1")
        .unwrap();
    assert!(stuck["attention"]["reason"].is_string());
    assert!(
        stuck.get("speed").is_none(),
        "a complete download does not move"
    );
    assert_eq!(stuck["timeline"][0]["stage"], "downloading");
    let manual = queue
        .iter()
        .find(|q| q["release"].as_str().unwrap().starts_with("A.Manual"))
        .unwrap();
    assert_eq!(manual["stage"], "unassigned");
    assert_eq!(manual["kind"], "movie");
    let history = activity["history"].as_array().unwrap();
    let upgraded = history.iter().find(|h| h["event"] == "upgraded").unwrap();
    assert_eq!(upgraded["from"], "Bluray-1080p");
    assert_eq!(upgraded["kind"], "movie");
    assert!(
        !history.iter().any(|h| h["event"] == "removed"),
        "the upgrade's deletion is folded in"
    );
    assert!(
        history
            .iter()
            .any(|h| h["kind"] == "show" && h["episode"]["number"] == 2)
    );
    assert_eq!(history.len(), 5, "a rename is not shown");
    assert!(
        history
            .windows(2)
            .all(|w| w[0]["at"].as_str() >= w[1]["at"].as_str()),
        "newest first"
    );
    let wanted = activity["wanted"].as_array().unwrap();
    assert_eq!(wanted.len(), 3);
    let upgrade = wanted.iter().find(|w| w["why"] == "upgrade").unwrap();
    assert_eq!(upgrade["title"]["id"], "movie-673");
    assert_eq!(upgrade["have"], "WEBDL-2160p");
    assert!(
        wanted
            .iter()
            .any(|w| w["kind"] == "show" && w["episode"]["title"] == "Pilot")
    );

    // One part of Activity at a time: only that part is read and sent. A state of its own keeps
    // nothing of the managers yet, so a history or a wanted list would have to be asked for.
    let fresh = routes::router(state_on(&t.pool, journal.clone(), &env, recorded.clone()));
    let parts = |answer: &serde_json::Value| -> Vec<String> {
        let mut names: Vec<String> = answer.as_object().unwrap().keys().cloned().collect();
        names.sort();
        names
    };
    let asked_since = |mark: usize| -> Vec<String> { recorded.urls_seen()[mark..].to_vec() };
    let mark = recorded.urls_seen().len();
    let only_queue = get(&fresh, "/api/v1/activity/queue").await;
    assert_eq!(parts(&only_queue), vec!["queue"], "{only_queue}");
    assert_eq!(only_queue["queue"], activity["queue"]);
    let asked = asked_since(mark);
    assert!(
        !asked
            .iter()
            .any(|u| u.contains("/history") || u.contains("/wanted")),
        "the queue asks no manager for its history or its wanted lists: {asked:?}"
    );
    let mark = recorded.urls_seen().len();
    let only_history = get(&fresh, "/api/v1/activity/history").await;
    assert_eq!(parts(&only_history), vec!["history"]);
    assert_eq!(only_history["history"], activity["history"]);
    let asked = asked_since(mark);
    assert!(asked.iter().any(|u| u.contains("/history")), "{asked:?}");
    assert!(!asked.iter().any(|u| u.contains("/wanted")), "{asked:?}");
    let mark = recorded.urls_seen().len();
    let only_wanted = get(&fresh, "/api/v1/activity/wanted").await;
    assert_eq!(parts(&only_wanted), vec!["wanted"]);
    assert_eq!(only_wanted["wanted"], activity["wanted"]);
    let asked = asked_since(mark);
    assert!(
        asked.iter().any(|u| u.contains("/wanted/missing"))
            && asked.iter().any(|u| u.contains("/wanted/cutoff")),
        "{asked:?}"
    );
    assert!(!asked.iter().any(|u| u.contains("/history")), "{asked:?}");
    // The whole record at one address is gone: EasyPrivacy blocks an address ending in
    // /api/v1/activity, so no page may depend on one.
    let (status, _, _) = call(&fresh, "GET", "/api/v1/activity", &session, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Requests.
    let page = get(&app, "/api/v1/requests").await;
    within(&page, "requests");
    let list = page["requests"].as_array().unwrap();
    assert_eq!(list.len(), 8);
    let words = [
        "requested",
        "approved",
        "downloading",
        "importing",
        "attention",
        "partial",
        "available",
        "missing",
        "declined",
        "failed",
        "pending",
    ];
    assert!(
        list.iter()
            .all(|r| words.contains(&r["status"].as_str().unwrap())),
        "{list:?}"
    );
    assert!(
        list.iter()
            .any(|r| r["what"]["type"] == "seasons" && r["what"]["seasons"].is_array())
    );
    assert!(
        list.iter()
            .any(|r| r["what"] == serde_json::json!({"type": "movie"}))
    );
    assert!(!page["people"].as_array().unwrap().is_empty());

    // The calendar.
    let cal = get(&app, "/api/v1/calendar?from=2026-11-16&to=2026-12-13").await;
    within(&cal, "calendar");
    let items = cal["items"].as_array().unwrap();
    assert_eq!(items.len(), 5, "{items:?}");
    assert!(
        items.iter().any(|i| i["kind"] == "movie"
            && i["release"] == "theaters"
            && i["state"] == "unreleased")
    );
    assert!(
        items
            .iter()
            .any(|i| i["kind"] == "show" && i["state"] == "available")
    );
    let (status, _, _) = call(&app, "GET", "/api/v1/calendar?from=23-11", &session, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Home.
    let home = get(&app, "/api/v1/home").await;
    within(&home, "home");
    assert!(home.get("storage").is_none(), "free space is not known");
    let stats: Vec<&str> = home["stats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        stats,
        vec!["movies", "shows", "queue", "requests", "wanted", "playing"]
    );
    assert_eq!(home["queue"]["movies"], 4);
    assert_eq!(home["queue"]["shows"], 1);
    let playing = home["nowPlaying"].as_array().unwrap();
    assert_eq!(playing.len(), 2);
    assert_eq!(playing[0]["title"]["id"], "movie-333371");
    assert_eq!(playing[0]["stream"], "Transcode");
    assert_eq!(playing[1]["title"]["title"], "Breaking Bad");
    assert!(!home["added"]["movies"].as_array().unwrap().is_empty());
    assert!(!home["added"]["episodes"].as_array().unwrap().is_empty());

    // Nothing but GET reached a manager or the server; the one POST is the qBittorrent sign-in.
    for (method, url) in recorded.seen.lock().unwrap().iter() {
        assert!(
            *method == Method::Get || url.contains("/api/v2/auth/login"),
            "{} {url}",
            method.as_str()
        );
    }
    t.drop_db().await;
}

#[tokio::test]
async fn search_falls_back_to_built_in_when_meilisearch_does_not_answer() {
    let Some(t) = TestDb::new("search").await else {
        return;
    };
    let recorded = Recorded::new();
    record_radarr(
        &recorded,
        RADARR,
        &fixture("radarr_movie.json"),
        &fixture("radarr_moviefile.json"),
    );
    let at = |path: &str| format!("{MEILI}{path}");
    recorded.answer(
        Method::Put,
        &at("/indexes/singularr_library/settings/"),
        202,
        r#"{"taskUid": 1}"#,
    );
    recorded.answer(
        Method::Post,
        &at("/indexes/singularr_library/documents?primaryKey=id"),
        202,
        r#"{"taskUid": 2}"#,
    );
    recorded.answer(
        Method::Post,
        &at("/indexes/singularr_library/documents/delete"),
        202,
        r#"{"taskUid": 3}"#,
    );
    recorded.answer(
        Method::Get,
        &at("/tasks/"),
        200,
        r#"{"status": "succeeded"}"#,
    );
    recorded.answer(
        Method::Get,
        &at("/indexes/singularr_library/stats"),
        200,
        r#"{"numberOfDocuments": 3}"#,
    );
    recorded.answer(
        Method::Post,
        &at("/multi-search"),
        503,
        r#"{"message": "the search engine is overloaded"}"#,
    );
    let out = outbound(recorded.clone());
    let engine: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![engine.clone()],
        None,
        slots(&[("radarr", "radarr", Some("radarr"))]),
    ));
    let journal = Journal::new();
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(&t.pool, &journal, engines, &policy);
    titles_pass(&deps, &engine, true).await.unwrap();

    let st = state_on(
        &t.pool,
        journal.clone(),
        &[
            ("SINGULARR_SEARCH_URL", MEILI),
            ("SINGULARR_SEARCH_KEY", "meili-key"),
        ],
        recorded.clone(),
    );
    let report = singularr::search::index::pass(&t.pool, &st.search, &st.metadata)
        .await
        .unwrap();
    assert!(report.builtin.is_ok(), "{report:?}");
    assert_eq!(report.meilisearch, Some(Ok(3)), "{report:?}");
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, before) = call(&app, "GET", "/api/v1/search/status", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{before}");
    assert_eq!(
        (before["provider"].as_str(), before["configured"].as_str()),
        (Some("meilisearch"), Some("meilisearch"))
    );
    assert_eq!(
        (before["state"].as_str(), before["documents"].as_i64()),
        (Some("ok"), Some(3))
    );

    // Meilisearch does not answer: built-in search does, and the status says so.
    let (status, _, found) = call(
        &app,
        "GET",
        "/api/v1/search?q=cloverfield&kind=movie",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["library"][0]["id"], "movie-333371", "{found}");
    let (status, _, after) = call(&app, "GET", "/api/v1/search/status", &session, None).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(
        (
            after["provider"].as_str(),
            after["configured"].as_str(),
            after["state"].as_str()
        ),
        (Some("postgresql"), Some("meilisearch"), Some("warn"))
    );
    assert!(
        after["detail"].as_str().unwrap().contains("overloaded"),
        "{after}"
    );
    // Writes went to Meilisearch alone, with its key in a header and never in an address.
    let seen = recorded.seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .filter(|(m, _)| *m != Method::Get)
            .all(|(_, u)| u.starts_with(MEILI)),
        "{seen:?}"
    );
    assert!(seen.iter().all(|(_, u)| !u.contains("meili-key")));
    t.drop_db().await;
}

// What the recorded ledger's jobs name, as the managers list it: a movie whose file is the
// result of an imported run, a movie an open run was staged from, and a show with the episode
// an imported run made. The second movie moved to another root since its run was staged
// (the ledger has it under `/media/movies`), so no folder the catalog knows holds the
// run's file: the run is linked by the id its folder's name carries.
const DAMSEL_FOLDER: &str = "/media/movies/Damsel (2024) {tmdb-763215}";
const DAMSEL_FILE: &str = "Damsel (2024) {imdb-tt13452446} [WEBDL-1080p][Opus 5.1][HEVC]-FLUX.mkv";
const KINGDOM_FOLDER: &str = "/media/4kmovies/Kingdom of Heaven (2005) {tmdb-1495}";
const KINGDOM_FILE: &str = "Kingdom of Heaven (2005) {imdb-tt0320661} {edition-Director's Cut} [Bluray-1080p][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv";
const AVATAR_FOLDER: &str = "/media/shows/Avatar - The Last Airbender (2024) {tvdb-385925}";
const AVATAR_FILE: &str =
    "Avatar - The Last Airbender (2024) - S01E01 - Aang [WEBDL-1080p][Opus 5.1][HEVC]-Kitsune.mkv";

/// A file as a manager lists it.
fn manager_file(id: i64, folder: &str, relative: &str, size: i64) -> serde_json::Value {
    serde_json::json!({
        "id": id, "relativePath": relative, "path": format!("{folder}/{relative}"),
        "size": size, "dateAdded": "2026-11-22T12:34:54Z", "releaseGroup": "GRP",
        "languages": [{"id": 1, "name": "English"}],
        "quality": {
            "quality": {"id": 3, "name": "WEBDL-1080p"},
            "revision": {"version": 1, "real": 0, "isRepack": false}
        },
        "customFormats": [], "customFormatScore": 0
    })
}

/// A movie as Radarr lists it, with its one file.
fn radarr_movie(
    id: i64,
    title: &str,
    year: i32,
    tmdb: i64,
    folder: &str,
    file: &serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id, "title": title, "sortTitle": title.to_lowercase(), "year": year,
        "tmdbId": tmdb, "status": "released", "monitored": true, "qualityProfileId": 4,
        "tags": [], "path": folder, "added": "2025-01-05T10:00:00Z", "hasFile": true,
        "sizeOnDisk": file["size"],
        "images": [{
            "coverType": "poster",
            "remoteUrl": format!("https://image.tmdb.org/t/p/original/poster-{tmdb}.jpg")
        }],
        "movieFile": file
    })
}

/// The catalog of the titles the recorded ledger's files belong to, read from a recorded
/// Radarr and Sonarr the way every catalog is.
async fn ledger_catalog(pool: &PgPool, journal: &Journal, recorded: &Arc<Recorded>) {
    let mut damsel_file = manager_file(18475, DAMSEL_FOLDER, DAMSEL_FILE, 4_618_100_950);
    damsel_file["movieId"] = serde_json::json!(500);
    let mut kingdom_file = manager_file(9100, KINGDOM_FOLDER, KINGDOM_FILE, 9_295_592_609);
    kingdom_file["movieId"] = serde_json::json!(501);
    kingdom_file["edition"] = serde_json::json!("Director's Cut");
    let movies = serde_json::json!([
        radarr_movie(500, "Damsel", 2024, 763215, DAMSEL_FOLDER, &damsel_file),
        radarr_movie(
            501,
            "Kingdom of Heaven",
            2005,
            1495,
            KINGDOM_FOLDER,
            &kingdom_file
        ),
    ]);
    record_radarr(
        recorded,
        RADARR,
        &movies.to_string(),
        &serde_json::json!([damsel_file, kingdom_file]).to_string(),
    );
    let mut episode_file = manager_file(
        34462,
        AVATAR_FOLDER,
        &format!("Season 01/{AVATAR_FILE}"),
        2_021_176_200,
    );
    episode_file["seriesId"] = serde_json::json!(7);
    episode_file["seasonNumber"] = serde_json::json!(1);
    let statistics = serde_json::json!({
        "episodeFileCount": 1, "episodeCount": 8, "totalEpisodeCount": 8,
        "sizeOnDisk": 2_021_176_200_i64
    });
    let series = serde_json::json!([{
        "id": 7, "title": "Avatar: The Last Airbender",
        "sortTitle": "avatar the last airbender", "status": "continuing", "year": 2024,
        "tvdbId": 385925, "tmdbId": 82452, "monitored": true, "qualityProfileId": 4,
        "tags": [], "path": AVATAR_FOLDER, "added": "2024-02-22T08:00:00Z",
        "images": [{
            "coverType": "poster",
            "remoteUrl": "https://image.tmdb.org/t/p/original/poster-82452.jpg"
        }],
        "seasons": [{"seasonNumber": 1, "monitored": true, "statistics": statistics}],
        "statistics": statistics
    }]);
    let at = |path: &str| format!("{SONARR}/api/v3{path}");
    recorded.answer(
        Method::Get,
        &at("/system/status"),
        200,
        &fixture("radarr_system_status.json"),
    );
    recorded.answer(Method::Get, &at("/tag"), 200, "[]");
    recorded.answer(
        Method::Get,
        &at("/qualityprofile"),
        200,
        &fixture("radarr_qualityprofile.json"),
    );
    recorded.answer(Method::Get, &at("/series"), 200, &series.to_string());
    recorded.answer(
        Method::Get,
        &at("/episodefile?seriesId=7"),
        200,
        &serde_json::json!([episode_file]).to_string(),
    );
    let out = outbound(recorded.clone());
    let radarr: Arc<dyn LibraryEngine> = Arc::new(Radarr::new(arr(
        "radarr",
        ArrKind::Radarr,
        "radarr",
        RADARR,
        &out,
    )));
    let sonarr: Arc<dyn LibraryEngine> = Arc::new(Sonarr(arr(
        "sonarr",
        ArrKind::Sonarr,
        "sonarr",
        SONARR,
        &out,
    )));
    let engines = Arc::new(EngineSet::new(
        vec![radarr.clone(), sonarr.clone()],
        None,
        slots(&[
            ("radarr", "radarr", Some("radarr")),
            ("sonarr", "sonarr", Some("sonarr")),
        ]),
    ));
    let policy = Arc::new(ActivePolicy::builtin());
    let deps = deps(pool, journal, engines, &policy);
    titles_pass(&deps, &radarr, true).await.unwrap();
    titles_pass(&deps, &sonarr, true).await.unwrap();
}

/// The ids of the runs a filter asks for, newest first, and how many there are in all.
async fn runs_where(pool: &PgPool, filter: RunFilter, busy: &[&str]) -> (Vec<String>, i64) {
    let moment = Moment {
        busy_paths: busy.iter().map(|p| p.to_string()).collect(),
        stuck_before: chrono::Utc::now() - chrono::Duration::hours(6),
    };
    let (runs, total) = processing_store::list(pool, &filter, &moment, 50, 0)
        .await
        .unwrap();
    (runs.into_iter().map(|r| r.job_id).collect(), total)
}

#[tokio::test]
async fn processing_runs_are_read_kept_linked_and_shown() {
    let Some(t) = TestDb::new("processing").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    ledger_catalog(&t.pool, &journal, &recorded).await;
    let damsel = title_with_tmdb(&t.pool, 763215).await;
    let kingdom = title_with_tmdb(&t.pool, 1495).await;
    let avatar = title_with_tvdb(&t.pool, 385925).await;

    // The two services answer as they were recorded.
    let ledger = fixture("hush_history.json");
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture("hush_health.json"),
    );
    recorded.answer(Method::Get, &format!("{HUSH}/history.json"), 200, &ledger);
    recorded.answer(
        Method::Get,
        &format!("{TDARR}/api/v2/status"),
        200,
        &fixture("tdarr_status.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{TDARR}/api/v2/get-nodes"),
        200,
        &fixture("tdarr_nodes_idle.json"),
    );
    // The worker's own records, which it takes questions about with POST: its libraries, its
    // flows and its totals as they were saved. It has no record of a file it is asked for (an
    // empty body), until one is recorded further down.
    let records = format!("{TDARR}/api/v2/cruddb");
    for (collection, file) in [
        ("LibrarySettingsJSONDB", "tdarr_libraries.json"),
        ("FlowsJSONDB", "tdarr_flows.json"),
        ("StatisticsJSONDB", "tdarr_statistics.json"),
    ] {
        recorded.answer_body(Method::Post, &records, collection, 200, &fixture(file));
    }
    recorded.answer_body(Method::Post, &records, "FileJSONDB", 200, "");
    // Its queue, as it answered when nothing waited.
    recorded.answer_body(
        Method::Post,
        &format!("{TDARR}/api/v2/client/status-tables"),
        "table1",
        200,
        &fixture("tdarr_table_queue_empty.json"),
    );
    let st = state_on(
        &t.pool,
        journal.clone(),
        &[
            ("HUSH_URL", HUSH),
            ("TDARR_URL", TDARR),
            ("RADARR_API_KEY", "test-key"),
            ("SONARR_API_KEY", "test-key"),
            ("SINGULARR_TMDB_TOKEN", "test-token"),
        ],
        recorded.clone(),
    );
    let bridge = st.engines.processor("hush").unwrap().clone();
    let worker = st.engines.processor("tdarr").unwrap().clone();

    // The first pass of a process reads the whole ledger: every job becomes a run, linked to
    // the title whose folder its file lies in. The clock of the passes is the test's.
    let mut reader = processing::Reader::new(bridge.clone());
    let start = std::time::Instant::now();
    let minute = Duration::from_secs(60);
    let first = reader.pass(&t.pool, start).await.unwrap();
    assert_eq!(
        first.report,
        processing::PassReport {
            read: processing::Read::Whole,
            listed: 12,
            older: 0,
            rest: processing::Rest::Nothing,
            followed: 0,
            added: 12,
            updated: 0,
            linked: 3,
            open: 2
        }
    );
    assert_eq!(first.detail, "12 runs");
    assert_eq!(
        first.ok.counts,
        serde_json::json!({"runs": 12, "runs_open": 2})
    );
    assert_eq!(first.ok.version, None);
    st.engines
        .report(&t.pool, &journal, "hush", processing::PASS, Ok(first.ok))
        .await
        .unwrap();
    let run = |job: &'static str| {
        let pool = t.pool.clone();
        async move {
            processing_store::one(&pool, &format!("hush:{job}"))
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{job} is stored"))
        }
    };
    let imported = run("20261122T121501Z-52c0381d").await;
    assert_eq!(
        (imported.title_id, imported.season, imported.episode),
        (Some(damsel), None, None)
    );
    assert_eq!(imported.state, "imported");
    assert_eq!(
        (
            imported.source_size,
            imported.result_size,
            imported.manager_file_id
        ),
        (Some(6_101_211_908), Some(4_618_100_950), Some(18475))
    );
    assert_eq!(imported.replaced.len(), 1);
    assert_eq!(imported.events.len(), 4);
    let episode = run("20261123T145500Z-57ec91da").await;
    assert_eq!(
        (episode.title_id, episode.season, episode.episode),
        (Some(avatar), Some(1), Some(1))
    );
    // A run whose file lies in no folder the catalog knows (its title moved since) is linked
    // by the TMDB id in its folder's name.
    let staged = run("20261115T001015Z-cb34e7ec").await;
    assert!(
        !staged
            .source_path
            .as_deref()
            .unwrap()
            .starts_with(KINGDOM_FOLDER)
    );
    assert_eq!(
        (staged.title_id, staged.state.as_str(), staged.finished_at),
        (Some(kingdom), "staged", None)
    );
    let queue = staged.queue_path.clone().unwrap();
    // A file in a folder no title has stays unlinked, with the episode its name reads as.
    let unlinked = run("20261123T134700Z-6b66bd7b").await;
    assert_eq!(
        (unlinked.title_id, unlinked.season, unlinked.episode),
        (None, Some(1), Some(7))
    );
    let raw = processing_store::raw(&t.pool, &imported.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw["promoted"]["arr_file_id"], 18475);
    assert_eq!(raw["source"]["sample"].as_str().map(str::len), Some(64));

    // An ordinary pass asks for the newest jobs alone, and one that finds nothing new writes
    // nothing.
    let read = |p: &processing::Passed| {
        (
            p.report.read,
            p.report.listed,
            p.report.added,
            p.report.updated,
        )
    };
    let again = reader.pass(&t.pool, start + minute).await.unwrap();
    assert_eq!(read(&again), (processing::Read::Newest, 12, 0, 0));
    assert_eq!(again.detail, "12 newest runs");
    assert_eq!(run("20261122T121501Z-52c0381d").await, imported);

    // The service changes an older job. It lists its jobs newest first by when they started,
    // so that job is not among the newest (five here, as if the ledger were long).
    let mut moved: serde_json::Value = serde_json::from_str(&ledger).unwrap();
    {
        let job = &mut moved["jobs"][6];
        assert_eq!(job["id"], "20261115T001015Z-cb34e7ec");
        job["state"] = serde_json::json!("output_staged");
        job["updated"] = serde_json::json!("2026-11-23T16:10:00Z");
        job["staged_output"] = serde_json::json!("/media/hush/staging/movies/k.mkv");
        job["events"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "ts": "2026-11-23T16:10:00Z", "event": "output_staged", "state": "output_staged",
                "actor": "radarr-finalizer"
            }));
    }
    // The answer recorded last for an address is the one given, and the longest recorded part
    // of an address wins: the whole ledger, and what the short read is answered with.
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/history.json"),
        200,
        &moved.to_string(),
    );
    let newest = |jobs: usize| {
        let mut slice = moved.clone();
        slice["jobs"].as_array_mut().unwrap().truncate(jobs);
        slice.to_string()
    };
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/history.json?limit=60"),
        200,
        &newest(5),
    );
    // An ordinary pass does not see the change, and counts what the table holds.
    let unseen = reader.pass(&t.pool, start + 2 * minute).await.unwrap();
    assert_eq!(read(&unseen), (processing::Read::Newest, 5, 0, 0));
    assert_eq!(unseen.detail, "5 newest runs");
    assert_eq!(
        unseen.ok.counts,
        serde_json::json!({"runs": 12, "runs_open": 2})
    );
    assert_eq!(run("20261115T001015Z-cb34e7ec").await.state, "staged");
    // The whole read sees it. It is made again when the last one is as old as it stands.
    let late = start + processing::WHOLE_READ_EVERY;
    assert_eq!(
        reader.read_at(late - Duration::from_secs(1)),
        processing::Read::Newest
    );
    let changed = reader.pass(&t.pool, late).await.unwrap();
    assert_eq!(read(&changed), (processing::Read::Whole, 12, 0, 1));
    assert_eq!(changed.report.open, 2);
    let verifying = run("20261115T001015Z-cb34e7ec").await;
    assert_eq!(verifying.state, "verifying");
    assert_eq!(
        verifying.output_path.as_deref(),
        Some("/media/hush/staging/movies/k.mkv")
    );
    assert_eq!(verifying.events.len(), 2);
    assert_eq!(verifying.first_seen_at, staged.first_seen_at);
    // A job the service no longer lists stays: the table is the history.
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/history.json?limit=60"),
        200,
        &newest(2),
    );
    let fewer = reader.pass(&t.pool, late + minute).await.unwrap();
    assert_eq!(read(&fewer), (processing::Read::Newest, 2, 0, 0));
    assert_eq!(processing_store::counts(&t.pool).await.unwrap().total, 12);
    // The ledger as it was recorded again, for everything below.
    for part in ["", "?limit=60"] {
        recorded.answer(
            Method::Get,
            &format!("{HUSH}/history.json{part}"),
            200,
            &ledger,
        );
    }
    let back = reader.pass(&t.pool, late + 2 * minute).await.unwrap();
    assert_eq!(read(&back), (processing::Read::Newest, 12, 0, 1));
    assert_eq!(run("20261115T001015Z-cb34e7ec").await.state, "staged");

    // The worker has no history to read: its pass says how it is, and reads its own records
    // for the pages: its libraries and flows with the whole read, its totals with every pass.
    assert!(worker.setup().is_none() && worker.totals().is_none());
    let idle = processing::pass(&t.pool, &worker, processing::Read::Whole)
        .await
        .unwrap();
    assert_eq!(read(&idle), (processing::Read::Whole, 0, 0, 0));
    assert_eq!(idle.detail, "1 node, 0 busy");
    assert_eq!(idle.unread, None);
    let setup = worker
        .setup()
        .expect("the whole read kept its configuration");
    assert_eq!((setup.libraries.len(), setup.flows.len()), (7, 4));
    assert_eq!(worker.totals().map(|t| t.files), Some(15));
    // Records it does not give leave the worker up: the pass says what it could not read,
    // keeps what it knew, and its whole read does not stand.
    recorded.answer_body(Method::Post, &records, "FlowsJSONDB", 503, "");
    let mut worker_reader = processing::Reader::new(worker.clone());
    let partial = worker_reader.pass(&t.pool, start).await.unwrap();
    assert!(
        partial
            .detail
            .starts_with("1 node, 0 busy; Libraries and flows not read: "),
        "{}",
        partial.detail
    );
    assert!(partial.unread.as_deref().is_some_and(|u| u.contains("503")));
    assert_eq!(worker.setup().map(|s| s.flows.len()), Some(4));
    assert_eq!(
        worker_reader.read_at(start + minute),
        processing::Read::Whole,
        "a whole read that left something unread is made again"
    );
    recorded.answer_body(
        Method::Post,
        &records,
        "FlowsJSONDB",
        200,
        &fixture("tdarr_flows.json"),
    );
    let whole = worker_reader.pass(&t.pool, start + minute).await.unwrap();
    assert_eq!(whole.unread, None);
    assert_eq!(
        worker_reader.read_at(start + 2 * minute),
        processing::Read::Newest
    );
    // An ordinary pass asks for the totals and the queue alone.
    let asked_before = recorded.calls_sent().len();
    worker_reader
        .pass(&t.pool, start + 2 * minute)
        .await
        .unwrap();
    let posts: Vec<String> = recorded.calls_sent()[asked_before..]
        .iter()
        .filter(|c| c.method == Method::Post)
        .map(|c| match &c.body {
            Sent::Json(text) => text.clone(),
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert!(posts[0].contains("StatisticsJSONDB") && posts[0].contains("getAll"));
    assert!(posts[1].contains("table1"), "{posts:?}");
    assert_eq!(idle.ok.version.as_deref(), Some("3.09.00"));
    assert_eq!(
        idle.ok.counts,
        serde_json::json!({"nodes": 1, "workers_busy": 0})
    );
    st.engines
        .report(&t.pool, &journal, "tdarr", processing::PASS, Ok(idle.ok))
        .await
        .unwrap();
    // A service that does not answer fails its pass and nothing is lost. A whole read that
    // failed is made again by the next pass.
    recorded.answer(Method::Get, &format!("{HUSH}/health"), 503, "");
    let due = late + processing::WHOLE_READ_EVERY;
    let failed = reader.pass(&t.pool, due).await.unwrap_err();
    assert!(failed.contains("503"), "{failed}");
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture("hush_health.json"),
    );
    let retried = reader.pass(&t.pool, due + minute).await.unwrap();
    assert_eq!(read(&retried), (processing::Read::Whole, 12, 0, 0));
    assert_eq!(
        reader.read_at(due + 2 * minute),
        processing::Read::Newest,
        "a whole read that succeeded stands"
    );
    // What each pass asked the ledger for: the whole of it first, after half an hour, and
    // again after the whole read that failed before it got that far.
    let limits: Vec<String> = recorded
        .urls_seen()
        .iter()
        .filter_map(|u| u.split_once("/history.json?limit="))
        .map(|(_, limit)| limit.to_string())
        .collect();
    assert_eq!(limits, ["2000", "60", "60", "2000", "60", "60", "2000"]);

    // What the table holds, in numbers.
    let counts = processing_store::counts(&t.pool).await.unwrap();
    assert_eq!(
        (
            counts.total,
            counts.staged,
            counts.verifying,
            counts.imported
        ),
        (12, 2, 0, 6)
    );
    assert_eq!(
        (
            counts.failed,
            counts.refused,
            counts.pruned,
            counts.superseded
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(counts.saved_bytes, 38_408_951_548);
    assert_eq!(counts.since, Some("2026-11-11T14:35:41Z".parse().unwrap()));
    let (integrations, modes, flows) = processing::flow_groups(&st.engines);
    let stats = processing_store::flow_stats(&t.pool, &integrations, &modes, &flows)
        .await
        .unwrap();
    // Per flow: its runs, the imported ones, the percent they saved, the median of their seconds.
    let numbers: Vec<String> = stats
        .iter()
        .map(|s| {
            format!(
                "{} {} {} {:?} {:?}",
                s.flow,
                s.runs,
                s.imported,
                s.saved_percent(),
                s.median_seconds
            )
        })
        .collect();
    // The modes that share a label count as one flow: `uhd-audio` with `audio`.
    assert_eq!(
        numbers,
        vec![
            "audio 2 1 Some(1.2) Some(831.0)",
            "downscale 1 1 Some(69.5) Some(1888.0)",
            "encode 1 1 Some(48.0) Some(1275.0)",
            "hevc 8 3 Some(26.5) Some(675.0)",
        ]
    );
    assert_eq!(
        stats[0].last_run,
        Some("2026-11-20T18:46:00Z".parse().unwrap())
    );

    // The lists the pages read.
    let open = processing_store::open(&t.pool).await.unwrap();
    let ids: Vec<&str> = open.iter().map(|r| r.job_id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["20261111T214740Z-e90af04d", "20261115T001015Z-cb34e7ec"],
        "oldest first"
    );
    let recent = processing_store::recent_closed(&t.pool, 3).await.unwrap();
    let ids: Vec<&str> = recent.iter().map(|r| r.job_id.as_str()).collect();
    assert_eq!(
        ids,
        vec![
            "20261123T145500Z-57ec91da",
            "20261123T134700Z-6b66bd7b",
            "20261122T121501Z-52c0381d"
        ],
        "the last to close first"
    );
    let since = "2026-11-11T15:00:00Z".parse().unwrap();
    let broken = processing_store::closed_since(&t.pool, &["failed", "refused"], since, 10)
        .await
        .unwrap();
    assert_eq!(broken.len(), 1, "the refusal closed before that");
    assert_eq!(broken[0].state, "failed");
    let of_title = processing_store::of_title(&t.pool, kingdom, 50)
        .await
        .unwrap();
    assert_eq!(of_title.len(), 1);
    assert_eq!(
        processing_store::title_counts(&t.pool, kingdom)
            .await
            .unwrap(),
        (1, 1)
    );
    assert_eq!(
        processing_store::title_counts(&t.pool, damsel)
            .await
            .unwrap(),
        (1, 0)
    );
    let marks = processing_store::marks(&t.pool, avatar).await.unwrap();
    assert_eq!(marks.len(), 1);
    assert!(
        marks[0]
            .result_path
            .as_deref()
            .unwrap()
            .ends_with(AVATAR_FILE)
    );

    // Every filter of the runs list.
    let all = RunFilter::default();
    let (ids, total) = runs_where(&t.pool, all.clone(), &[]).await;
    assert_eq!((ids.len(), total), (12, 12));
    assert_eq!(ids[0], "20261123T145500Z-57ec91da", "newest first");
    let count = |state: StateFilter, busy: &'static [&'static str]| {
        let pool = t.pool.clone();
        async move {
            let filter = RunFilter {
                state: Some(state),
                ..RunFilter::default()
            };
            runs_where(&pool, filter, busy).await.1
        }
    };
    assert_eq!(count(StateFilter::Open, &[]).await, 2);
    assert_eq!(count(StateFilter::Closed, &[]).await, 10);
    assert_eq!(count(StateFilter::Is("imported".to_string()), &[]).await, 6);
    assert_eq!(
        count(StateFilter::Is("verifying".to_string()), &[]).await,
        0
    );
    assert_eq!(count(StateFilter::Stuck, &[]).await, 2);
    assert_eq!(count(StateFilter::Waiting, &[]).await, 2);
    assert_eq!(count(StateFilter::Working, &[]).await, 0);
    // With a worker on one staged run's queue path, that run is working and not stuck.
    let busy = [queue.as_str()];
    let on = |state: StateFilter| RunFilter {
        state: Some(state),
        ..RunFilter::default()
    };
    assert_eq!(
        runs_where(&t.pool, on(StateFilter::Working), &busy).await.0,
        vec!["20261115T001015Z-cb34e7ec"]
    );
    assert_eq!(
        runs_where(&t.pool, on(StateFilter::Waiting), &busy).await.0,
        vec!["20261111T214740Z-e90af04d"]
    );
    assert_eq!(
        runs_where(&t.pool, on(StateFilter::Stuck), &busy).await.0,
        vec!["20261111T214740Z-e90af04d"]
    );
    assert_eq!(runs_where(&t.pool, on(StateFilter::Open), &busy).await.1, 2);
    let by = |filter: RunFilter| {
        let pool = t.pool.clone();
        async move { runs_where(&pool, filter, &[]).await.1 }
    };
    let flows_of = |modes: &[&str]| RunFilter {
        flows: Some(modes.iter().map(|m| m.to_string()).collect()),
        ..RunFilter::default()
    };
    assert_eq!(by(flows_of(&["audio", "uhd-audio"])).await, 2);
    assert_eq!(by(flows_of(&["uhd-audio"])).await, 1);
    assert_eq!(by(flows_of(&["shrink"])).await, 0);
    let of = |integration: &str| RunFilter {
        integration: Some(integration.to_string()),
        ..RunFilter::default()
    };
    assert_eq!((by(of("hush")).await, by(of("tdarr")).await), (12, 0));
    let linked_to = |titles: Vec<i64>| RunFilter {
        titles: Some(titles),
        ..RunFilter::default()
    };
    assert_eq!(by(linked_to(vec![damsel])).await, 1);
    assert_eq!(by(linked_to(vec![damsel, avatar, kingdom])).await, 3);
    assert_eq!(by(linked_to(Vec::new())).await, 0);
    // Words match file names, never the folders above them.
    let named = |words: &[&str]| RunFilter {
        words: words.iter().map(|w| w.to_string()).collect(),
        ..RunFilter::default()
    };
    assert_eq!(by(named(&["windmills"])).await, 1);
    assert_eq!(by(named(&["kitsune"])).await, 2);
    assert_eq!(by(named(&["KITSUNE", "aang"])).await, 1);
    assert_eq!(
        by(named(&["10bit"])).await,
        3,
        "the name of what the worker made"
    );
    assert_eq!(
        by(named(&["shows"])).await,
        0,
        "a folder is not a file name"
    );
    assert_eq!(by(named(&["100%"])).await, 0);
    // Paging: a page of five, and the rest.
    let moment = Moment {
        busy_paths: Vec::new(),
        stuck_before: chrono::Utc::now(),
    };
    let (page, total) = processing_store::list(&t.pool, &all, &moment, 5, 10)
        .await
        .unwrap();
    assert_eq!((page.len(), total), (2, 12));
    assert_eq!(
        page[1].job_id, "20261111T143541Z-3a4c6489",
        "the oldest is last"
    );

    // The page: the services as they answer now, the runs in numbers, the flows, the open runs
    // and the ones that closed last.
    use serde_json::json;
    // The worker has a record of the movie whose run waits: it is in its queue. Of the other
    // open run's file it has none.
    recorded.answer_body(
        Method::Post,
        &records,
        "Kingdom of Heaven (2005) {imdb-tt0320661}",
        200,
        &json!({
            "_id": queue, "DB": "DRItYe5ns", "TranscodeDecisionMaker": "Queued",
            "HealthCheck": "Queued", "holdUntil": 0
        })
        .to_string(),
    );
    let app = routes::router(st.clone());
    let page = get_ok(&app, "/api/v1/processing").await;
    let keys =
        |v: &serde_json::Value| -> Vec<String> { v.as_object().unwrap().keys().cloned().collect() };
    assert_eq!(
        keys(&page),
        ["counts", "flows", "open", "recent", "services", "workers"]
    );
    let services = page["services"].as_array().unwrap();
    assert_eq!(services.len(), 2);
    assert!(services.iter().all(|s| s["lastSync"].is_string()));
    assert_eq!(
        services[0],
        json!({
            "id": "hush", "type": "hush", "name": "HUSH", "role": "bridge", "state": "ok",
            "detail": "Ledger on, tag watcher running", "version": null,
            "lastSync": services[0]["lastSync"],
            "facts": {
                "tagWatcher": true, "stripWorker": true, "ledger": true, "identity": "enforce"
            }
        })
    );
    // The worker, with its own records as its pass read them: what it did in numbers, its
    // libraries (the folders it watches and the flow of each), and its flows with their steps.
    assert_eq!(
        services[1],
        json!({
            "id": "tdarr", "type": "tdarr", "name": "Tdarr", "role": "worker", "state": "ok",
            "detail": "1 node, 0 busy", "version": "3.09.00",
            "lastSync": services[1]["lastSync"],
            "nodes": [{
                "id": "TNUn37R", "name": "Tdarr-Node-5090", "paused": false, "busy": 0,
                "limits": {"transcodeGpu": 4, "transcodeCpu": 0, "healthGpu": 0, "healthCpu": 2},
                "queue": {"transcodeGpu": 0, "transcodeCpu": 0, "healthGpu": 0, "healthCpu": 0}
            }],
            "totals": {
                "files": 15, "transcodes": 8575, "healthChecks": 36734,
                "savedBytes": 17_581_694_338_397_i64,
                "tables": {
                    "queue": 0, "done": 15, "failed": 0,
                    "healthQueue": 0, "healthy": 15, "healthFailed": 0
                }
            },
            "libraries": services[1]["libraries"],
            "flows": services[1]["flows"],
            "queue": {"total": 0, "files": [], "readAt": services[1]["queue"]["readAt"]}
        })
    );
    let worker_libraries = services[1]["libraries"].as_array().unwrap();
    assert_eq!(worker_libraries.len(), 7);
    assert_eq!(
        worker_libraries[4],
        json!({
            "id": "HushMovHEVC_1080", "name": "Movies - HUSH HEVC 1080p",
            "folder": "/media/hush/queue/movies/hevc_1080p",
            "flow": {"id": "hush_movie_hevc_1080_v1", "name": "HUSH Movie HEVC 1080p (SHH)"},
            "priority": 15, "held": false, "watching": true, "transcodes": 108,
            "savedBytes": 224_777_461_416_i64
        })
    );
    let worker_flows = services[1]["flows"].as_array().unwrap();
    assert_eq!(worker_flows.len(), 4);
    assert_eq!(keys(&worker_flows[1]), ["about", "id", "name", "steps"]);
    assert_eq!(
        (&worker_flows[1]["id"], &worker_flows[1]["steps"]),
        (
            &json!("hush_movie_hevc_1080_v1"),
            &json!([
                "Input",
                "Check File Size (Skip tiny samples)",
                "HEVC + Audio Gate",
                "Verify",
                "Radarr Finalizer (SHH)",
                "Set Original File"
            ])
        )
    );
    // The bridge has no such records.
    for own in ["totals", "libraries", "flows", "queue"] {
        assert!(services[0].get(own).is_none(), "{own}");
    }
    assert_eq!(page["workers"], json!([]));
    assert_eq!(
        page["counts"],
        json!({
            "open": 2, "working": 0, "waiting": 2, "verifying": 0, "stuck": 2, "imported": 6,
            "failed": 1, "refused": 1, "pruned": 1, "superseded": 1, "total": 12,
            "savedBytes": 38_408_951_548_i64, "since": "2026-11-11T14:35:41Z"
        })
    );
    // One flow per label, in the order a person chooses from, each with its numbers.
    let flows = page["flows"].as_array().unwrap();
    let ids: Vec<&str> = flows.iter().map(|f| f["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![
            "hevc",
            "av1",
            "encode",
            "audio",
            "downscale",
            "shrink",
            "dual-encode",
            "dual-audio",
            "strip"
        ]
    );
    assert_eq!(
        flows[0],
        json!({
            "id": "hevc", "label": "Convert to HEVC", "about": "Re-encodes the video to HEVC",
            "service": "hush", "tag": "hush-hevc", "modes": ["hevc"],
            "needs": {"notCodec": ["HEVC", "AV1"]}, "runs": 8, "imported": 3,
            "lastRun": "2026-11-23T14:55:00Z", "savedPercent": 26.5, "medianSeconds": 675
        })
    );
    assert_eq!(
        flows[3],
        json!({
            "id": "audio", "label": "Rebuild audio", "about": "Rebuilds the audio tracks",
            "service": "hush", "tag": "hush-audio", "modes": ["audio", "uhd-audio"],
            "runs": 2, "imported": 1, "lastRun": "2026-11-20T18:46:00Z",
            "savedPercent": 1.2, "medianSeconds": 831
        }),
        "the modes that share a label are one flow, and a flow without needs has no `needs`"
    );
    assert_eq!(flows[2]["modes"], json!(["encode", "uhd"]));
    assert_eq!(flows[4]["needs"], json!({"minResolution": 2160}));
    assert_eq!(flows[1]["needs"], json!({"notCodec": ["AV1"]}));
    assert_eq!(
        (
            &flows[1]["runs"],
            &flows[1]["lastRun"],
            &flows[1]["savedPercent"],
            &flows[1]["medianSeconds"]
        ),
        (&json!(0), &json!(null), &json!(null), &json!(null)),
        "a flow no run has used is offered all the same"
    );

    // The open runs, oldest first. Both started days ago and no worker is on them: stuck.
    let open = page["open"].as_array().unwrap();
    assert_eq!(open.len(), 2);
    let waiting = &open[0];
    assert_eq!(waiting["id"], "hush:20261111T214740Z-e90af04d");
    assert_eq!(
        (&waiting["state"], &waiting["stuck"], &waiting["title"]),
        (&json!("staged"), &json!(true), &json!(null))
    );
    assert_eq!(
        waiting["episode"],
        json!(null),
        "a run no title holds names no episode"
    );
    assert_eq!(
        waiting["trigger"],
        json!({"kind": "backlog", "label": "Backlog", "raw": "backlog drain"})
    );
    // Where it is in the worker's own records: the library whose folder its file waits in,
    // that library's flow, and where the file stands there now. The worker has no record of
    // this one, which is why it is stuck.
    assert!(waiting["inWorker"]["checkedAt"].is_string(), "{waiting}");
    assert_eq!(
        waiting["inWorker"],
        json!({
            "service": "tdarr",
            "library": {"id": "HushShowHEVC_1080", "name": "TV - HUSH HEVC 1080p"},
            "flow": {"id": "hush_tv_hevc_1080_v1", "name": "HUSH TV HEVC 1080p (SHH)"},
            "state": "absent", "words": null,
            "checkedAt": waiting["inWorker"]["checkedAt"]
        })
    );
    assert_eq!(waiting["stuckWhy"], "Not in worker queue");
    let staged = &open[1];
    assert_eq!(
        staged["title"],
        json!({
            "id": "movie-1495", "kind": "movie", "title": "Kingdom of Heaven", "year": 2005,
            "poster": "/poster-1495.jpg"
        })
    );
    assert_eq!(
        (&staged["flow"], &staged["flowLabel"], &staged["stuck"]),
        (&json!("audio"), &json!("Rebuild audio"), &json!(true))
    );
    assert_eq!(
        staged["trigger"],
        json!({"kind": "tag", "label": "Tag hush-audio", "raw": "tag:hush-audio"})
    );
    for absent in [
        "output", "result", "saved", "finished", "took", "worker", "detail", "episode",
    ] {
        assert!(staged[absent].is_null(), "{absent}: {staged}");
    }
    assert_eq!(staged["source"]["file"], KINGDOM_FILE);
    assert_eq!(staged["source"]["size"], 9_295_592_609_i64);
    // The worker has this one in its queue, in its own word.
    assert_eq!(
        (
            &staged["inWorker"]["library"],
            &staged["inWorker"]["flow"],
            &staged["inWorker"]["state"],
            &staged["inWorker"]["words"]
        ),
        (
            &json!({"id": "DRItYe5ns", "name": "Movies - HUSH 1080p Audio"}),
            &json!({
                "id": "hush_pipeline_movie_1080_master_v1", "name": "HUSH Movie 1080p Master"
            }),
            &json!("queued"),
            &json!("Queued")
        )
    );
    assert_eq!(staged["stuckWhy"], "In worker queue");

    // The runs that closed last, the last to close first. An imported run in full.
    let recent = page["recent"].as_array().unwrap();
    let states: Vec<&str> = recent
        .iter()
        .map(|r| r["state"].as_str().unwrap())
        .collect();
    assert_eq!(
        states,
        vec![
            "imported",
            "imported",
            "imported",
            "imported",
            "imported",
            "imported",
            "pruned",
            "superseded",
            "failed",
            "refused"
        ]
    );
    let source = "Avatar - The Last Airbender (2024) - S01E01 - Aang [WEBDL-1080p][EAC3 Atmos 5.1][x264]-Kitsune.mkv";
    let made = "Avatar - The Last Airbender (2024) - - S01E01 [NF WEB-DL-1080p][Opus 5.1][HEVC HUSH]-[Kitsune].mkv";
    let staging =
        "/media/hush/staging/shows/hevc_1080p/Avatar - The Last Airbender (2024) -/Season 01";
    assert_eq!(
        recent[0],
        json!({
            "id": "hush:20261123T145500Z-57ec91da", "service": "hush",
            "flow": "hevc", "flowLabel": "Convert to HEVC",
            "trigger": {"kind": "rule", "label": "Import rule", "raw": "hush_rules"},
            "state": "imported",
            "at": "2026-11-23T14:55:00Z", "updated": "2026-11-23T15:06:15Z",
            "finished": "2026-11-23T15:06:15Z", "took": 675,
            "title": {
                "id": "show-82452", "kind": "show", "title": "Avatar: The Last Airbender",
                "year": 2024, "poster": "/poster-82452.jpg"
            },
            "episode": {"season": 1, "number": 1},
            "source": {
                "file": source, "path": format!("{AVATAR_FOLDER}/Season 01/{source}"),
                "size": 2_562_033_987_i64
            },
            "output": {
                "file": made, "path": format!("{staging}/{made}"), "size": 2_021_176_200_i64
            },
            "result": {
                "file": AVATAR_FILE, "path": format!("{AVATAR_FOLDER}/Season 01/{AVATAR_FILE}"),
                "size": 2_021_176_200_i64
            },
            "saved": 540_857_787,
            "replaced": [{
                "file": source, "path": format!("{AVATAR_FOLDER}/Season 01/{source}"),
                "size": 2_562_033_987_i64, "stillThere": false
            }],
            "detail": null,
            "stuck": false,
            "worker": null,
            "events": [
                {"at": "2026-11-23T14:55:00Z", "event": "staged", "actor": "hushq", "note": null},
                {"at": "2026-11-23T15:06:13Z", "event": "output_staged",
                 "actor": "sonarr-finalizer", "note": null},
                {"at": "2026-11-23T15:06:13Z", "event": "identity_ok", "actor": "hush-api",
                 "note": "content identity, 1 file(s)"},
                {"at": "2026-11-23T15:06:15Z", "event": "promoted", "actor": "hush-api",
                 "note": null},
                {"at": "2026-11-23T15:06:15Z", "event": "removed", "actor": "hush-api",
                 "note": "import confirmed"}
            ],
            "inWorker": {
                "service": "tdarr",
                "library": {"id": "HushShowHEVC_1080", "name": "TV - HUSH HEVC 1080p"},
                "flow": {"id": "hush_tv_hevc_1080_v1", "name": "HUSH TV HEVC 1080p (SHH)"},
                "state": null, "words": null, "checkedAt": null
            },
            "stuckWhy": null
        })
    );
    let failed = &recent[8];
    assert_eq!(
        failed["detail"],
        "import not confirmed; the output stays in staging"
    );
    assert_eq!(
        (&failed["took"], &failed["saved"], &failed["result"]),
        (&json!(821), &json!(null), &json!(null))
    );
    assert!(failed["output"]["size"].is_i64() && failed["title"].is_null());

    // The runs list: filters, paging, and what a query may not say.
    let total = |query: &'static str| {
        let app = app.clone();
        async move {
            get_ok(&app, &format!("/api/v1/processing/runs{query}")).await["total"]
                .as_i64()
                .unwrap()
        }
    };
    let list = get_ok(&app, "/api/v1/processing/runs").await;
    assert_eq!(keys(&list), ["limit", "offset", "runs", "total"]);
    assert_eq!(
        (&list["total"], &list["limit"], &list["offset"]),
        (&json!(12), &json!(50), &json!(0))
    );
    let runs = list["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 12);
    assert_eq!(
        runs[0], recent[0],
        "a run reads the same wherever it is listed"
    );
    assert!(
        runs.windows(2)
            .all(|w| w[0]["at"].as_str() >= w[1]["at"].as_str()),
        "newest first"
    );
    let rest = get_ok(&app, "/api/v1/processing/runs?limit=5&offset=10").await;
    assert_eq!(
        (
            rest["runs"].as_array().unwrap().len(),
            &rest["total"],
            &rest["limit"],
            &rest["offset"]
        ),
        (2, &json!(12), &json!(5), &json!(10))
    );
    assert_eq!(
        get_ok(&app, "/api/v1/processing/runs?limit=9999").await["limit"],
        500
    );
    // Filters left empty are filters left out.
    assert_eq!(
        total("?state=&flow=&service=&title=&q=&limit=&offset=").await,
        12
    );
    assert_eq!(total("?state=open").await, 2);
    assert_eq!(total("?state=closed").await, 10);
    assert_eq!(total("?state=imported").await, 6);
    assert_eq!(total("?state=staged").await, 2);
    assert_eq!(total("?state=working").await, 0);
    assert_eq!(total("?state=verifying").await, 0);
    assert_eq!(total("?state=stuck").await, 2);
    assert_eq!(total("?state=refused").await, 1);
    // A flow's id asks for every mode of it; a mode asks for itself.
    assert_eq!(total("?flow=audio").await, 2);
    assert_eq!(total("?flow=uhd-audio").await, 1);
    assert_eq!(total("?flow=hevc").await, 8);
    assert_eq!(total("?flow=encode&service=hush").await, 1);
    assert_eq!(total("?flow=audio&state=open").await, 1);
    assert_eq!(total("?flow=shrink").await, 0);
    assert_eq!(total("?service=hush").await, 12);
    assert_eq!(total("?service=tdarr").await, 0);
    assert_eq!(total("?title=movie-763215").await, 1);
    assert_eq!(total("?title=show-82452").await, 1);
    assert_eq!(
        total("?title=movie-82452").await,
        0,
        "a show is not a movie"
    );
    assert_eq!(total("?title=movie-999").await, 0);
    assert_eq!(total("?q=windmills").await, 1);
    assert_eq!(total("?q=kitsune%20AANG").await, 1);
    assert_eq!(total("?q=shows").await, 0);
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    for bad in [
        "?state=promoted",
        "?title=the-matrix",
        "?title=show-x",
        "?limit=many",
        "?offset=1.5",
    ] {
        let path = format!("/api/v1/processing/runs{bad}");
        let (status, content_type, body) = call(&app, "GET", &path, &session, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(
            content_type.starts_with("application/problem+json"),
            "{bad}"
        );
    }

    // One run, with the job exactly as the service gave it.
    let one = get_ok(
        &app,
        "/api/v1/processing/runs/hush:20261122T121501Z-52c0381d",
    )
    .await;
    assert_eq!(one["title"]["id"], "movie-763215");
    assert_eq!(one["result"]["file"], DAMSEL_FILE);
    assert_eq!(one["saved"], 1_483_110_958_i64);
    assert!(one["episode"].is_null());
    let ledger_json: serde_json::Value = serde_json::from_str(&ledger).unwrap();
    assert_eq!(one["raw"], ledger_json["jobs"][2]);
    // No worker holds a record of this run's file, so there are no reports of it.
    assert_eq!(one["reports"], json!(null));
    let mut without_raw = one.clone();
    without_raw.as_object_mut().unwrap().remove("raw");
    without_raw.as_object_mut().unwrap().remove("reports");
    assert_eq!(
        keys(&without_raw),
        keys(&recent[0]),
        "a run and nothing else"
    );
    // Read by its id, a run says what its worker flow is for and its steps; in a list it
    // names the flow. Everything else reads the same.
    let listed_run = runs.iter().find(|r| r["id"] == one["id"]).unwrap();
    assert_eq!(
        listed_run["inWorker"]["flow"],
        json!({"id": "hush_movie_hevc_1080_v1", "name": "HUSH Movie HEVC 1080p (SHH)"})
    );
    let flow = &one["inWorker"]["flow"];
    assert_eq!(keys(flow), ["about", "id", "name", "steps"]);
    assert_eq!(flow["steps"], worker_flows[1]["steps"]);
    assert!(
        flow["about"]
            .as_str()
            .unwrap()
            .starts_with("Codec conversion and nothing else.")
    );
    let mut as_listed = without_raw.clone();
    as_listed["inWorker"]["flow"] = listed_run["inWorker"]["flow"].clone();
    assert_eq!(&as_listed, listed_run);
    assert_eq!(
        one["inWorker"]["state"],
        json!(null),
        "a closed run is not asked about"
    );
    let encoded = get_ok(
        &app,
        "/api/v1/processing/runs/hush%3A20261122T121501Z-52c0381d",
    )
    .await;
    assert_eq!(encoded["id"], one["id"]);
    let (status, content_type, body) = call(
        &app,
        "GET",
        "/api/v1/processing/runs/hush:nothing",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(content_type.starts_with("application/problem+json"));
    assert_eq!(body["code"], "singularr.not_found");
    let (status, _, _) = call(&app, "GET", "/api/v1/processing", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The title record: the title's own runs, and each of its files with what processing did
    // or does to it.
    let tmdb = |path: &str| format!("https://api.themoviedb.org/3{path}");
    for id in [603, 763215, 1495] {
        recorded.answer(
            Method::Get,
            &tmdb(&format!("/movie/{id}?append_to_response=")),
            200,
            &fixture("tmdb_movie_603.json").replacen("\"id\": 603", &format!("\"id\": {id}"), 1),
        );
    }
    recorded.answer(
        Method::Get,
        &tmdb("/tv/82452?append_to_response="),
        200,
        &fixture("tmdb_tv_1396.json").replacen("\"id\": 1396", "\"id\": 82452", 1),
    );
    recorded.answer(
        Method::Get,
        &tmdb("/tv/82452/season/1"),
        200,
        &fixture("tmdb_tv_1396_season_1.json"),
    );
    // A run in a title's record is the run as the lists give it, without its title.
    let untitled = |run: &serde_json::Value| {
        let mut run = run.clone();
        run.as_object_mut().unwrap().remove("title");
        run
    };
    // An open run says when its worker was asked, which is not the same for two answers.
    let unchecked = |run: &serde_json::Value| {
        let mut run = run.clone();
        if let Some(in_worker) = run["inWorker"].as_object_mut() {
            in_worker.remove("checkedAt");
        }
        run
    };
    let files = |record: &serde_json::Value| -> Vec<serde_json::Value> {
        record["library"]["editions"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| e["files"].as_array().unwrap().clone())
            .collect()
    };
    // A movie whose file an imported run made: the file is processed, by that run.
    let made_by_a_run = get_ok(&app, "/api/v1/metadata/titles/movie/763215").await;
    assert_eq!(
        made_by_a_run["processing"],
        json!({"runs": [untitled(listed_run)], "open": 0, "total": 1})
    );
    let held = files(&made_by_a_run);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["fileName"], DAMSEL_FILE);
    assert_eq!(
        held[0]["processing"],
        json!({"state": "processed", "run": "hush:20261122T121501Z-52c0381d"})
    );
    // A movie whose file an open run was staged from: the file is being processed.
    let in_a_queue = get_ok(&app, "/api/v1/metadata/titles/movie/1495").await;
    assert_eq!(
        (
            &in_a_queue["processing"]["open"],
            &in_a_queue["processing"]["total"]
        ),
        (&json!(1), &json!(1))
    );
    let of_title = in_a_queue["processing"]["runs"].as_array().unwrap();
    assert_eq!(of_title.len(), 1);
    assert_eq!(unchecked(&of_title[0]), unchecked(&untitled(staged)));
    assert_eq!(of_title[0]["stuckWhy"], "In worker queue");
    let held = files(&in_a_queue);
    assert_eq!(held.len(), 1);
    assert_eq!(held[0]["fileName"], KINGDOM_FILE);
    assert_eq!(
        held[0]["processing"],
        json!({"state": "processing", "run": "hush:20261115T001015Z-cb34e7ec"})
    );
    // A show: its runs name their episodes, and a season's episode carries the mark on its
    // file.
    let show = get_ok(&app, "/api/v1/metadata/titles/show/82452").await;
    assert_eq!(
        show["processing"],
        json!({"runs": [untitled(&recent[0])], "open": 0, "total": 1})
    );
    assert_eq!(
        show["processing"]["runs"][0]["episode"],
        json!({"season": 1, "number": 1})
    );
    let season = get_ok(&app, "/api/v1/metadata/shows/82452/seasons/1").await;
    let first = &season["episodes"][0];
    assert_eq!(
        (&first["state"], &first["file"]["fileName"]),
        (&json!("available"), &json!(AVATAR_FILE))
    );
    assert_eq!(
        first["file"]["processing"],
        json!({"state": "processed", "run": "hush:20261123T145500Z-57ec91da"})
    );
    assert!(season["episodes"][1]["file"].is_null());
    // A title nothing was run on: the block is there, and it is empty.
    let untouched = get_ok(&app, "/api/v1/metadata/titles/movie/603").await;
    assert!(untouched["library"].is_null());
    assert_eq!(
        untouched["processing"],
        json!({"runs": [], "open": 0, "total": 0})
    );

    // The pages that list integrations and health know the two services: the integrations,
    // the workspace record, and a check each on the system page.
    let listed = get_ok(&app, "/api/v1/integrations").await;
    let panel = |id: &str| {
        listed["integrations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is listed"))
    };
    let bridge_panel = panel("hush");
    assert_eq!(
        (
            &bridge_panel["type"],
            &bridge_panel["group"],
            &bridge_panel["state"],
            &bridge_panel["configured"]
        ),
        (
            &json!("hush"),
            &json!("processing"),
            &json!("ok"),
            &json!(true)
        )
    );
    assert_eq!(bridge_panel["detail"], "12 runs, 2 open");
    assert!(!bridge_panel["reads"].as_array().unwrap().is_empty());
    // What Singularr could change in the bridge, and that it may not: its access is `read`.
    assert_eq!(bridge_panel["writes"], json!(["Send a file to a flow"]));
    assert_eq!(
        (&bridge_panel["access"], &bridge_panel["accessSource"]),
        (&json!("read"), &json!("default"))
    );
    let worker_panel = panel("tdarr");
    assert_eq!(
        (&worker_panel["group"], &worker_panel["detail"]),
        (&json!("processing"), &json!("1 node, 0 busy"))
    );
    let bridge_page = get_ok(&app, "/api/v1/integrations/hush").await;
    let fields: Vec<&str> = bridge_page["settings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        vec![
            "integrations.hush.name",
            "integrations.hush.address",
            "integrations.hush.apiKey",
            "integrations.hush.syncEvery",
            "integrations.hush.access"
        ]
    );
    let system = get_ok(&app, "/api/v1/system").await;
    let check = |id: &str| {
        system["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .cloned()
    };
    assert_eq!(
        check("processing-hush"),
        Some(json!({
            "id": "processing-hush", "label": "Processing service",
            "state": "ok", "detail": "HUSH: 12 runs, 2 open"
        }))
    );
    assert_eq!(
        check("processing-tdarr"),
        Some(json!({
            "id": "processing-tdarr", "label": "Processing service",
            "state": "ok", "detail": "Tdarr: 1 node, 0 busy"
        }))
    );
    let together = check("integrations").unwrap();
    assert!(
        !together["detail"].as_str().unwrap().contains("HUSH"),
        "a processing service is said by its own check: {together}"
    );
    let ws = get_ok(&app, "/api/v1/workspace").await;
    let mut in_workspace: Vec<(&str, &str)> = ws["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["role"] == "processing")
        .map(|i| (i["id"].as_str().unwrap(), i["state"].as_str().unwrap()))
        .collect();
    in_workspace.sort_unstable();
    assert_eq!(in_workspace, vec![("hush", "ok"), ("tdarr", "ok")]);
    // The two staged runs started days ago and no worker is on them: both are in the bell,
    // the newer first, each named by its title or, with no title, by its file.
    let bell: Vec<&serde_json::Value> = ws["attention"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == "processing")
        .collect();
    assert_eq!(bell.len(), 2, "{ws}");
    assert_eq!(
        (
            &bell[0]["id"],
            &bell[0]["title"],
            &bell[0]["tone"],
            &bell[0]["since"]
        ),
        (
            &json!("hush:20261115T001015Z-cb34e7ec"),
            &json!("Kingdom of Heaven"),
            &json!("warn"),
            &staged["at"]
        )
    );
    // Each says why it is stuck, in the sentence the worker's own records give.
    let why = bell[0]["detail"].as_str().unwrap();
    assert!(
        why.starts_with("Stuck ") && why.ends_with(" d, in worker queue"),
        "{why}"
    );
    assert_eq!(
        (&bell[1]["id"], &bell[1]["title"]),
        (
            &json!("hush:20261111T214740Z-e90af04d"),
            &json!("Shoresy (2022) - S01E03 - Know Your Role [HDTV-1080p][EAC3 5.1][h264].mkv")
        )
    );
    let why = bell[1]["detail"].as_str().unwrap();
    assert!(
        why.starts_with("Stuck ") && why.ends_with(" d, not in worker queue"),
        "{why}"
    );
    // Home lists the same runs, with the reason in its own field.
    for manager in [RADARR, SONARR] {
        recorded.answer(
            Method::Get,
            &format!("{manager}/api/v3/calendar"),
            200,
            "[]",
        );
    }
    let home = get_ok(&app, "/api/v1/home").await;
    let on_home: Vec<&serde_json::Value> = home["attention"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == "processing")
        .collect();
    assert_eq!(on_home.len(), 2, "{home}");
    assert_eq!(
        (&on_home[0]["id"], &on_home[0]["title"]),
        (&bell[0]["id"], &bell[0]["title"])
    );
    assert!(
        on_home[0]["reason"]
            .as_str()
            .unwrap()
            .ends_with("in worker queue"),
        "{home}"
    );

    // A worker on the staged run's queue path: the run is working, and it is not stuck.
    let mut busy_nodes: serde_json::Value =
        serde_json::from_str(&fixture("tdarr_nodes_idle.json")).unwrap();
    busy_nodes["TNUn37R"]["workers"] = json!({
        "w1": {
            "file": queue, "percentage": 43.2, "fps": 58, "ETA": "0:12:00",
            "status": "Processing", "workerType": "transcodegpu",
            "startTime": 1_795_446_060_000_i64
        }
    });
    recorded.answer(
        Method::Get,
        &format!("{TDARR}/api/v2/get-nodes"),
        200,
        &busy_nodes.to_string(),
    );
    // What the services say is kept a few seconds, so a state of its own asks them again.
    let env = [
        ("HUSH_URL", HUSH),
        ("TDARR_URL", TDARR),
        ("RADARR_API_KEY", "test-key"),
        ("SONARR_API_KEY", "test-key"),
        ("SINGULARR_TMDB_TOKEN", "test-token"),
    ];
    let busy = routes::router(state_on(&t.pool, journal.clone(), &env, recorded.clone()));
    let page = get_ok(&busy, "/api/v1/processing").await;
    assert_eq!(
        page["workers"],
        json!([{
            "service": "tdarr", "node": "Tdarr-Node-5090", "file": queue, "percent": 43.2,
            "fps": 58.0, "eta": "0:12:00", "status": "Processing", "kind": "transcodegpu",
            "startedAt": "2026-11-23T15:01:00Z", "run": "hush:20261115T001015Z-cb34e7ec"
        }])
    );
    assert_eq!(page["services"][1]["detail"], "1 node, 1 busy");
    assert_eq!(page["services"][1]["nodes"][0]["busy"], 1);
    assert_eq!(
        (
            &page["counts"]["open"],
            &page["counts"]["working"],
            &page["counts"]["waiting"],
            &page["counts"]["stuck"]
        ),
        (&json!(2), &json!(1), &json!(1), &json!(1))
    );
    let working = &page["open"][1];
    assert_eq!(
        (&working["state"], &working["stuck"]),
        (&json!("working"), &json!(false))
    );
    // The worker says so in its own word, and a run that is worked on has no reason to be
    // stuck. This worker's libraries were not read yet (a new process), so no library is named.
    assert_eq!(
        (
            &working["inWorker"]["service"],
            &working["inWorker"]["library"],
            &working["inWorker"]["state"],
            &working["inWorker"]["words"],
            &working["stuckWhy"]
        ),
        (
            &json!("tdarr"),
            &json!(null),
            &json!("working"),
            &json!("Processing"),
            &json!(null)
        )
    );
    assert_eq!(page["open"][0]["inWorker"]["state"], "absent");
    assert!(page["services"][1].get("libraries").is_none());
    let mut on_it = page["workers"][0].clone();
    on_it.as_object_mut().unwrap().remove("run");
    assert_eq!(
        working["worker"], on_it,
        "the worker's entry, without `run`"
    );
    assert_eq!(page["open"][0]["state"], "staged");
    let count = |query: &'static str| {
        let app = busy.clone();
        async move {
            get_ok(&app, &format!("/api/v1/processing/runs{query}")).await["total"]
                .as_i64()
                .unwrap()
        }
    };
    assert_eq!(count("?state=working").await, 1);
    assert_eq!(count("?state=staged").await, 1);
    assert_eq!(count("?state=stuck").await, 1);
    assert_eq!(count("?state=open").await, 2);
    let listed = get_ok(&busy, "/api/v1/processing/runs?state=working").await;
    assert_eq!(listed["runs"][0]["worker"], on_it);
    let one = get_ok(
        &busy,
        "/api/v1/processing/runs/hush:20261115T001015Z-cb34e7ec",
    )
    .await;
    assert_eq!((&one["state"], &one["worker"]), (&json!("working"), &on_it));
    // The title's record says the same of its run.
    let record = get_ok(&busy, "/api/v1/metadata/titles/movie/1495").await;
    let mut worked_on = untitled(&one);
    worked_on.as_object_mut().unwrap().remove("raw");
    worked_on.as_object_mut().unwrap().remove("reports");
    assert_eq!(
        (
            &record["processing"]["open"],
            &record["processing"]["total"]
        ),
        (&json!(1), &json!(1))
    );
    assert_eq!(
        unchecked(&record["processing"]["runs"][0]),
        unchecked(&worked_on)
    );
    assert_eq!(
        files(&record)[0]["processing"]["state"],
        "processing",
        "a file a worker is on is being processed"
    );

    // A service that does not answer is bad with why; the runs are answered all the same.
    recorded.answer(Method::Get, &format!("{TDARR}/api/v2/status"), 503, "");
    let down = routes::router(state_on(&t.pool, journal.clone(), &env, recorded.clone()));
    let page = get_ok(&down, "/api/v1/processing").await;
    let worker_service = &page["services"][1];
    assert_eq!(
        (&worker_service["id"], &worker_service["state"]),
        (&json!("tdarr"), &json!("bad"))
    );
    assert!(
        worker_service["detail"].as_str().unwrap().contains("503"),
        "{worker_service}"
    );
    assert!(worker_service.get("nodes").is_none());
    assert_eq!(
        worker_service["version"], "3.09.00",
        "as its last pass read it"
    );
    assert_eq!(page["services"][0]["state"], "ok");
    assert_eq!(page["workers"], json!([]));
    assert_eq!(page["counts"]["total"], 12);
    assert_eq!(page["open"].as_array().unwrap().len(), 2);
    // With the sync switched off no service is asked: the page says what the last pass left.
    let asked = recorded.urls_seen().len();
    let mut quiet_env = env.to_vec();
    quiet_env.push(("SINGULARR_SYNC_ENABLED", "false"));
    let quiet = routes::router(state_on(
        &t.pool,
        journal.clone(),
        &quiet_env,
        recorded.clone(),
    ));
    let page = get_ok(&quiet, "/api/v1/processing").await;
    assert_eq!(recorded.urls_seen().len(), asked, "nothing was asked");
    assert_eq!(
        (
            &page["services"][0]["state"],
            &page["services"][0]["detail"]
        ),
        (&json!("ok"), &json!("12 runs, 2 open"))
    );
    assert!(page["services"][0].get("facts").is_none());
    assert_eq!(page["counts"]["stuck"], 2);

    // An installation without a processing integration says nothing of processing on a title.
    let plain = routes::router(state_on(
        &t.pool,
        journal.clone(),
        &[
            ("RADARR_API_KEY", "test-key"),
            ("SONARR_API_KEY", "test-key"),
            ("SINGULARR_TMDB_TOKEN", "test-token"),
        ],
        recorded.clone(),
    ));
    let record = get_ok(&plain, "/api/v1/metadata/titles/movie/763215").await;
    assert!(
        record.get("processing").is_some_and(|p| p.is_null()),
        "{record}"
    );
    let held = files(&record);
    assert!(
        held[0].get("processing").is_some_and(|p| p.is_null()),
        "{held:?}"
    );

    // A run that is stored unlinked is linked by the next whole read once the catalog has its
    // title. The show is inserted here as a title alone, with no folder: the TVDB id in the
    // name of the run's folder is what links it.
    sqlx::query(
        "INSERT INTO titles (ext_key, kind, tvdb_id, title, sort_title, year)
         VALUES ('test:the-expanse', 'show', 280619, 'The Expanse', 'expanse', 2015)",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let expanse = title_with_tvdb(&t.pool, 280619).await;
    let mut restarted = processing::Reader::new(bridge.clone());
    let relinked = restarted
        .pass(&t.pool, std::time::Instant::now())
        .await
        .unwrap();
    assert_eq!(read(&relinked), (processing::Read::Whole, 12, 0, 1));
    assert_eq!(relinked.report.linked, 4);
    let linked = run("20261123T134700Z-6b66bd7b").await;
    assert_eq!(
        (linked.title_id, linked.season, linked.episode),
        (Some(expanse), Some(1), Some(7))
    );

    // A run that failed lately is in the bell too, with what its service said. The recorded
    // failure is weeks old, so the table is told it closed an hour ago.
    sqlx::query(
        "UPDATE processing_runs SET finished_at = now() - interval '1 hour'
         WHERE id = 'hush:20261111T150853Z-4cec4841'",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let ws = get_ok(&app, "/api/v1/workspace").await;
    let failed_lately = ws["attention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "processing" && a["tone"] == "bad")
        .cloned()
        .unwrap_or_else(|| panic!("{ws}"));
    assert_eq!(failed_lately["id"], "hush:20261111T150853Z-4cec4841");
    assert_eq!(
        failed_lately["detail"],
        "Failed: import not confirmed; the output stays in staging"
    );
    assert_eq!(
        failed_lately["title"],
        "Darkness - Those Who Kill (2019) - S04E04 - Episode 4 [WEBDL-1080p][EAC3 5.1][h264]-Kitsune.mkv"
    );

    // Nothing but GET reached a service or a manager, but for the questions the worker takes
    // with POST: its record store, in a mode that reads, and its queue table.
    let mut questions = 0;
    for call in recorded.calls_sent() {
        if call.method == Method::Get {
            continue;
        }
        let Sent::Json(text) = &call.body else {
            panic!("{:?}", call.body)
        };
        let body: serde_json::Value = serde_json::from_str(text).unwrap();
        if call.url == format!("{TDARR}/api/v2/client/status-tables") {
            assert_eq!(body["data"]["opts"], json!({"table": "table1"}), "{body}");
            continue;
        }
        assert_eq!(
            (call.method, call.url.as_str()),
            (Method::Post, records.as_str())
        );
        assert!(
            matches!(body["data"]["mode"].as_str(), Some("getAll" | "getById")),
            "{body}"
        );
        questions += 1;
    }
    assert!(questions > 0);
    t.drop_db().await;
}

/// GET a path as the owner and expect an answer.
async fn get_ok(app: &axum::Router, path: &str) -> serde_json::Value {
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, body) = call(app, "GET", path, &session, None).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

/// What Singularr may do with an integration is that integration's own permission: `read` by
/// default, changed by the owner, fixed by the environment, enforced where every call leaves.
#[tokio::test]
async fn what_singularr_may_do_with_an_integration_is_its_own_permission() {
    let Some(t) = TestDb::new("access").await else {
        return;
    };
    use serde_json::json;
    let journal = Journal::new();
    let recorded = Recorded::new();
    let out = outbound(recorded.clone());
    let services = [("HUSH_URL", HUSH), ("TDARR_URL", TDARR)];
    let st = state_over(&t.pool, journal.clone(), &services, &out);
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let stage = format!("{HUSH}/stage");
    let may_stage = |out: &Outbound| out.allowlist().check(Method::Post, &stage).is_ok();
    let integration = |ws: &serde_json::Value, id: &str| {
        ws["integrations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is in the workspace record"))
    };
    // Each action with its own access.
    let send = json!([{"id": "send", "label": "Send a file to a flow", "access": "read"}]);
    let manager = json!([
        {"id": "search", "label": "Search for releases", "access": "read"},
        {"id": "grab", "label": "Grab a release", "access": "read"},
        {"id": "import", "label": "Import a refused download", "access": "read"},
        {"id": "remove", "label": "Remove a download", "access": "read"}
    ]);

    // By default Singularr observes: every integration is read, and only the processing
    // services and the managers have actions that could be allowed.
    let ws = get_ok(&app, "/api/v1/workspace").await;
    assert_eq!(
        (&ws["mode"], &ws["acting"]),
        (&json!("observe"), &json!([]))
    );
    let bridge = integration(&ws, "hush");
    assert_eq!(
        (
            &bridge["access"],
            &bridge["accessSource"],
            &bridge["actions"]
        ),
        (&json!("read"), &json!("default"), &send)
    );
    let worker = integration(&ws, "tdarr");
    assert_eq!(
        (
            &worker["access"],
            &worker["accessSource"],
            &worker["actions"]
        ),
        (
            &json!("read"),
            &json!("default"),
            &json!([
                {"id": "limits", "label": "Change worker limits", "access": "read"},
                {"id": "pause", "label": "Pause and resume nodes", "access": "read"},
                {"id": "cancel", "label": "Cancel a worker's file", "access": "read"},
                {"id": "requeue", "label": "Requeue a file", "access": "read"},
                {"id": "scan", "label": "Scan a library", "access": "read"},
                {"id": "watch", "label": "Switch folder watching", "access": "read"},
                {"id": "flow", "label": "Assign a flow to a library", "access": "read"}
            ])
        )
    );
    let radarr = integration(&ws, "radarr");
    assert_eq!(
        (&radarr["access"], &radarr["actions"]),
        (&json!("read"), &manager)
    );
    assert!(
        !may_stage(&out),
        "the act is refused while the access is read"
    );
    let listed = get_ok(&app, "/api/v1/integrations").await;
    for panel in listed["integrations"].as_array().unwrap() {
        assert_eq!(panel["access"], "read", "{panel}");
        assert_eq!(panel["accessSource"], "default", "{panel}");
        assert_eq!(
            panel["actions"],
            if panel["id"] == "hush" {
                send.clone()
            } else if panel["id"] == "tdarr" {
                worker["actions"].clone()
            } else if panel["id"] == "radarr" || panel["id"] == "sonarr" {
                manager.clone()
            } else {
                json!([])
            },
            "{panel}"
        );
    }
    let field = |record: &serde_json::Value, key: &str| {
        record["settings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["key"] == key)
            .cloned()
    };
    let one = get_ok(&app, "/api/v1/integrations/hush").await;
    assert_eq!(
        (&one["access"], &one["accessSource"]),
        (&json!("read"), &json!("default"))
    );
    let access = field(&one, "integrations.hush.access").expect("the bridge has an access field");
    assert_eq!(
        (
            &access["value"],
            &access["default"],
            &access["source"],
            &access["editable"],
            &access["restart"],
            &access["part"],
            &access["type"]
        ),
        (
            &json!("read"),
            &json!("read"),
            &json!("default"),
            &json!(true),
            &json!(false),
            &json!("access"),
            &json!("select")
        )
    );
    assert_eq!(
        access["options"],
        json!([{"value": "read", "label": "Read-only"}, {"value": "act", "label": "Write access"}])
    );
    // A type without an action has no such field; a manager has.
    let one = get_ok(&app, "/api/v1/integrations/seerr").await;
    assert!(field(&one, "integrations.seerr.access").is_none());
    let one = get_ok(&app, "/api/v1/integrations/radarr").await;
    assert!(field(&one, "integrations.radarr.access").is_some());

    // Only the owner changes it: a key does not, whatever its scopes.
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let admin = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "admin".into(),
            scopes: vec![Scope::Admin],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", admin.secret);
    let act = r#"{"access": "act"}"#;
    let (status, _, refused) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &[("authorization", bearer.as_str())],
        Some(act),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert_eq!(refused["code"], "singularr.forbidden");
    let (status, _, _) = call(&app, "PATCH", "/api/v1/integrations/hush", &[], Some(act)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    // What cannot be asked for: an integration that is not there, a type without an action
    // (a request site, a metadata provider), a word that is neither, a field that is not
    // `access`.
    for (path, body, expected) in [
        ("/api/v1/integrations/nothing", act, StatusCode::NOT_FOUND),
        ("/api/v1/integrations/seerr", act, StatusCode::BAD_REQUEST),
        ("/api/v1/integrations/tmdb", act, StatusCode::BAD_REQUEST),
        (
            "/api/v1/integrations/hush",
            r#"{"access": "write"}"#,
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/integrations/hush",
            r#"{"access": "act", "name": "Bridge"}"#,
            StatusCode::BAD_REQUEST,
        ),
        ("/api/v1/integrations/hush", "{}", StatusCode::BAD_REQUEST),
    ] {
        let (status, content_type, problem) = call(&app, "PATCH", path, &session, Some(body)).await;
        assert_eq!(status, expected, "{path} {body}: {problem}");
        assert!(
            content_type.starts_with("application/problem+json"),
            "{path}"
        );
    }
    let (_, _, no_action) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/seerr",
        &session,
        Some(act),
    )
    .await;
    assert_eq!(no_action["detail"], "Seerr is read-only.");
    assert!(!may_stage(&out) && st.engines.access.acting().is_empty());
    let events = |pool: &PgPool| {
        let pool = pool.clone();
        async move {
            kinds(&pool, 0)
                .await
                .into_iter()
                .filter(|k| k == "integration_access_changed")
                .count()
        }
    };
    assert_eq!(events(&t.pool).await, 0, "nothing was changed so far");

    // The owner allows acting on the bridge: it is in force at once, without a restart.
    let (status, _, allowed) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(act),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{allowed}");
    assert_eq!(
        (&allowed["id"], &allowed["access"], &allowed["accessSource"]),
        (&json!("hush"), &json!("act"), &json!("saved"))
    );
    assert_eq!(
        allowed["actions"],
        json!([{"id": "send", "label": "Send a file to a flow", "access": "act"}])
    );
    assert_eq!(allowed["writes"], json!(["Send a file to a flow"]));
    let access = field(&allowed, "integrations.hush.access").unwrap();
    assert_eq!(
        (&access["value"], &access["source"], &access["editable"]),
        (&json!("act"), &json!("saved"), &json!(true))
    );
    assert!(may_stage(&out), "the act passes with the next call");
    // Acting never turns a host into a free one, and one integration's access is its own.
    for (method, url) in [
        (Method::Post, format!("{HUSH}/strip-subs")),
        (Method::Delete, stage.clone()),
        (Method::Post, format!("{TDARR}/stage")),
        (Method::Post, format!("{RADARR}/api/v3/command")),
    ] {
        assert!(
            out.allowlist().check(method, &url).is_err(),
            "{} {url}",
            method.as_str()
        );
    }
    let ws = get_ok(&app, "/api/v1/workspace").await;
    assert_eq!(
        (&ws["mode"], &ws["acting"]),
        (&json!("act"), &json!([{"id": "hush", "name": "HUSH"}]))
    );
    assert_eq!(integration(&ws, "hush")["access"], "act");
    assert_eq!(integration(&ws, "tdarr")["access"], "read");
    assert_eq!(get_ok(&app, "/api/v1/system").await["mode"], "act");
    // The journal has the change, in the owner's name, about the integration.
    let page = get_ok(&app, "/api/v1/journal?kind=integration.access").await;
    let changes = page["events"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{page}");
    assert_eq!(
        (
            &changes[0]["kind"],
            &changes[0]["code"],
            &changes[0]["summary"]
        ),
        (
            &json!("integration.access"),
            &json!("integration_access_changed"),
            &json!("Send a file to a flow: access changed to write")
        )
    );
    assert_eq!(
        changes[0]["actor"],
        json!({"kind": "user", "id": "owner@example.com", "name": "owner@example.com"})
    );
    assert_eq!(
        changes[0]["subject"],
        json!({"type": "integration", "id": "hush", "label": "HUSH"})
    );
    assert_eq!(
        changes[0]["body"],
        json!({
            "integration": "hush",
            "name": "HUSH",
            "action": "send",
            "label": "Send a file to a flow",
            "from": "read",
            "to": "act"
        })
    );
    // Asked for what is in force already, nothing is written.
    let (status, _, same) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(act),
    )
    .await;
    assert_eq!((status, &same["access"]), (StatusCode::OK, &json!("act")));
    assert_eq!(events(&t.pool).await, 1);

    // Taken back, the act is refused again with the next call.
    let (status, _, back) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(r#"{"access": "read"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{back}");
    assert_eq!(
        (&back["access"], &back["accessSource"]),
        (&json!("read"), &json!("saved"))
    );
    assert!(!may_stage(&out));
    let ws = get_ok(&app, "/api/v1/workspace").await;
    assert_eq!(
        (&ws["mode"], &ws["acting"]),
        (&json!("observe"), &json!([]))
    );
    let page = get_ok(&app, "/api/v1/journal?kind=integration.access").await;
    assert_eq!(page["events"].as_array().unwrap().len(), 2);
    assert_eq!(
        page["events"][0]["summary"],
        "Send a file to a flow: access changed to read-only"
    );

    // What was saved is in force again after a restart, once it is read from the database.
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(act),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let restarted = outbound(recorded.clone());
    let st = state_over(&t.pool, journal.clone(), &services, &restarted);
    assert!(!may_stage(&restarted), "nothing saved is known yet");
    assert_eq!(st.engines.access.load(&t.pool).await.unwrap(), 1);
    assert!(may_stage(&restarted));
    let saved = st.engines.access.of("hush");
    assert_eq!(
        (saved.access.as_str(), saved.source, saved.editable()),
        ("act", "saved", true)
    );
    assert_eq!(st.engines.access.mode(), "act");

    // The environment fixes it: it wins over what was saved, and the owner cannot change it
    // here. The refusal names the variable.
    let pinned = outbound(recorded.clone());
    let st = state_over(
        &t.pool,
        journal.clone(),
        &[("HUSH_URL", HUSH), ("HUSH_ACCESS", "read")],
        &pinned,
    );
    st.engines.access.load(&t.pool).await.unwrap();
    assert!(!may_stage(&pinned), "the saved `act` does not win");
    let app = routes::router(st.clone());
    let one = get_ok(&app, "/api/v1/integrations/hush").await;
    assert_eq!(
        (&one["access"], &one["accessSource"]),
        (&json!("read"), &json!("environment"))
    );
    let access = field(&one, "integrations.hush.access").unwrap();
    assert_eq!(
        (&access["source"], &access["env"], &access["editable"]),
        (&json!("environment"), &json!("HUSH_ACCESS"), &json!(false))
    );
    let (status, content_type, fixed) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(act),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{fixed}");
    assert!(content_type.starts_with("application/problem+json"));
    assert_eq!(fixed["code"], "singularr.conflict");
    assert_eq!(
        fixed["detail"],
        "Access to HUSH is set by HUSH_ACCESS in the environment."
    );
    assert!(!may_stage(&pinned));
    assert_eq!(get_ok(&app, "/api/v1/workspace").await["mode"], "observe");
    // It can fix acting too, with nothing saved for it.
    let pinned = outbound(recorded.clone());
    let st = state_over(
        &t.pool,
        journal.clone(),
        &[("HUSH_URL", HUSH), ("HUSH_ACCESS", "act")],
        &pinned,
    );
    assert!(may_stage(&pinned));
    let app = routes::router(st.clone());
    let ws = get_ok(&app, "/api/v1/workspace").await;
    assert_eq!(ws["mode"], "act");
    assert_eq!(integration(&ws, "hush")["accessSource"], "environment");
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(r#"{"access": "read"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(may_stage(&pinned));
    assert_eq!(events(&t.pool).await, 3, "a refused change is no event");

    // A permission does not outlive the listing it was given to: an integration the
    // configuration no longer lists loses what was saved for it.
    let without = Config::from(&BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("TDARR_URL", TDARR),
    ]))
    .unwrap();
    let report = singularr::api::state::prune_unconfigured(&t.pool, &journal, &without)
        .await
        .unwrap();
    assert_eq!(report.access, 1, "{report:?}");
    let again = outbound(recorded.clone());
    let st = state_over(&t.pool, journal.clone(), &services, &again);
    assert_eq!(st.engines.access.load(&t.pool).await.unwrap(), 0);
    assert_eq!(st.engines.access.of("hush").source, "default");
    assert!(!may_stage(&again));
    // No call left for a service in all of this.
    assert!(recorded.seen.lock().unwrap().is_empty());
    t.drop_db().await;
}

/// What a recorded bridge and worker answer: the bridge's health (`health` says whether it
/// offers staging and jobs by id) and its ledger, the worker's state, its nodes and its own
/// records.
fn record_processing(recorded: &Recorded, health: &str) {
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture(health),
    );
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/history.json"),
        200,
        &fixture("hush_history.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{TDARR}/api/v2/status"),
        200,
        &fixture("tdarr_status.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{TDARR}/api/v2/get-nodes"),
        200,
        &fixture("tdarr_nodes_idle.json"),
    );
    let records = format!("{TDARR}/api/v2/cruddb");
    for (collection, file) in [
        ("LibrarySettingsJSONDB", "tdarr_libraries.json"),
        ("FlowsJSONDB", "tdarr_flows.json"),
        ("StatisticsJSONDB", "tdarr_statistics.json"),
    ] {
        recorded.answer_body(Method::Post, &records, collection, 200, &fixture(file));
    }
    recorded.answer_body(Method::Post, &records, "FileJSONDB", 200, "");
    // Its queue, as it answered when nothing waited.
    recorded.answer_body(
        Method::Post,
        &format!("{TDARR}/api/v2/client/status-tables"),
        "table1",
        200,
        &fixture("tdarr_table_queue_empty.json"),
    );
}

/// The calls that would change something in the bridge: every POST that left for it.
fn staged_calls(recorded: &Recorded) -> usize {
    recorded
        .calls_sent()
        .iter()
        .filter(|c| c.method == Method::Post && c.url.starts_with(HUSH))
        .count()
}

/// A runtime of the acts queue alone over the state's outbox, as the owner process runs it. Its
/// waits are short enough for a test.
fn acts_runtime(st: &AppState) -> runtime::Running {
    use singularr::core::outbox;
    let own = PgPoolOptions::new()
        .max_connections(8)
        .connect_lazy_with(st.pool.connect_options().as_ref().clone());
    let acts = Arc::new(st.outbox());
    runtime::Runtime::new(
        own,
        runtime::Timing {
            tick: Duration::from_millis(100),
            poll: Duration::from_millis(200),
            reap: Duration::from_millis(200),
            lease: Duration::from_secs(5),
            grace: Duration::from_secs(1),
        },
    )
    .handle(outbox::SEND_KIND, Arc::new(outbox::SendJob(acts.clone())))
    .handle(outbox::CONFIRM_KIND, Arc::new(outbox::ConfirmJob(acts)))
    .workers(outbox::QUEUE, outbox::WORKERS)
    .start()
}

/// Wait until no act is pending or sending and no job of the acts queue is due or running.
async fn acts_rest(pool: &PgPool) {
    for _ in 0..300 {
        let busy: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM acts WHERE state IN ('pending', 'sending'))
                  + (SELECT count(*) FROM jobs WHERE queue = 'acts'
                     AND (state = 'running' OR (state = 'queued' AND run_at <= now())))",
        )
        .fetch_one(pool)
        .await
        .unwrap();
        if busy == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the acts did not rest within 15 s");
}

/// The acts of the outbox delivered as the acts queue would, at once: each queued job of a send
/// or a read runs in turn, its wait skipped, until none is queued.
async fn deliver_acts(st: &AppState) {
    use singularr::core::outbox;
    let acts = st.outbox();
    loop {
        let next: Option<(String, serde_json::Value)> = sqlx::query_as(
            "UPDATE jobs SET state = 'done', finished_at = now()
             WHERE id = (SELECT id FROM jobs WHERE queue = 'acts' AND state = 'queued'
                         ORDER BY id LIMIT 1)
             RETURNING kind, payload",
        )
        .fetch_optional(&st.pool)
        .await
        .unwrap();
        let Some((kind, payload)) = next else {
            return;
        };
        let act = payload["act"].as_i64().unwrap();
        if kind == outbox::SEND_KIND {
            acts.send(act).await.unwrap();
        } else {
            let send = i32::try_from(payload["send"].as_i64().unwrap()).unwrap();
            let read = u32::try_from(payload["read"].as_i64().unwrap()).unwrap();
            acts.confirm(act, send, read).await.unwrap();
        }
    }
}

/// The owner's send of a file, one click of the web client: a key of its own.
async fn send_click(app: &axum::Router, body: &str) -> (StatusCode, String, serde_json::Value) {
    static CLICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let key = format!(
        "click-{}",
        CLICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    call(
        app,
        "POST",
        "/api/v1/processing/runs",
        &[
            ("x-auth-request-email", "owner@example.com"),
            ("idempotency-key", key.as_str()),
        ],
        Some(body),
    )
    .await
}

/// The bridge's ledger in the table. Its stage takes files and names the job it opened. It gives
/// that job by id. Write access to it. The state over that bridge.
async fn sending_bridge(t: &TestDb, journal: &Journal, recorded: &Arc<Recorded>) -> AppState {
    use singularr::core::outbound::Access;
    ledger_catalog(&t.pool, journal, recorded).await;
    record_processing(recorded, "hush_health_live.json");
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        201,
        &fixture("hush_stage_staged.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids=20261123T160500Z-0a1b2c3d"),
        200,
        &fixture("hush_jobs_by_id.json"),
    );
    let out = outbound(recorded.clone());
    let st = state_over(&t.pool, journal.clone(), &[("HUSH_URL", HUSH)], &out);
    let bridge = st.engines.processor("hush").unwrap().clone();
    processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    st.engines
        .access
        .set(
            &t.pool,
            journal,
            "user:owner@example.com",
            "hush",
            Access::Act,
        )
        .await
        .unwrap();
    st
}

/// An act's key is its request: two posts with one key send once. The key with another request
/// is refused. A double click without a key sends once. `GET /api/v1/acts/{id}` reads the act,
/// for the owner alone.
#[tokio::test]
async fn two_posts_with_one_key_send_once() {
    let Some(t) = TestDb::new("acts_key").await else {
        return;
    };
    use serde_json::json;
    let journal = Journal::new();
    let recorded = Recorded::new();
    let st = sending_bridge(&t, &journal, &recorded).await;
    let acts = acts_runtime(&st);
    let app = routes::router(st.clone());
    let runs = "/api/v1/processing/runs";
    let damsel = json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio"}).to_string();
    let keyed = |key: &'static str| {
        [
            ("x-auth-request-email", "owner@example.com"),
            ("idempotency-key", key),
        ]
    };
    let (status, _, first) = call(&app, "POST", runs, &keyed("k-1"), Some(&damsel)).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let (status, _, second) = call(&app, "POST", runs, &keyed("k-1"), Some(&damsel)).await;
    assert_eq!(status, StatusCode::CREATED, "{second}");
    assert_eq!(second["act"], first["act"]);
    assert_eq!(second["run"]["id"], "hush:20261123T160500Z-0a1b2c3d");
    assert_eq!(staged_calls(&recorded), 1, "one send");
    // The key with another request.
    let strip = json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "strip"}).to_string();
    let (status, _, refused) = call(&app, "POST", runs, &keyed("k-1"), Some(&strip)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(
        (&refused["code"], &refused["act"]),
        (&json!("singularr.idempotency_mismatch"), &first["act"])
    );
    assert_eq!(staged_calls(&recorded), 1);
    // The act as it stands.
    let act = get_ok(&app, &format!("/api/v1/acts/{}", first["act"])).await;
    assert_eq!(
        (
            &act["state"],
            &act["integration"],
            &act["action"],
            &act["actor"],
            &act["attempts"]
        ),
        (
            &json!("confirmed"),
            &json!("hush"),
            &json!("send"),
            &json!("user:owner@example.com"),
            &json!(1)
        )
    );
    assert_eq!(
        (&act["request"]["path"], &act["request"]["flow"]),
        (
            &json!(format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}")),
            &json!("audio")
        )
    );
    assert_eq!(
        act["result"]["confirmed"]["run"],
        "hush:20261123T160500Z-0a1b2c3d"
    );
    // A double click without a key: one act, one send.
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let (status, _, x) = call(&app, "POST", runs, &session, Some(&damsel)).await;
    assert_eq!(status, StatusCode::CREATED, "{x}");
    let (status, _, y) = call(&app, "POST", runs, &session, Some(&damsel)).await;
    assert_eq!(status, StatusCode::CREATED, "{y}");
    assert_ne!(x["act"], first["act"]);
    assert_eq!(x["act"], y["act"]);
    assert_eq!(staged_calls(&recorded), 2);
    // A key that is no key.
    let (status, _, bad) = call(&app, "POST", runs, &keyed("two words"), Some(&damsel)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    // An act is the owner's to read.
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let admin = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "admin".into(),
            scopes: vec![Scope::Admin],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", admin.secret);
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/api/v1/acts/{}", first["act"]),
        &[("authorization", bearer.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(&app, "GET", "/api/v1/acts/999999", &session, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    acts.stop().await;
    t.drop_db().await;
}

/// A send whose answer is lost ends confirmed by its read: the run it opened shows. A send that
/// timed out is unknown: a second send of the file and the flow is that act. The owner marks it
/// not sent: it fails. The file is free for a new send.
#[tokio::test]
async fn an_unconfirmed_send_ends_confirmed_by_its_read_or_failed_by_the_owner() {
    let Some(t) = TestDb::new("acts_unknown").await else {
        return;
    };
    use serde_json::json;
    let journal = Journal::new();
    let recorded = Recorded::new();
    let st = sending_bridge(&t, &journal, &recorded).await;
    let acts = acts_runtime(&st);
    let app = routes::router(st.clone());
    let stage = format!("{HUSH}/stage");
    let damsel_path = format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}");
    let damsel = json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio"}).to_string();
    // The answer is lost: no clear answer.
    recorded.fail_once(Method::Post, &stage, "connection reset by peer");
    let (status, _, lost) = send_click(&app, &damsel).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{lost}");
    assert_eq!(lost["code"], "singularr.unconfirmed");
    let act = lost["act"].as_i64().expect("the problem names its act");
    let read = |id: i64| {
        let app = app.clone();
        async move { get_ok(&app, &format!("/api/v1/acts/{id}")).await }
    };
    assert_eq!(read(act).await["state"], "unknown");
    // The run the send opened shows: the next read confirms it. The send is journaled.
    sqlx::query(
        "INSERT INTO processing_runs
             (id, integration, job_id, flow, trigger_text, state, created_at, source_path, raw)
         VALUES ('hush:j-lost', 'hush', 'j-lost', 'audio', 'api:singularr', 'staged', now(),
                 $1, '{}')",
    )
    .bind(&damsel_path)
    .execute(&t.pool)
    .await
    .unwrap();
    assert_eq!(st.outbox().confirm(act, 1, 2).await.unwrap(), "Confirmed");
    let confirmed = read(act).await;
    assert_eq!(
        (
            &confirmed["state"],
            &confirmed["result"]["confirmed"]["run"]
        ),
        (&json!("confirmed"), &json!("hush:j-lost"))
    );
    let journaled = get_ok(&app, "/api/v1/journal?kind=processing.sent").await;
    assert_eq!(journaled["events"][0]["body"]["run"], "hush:j-lost");
    assert_eq!(staged_calls(&recorded), 1);

    // A timeout: unknown. A second send of the file and the flow is that act: no second call.
    let kingdom = json!({"title": "movie-1495", "file": KINGDOM_FILE, "flow": "hevc"}).to_string();
    recorded.fail(Method::Post, &stage, singularr::core::outbound::TIMED_OUT);
    let (status, _, timed_out) = send_click(&app, &kingdom).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{timed_out}");
    let (status, _, again) = send_click(&app, &kingdom).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{again}");
    assert_eq!(again["act"], timed_out["act"]);
    assert_eq!(staged_calls(&recorded), 2);
    // The owner marks it not sent: it fails. Only an unconfirmed act takes that word.
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let not_sent = format!("/api/v1/acts/{}/not-sent", timed_out["act"]);
    let (status, _, marked) = call(&app, "POST", &not_sent, &session, None).await;
    assert_eq!(status, StatusCode::OK, "{marked}");
    assert_eq!(
        (&marked["state"], &marked["errorClass"], &marked["error"]),
        (&json!("failed"), &json!("by_hand"), &json!("Not sent"))
    );
    let (status, _, twice) = call(&app, "POST", &not_sent, &session, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{twice}");
    // The file is free: a new send goes through.
    recorded.answer_once(
        Method::Post,
        &stage,
        201,
        &fixture("hush_stage_staged.json"),
        &[],
    );
    let (status, _, sent) = send_click(&app, &kingdom).await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_ne!(sent["act"], timed_out["act"]);
    assert_eq!(staged_calls(&recorded), 3);
    acts.stop().await;
    t.drop_db().await;
}

/// An act goes out only while Singularr may still do it: one whose action's access was taken
/// back while it waited fails before anything is sent. A pending act is cancelled before a worker
/// takes it. A rule's claim of a cancelled act frees its file.
#[tokio::test]
async fn an_act_is_refused_when_its_access_changed_or_cancelled_while_pending() {
    let Some(t) = TestDb::new("acts_refused").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::outbound::Access;
    use singularr::processing::rules::{NewLog, RuleBody, RuleConditions};
    use singularr::processing::sends::{self, RuleSend};
    let journal = Journal::new();
    let recorded = Recorded::new();
    let st = sending_bridge(&t, &journal, &recorded).await;
    let outbox = st.outbox();
    let damsel =
        singularr::catalog::store::title_refs(&t.pool, &[title_with_tmdb(&t.pool, 763215).await])
            .await
            .unwrap()
            .remove(0);
    let damsel_path = format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}");
    let mut tx = journal.begin(&t.pool).await.unwrap();
    let act = outbox
        .enqueue(
            &mut tx,
            sends::by_hand(
                "hush",
                &damsel,
                &damsel_path,
                "audio",
                "user:owner@example.com",
                "api:user:owner@example.com:c-1".to_string(),
            ),
        )
        .await
        .unwrap()
        .into_act();
    tx.commit().await.unwrap();
    // The access is taken back before a worker takes the act.
    st.engines
        .access
        .set(
            &t.pool,
            &journal,
            "user:owner@example.com",
            "hush",
            Access::Read,
        )
        .await
        .unwrap();
    deliver_acts(&st).await;
    let refused = singularr::core::outbox::act(&t.pool, act.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            refused.state.as_str(),
            refused.error_class.as_deref(),
            refused.attempts
        ),
        ("failed", Some("access"), 0)
    );
    assert_eq!(staged_calls(&recorded), 0, "nothing left for the bridge");
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    // A rule's claim and its act, pending: the owner cancels it.
    st.engines
        .access
        .set(
            &t.pool,
            &journal,
            "user:owner@example.com",
            "hush",
            Access::Act,
        )
        .await
        .unwrap();
    let mut conn = t.pool.acquire().await.unwrap();
    let rule = processing_store::insert_rule(
        &mut conn,
        &RuleBody {
            name: "Opus audio".to_string(),
            enabled: true,
            when: "request".to_string(),
            conditions: RuleConditions {
                file: serde_json::from_value(json!({"audio_codec": ["Opus"]})).unwrap(),
                ..RuleConditions::default()
            },
            service: "hush".to_string(),
            flow: "audio".to_string(),
        },
        "user:owner@example.com",
    )
    .await
    .unwrap();
    drop(conn);
    let entry = NewLog {
        claimed: false,
        rule_id: rule.id,
        rule_version: rule.version,
        event_id: None,
        title_id: Some(damsel.id),
        copy_id: Some(7),
        path: damsel_path.clone(),
        service: "hush".to_string(),
        flow: "audio".to_string(),
        outcome: "sending",
        detail: None,
        evidence: None,
        run_id: None,
    };
    let mut tx = journal.begin(&t.pool).await.unwrap();
    let claim = processing_store::claim_send(tx.conn(), &entry)
        .await
        .unwrap()
        .unwrap();
    let pending = outbox
        .enqueue(
            &mut tx,
            sends::by_rule(RuleSend {
                rule: rule.id,
                copy: 7,
                claim,
                service: "hush",
                title: &damsel,
                path: &damsel_path,
                flow: "audio",
                event: None,
            }),
        )
        .await
        .unwrap()
        .into_act();
    processing_store::name_act(tx.conn(), claim, pending.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let cancel = format!("/api/v1/acts/{}/cancel", pending.id);
    let (status, _, cancelled) = call(&app, "POST", &cancel, &session, None).await;
    assert_eq!(status, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["state"], "cancelled");
    let (status, _, twice) = call(&app, "POST", &cancel, &session, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{twice}");
    let freed = processing_store::rule_log(&t.pool, rule.id, 1)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (freed.outcome.as_str(), freed.detail.as_deref()),
        ("held", Some("Cancelled"))
    );
    assert!(
        processing_store::sent_paths(&t.pool, std::slice::from_ref(&damsel_path))
            .await
            .unwrap()
            .is_empty(),
        "the file is free"
    );
    deliver_acts(&st).await;
    assert_eq!(staged_calls(&recorded), 0, "a cancelled act is never sent");
    t.drop_db().await;
}

/// The processing rules never send one file twice across a restart: a pass read again after one
/// that was cut short makes no second claim and no second act. A process that stops before the
/// send leaves the act to the next one. That one sends it once. A job of a settled act sends
/// nothing.
/// A send whose lease ran out is unknown, never pending: it is not sent again.
#[tokio::test]
async fn the_rules_never_send_one_file_twice_across_a_restart() {
    let Some(t) = TestDb::new("acts_restart").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::journal::event;
    use singularr::processing::rules::{self, RuleBody, RuleConditions};
    use singularr::processing::sends;
    let journal = Journal::new();
    let recorded = Recorded::new();
    let st = sending_bridge(&t, &journal, &recorded).await;
    let outbox = st.outbox();
    let policy = ActivePolicy::builtin().policy;
    let mut conn = t.pool.acquire().await.unwrap();
    let rule = processing_store::insert_rule(
        &mut conn,
        &RuleBody {
            name: "Atmos audio".to_string(),
            enabled: true,
            when: "import".to_string(),
            conditions: RuleConditions {
                file: serde_json::from_value(json!({"audio_codec": ["Eac3"], "atmos": true}))
                    .unwrap(),
                ..RuleConditions::default()
            },
            service: "hush".to_string(),
            flow: "audio".to_string(),
        },
        "user:owner@example.com",
    )
    .await
    .unwrap();
    drop(conn);
    let pass = || rules::import_pass(&t.pool, &journal, &st.engines, &policy, true, &outbox);
    pass().await.unwrap();
    let before: i64 = sqlx::query_scalar("SELECT max(id) FROM events")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let kingdom: i64 = sqlx::query_scalar("SELECT id FROM copies WHERE relative_path = $1")
        .bind(KINGDOM_FILE)
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let mut tx = journal.begin(&t.pool).await.unwrap();
    tx.append_one(event(
        "copy_added",
        "copy",
        kingdom,
        "sync:radarr",
        json!({}),
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    // The pass claims the file and queues its send. No worker of the acts queue runs yet.
    let first = pass().await.unwrap();
    assert_eq!((first.imports, first.logged, first.sent), (1, 1, 1));
    // The pass is read again, as one cut short before its cursor moved leaves it.
    sqlx::query(
        "UPDATE markers SET value = jsonb_build_object('id', $1::bigint)
         WHERE scope = 'singularr' AND name = 'rules.cursor'",
    )
    .bind(before)
    .execute(&t.pool)
    .await
    .unwrap();
    let again = pass().await.unwrap();
    assert_eq!((again.imports, again.logged, again.sent), (1, 0, 0));
    let acts_count = || {
        let pool = t.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT count(*) FROM acts")
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(acts_count().await, 1);
    assert_eq!(staged_calls(&recorded), 0);
    // The next process sends it, once.
    let acts = acts_runtime(&st);
    acts_rest(&t.pool).await;
    acts.stop().await;
    assert_eq!(staged_calls(&recorded), 1);
    let log = processing_store::rule_log(&t.pool, rule.id, 10)
        .await
        .unwrap();
    assert_eq!(
        log.iter()
            .map(|e| (e.outcome.as_str(), e.run_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![("sent", Some("hush:20261123T160500Z-0a1b2c3d"))]
    );
    // Another start finds a job of the settled act: it sends nothing.
    let settled = log[0].act_id.unwrap();
    sqlx::query(
        "INSERT INTO jobs (kind, queue, payload, idempotency_key)
         VALUES ('act.send', 'acts', jsonb_build_object('act', $1::bigint), 'act:stale')",
    )
    .bind(settled)
    .execute(&t.pool)
    .await
    .unwrap();
    // A send by hand whose run lost its lease while it was sending: its job's lease ran out.
    let damsel =
        singularr::catalog::store::title_refs(&t.pool, &[title_with_tmdb(&t.pool, 763215).await])
            .await
            .unwrap()
            .remove(0);
    let mut tx = journal.begin(&t.pool).await.unwrap();
    let lost = outbox
        .enqueue(
            &mut tx,
            sends::by_hand(
                "hush",
                &damsel,
                &format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}"),
                "audio",
                "user:owner@example.com",
                "api:user:owner@example.com:lost".to_string(),
            ),
        )
        .await
        .unwrap()
        .into_act();
    tx.commit().await.unwrap();
    sqlx::query("UPDATE acts SET state = 'sending', attempts = 1, sent_at = now() WHERE id = $1")
        .bind(lost.id)
        .execute(&t.pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE jobs SET state = 'running', attempts = 1, locked_by = 'gone',
                locked_until = now() - interval '1 second', started_at = now()
         WHERE kind = 'act.send' AND (payload->>'act')::bigint = $1",
    )
    .bind(lost.id)
    .execute(&t.pool)
    .await
    .unwrap();
    let acts = acts_runtime(&st);
    acts_rest(&t.pool).await;
    acts.stop().await;
    let after = singularr::core::outbox::act(&t.pool, lost.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            after.state.as_str(),
            after.error_class.as_deref(),
            after.attempts
        ),
        ("unknown", Some("unrecorded"), 1)
    );
    assert_eq!(staged_calls(&recorded), 1, "neither act was sent again");
    t.drop_db().await;
}

/// A rule's claim of a file and the act of its send, pending: what the rules pass leaves before a
/// worker takes the act. The rule, the claim and the act.
async fn claimed_send(
    t: &TestDb,
    journal: &Journal,
    st: &AppState,
    path: &str,
) -> (i64, i64, singularr::core::outbox::Act) {
    use serde_json::json;
    use singularr::processing::rules::{NewLog, RuleBody, RuleConditions};
    use singularr::processing::sends::{self, RuleSend};
    let damsel =
        singularr::catalog::store::title_refs(&t.pool, &[title_with_tmdb(&t.pool, 763215).await])
            .await
            .unwrap()
            .remove(0);
    let mut conn = t.pool.acquire().await.unwrap();
    let rule = processing_store::insert_rule(
        &mut conn,
        &RuleBody {
            name: "Opus audio".to_string(),
            enabled: true,
            when: "import".to_string(),
            conditions: RuleConditions {
                file: serde_json::from_value(json!({"audio_codec": ["Opus"]})).unwrap(),
                ..RuleConditions::default()
            },
            service: "hush".to_string(),
            flow: "audio".to_string(),
        },
        "user:owner@example.com",
    )
    .await
    .unwrap();
    drop(conn);
    let entry = NewLog {
        claimed: false,
        rule_id: rule.id,
        rule_version: rule.version,
        event_id: None,
        title_id: Some(damsel.id),
        copy_id: Some(7),
        path: path.to_string(),
        service: "hush".to_string(),
        flow: "audio".to_string(),
        outcome: "sending",
        detail: None,
        evidence: None,
        run_id: None,
    };
    let mut tx = journal.begin(&t.pool).await.unwrap();
    let claim = processing_store::claim_send(tx.conn(), &entry)
        .await
        .unwrap()
        .unwrap();
    let act = st
        .outbox()
        .enqueue(
            &mut tx,
            sends::by_rule(RuleSend {
                rule: rule.id,
                copy: 7,
                claim,
                service: "hush",
                title: &damsel,
                path,
                flow: "audio",
                event: None,
            }),
        )
        .await
        .unwrap()
        .into_act();
    processing_store::name_act(tx.conn(), claim, act.id)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (rule.id, claim, act)
}

/// With the sync off no send goes out. Every start delivers the sends all the same: a rule's act
/// left pending when the sync was turned off fails before anything is sent. Its claim is held.
/// The file is free. The job of its send ends well.
#[tokio::test]
async fn a_send_pending_when_the_sync_is_turned_off_fails_and_frees_its_file() {
    let Some(t) = TestDb::new("acts_sync_off").await else {
        return;
    };
    use singularr::processing::sends;
    let journal = Journal::new();
    let recorded = Recorded::new();
    let st = sending_bridge(&t, &journal, &recorded).await;
    let path = format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}");
    let (rule, _, act) = claimed_send(&t, &journal, &st, &path).await;
    // The next start runs without the sync: no processing pass, no rules.
    let off = state_over(
        &t.pool,
        journal.clone(),
        &[("HUSH_URL", HUSH), ("SINGULARR_SYNC_ENABLED", "false")],
        &outbound(recorded.clone()),
    );
    assert!(!off.config.sync.enabled);
    off.engines.access.load(&t.pool).await.unwrap();
    let jobs = start_jobs(&off).await;
    acts_rest(&t.pool).await;
    jobs.stop().await;
    let failed = singularr::core::outbox::act(&t.pool, act.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            failed.state.as_str(),
            failed.error_class.as_deref(),
            failed.error.as_deref(),
            failed.attempts
        ),
        (
            "failed",
            Some(sends::NO_SYNC),
            Some("Integration sync is off"),
            0
        )
    );
    assert_eq!(staged_calls(&recorded), 0, "nothing went out");
    let log = processing_store::rule_log(&t.pool, rule, 1)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(
        (log.outcome.as_str(), log.detail.as_deref(), log.attention),
        ("held", Some("Integration sync is off"), false)
    );
    assert!(
        processing_store::sent_paths(&t.pool, std::slice::from_ref(&path))
            .await
            .unwrap()
            .is_empty(),
        "the file is free"
    );
    let job: String = sqlx::query_scalar(
        "SELECT state FROM jobs WHERE kind = 'act.send' AND (payload->>'act')::bigint = $1",
    )
    .bind(act.id)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(job, "done");
    t.drop_db().await;
}

/// A read confirms an unconfirmed send only by a run of its own file, flow and trigger. A run of
/// the file in another flow leaves it unconfirmed. So does one the service started on its own.
#[tokio::test]
async fn a_read_that_sees_a_run_of_another_flow_or_trigger_leaves_the_send_unconfirmed() {
    let Some(t) = TestDb::new("acts_other_run").await else {
        return;
    };
    use serde_json::json;
    use singularr::processing::sends;
    let journal = Journal::new();
    let recorded = Recorded::new();
    let st = sending_bridge(&t, &journal, &recorded).await;
    let acts = acts_runtime(&st);
    let app = routes::router(st.clone());
    let path = format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}");
    recorded.fail_once(
        Method::Post,
        &format!("{HUSH}/stage"),
        "connection reset by peer",
    );
    let damsel = json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio"}).to_string();
    let (status, _, lost) = send_click(&app, &damsel).await;
    acts.stop().await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{lost}");
    let act = lost["act"].as_i64().unwrap();
    let run = |id: &str, flow: &str, trigger: &str| {
        sqlx::query(
            "INSERT INTO processing_runs
                 (id, integration, job_id, flow, trigger_text, state, created_at, source_path, raw)
             VALUES ($1, 'hush', $1, $2, $3, 'staged', now(), $4, '{}')",
        )
        .bind(id.to_string())
        .bind(flow.to_string())
        .bind(trigger.to_string())
        .bind(path.clone())
        .execute(&t.pool)
    };
    run("hush:j-hevc", "hevc", "api:singularr").await.unwrap();
    run("hush:j-tag", "audio", "tag:hush-audio").await.unwrap();
    let outbox = st.outbox();
    assert_eq!(
        outbox.confirm(act, 1, 2).await.unwrap(),
        "Not seen yet: read 2 of 6"
    );
    assert_eq!(
        sends::confirm_by_runs(&outbox, &t.pool, "hush")
            .await
            .unwrap(),
        0
    );
    let state = |id: i64| {
        let pool = t.pool.clone();
        async move {
            singularr::core::outbox::act(&pool, id)
                .await
                .unwrap()
                .unwrap()
        }
    };
    assert_eq!(state(act).await.state, "unknown");
    // The run of its own flow and trigger confirms it.
    run("hush:j-own", "audio", "api:singularr").await.unwrap();
    assert_eq!(
        sends::confirm_by_runs(&outbox, &t.pool, "hush")
            .await
            .unwrap(),
        1
    );
    let confirmed = state(act).await;
    assert_eq!(
        (confirmed.state.as_str(), sends::confirmed_run(&confirmed)),
        ("confirmed", Some("hush:j-own"))
    );
    t.drop_db().await;
}

/// A send by hand of a file whose rule's act is sent is an act of its own: a sent act holds the
/// file and the flow no more. The service refuses the second copy itself. The rule's act and its
/// claim stay as they were until a read confirms the act.
#[tokio::test]
async fn a_send_by_hand_while_the_rule_s_act_is_sent_is_an_act_of_its_own() {
    let Some(t) = TestDb::new("acts_hand_sent").await else {
        return;
    };
    use serde_json::json;
    let journal = Journal::new();
    let recorded = Recorded::new();
    let st = sending_bridge(&t, &journal, &recorded).await;
    let path = format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}");
    let (rule, _, ruled) = claimed_send(&t, &journal, &st, &path).await;
    // The rule's act goes out: the bridge takes the file. Its read waits.
    assert_eq!(st.outbox().send(ruled.id).await.unwrap(), "Sent");
    sqlx::query("UPDATE jobs SET run_at = now() + interval '1 hour' WHERE kind = 'act.confirm'")
        .execute(&t.pool)
        .await
        .unwrap();
    // The owner sends the file to the flow by hand: the bridge has it in a run.
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        409,
        &json!({"staged": false, "reason": "already_staged", "job": "20261123T160500Z-0a1b2c3d"})
            .to_string(),
    );
    let acts = acts_runtime(&st);
    let app = routes::router(st.clone());
    let damsel = json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio"}).to_string();
    let (status, _, refused) = send_click(&app, &damsel).await;
    acts.stop().await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["code"], "singularr.already_staged");
    assert_ne!(refused["act"], json!(ruled.id), "an act of its own");
    assert_eq!(staged_calls(&recorded), 2);
    let act = |id: i64| {
        let pool = t.pool.clone();
        async move {
            singularr::core::outbox::act(&pool, id)
                .await
                .unwrap()
                .unwrap()
        }
    };
    assert_eq!(act(ruled.id).await.state, "sent");
    let log = || {
        let pool = t.pool.clone();
        async move {
            processing_store::rule_log(&pool, rule, 1)
                .await
                .unwrap()
                .remove(0)
        }
    };
    let claim = log().await;
    assert_eq!(
        (claim.outcome.as_str(), claim.attention),
        ("sending", false)
    );
    // Its read confirms it: the claim is sent.
    deliver_acts(&st).await;
    assert_eq!(act(ruled.id).await.state, "confirmed");
    let claim = log().await;
    assert_eq!(
        (claim.outcome.as_str(), claim.run_id.as_deref()),
        ("sent", Some("hush:20261123T160500Z-0a1b2c3d"))
    );
    t.drop_db().await;
}

/// Migration 0029 turns each claim of 0028 in `sending` into an unconfirmed act that holds the
/// open key of its file and flow. The claim names the act. A settled row names none. The claim's
/// job and 0028's index of the claims by service are gone.
#[tokio::test]
async fn the_outbox_migration_turns_a_sending_claim_into_an_unconfirmed_act() {
    let Some(t) = TestDb::bare("m0029").await else {
        return;
    };
    use serde_json::json;
    db::MIGRATOR
        .run_to(28, &t.pool)
        .await
        .expect("the migrations before 0029 apply");
    let pending = db::pending_migrations(&t.pool).await.unwrap();
    assert!(pending[0].starts_with("29 "), "{pending:?}");
    let title: i64 = sqlx::query_scalar(
        "INSERT INTO titles (ext_key, kind, tmdb_id, title, sort_title, year)
         VALUES ('tmdb:movie:763215', 'movie', 763215, 'Damsel', 'damsel', 2024) RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let rule: i64 = sqlx::query_scalar(
        "INSERT INTO processing_rules (name, enabled, run_on, service, flow, created_by, updated_by)
         VALUES ('Opus audio', true, 'import', 'hush', 'audio', 'user:o', 'user:o') RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let row = |path: &'static str, outcome: &'static str| {
        sqlx::query_scalar::<_, i64>(
            "INSERT INTO processing_rule_log
                 (rule_id, rule_version, at, event_id, title_id, copy_id, path, service, flow,
                  outcome, detail, job_id)
             VALUES ($1, 1, '2026-11-24T10:00:00Z', 41, $2, 7, $3, 'hush', 'audio', $4,
                     CASE WHEN $4 = 'sending' THEN 'No clear answer' END, 12)
             RETURNING id",
        )
        .bind(rule)
        .bind(title)
        .bind(path)
        .bind(outcome)
        .fetch_one(&t.pool)
    };
    let claim = row("/movies/Damsel (2024)/Damsel.mkv", "sending")
        .await
        .unwrap();
    let sent = row("/movies/Damsel (2024)/Damsel.2160p.mkv", "sent")
        .await
        .unwrap();

    db::migrate(&t.pool).await.expect("0029 applies");
    assert!(db::pending_migrations(&t.pool).await.unwrap().is_empty());
    let named = |id: i64| {
        let pool = t.pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<i64>>(
                "SELECT act_id FROM processing_rule_log WHERE id = $1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    let act_id = named(claim).await.expect("the claim names its act");
    assert_eq!(named(sent).await, None);
    let act = singularr::core::outbox::act(&t.pool, act_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            act.integration.as_str(),
            act.action.as_str(),
            act.state.as_str(),
            act.attempts,
            act.error.as_deref(),
            act.error_class.as_deref(),
        ),
        (
            "hush",
            "send",
            "unknown",
            1,
            Some("No clear answer"),
            Some("unknown")
        )
    );
    assert_eq!(act.idempotency_key, format!("rule:{rule}:7:{claim}"));
    assert_eq!(
        act.open_key.as_deref(),
        Some("send:hush:/movies/Damsel (2024)/Damsel.mkv:audio")
    );
    assert_eq!(
        (act.actor, act.caused_by),
        (format!("rule:{rule}"), Some(41))
    );
    assert_eq!(
        (act.subject_type.as_deref(), act.subject_id),
        (Some("title"), Some(title.to_string()))
    );
    assert_eq!(
        act.request,
        json!({
            "path": "/movies/Damsel (2024)/Damsel.mkv",
            "flow": "audio",
            "title": {"id": title, "kind": "movie", "tmdb_id": 763215, "title": "Damsel",
                      "year": 2024}
        })
    );
    assert_eq!(act.sent_at, Some(act.created_at));
    // Its hash is the request's text alone, as the column says of a converted act.
    let own: bool = sqlx::query_scalar(
        "SELECT request_hash = encode(sha256(convert_to(request::text, 'UTF8')), 'hex')
         FROM acts WHERE id = $1",
    )
    .bind(act_id)
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert!(own);
    // The act holds the file and the flow: a second act of them is refused by the index.
    let second = sqlx::query(
        "INSERT INTO acts (integration, action, idempotency_key, open_key, request_hash, request,
                           actor)
         VALUES ('hush', 'send', 'api:x:k', $1, 'h', '{}', 'user:o')",
    )
    .bind(act.open_key.as_deref())
    .execute(&t.pool)
    .await;
    assert!(second.is_err(), "the open key holds");
    let gone: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM information_schema.columns
                 WHERE table_name = 'processing_rule_log' AND column_name = 'job_id'),
                (SELECT count(*) FROM pg_indexes WHERE indexname = 'processing_rule_log_sending')",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(gone, (0, 0));
    t.drop_db().await;
}

/// Sending a file to a flow is the one thing Singularr does to a service: only for the owner,
/// only while the integration's access is `act`, never for a title its owner protects. The path
/// is worked out from the title's own files.
#[tokio::test]
async fn a_file_is_sent_to_a_flow_when_the_owner_allows_it() {
    let Some(t) = TestDb::new("send").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::outbound::encode_component;
    let journal = Journal::new();
    let recorded = Recorded::new();
    ledger_catalog(&t.pool, &journal, &recorded).await;
    let damsel = title_with_tmdb(&t.pool, 763215).await;
    let kingdom = title_with_tmdb(&t.pool, 1495).await;
    record_processing(&recorded, "hush_health_live.json");
    // What the bridge says it would do with a file: its real answer for the first movie.
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/stage?path="),
        200,
        &fixture("hush_stage_movie_live.json"),
    );
    let out = outbound(recorded.clone());
    let services = [("HUSH_URL", HUSH), ("TDARR_URL", TDARR)];
    let st = state_over(&t.pool, journal.clone(), &services, &out);
    let app = routes::router(st.clone());
    let acts = acts_runtime(&st);
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let bridge = st.engines.processor("hush").unwrap().clone();
    let worker = st.engines.processor("tdarr").unwrap().clone();
    // One whole pass of each: the ledger is in the table. The worker's libraries and flows are
    // in memory.
    let read = processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    assert_eq!(read.detail, "12 runs");
    processing::pass(&t.pool, &worker, processing::Read::Whole)
        .await
        .unwrap();

    let damsel_path = format!("{DAMSEL_FOLDER}/{DAMSEL_FILE}");
    let kingdom_path = format!("{KINGDOM_FOLDER}/{KINGDOM_FILE}");
    let options_of = |title: &str, file: &str| {
        format!(
            "/api/v1/processing/options?title={title}&file={}",
            encode_component(file)
        )
    };
    let option = |answer: &serde_json::Value, flow: &str| {
        answer["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["flow"] == flow)
            .cloned()
            .unwrap_or_else(|| panic!("{flow} is among the options"))
    };
    let read_only = "Send a file to a flow is read-only on HUSH.";

    // The options, while Singularr may only read the bridge. The path is the server's: the
    // title's folder as its manager names it, then the file's name.
    let options = get_ok(&app, &options_of("movie-763215", DAMSEL_FILE)).await;
    assert_eq!(options["title"], "movie-763215");
    assert_eq!(
        options["file"],
        json!({"name": DAMSEL_FILE, "path": damsel_path, "size": 4_618_100_950_i64})
    );
    assert_eq!(
        options["service"],
        json!({"id": "hush", "name": "HUSH", "access": "read", "offers": true})
    );
    assert_eq!(
        (&options["canSend"], &options["why"]),
        (&json!(false), &json!("Read-only"))
    );
    // The bridge was asked with a GET for that very path.
    let asked = recorded.calls_sent();
    let asked = asked.last().unwrap();
    assert_eq!(
        (asked.method, asked.url.as_str()),
        (
            Method::Get,
            format!("{HUSH}/stage?path={}", encode_component(&damsel_path)).as_str()
        )
    );
    // The flows that fit the file and have runs first (most runs first), then the ones that
    // fit, then the ones that do not: this file is a 1080p HEVC file.
    let order: Vec<&str> = options["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["flow"].as_str().unwrap())
        .collect();
    assert_eq!(
        order,
        vec![
            "audio",
            "encode",
            "av1",
            "shrink",
            "dual-encode",
            "dual-audio",
            "strip",
            "hevc",
            "downscale"
        ]
    );
    let hevc = option(&options, "hevc");
    assert_eq!(
        (&hevc["fits"], &hevc["unfit"], &hevc["label"]),
        (
            &json!(false),
            &json!("Already HEVC"),
            &json!("Convert to HEVC")
        )
    );
    // What the bridge says it would do, whatever Singularr thinks of the fit.
    assert_eq!(
        (
            &hevc["mode"],
            &hevc["wouldSend"],
            &hevc["reason"],
            &hevc["openRun"],
            &hevc["queue"]
        ),
        (
            &json!("hevc"),
            &json!(true),
            &json!(null),
            &json!(null),
            &json!("/media/hush/queue/movies/hevc_1080p")
        )
    );
    // The worker library whose folder holds that queue, and the flow it runs, with its steps.
    assert_eq!(
        (&hevc["worker"]["service"], &hevc["worker"]["library"]),
        (
            &json!("tdarr"),
            &json!({"id": "HushMovHEVC_1080", "name": "Movies - HUSH HEVC 1080p"})
        )
    );
    let flow = &hevc["worker"]["flow"];
    assert_eq!(
        (&flow["id"], &flow["name"]),
        (
            &json!("hush_movie_hevc_1080_v1"),
            &json!("HUSH Movie HEVC 1080p (SHH)")
        )
    );
    assert_eq!(flow["steps"].as_array().map(Vec::len), Some(6));
    assert!(
        flow["about"]
            .as_str()
            .is_some_and(|a| a.starts_with("Codec conversion")),
        "{flow}"
    );
    // Its runs so far, in numbers.
    assert_eq!(
        (&hevc["runs"], &hevc["lastRun"]),
        (&json!(8), &json!("2026-11-23T14:55:00Z"))
    );
    assert!(hevc["savedPercent"].is_number() && hevc["medianSeconds"].is_number());
    let downscale = option(&options, "downscale");
    assert_eq!(
        (&downscale["fits"], &downscale["unfit"]),
        (&json!(false), &json!("Needs a 4K file"))
    );
    assert_eq!(
        downscale["worker"]["library"]["name"],
        "Movies - HUSH 4K Downscale"
    );
    let audio = option(&options, "audio");
    assert_eq!(
        (&audio["fits"], &audio["unfit"], &audio["runs"]),
        (&json!(true), &json!(null), &json!(2))
    );
    assert_eq!(
        audio["worker"]["library"]["name"],
        "Movies - HUSH 1080p Audio"
    );
    // A queue no worker library holds, and a flow without a queue folder.
    let av1 = option(&options, "av1");
    assert_eq!(
        (&av1["fits"], &av1["worker"], &av1["runs"], &av1["lastRun"]),
        (&json!(true), &json!(null), &json!(0), &json!(null))
    );
    assert_eq!(av1["queue"], "/media/hush/queue/movies/av1_1080p");
    let strip = option(&options, "strip");
    assert_eq!(
        (&strip["wouldSend"], &strip["queue"], &strip["worker"]),
        (&json!(true), &json!(null), &json!(null))
    );

    // What cannot be asked about: no title, no such title, no such file of it, and a file
    // is never named by a path.
    for (path, expected) in [
        (
            "/api/v1/processing/options".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/processing/options?title=movie-763215".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (options_of("damsel", DAMSEL_FILE), StatusCode::BAD_REQUEST),
        (options_of("movie-1", DAMSEL_FILE), StatusCode::NOT_FOUND),
        (
            options_of("movie-763215", "another.mkv"),
            StatusCode::NOT_FOUND,
        ),
        (
            options_of("movie-763215", &damsel_path),
            StatusCode::NOT_FOUND,
        ),
        (
            format!("{}&service=tdarr2", options_of("movie-763215", DAMSEL_FILE)),
            StatusCode::NOT_FOUND,
        ),
        // A worker offers no flow a person chooses: no file is sent through it.
        (
            format!("{}&service=tdarr", options_of("movie-763215", DAMSEL_FILE)),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, content_type, problem) = call(&app, "GET", &path, &session, None).await;
        assert_eq!(status, expected, "{path}: {problem}");
        assert!(
            content_type.starts_with("application/problem+json"),
            "{path}"
        );
    }

    // Sending, while the access is `read`: refused with the same sentence, and nothing left
    // for the bridge, not even a question.
    let send_damsel =
        json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio"}).to_string();
    let runs = "/api/v1/processing/runs";
    let before = recorded.calls_sent().len();
    let (status, content_type, refused) =
        call(&app, "POST", runs, &session, Some(&send_damsel)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{refused}");
    assert!(content_type.starts_with("application/problem+json"));
    assert_eq!(
        (&refused["code"], &refused["detail"]),
        (&json!("singularr.not_allowed"), &json!(read_only))
    );
    assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");
    assert_eq!(staged_calls(&recorded), 0);
    // Only the owner sends: a key does not, whatever its scopes, and nobody does unsigned.
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let admin = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "admin".into(),
            scopes: vec![Scope::Admin],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", admin.secret);
    let (status, _, by_key) = call(
        &app,
        "POST",
        runs,
        &[("authorization", bearer.as_str())],
        Some(&send_damsel),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{by_key}");
    assert_eq!(by_key["code"], "singularr.forbidden");
    let (status, _, _) = call(&app, "POST", runs, &[], Some(&send_damsel)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // The owner allows acting on the bridge.
    let (status, _, allowed) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(r#"{"access": "act"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{allowed}");
    let options = get_ok(&app, &options_of("movie-763215", DAMSEL_FILE)).await;
    assert_eq!(
        (
            &options["canSend"],
            &options["why"],
            &options["service"]["access"]
        ),
        (&json!(true), &json!(null), &json!("act"))
    );

    // What cannot be sent: a title or a file that is not there, a flow the service does not
    // have, a body with a path in it.
    for (body, expected) in [
        (
            json!({"title": "movie-1", "file": DAMSEL_FILE, "flow": "audio"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"title": "movie-763215", "file": "another.mkv", "flow": "audio"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "uhd-audio"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio", "path": damsel_path}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio", "service": "tdarr2"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "audio", "service": "tdarr"}),
            StatusCode::NOT_FOUND,
        ),
        (
            json!({"title": "movie-763215", "flow": "audio"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (status, _, problem) =
            call(&app, "POST", runs, &session, Some(&body.to_string())).await;
        assert_eq!(status, expected, "{body}: {problem}");
    }
    assert_eq!(staged_calls(&recorded), 0, "none of these left");

    // A good send: the bridge takes the file and names its job; the job is asked for by id
    // and the run is stored before the answer.
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        201,
        &fixture("hush_stage_staged.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids=20261123T160500Z-0a1b2c3d"),
        200,
        &fixture("hush_jobs_by_id.json"),
    );
    let (status, _, sent) = send_click(&app, &send_damsel).await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_eq!(
        (
            &sent["sent"],
            &sent["service"],
            &sent["flow"],
            &sent["mode"]
        ),
        (
            &json!(true),
            &json!("hush"),
            &json!("audio"),
            &json!("audio")
        )
    );
    let run = &sent["run"];
    assert_eq!(
        (&run["id"], &run["state"], &run["flow"], &run["flowLabel"]),
        (
            &json!("hush:20261123T160500Z-0a1b2c3d"),
            &json!("staged"),
            &json!("audio"),
            &json!("Rebuild audio")
        )
    );
    assert_eq!(
        run["trigger"],
        json!({"kind": "api", "label": "Singularr", "raw": "api:marquee"})
    );
    assert_eq!(
        (&run["title"]["id"], &run["source"]["path"]),
        (&json!("movie-763215"), &json!(damsel_path))
    );
    assert_eq!(
        run["inWorker"]["library"]["name"],
        "Movies - HUSH 1080p Audio"
    );
    // What left for the bridge: one POST with the path the server worked out, the flow and
    // who asks. No key is configured, so none is sent.
    assert_eq!(staged_calls(&recorded), 1);
    let calls = recorded.calls_sent();
    let stage = calls
        .iter()
        .rfind(|c| c.method == Method::Post && c.url.starts_with(HUSH))
        .unwrap();
    assert_eq!(stage.url, format!("{HUSH}/stage"));
    let Sent::Json(text) = &stage.body else {
        panic!("{:?}", stage.body)
    };
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(text).unwrap(),
        json!({"path": damsel_path, "flow": "audio", "requested_by": "singularr"})
    );
    assert!(
        !stage
            .headers
            .iter()
            .any(|(n, _)| n.eq_ignore_ascii_case("authorization"))
    );
    // The run is in the table, linked to its title, and on the pages at once.
    let stored = processing_store::one(&t.pool, "hush:20261123T160500Z-0a1b2c3d")
        .await
        .unwrap()
        .expect("the run was stored before the answer");
    assert_eq!(
        (
            stored.title_id,
            stored.state.as_str(),
            stored.trigger.as_str()
        ),
        (Some(damsel), "staged", "api:marquee")
    );
    let page = get_ok(&app, "/api/v1/processing").await;
    assert!(
        page["open"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == "hush:20261123T160500Z-0a1b2c3d"),
        "the run is among the open ones"
    );
    // The journal has it: about the title, by the person, with the file, the flow, the mode,
    // the service and the job.
    let journaled = get_ok(&app, "/api/v1/journal?kind=processing.sent").await;
    let events = journaled["events"].as_array().unwrap();
    assert_eq!(events.len(), 1, "{journaled}");
    assert_eq!(
        (
            &events[0]["kind"],
            &events[0]["code"],
            &events[0]["summary"]
        ),
        (
            &json!("processing.sent"),
            &json!("processing_sent"),
            &json!(format!("Sent to HUSH for Rebuild audio: {DAMSEL_FILE}"))
        )
    );
    assert_eq!(
        events[0]["actor"],
        json!({"kind": "user", "id": "owner@example.com", "name": "owner@example.com"})
    );
    assert_eq!(
        events[0]["subject"],
        json!({"type": "title", "id": "movie-763215", "label": "Damsel"})
    );
    let body = &events[0]["body"];
    assert_eq!(
        (
            &body["file"],
            &body["flow"],
            &body["mode"],
            &body["service"],
            &body["job"],
            &body["run"]
        ),
        (
            &json!(DAMSEL_FILE),
            &json!("audio"),
            &json!("audio"),
            &json!("hush"),
            &json!("20261123T160500Z-0a1b2c3d"),
            &json!("hush:20261123T160500Z-0a1b2c3d")
        )
    );

    // A bridge that takes the file and names no job (a flow without a queue): still sent,
    // with no run to show yet.
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        202,
        r#"{"staged": true, "flow": "strip", "mode": "strip"}"#,
    );
    let send_strip =
        json!({"title": "movie-763215", "file": DAMSEL_FILE, "flow": "strip"}).to_string();
    let (status, _, sent) = send_click(&app, &send_strip).await;
    assert_eq!(status, StatusCode::CREATED, "{sent}");
    assert_eq!(
        (&sent["sent"], &sent["mode"], &sent["run"]),
        (&json!(true), &json!("strip"), &json!(null))
    );

    // The file is with the bridge already: the answer names the open run when the table
    // holds one for the job the bridge names, and says there is none to open when that job
    // is over (its file still lies in a queue).
    let already =
        |job: &str| json!({"staged": false, "reason": "already_staged", "job": job}).to_string();
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        409,
        &already("20261123T160500Z-0a1b2c3d"),
    );
    let (status, _, taken) = send_click(&app, &send_damsel).await;
    assert_eq!(status, StatusCode::CONFLICT, "{taken}");
    assert_eq!(
        (&taken["code"], &taken["detail"]),
        (
            &json!("singularr.already_staged"),
            &json!("HUSH already has this file in run hush:20261123T160500Z-0a1b2c3d.")
        )
    );
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        409,
        &already("20261111T150853Z-4cec4841"),
    );
    let (status, _, taken) = send_click(&app, &send_damsel).await;
    assert_eq!(status, StatusCode::CONFLICT, "{taken}");
    assert_eq!(taken["detail"], "HUSH already has this file in a queue.");
    // The options say the same, flow by flow: a file that lies in a queue is taken for no
    // flow that has a queue, each names the job, and the run is given where it is open.
    let kingdom_stage = format!("{HUSH}/stage?path={}", encode_component(&kingdom_path));
    let in_queue = |job: &str| {
        let taken = |flow: &str| {
            json!({"flow": flow, "mode": flow, "would_stage": false,
                   "queue": format!("/media/hush/queue/movies/{flow}_1080p"),
                   "queue_path": null, "reason": "already_staged", "job": job})
        };
        json!({"path": kingdom_path, "exists": true, "flows": [
            taken("audio"), taken("hevc"),
            {"flow": "strip", "mode": "strip", "would_stage": true, "queue": null,
             "queue_path": null, "reason": null, "job": null}
        ]})
        .to_string()
    };
    recorded.answer(
        Method::Get,
        &kingdom_stage,
        200,
        &in_queue("20261115T001015Z-cb34e7ec"),
    );
    let options = get_ok(&app, &options_of("movie-1495", KINGDOM_FILE)).await;
    assert_eq!(options["file"]["path"], json!(kingdom_path));
    assert_eq!(options["canSend"], true);
    for flow in ["audio", "hevc"] {
        let taken = option(&options, flow);
        assert_eq!(
            (&taken["wouldSend"], &taken["reason"], &taken["openRun"]),
            (
                &json!(false),
                &json!("already_staged"),
                &json!("hush:20261115T001015Z-cb34e7ec")
            ),
            "{flow}"
        );
    }
    // The queue folder alone places the file with its worker library.
    assert_eq!(
        option(&options, "audio")["worker"]["library"]["name"],
        "Movies - HUSH 1080p Audio"
    );
    let strip = option(&options, "strip");
    assert_eq!(
        (&strip["wouldSend"], &strip["openRun"]),
        (&json!(true), &json!(null))
    );
    // A flow the bridge said nothing about has no word of its.
    let av1 = option(&options, "av1");
    assert_eq!(
        (&av1["wouldSend"], &av1["mode"], &av1["reason"]),
        (&json!(null), &json!(null), &json!(null))
    );
    recorded.answer(
        Method::Get,
        &kingdom_stage,
        200,
        &in_queue("20261111T150853Z-4cec4841"),
    );
    let options = get_ok(&app, &options_of("movie-1495", KINGDOM_FILE)).await;
    let taken = option(&options, "audio");
    assert_eq!(
        (&taken["wouldSend"], &taken["reason"], &taken["openRun"]),
        (&json!(false), &json!("already_staged"), &json!(null)),
        "a job that is over is no run to open"
    );

    // The bridge refuses: its own words. It fails: a problem of the service. It gives no clear
    // answer: said so. The call is not made again.
    let send_kingdom =
        json!({"title": "movie-1495", "file": KINGDOM_FILE, "flow": "hevc"}).to_string();
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        422,
        r#"{"staged": false, "reason": "refused", "detail": "hushq: no such queue"}"#,
    );
    let (status, _, refused) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(
        (&refused["code"], &refused["detail"]),
        (
            &json!("singularr.refused"),
            &json!("HUSH refused the file: hushq: no such queue")
        )
    );
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        422,
        r#"{"staged": false, "reason": "outside_roots"}"#,
    );
    let (status, _, refused) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(refused["detail"], "HUSH refused the file: outside_roots");
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        404,
        r#"{"staged": false, "reason": "not_found"}"#,
    );
    let (status, _, gone) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{gone}");
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        500,
        r#"{"staged": false, "reason": "error", "detail": "ledger locked"}"#,
    );
    let (status, _, failed) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{failed}");
    assert_eq!(failed["code"], "singularr.upstream");
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        401,
        r#"{"staged": false, "reason": "unauthorized"}"#,
    );
    let (status, _, unkeyed) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{unkeyed}");
    let unclear = "HUSH gave no clear answer and may have taken the file.";
    let before = staged_calls(&recorded);
    recorded.fail(
        Method::Post,
        &format!("{HUSH}/stage"),
        singularr::core::outbound::TIMED_OUT,
    );
    let (status, _, timed_out) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{timed_out}");
    assert_eq!(
        (
            &timed_out["code"],
            &timed_out["title"],
            &timed_out["detail"]
        ),
        (
            &json!("singularr.unconfirmed"),
            &json!("Unconfirmed"),
            &json!(unclear)
        )
    );
    assert_eq!(staged_calls(&recorded), before + 1, "asked once, not again");
    // A send of the file and the flow while that act is unconfirmed is that act: no second call.
    let (status, _, joined) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{joined}");
    assert_eq!(
        (&joined["detail"], &joined["act"]),
        (&json!(unclear), &timed_out["act"])
    );
    assert_eq!(staged_calls(&recorded), before + 1);
    // The owner marks it not sent: the file and the flow are free again.
    let not_sent = |act: &serde_json::Value| format!("/api/v1/acts/{act}/not-sent");
    let (status, _, _) = call(&app, "POST", &not_sent(&timed_out["act"]), &session, None).await;
    assert_eq!(status, StatusCode::OK);
    // The bridge's own word that it gave up waiting reads the same.
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        504,
        r#"{"staged": false, "reason": "timeout"}"#,
    );
    let (status, _, gave_up) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(gave_up["detail"], unclear);
    let (status, _, _) = call(&app, "POST", &not_sent(&gave_up["act"]), &session, None).await;
    assert_eq!(status, StatusCode::OK);
    // A connection that broke after the call went out reads the same. So does a 502 without the
    // bridge's own words. Each is asked once.
    let before = staged_calls(&recorded);
    recorded.fail(
        Method::Post,
        &format!("{HUSH}/stage"),
        "connection reset by peer",
    );
    let (status, _, broke) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{broke}");
    assert_eq!(broke["code"], "singularr.unconfirmed");
    let (status, _, _) = call(&app, "POST", &not_sent(&broke["act"]), &session, None).await;
    assert_eq!(status, StatusCode::OK);
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        502,
        "<html>Bad Gateway</html>",
    );
    let (status, _, gateway) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT, "{gateway}");
    assert_eq!(gateway["detail"], unclear);
    let (status, _, _) = call(&app, "POST", &not_sent(&gateway["act"]), &session, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(staged_calls(&recorded), before + 2);
    // None of these is a send: the journal has the one a read confirmed. The send whose bridge
    // named no job waits for its run.
    let journaled = get_ok(&app, "/api/v1/journal?kind=processing.sent").await;
    assert_eq!(journaled["events"].as_array().unwrap().len(), 1);

    // A title its owner protects in its manager is never sent, whatever the bridge would do
    // with the file: the tag is the bridge adapter's to name.
    let tag = bridge
        .protecting_tag()
        .expect("the bridge has a protecting tag");
    sqlx::query("UPDATE title_engines SET tags = $2 WHERE title_id = $1")
        .bind(kingdom)
        .bind(vec![tag.to_string()])
        .execute(&t.pool)
        .await
        .unwrap();
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        201,
        &fixture("hush_stage_staged.json"),
    );
    recorded.answer(
        Method::Get,
        &kingdom_stage,
        200,
        &fixture("hush_stage_movie_live.json"),
    );
    let protected = "Protected by tag hush-keep";
    let options = get_ok(&app, &options_of("movie-1495", KINGDOM_FILE)).await;
    assert_eq!(
        (&options["canSend"], &options["why"]),
        (&json!(false), &json!(protected))
    );
    // Every option keeps its other facts.
    let hevc = option(&options, "hevc");
    assert_eq!(
        (&hevc["wouldSend"], &hevc["mode"], &hevc["runs"]),
        (&json!(true), &json!("hevc"), &json!(8))
    );
    assert_eq!(
        hevc["worker"]["library"]["name"],
        "Movies - HUSH HEVC 1080p"
    );
    let before = recorded.calls_sent().len();
    let (status, _, kept) = send_click(&app, &send_kingdom).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{kept}");
    assert_eq!(
        (&kept["code"], &kept["detail"]),
        (
            &json!("singularr.not_allowed"),
            &json!(format!("{protected}."))
        )
    );
    assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");
    // The other title is not protected by it.
    let options = get_ok(&app, &options_of("movie-763215", DAMSEL_FILE)).await;
    assert_eq!(options["canSend"], true);

    // A bridge without the routes tells it in its health, by saying nothing. It is asked for
    // nothing new. The options hold what Singularr knows itself. A send is refused here. Its acts
    // are delivered by a process over that bridge.
    acts.stop().await;
    let old = Recorded::new();
    record_processing(&old, "hush_health.json");
    let old_out = outbound(old.clone());
    let old_st = state_over(&t.pool, journal.clone(), &services, &old_out);
    assert_eq!(old_st.engines.access.load(&t.pool).await.unwrap(), 1);
    let old_app = routes::router(old_st.clone());
    let old_acts = acts_runtime(&old_st);
    let options = get_ok(&old_app, &options_of("movie-763215", DAMSEL_FILE)).await;
    assert_eq!(
        options["service"],
        json!({"id": "hush", "name": "HUSH", "access": "act", "offers": false})
    );
    assert_eq!(
        (&options["canSend"], &options["why"]),
        (&json!(false), &json!("Sending not supported by HUSH"))
    );
    let hevc = option(&options, "hevc");
    assert_eq!(
        (
            &hevc["fits"],
            &hevc["wouldSend"],
            &hevc["mode"],
            &hevc["queue"],
            &hevc["worker"],
            &hevc["runs"]
        ),
        (
            &json!(false),
            &json!(null),
            &json!(null),
            &json!(null),
            &json!(null),
            &json!(8)
        )
    );
    // Without the bridge's word, the open run that was staged from the file is the table's.
    assert_eq!(hevc["openRun"], "hush:20261123T160500Z-0a1b2c3d");
    let (status, _, not_offered) = send_click(&old_app, &send_damsel).await;
    assert_eq!(status, StatusCode::CONFLICT, "{not_offered}");
    assert_eq!(
        (&not_offered["code"], &not_offered["detail"]),
        (
            &json!("singularr.not_offered"),
            &json!("Sending not supported by HUSH.")
        )
    );
    assert!(
        old.methods_seen().iter().all(|m| *m == Method::Get),
        "a bridge without the routes is only read"
    );
    assert!(
        old.urls_seen()
            .iter()
            .all(|u| !u.contains("/stage") && !u.contains("/jobs")),
        "{:?}",
        old.urls_seen()
    );

    // The line holds where every call leaves, not only in the route: with the access taken
    // back, the send itself is refused on the way out and nothing reaches the bridge.
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/integrations/hush",
        &session,
        Some(r#"{"access": "read"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let before = recorded.calls_sent().len();
    let direct = processing::send(&bridge, &damsel_path, "audio").await;
    assert!(
        matches!(
            direct,
            Err(singularr::engines::EngineError::Outbound(
                singularr::core::outbound::OutboundError::NotAllowed { .. }
            ))
        ),
        "{direct:?}"
    );
    assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");
    old_acts.stop().await;
    t.drop_db().await;
}

/// A bridge lists its newest jobs and no more than 2,000 at once. Where it can be asked for
/// more, an old open run is followed by its id, and a whole read that came back full reads
/// on into the older jobs.
#[tokio::test]
async fn open_runs_are_followed_by_id_and_a_full_listing_is_read_on() {
    let Some(t) = TestDb::new("follow").await else {
        return;
    };
    use serde_json::json;
    let journal = Journal::new();
    let recorded = Recorded::new();
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture("hush_health_live.json"),
    );
    let ledger = fixture("hush_history.json");
    recorded.answer(Method::Get, &format!("{HUSH}/history.json"), 200, &ledger);
    let st = state_on(
        &t.pool,
        journal.clone(),
        &[("HUSH_URL", HUSH)],
        recorded.clone(),
    );
    let bridge = st.engines.processor("hush").unwrap().clone();
    let mut reader = processing::Reader::new(bridge.clone());
    let start = std::time::Instant::now();
    let minute = Duration::from_secs(60);
    let by_id = |recorded: &Recorded| -> Vec<String> {
        recorded
            .urls_seen()
            .iter()
            .filter_map(|u| u.split_once("/jobs?ids=").map(|(_, ids)| ids.to_string()))
            .collect()
    };
    let state_of = |job: &'static str| {
        let pool = t.pool.clone();
        async move {
            processing_store::one(&pool, &format!("hush:{job}"))
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{job} is stored"))
        }
    };

    // The whole read carries both open runs: nothing is asked for by id.
    let first = reader.pass(&t.pool, start).await.unwrap();
    assert_eq!(first.detail, "12 runs");
    assert_eq!((first.report.open, first.report.followed), (2, 0));
    assert!(by_id(&recorded).is_empty());

    // Newer jobs push the two open runs out of the newest jobs (five here, as if the ledger
    // were long), and the bridge closes one of them. The newest read does not carry them, so
    // the bridge is asked for them by id, the oldest first, and the run closes in this pass.
    let all: serde_json::Value = serde_json::from_str(&ledger).unwrap();
    let mut newest = all.clone();
    newest["jobs"].as_array_mut().unwrap().truncate(5);
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/history.json?limit=60"),
        200,
        &newest.to_string(),
    );
    let mut closed = all["jobs"][6].clone();
    assert_eq!(closed["id"], "20261115T001015Z-cb34e7ec");
    closed["state"] = json!("failed");
    closed["updated"] = json!("2026-11-23T16:20:00Z");
    closed["events"].as_array_mut().unwrap().push(json!({
        "ts": "2026-11-23T16:20:00Z", "event": "failed", "state": "failed",
        "actor": "hush-api", "error": "the worker gave up"
    }));
    let still_open = all["jobs"][7].clone();
    assert_eq!(still_open["id"], "20261111T214740Z-e90af04d");
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids="),
        200,
        &json!({"jobs": [closed, still_open], "missing": []}).to_string(),
    );
    let followed = reader.pass(&t.pool, start + minute).await.unwrap();
    assert_eq!(
        (
            followed.report.read,
            followed.report.listed,
            followed.report.followed,
            followed.report.added,
            followed.report.updated
        ),
        (processing::Read::Newest, 5, 2, 0, 1)
    );
    assert_eq!(followed.detail, "5 newest runs, 2 older open");
    assert_eq!(
        by_id(&recorded),
        vec!["20261111T214740Z-e90af04d,20261115T001015Z-cb34e7ec"]
    );
    let run = state_of("20261115T001015Z-cb34e7ec").await;
    assert_eq!(
        (run.state.as_str(), run.detail.as_deref()),
        ("failed", Some("the worker gave up"))
    );
    assert_eq!(
        run.finished_at,
        Some("2026-11-23T16:20:00Z".parse().unwrap())
    );
    assert_eq!(followed.ok.counts, json!({"runs": 12, "runs_open": 1}));
    // The run that is still open is asked for again with the next pass, alone. A job the
    // bridge has no record of any more stays as the table has it.
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids="),
        200,
        r#"{"jobs": [], "missing": ["20261111T214740Z-e90af04d"]}"#,
    );
    let again = reader.pass(&t.pool, start + 2 * minute).await.unwrap();
    assert_eq!(again.detail, "5 newest runs");
    assert_eq!(
        by_id(&recorded).last().unwrap(),
        "20261111T214740Z-e90af04d"
    );
    assert_eq!(state_of("20261111T214740Z-e90af04d").await.state, "staged");
    // A bridge that fails that question is not down: the pass says what it could not ask.
    recorded.answer(Method::Get, &format!("{HUSH}/jobs?ids="), 503, "");
    let partial = reader.pass(&t.pool, start + 3 * minute).await.unwrap();
    assert!(
        partial
            .detail
            .starts_with("5 newest runs; Open runs not read: "),
        "{}",
        partial.detail
    );
    assert!(partial.unread.is_some() && !partial.again);

    // A whole read that comes back full: the bridge has older jobs. It is asked for them
    // page by page, from the oldest job of the listing on, until it names no next page.
    let base: chrono::DateTime<chrono::Utc> = "2026-06-01T00:00:00Z".parse().unwrap();
    let id_of = |n: u32| {
        let at = base - chrono::Duration::minutes(i64::from(n));
        format!("{}-{n:08x}", at.format("%Y%m%dT%H%M%SZ"))
    };
    let job = |n: u32| {
        let at = (base - chrono::Duration::minutes(i64::from(n)))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        json!({
            "id": id_of(n), "created": at, "updated": at, "mode": "hevc",
            "trigger": "hush_rules", "state": "promoted",
            "source": {"path": format!("/media/elsewhere/A Show/Season 01/A Show - S01E{:02} - {n}.mkv", n % 90 + 1), "size": 1000 + n}
        })
    };
    let listing: Vec<serde_json::Value> = (0..2000).map(job).collect();
    let long = Recorded::new();
    long.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture("hush_health_live.json"),
    );
    long.answer(
        Method::Get,
        &format!("{HUSH}/history.json"),
        200,
        &json!({"jobs": listing}).to_string(),
    );
    long.answer(
        Method::Get,
        &format!("{HUSH}/jobs?before={}", id_of(1999)),
        200,
        &json!({"jobs": [job(2000), job(2001), job(2002)], "next": id_of(2002), "scanned": 3})
            .to_string(),
    );
    long.answer(
        Method::Get,
        &format!("{HUSH}/jobs?before={}", id_of(2002)),
        200,
        &json!({"jobs": [job(2003), job(2004)], "next": null, "scanned": 2}).to_string(),
    );
    long.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids="),
        200,
        r#"{"jobs": [], "missing": ["20261111T214740Z-e90af04d"]}"#,
    );
    let long_st = state_on(
        &t.pool,
        journal.clone(),
        &[("HUSH_URL", HUSH)],
        long.clone(),
    );
    let long_bridge = long_st.engines.processor("hush").unwrap().clone();
    let whole = processing::pass(&t.pool, &long_bridge, processing::Read::Whole)
        .await
        .unwrap();
    assert_eq!(
        (
            whole.report.listed,
            whole.report.older,
            whole.report.rest,
            whole.report.added
        ),
        (2000, 5, processing::Rest::Nothing, 2005)
    );
    assert_eq!(whole.detail, "2,005 runs");
    assert_eq!(whole.unread, None);
    assert_eq!(
        processing_store::counts(&t.pool).await.unwrap().total,
        12 + 2005
    );
    let pages = |recorded: &Recorded| -> Vec<String> {
        recorded
            .urls_seen()
            .iter()
            .filter_map(|u| {
                u.split_once("/jobs?before=")
                    .map(|(_, rest)| rest.to_string())
            })
            .collect()
    };
    assert_eq!(
        pages(&long),
        vec![
            format!("{}&limit=2000", id_of(1999)),
            format!("{}&limit=2000", id_of(2002))
        ]
    );
    // The stored open run this read did not carry was asked for by id, and the bridge was
    // never asked for its jobs by state, which has it read its whole ledger.
    assert_eq!(by_id(&long), vec!["20261111T214740Z-e90af04d"]);
    assert!(long.urls_seen().iter().all(|u| !u.contains("state=")));
    assert!(long.methods_seen().iter().all(|m| *m == Method::Get));
    // An ordinary pass reads the newest alone and no page.
    let ordinary = processing::pass(&t.pool, &long_bridge, processing::Read::Newest)
        .await
        .unwrap();
    assert_eq!(ordinary.report.older, 0);
    assert_eq!(pages(&long).len(), 2);

    // A ledger longer than one whole read goes back to: ten pages, and the line says so.
    let endless = Recorded::new();
    endless.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture("hush_health_live.json"),
    );
    endless.answer(
        Method::Get,
        &format!("{HUSH}/history.json"),
        200,
        &json!({"jobs": listing}).to_string(),
    );
    endless.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids="),
        200,
        r#"{"jobs": [], "missing": []}"#,
    );
    let older = |page: u32| format!("20000101T0000{page:02}Z-00000000");
    endless.answer(
        Method::Get,
        &format!("{HUSH}/jobs?before={}", id_of(1999)),
        200,
        &json!({"jobs": [], "next": older(0)}).to_string(),
    );
    for page in 0..12 {
        endless.answer(
            Method::Get,
            &format!("{HUSH}/jobs?before={}", older(page)),
            200,
            &json!({"jobs": [], "next": older(page + 1)}).to_string(),
        );
    }
    let endless_st = state_on(
        &t.pool,
        journal.clone(),
        &[("HUSH_URL", HUSH)],
        endless.clone(),
    );
    let endless_bridge = endless_st.engines.processor("hush").unwrap().clone();
    let capped = processing::pass(&t.pool, &endless_bridge, processing::Read::Whole)
        .await
        .unwrap();
    assert_eq!(capped.report.rest, processing::Rest::Left);
    assert_eq!(capped.detail, "2,000 newest runs");
    assert_eq!(pages(&endless).len(), processing::OLDER_PAGES);

    // A bridge that cannot be asked for more than its listing: as before, and said so.
    let short = Recorded::new();
    short.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture("hush_health.json"),
    );
    short.answer(
        Method::Get,
        &format!("{HUSH}/history.json"),
        200,
        &json!({"jobs": listing}).to_string(),
    );
    let short_st = state_on(
        &t.pool,
        journal.clone(),
        &[("HUSH_URL", HUSH)],
        short.clone(),
    );
    let short_bridge = short_st.engines.processor("hush").unwrap().clone();
    let full = processing::pass(&t.pool, &short_bridge, processing::Read::Whole)
        .await
        .unwrap();
    assert_eq!(full.report.rest, processing::Rest::NotGiven);
    assert_eq!(full.detail, "2,000 newest runs");
    assert!(
        short.urls_seen().iter().all(|u| !u.contains("/jobs")),
        "{:?}",
        short.urls_seen()
    );
    t.drop_db().await;
}

/// A worker that runs a flow works on a copy of the file in its own cache folder, so the file
/// it reports is not the path the run waits at. The run is joined to it by the footprint of
/// the file: the worker names it, and the worker's record of the waiting file carries it.
/// The worker's answers here are the ones saved while that happened.
#[tokio::test]
async fn a_run_is_working_while_a_worker_runs_a_flow_on_a_copy_of_its_file() {
    let Some(t) = TestDb::new("footprint").await else {
        return;
    };
    use serde_json::json;
    let journal = Journal::new();
    let recorded = Recorded::new();
    record_processing(&recorded, "hush_health.json");
    // The bridge's job: staged days ago, waiting at its queue path.
    let queue = "/media/hush/queue/movies/audio_4k/Tron (1982) {tmdb-97}/Tron (1982) {imdb-tt0084827} [Remux-2160p Proper][DV HDR10][TrueHD Atmos 7.1][HEVC]-TRiToN.mkv";
    let job = json!({
        "id": "20261120T100000Z-0f0e0d0c", "created": "2026-11-20T10:00:00Z",
        "updated": "2026-11-20T10:00:05Z", "mode": "uhd-audio", "trigger": "tag:hush-audio",
        "state": "staged", "queue_path": queue,
        "source": {
            "path": "/media/4kmovies/Tron (1982) {tmdb-97}/Tron (1982) {imdb-tt0084827} [Remux-2160p Proper][DV HDR10][TrueHD Atmos 7.1][HEVC]-TRiToN.mkv",
            "size": 48_307_947_000_i64
        },
        "events": [{"ts": "2026-11-20T10:00:05Z", "event": "staged", "actor": "hushq"}]
    });
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/history.json"),
        200,
        &json!({"jobs": [job]}).to_string(),
    );
    // The worker as it answered then: its nodes with the busy worker, and its record of the
    // file at that queue path.
    let nodes_at = format!("{TDARR}/api/v2/get-nodes");
    let busy_nodes = fixture("tdarr_nodes_busy_live.json");
    recorded.answer(Method::Get, &nodes_at, 200, &busy_nodes);
    recorded.answer_body(
        Method::Post,
        &format!("{TDARR}/api/v2/cruddb"),
        "audio_4k/Tron (1982)",
        200,
        &fixture("tdarr_file_working_live.json"),
    );
    let env = [("HUSH_URL", HUSH), ("TDARR_URL", TDARR)];
    let st = state_on(&t.pool, journal.clone(), &env, recorded.clone());
    let bridge = st.engines.processor("hush").unwrap().clone();
    let read = processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    assert_eq!((read.report.listed, read.report.open), (1, 1));
    let run_id = "hush:20261120T100000Z-0f0e0d0c";
    let total = |app: &axum::Router, state: &'static str| {
        let app = app.clone();
        async move {
            get_ok(&app, &format!("/api/v1/processing/runs?state={state}")).await["total"]
                .as_i64()
                .unwrap()
        }
    };

    // The worker is on its own copy of the file, and the run is its run all the same: the
    // run is working with the worker's progress, the counts say so, and it is not stuck.
    let app = routes::router(st.clone());
    let page = get_ok(&app, "/api/v1/processing").await;
    assert_eq!(page["workers"].as_array().unwrap().len(), 1);
    let worker = &page["workers"][0];
    assert_ne!(worker["file"], json!(queue));
    assert!(
        worker["file"]
            .as_str()
            .unwrap()
            .starts_with("/media/transcode/"),
        "{worker}"
    );
    assert_eq!(
        (
            &worker["service"],
            &worker["status"],
            &worker["kind"],
            &worker["run"]
        ),
        (
            &json!("tdarr"),
            &json!("Movie Finalizer (Audio)"),
            &json!("transcodegpu"),
            &json!(run_id)
        )
    );
    assert_eq!(
        (
            &page["counts"]["open"],
            &page["counts"]["working"],
            &page["counts"]["waiting"],
            &page["counts"]["stuck"]
        ),
        (&json!(1), &json!(1), &json!(0), &json!(0))
    );
    let run = &page["open"][0];
    assert_eq!(
        (&run["id"], &run["state"], &run["stuck"], &run["stuckWhy"]),
        (
            &json!(run_id),
            &json!("working"),
            &json!(false),
            &json!(null)
        )
    );
    let mut on_it = worker.clone();
    on_it.as_object_mut().unwrap().remove("run");
    assert_eq!(run["worker"], on_it, "the worker's entry, without `run`");
    assert_eq!(
        (
            &run["inWorker"]["service"],
            &run["inWorker"]["state"],
            &run["inWorker"]["words"]
        ),
        (
            &json!("tdarr"),
            &json!("working"),
            &json!("Movie Finalizer (Audio)")
        )
    );
    assert_eq!(total(&app, "working").await, 1);
    assert_eq!(total(&app, "staged").await, 0);
    assert_eq!(total(&app, "stuck").await, 0);
    // The reports the worker keeps of the jobs it ran on the file, by the footprint its busy
    // worker names, newest first, and one report's text, asked on its own. These answers are
    // shaped after the worker's API description: no real one was recorded yet.
    let report_name = "QEdffjm()3.09.00()transcode()d99SYzi()1795471357941.txt";
    let older = "QEdffjm()3.09.00()healthcheck()h1()1795400000000.txt";
    recorded.answer(
        Method::Post,
        &format!("{TDARR}/api/v2/list-footprintId-reports"),
        200,
        &json!([older, report_name]).to_string(),
    );
    recorded.answer(
        Method::Post,
        &format!("{TDARR}/api/v2/read-job-file"),
        200,
        r#"{"text": "Worker started", "isJobRunning": true, "jobReportExists": true}"#,
    );
    let one = get_ok(&app, &format!("/api/v1/processing/runs/{run_id}")).await;
    assert_eq!(
        (&one["state"], &one["worker"], &one["inWorker"]["state"]),
        (&json!("working"), &on_it, &json!("working"))
    );
    assert_eq!(
        (&one["reports"]["service"], &one["reports"]["footprint"]),
        (&json!("tdarr"), &json!("QEdffjm"))
    );
    let listed: Vec<&str> = one["reports"]["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, [report_name, older]);
    assert_eq!(
        (
            &one["reports"]["reports"][0]["job"],
            &one["reports"]["reports"][0]["kind"]
        ),
        (&json!("d99SYzi"), &json!("transcode"))
    );
    let text = get_ok(
        &app,
        &format!(
            "/api/v1/processing/runs/{run_id}/reports/{}",
            singularr::core::outbound::encode_component(report_name)
        ),
    )
    .await;
    assert_eq!(
        (
            &text["id"],
            &text["text"],
            &text["running"],
            &text["exists"]
        ),
        (
            &json!(report_name),
            &json!("Worker started"),
            &json!(true),
            &json!(true)
        )
    );
    let asked = recorded
        .calls_sent()
        .into_iter()
        .rfind(|c| c.url.ends_with("/api/v2/read-job-file"))
        .unwrap();
    assert!(
        matches!(&asked.body, Sent::Json(body) if body.contains("\"jobId\":\"d99SYzi\"")),
        "{:?}",
        asked.body
    );
    // A report the worker does not list for this file is not asked for.
    let session = [("x-auth-request-email", "owner@example.com")];
    let (status, _, _) = call(
        &app,
        "GET",
        &format!("/api/v1/processing/runs/{run_id}/reports/other.txt"),
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A worker that names another footprint, and one that reports no job at all, leave the
    // run waiting, whatever the name of the file they are on: what the worker's record says
    // of the run's file stands, and the run is stuck. What the services say is kept a few
    // seconds, so each case asks them through a state of its own.
    let mut another: serde_json::Value = serde_json::from_str(&busy_nodes).unwrap();
    another["TNUn37R"]["workers"]["yearly-yak"]["job"]["footprintId"] = json!("AnOther");
    let mut jobless = another.clone();
    jobless["TNUn37R"]["workers"]["yearly-yak"]
        .as_object_mut()
        .unwrap()
        .remove("job");
    for (case, nodes) in [("another footprint", another), ("no job", jobless)] {
        recorded.answer(Method::Get, &nodes_at, 200, &nodes.to_string());
        let app = routes::router(state_on(&t.pool, journal.clone(), &env, recorded.clone()));
        let page = get_ok(&app, "/api/v1/processing").await;
        let worker = &page["workers"][0];
        assert_eq!(
            (&worker["status"], &worker["run"]),
            (&json!("Movie Finalizer (Audio)"), &json!(null)),
            "{case}"
        );
        assert_eq!(
            (
                &page["counts"]["working"],
                &page["counts"]["waiting"],
                &page["counts"]["stuck"]
            ),
            (&json!(0), &json!(1), &json!(1)),
            "{case}"
        );
        let run = &page["open"][0];
        assert_eq!(
            (&run["state"], &run["worker"], &run["stuck"]),
            (&json!("staged"), &json!(null), &json!(true)),
            "{case}"
        );
        assert_eq!(
            (&run["inWorker"]["state"], &run["inWorker"]["words"]),
            (&json!("done"), &json!("Transcode success")),
            "{case}"
        );
        assert_eq!(total(&app, "working").await, 0, "{case}");
        assert_eq!(total(&app, "staged").await, 1, "{case}");
        assert_eq!(total(&app, "stuck").await, 1, "{case}");
    }
    // Nothing but GET reached a service, but for the worker's reads of its own records and of
    // its reports.
    let reads = ["cruddb", "list-footprintId-reports", "read-job-file"]
        .map(|path| format!("{TDARR}/api/v2/{path}"));
    assert!(
        recorded
            .calls_sent()
            .iter()
            .all(|c| c.method == Method::Get || reads.contains(&c.url))
    );
    t.drop_db().await;
}

/// The worker as it answered when nothing ran: its status, its nodes, its records and its
/// empty queue; and every act it is sent taken with 200.
fn record_worker(recorded: &Recorded) {
    recorded.answer(
        Method::Get,
        &format!("{TDARR}/api/v2/status"),
        200,
        &fixture("tdarr_status.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{TDARR}/api/v2/get-nodes"),
        200,
        &fixture("tdarr_nodes_idle.json"),
    );
    let records = format!("{TDARR}/api/v2/cruddb");
    for (collection, file) in [
        ("LibrarySettingsJSONDB", "tdarr_libraries.json"),
        ("FlowsJSONDB", "tdarr_flows.json"),
        ("StatisticsJSONDB", "tdarr_statistics.json"),
    ] {
        recorded.answer_body(Method::Post, &records, collection, 200, &fixture(file));
    }
    recorded.answer_body(
        Method::Post,
        &format!("{TDARR}/api/v2/client/status-tables"),
        "table1",
        200,
        &fixture("tdarr_table_queue_empty.json"),
    );
    // What the worker answers a change it takes.
    recorded.answer(Method::Post, &format!("{TDARR}/api/v2/"), 200, r#""OK""#);
}

/// The worker's changes are the owner's, each only while Singularr may act on the worker: under
/// read access every route answers 403 `singularr.not_allowed` and nothing at all is asked of
/// the worker; under write access each act goes in its one form and is journaled.
#[tokio::test]
async fn the_worker_is_changed_by_the_owner_only_with_write_access() {
    let Some(t) = TestDb::new("worker_acts").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::outbound::Access;
    let journal = Journal::new();
    let recorded = Recorded::new();
    record_worker(&recorded);
    let out = outbound(recorded.clone());
    let st = state_over(&t.pool, journal.clone(), &[("TDARR_URL", TDARR)], &out);
    let app = routes::router(st.clone());
    let worker = st.engines.processor("tdarr").unwrap().clone();
    processing::pass(&t.pool, &worker, processing::Read::Whole)
        .await
        .unwrap();
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let at = |path: &str| format!("/api/v1/processing/workers/tdarr{path}");
    // The calls that would change something in the worker, with their bodies: every POST that
    // is none of its reads (its record store in a reading mode, a table).
    let changes = |recorded: &Recorded| -> Vec<(String, serde_json::Value)> {
        recorded
            .calls_sent()
            .iter()
            .filter(|c| c.method == Method::Post)
            .filter_map(|c| {
                let Sent::Json(text) = &c.body else {
                    return None;
                };
                let body: serde_json::Value = serde_json::from_str(text).ok()?;
                if matches!(body["data"]["mode"].as_str(), Some("getAll" | "getById"))
                    || body["data"]["opts"].is_object()
                {
                    return None;
                }
                Some((c.url.trim_start_matches(TDARR).to_string(), body))
            })
            .collect()
    };
    let queue_file = "/media/hush/queue/shows/hevc_1080p/Show (2020)/Season 01/Show - S01E01.mkv";
    let every: Vec<(String, Option<String>)> = vec![
        (
            at("/nodes/TNUn37R/limits"),
            Some(json!({"kind": "transcodeGpu", "to": 6}).to_string()),
        ),
        (
            at("/nodes/TNUn37R/pause"),
            Some(json!({"paused": true}).to_string()),
        ),
        (at("/nodes/TNUn37R/workers/yearly-yak/cancel"), None),
        (
            at("/requeue"),
            Some(json!({"path": queue_file}).to_string()),
        ),
        (at("/libraries/HushShowHEVC_1080/scan"), None),
        (
            at("/libraries/HushShowHEVC_1080/watch"),
            Some(json!({"on": false}).to_string()),
        ),
        (
            at("/libraries/HushShowHEVC_1080/flow"),
            Some(json!({"flow": "hush_movie_hevc_1080_v1"}).to_string()),
        ),
    ];

    // Read access: every route says so, and nothing is asked of the worker, not even a read.
    let before = recorded.calls_sent().len();
    // Each refusal names its route's own action.
    let labels = [
        "Change worker limits",
        "Pause and resume nodes",
        "Cancel a worker's file",
        "Requeue a file",
        "Scan a library",
        "Switch folder watching",
        "Assign a flow to a library",
    ];
    for ((path, body), label) in every.iter().zip(labels) {
        let (status, content_type, problem) =
            call(&app, "POST", path, &session, body.as_deref()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {problem}");
        assert!(content_type.starts_with("application/problem+json"));
        assert_eq!(
            (&problem["code"], &problem["detail"]),
            (
                &json!("singularr.not_allowed"),
                &json!(format!("{label} is read-only on Tdarr."))
            ),
            "{path}"
        );
    }
    assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");

    // Write access. A key, whatever its scopes, changes nothing: only the owner does.
    st.engines
        .access
        .set(
            &t.pool,
            &journal,
            "user:owner@example.com",
            "tdarr",
            Access::Act,
        )
        .await
        .unwrap();
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let admin = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "everything".into(),
            scopes: vec![Scope::Admin],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", admin.secret);
    let with_key: [(&str, &str); 1] = [("authorization", bearer.as_str())];
    for (path, body) in &every {
        let (status, _, problem) = call(&app, "POST", path, &with_key, body.as_deref()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {problem}");
        assert_ne!(problem["code"], "singularr.not_allowed", "{path}");
    }
    assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");
    let after_access = core_store::latest_event_id(&t.pool).await.unwrap();

    // What cannot be asked: an unknown kind, a limit out of range, a node, a library or a
    // flow the worker does not have, a busy worker that is not busy, a file in no library.
    for (path, body, expected) in [
        (
            at("/nodes/TNUn37R/limits"),
            Some(json!({"kind": "gpu", "to": 2})),
            StatusCode::BAD_REQUEST,
        ),
        (
            at("/nodes/TNUn37R/limits"),
            Some(json!({"kind": "transcodeGpu", "to": 33})),
            StatusCode::BAD_REQUEST,
        ),
        (
            at("/nodes/gone/limits"),
            Some(json!({"kind": "transcodeGpu", "to": 2})),
            StatusCode::NOT_FOUND,
        ),
        (
            at("/nodes/TNUn37R/workers/yearly-yak/cancel"),
            None,
            StatusCode::NOT_FOUND,
        ),
        (at("/libraries/gone/scan"), None, StatusCode::NOT_FOUND),
        (
            at("/libraries/HushShowHEVC_1080/flow"),
            Some(json!({"flow": "gone"})),
            StatusCode::BAD_REQUEST,
        ),
        (
            at("/requeue"),
            Some(json!({"path": "/media/movies/A (2001)/a.mkv"})),
            StatusCode::BAD_REQUEST,
        ),
        (at("/requeue"), Some(json!({})), StatusCode::BAD_REQUEST),
        (
            "/api/v1/processing/workers/hush/requeue".to_string(),
            Some(json!({"path": queue_file})),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let body = body.map(|b| b.to_string());
        let (status, _, problem) = call(&app, "POST", &path, &session, body.as_deref()).await;
        assert_eq!(status, expected, "{path}: {problem}");
    }
    assert!(changes(&recorded).is_empty(), "nothing was changed");

    // Each act, as the owner: the limit moves from 4 to 6 in two steps.
    let mut answers = Vec::new();
    for (path, body) in &every {
        if path.ends_with("/cancel") {
            continue;
        }
        let (status, _, answer) = call(&app, "POST", path, &session, body.as_deref()).await;
        assert_eq!(status, StatusCode::OK, "{path}: {answer}");
        answers.push(answer);
    }
    assert_eq!(
        answers[0],
        json!({"act": "limits", "service": "tdarr", "steps": 2})
    );
    let sent = changes(&recorded);
    let paths: Vec<&str> = sent.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        paths,
        [
            "/api/v2/alter-worker-limit",
            "/api/v2/alter-worker-limit",
            "/api/v2/update-node",
            "/api/v2/bulk-update-files",
            "/api/v2/scan-files",
            "/api/v2/toggle-folder-watch",
            "/api/v2/cruddb",
        ]
    );
    assert_eq!(
        sent[0].1,
        json!({"data": {"nodeID": "TNUn37R", "process": "increase", "workerType": "transcodegpu"}})
    );
    assert_eq!(
        sent[6].1,
        json!({"data": {"collection": "LibrarySettingsJSONDB", "mode": "update", "docID": "HushShowHEVC_1080", "obj": {"flowId": "hush_movie_hevc_1080_v1"}}})
    );
    // Each change the worker took is one event, by the owner, about the worker.
    let events = core_store::events_after(&t.pool, after_access, 100)
        .await
        .unwrap();
    let changed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == "processing_worker_changed")
        .collect();
    assert_eq!(changed.len(), 6);
    assert!(changed.iter().all(|e| e.actor == "user:owner@example.com"
        && e.subject_type == "engine"
        && e.subject_id == "tdarr"));
    assert_eq!(
        (
            &changed[0].body["act"],
            &changed[0].body["from"],
            &changed[0].body["to"],
            &changed[0].body["steps"],
            &changed[0].body["node_name"]
        ),
        (
            &json!("limits"),
            &json!(4),
            &json!(6),
            &json!(2),
            &json!("Tdarr-Node-5090")
        )
    );
    assert_eq!(
        (
            &changed[5].body["act"],
            &changed[5].body["library_name"],
            &changed[5].body["flow_name"],
            &changed[5].body["from"]
        ),
        (
            &json!("flow"),
            &json!("TV - HUSH HEVC 1080p"),
            &json!("HUSH Movie HEVC 1080p (SHH)"),
            &json!("hush_tv_hevc_1080_v1")
        )
    );
    // The worker says no in its own words: nothing is journaled.
    recorded.answer(
        Method::Post,
        &format!("{TDARR}/api/v2/update-node"),
        400,
        r#""Node missing, disconnected, or did not acknowledge""#,
    );
    let (status, _, refused) = call(
        &app,
        "POST",
        &at("/nodes/TNUn37R/pause"),
        &session,
        Some(r#"{"paused": false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{refused}");
    assert_eq!(refused["code"], "singularr.refused");
    let later = core_store::events_after(&t.pool, after_access, 100)
        .await
        .unwrap();
    assert_eq!(
        later
            .iter()
            .filter(|e| e.kind == "processing_worker_changed")
            .count(),
        6
    );
    t.drop_db().await;
}

/// Processing rules against the imports. An off rule records what it would have sent; an on
/// rule sends only while its service's access is `act`; a file is sent once; never a file of a
/// protected title, never one processing made. On request a rule takes the files it matches
/// now.
#[tokio::test]
async fn processing_rules_watch_imports_and_send_once() {
    let Some(t) = TestDb::new("rules").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::journal::event;
    use singularr::core::outbound::Access;
    use singularr::processing::rules::{self, RuleBody, RuleConditions};
    let journal = Journal::new();
    let recorded = Recorded::new();
    record_processing(&recorded, "hush_health_live.json");
    recorded.answer(
        Method::Post,
        &format!("{HUSH}/stage"),
        201,
        &fixture("hush_stage_staged.json"),
    );
    let out = outbound(recorded.clone());
    let services = [("HUSH_URL", HUSH), ("TDARR_URL", TDARR)];
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/jobs?ids=20261123T160500Z-0a1b2c3d"),
        200,
        &fixture("hush_jobs_by_id.json"),
    );
    let st = state_over(&t.pool, journal.clone(), &services, &out);
    let policy = ActivePolicy::builtin().policy;
    let outbox = st.outbox();
    let pass = || rules::import_pass(&t.pool, &journal, &st.engines, &policy, true, &outbox);
    // The first pass marks where the journal stands: the rules start with what comes after.
    assert_eq!(pass().await.unwrap(), rules::ImportPassed::default());

    let rule =
        |name: &str, enabled: bool, when: &str, file: serde_json::Value, flow: &str| RuleBody {
            name: name.to_string(),
            enabled,
            when: when.to_string(),
            conditions: RuleConditions {
                file: serde_json::from_value(file).unwrap(),
                ..RuleConditions::default()
            },
            service: "hush".to_string(),
            flow: flow.to_string(),
        };
    let owner = "user:owner@example.com";
    let mut conn = t.pool.acquire().await.unwrap();
    let atmos = processing_store::insert_rule(
        &mut conn,
        &rule(
            "Atmos audio",
            true,
            "import",
            json!({"audio_codec": ["Eac3"], "atmos": true}),
            "audio",
        ),
        owner,
    )
    .await
    .unwrap();
    let watched = processing_store::insert_rule(
        &mut conn,
        &rule(
            "Every HEVC file",
            false,
            "import",
            json!({"video_codec": ["H265"]}),
            "hevc",
        ),
        owner,
    )
    .await
    .unwrap();
    let opus = processing_store::insert_rule(
        &mut conn,
        &rule(
            "Opus audio",
            true,
            "import",
            json!({"audio_codec": ["Opus"]}),
            "audio",
        ),
        owner,
    )
    .await
    .unwrap();
    drop(conn);
    assert_eq!(atmos.version, 1);
    let versions = processing_store::rule_versions(&t.pool, atmos.id)
        .await
        .unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].body["name"], "Atmos audio");

    // The imports: the catalog of the ledger's titles, read from their managers; then the
    // ledger, so the files processing made are known.
    ledger_catalog(&t.pool, &journal, &recorded).await;
    let bridge = st.engines.processor("hush").unwrap().clone();
    processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    let passed = pass().await.unwrap();
    assert_eq!((passed.imports, passed.logged, passed.sent), (3, 6, 0));
    assert_eq!(
        staged_calls(&recorded),
        0,
        "nothing is sent while the access is read"
    );
    let log = |id: i64| {
        let pool = t.pool.clone();
        async move { processing_store::rule_log(&pool, id, 50).await.unwrap() }
    };
    // The on rule could not send: its service is read-only.
    let held = log(atmos.id).await;
    assert_eq!(held.len(), 1);
    assert!(held[0].path.ends_with(KINGDOM_FILE), "{}", held[0].path);
    assert_eq!(
        (held[0].outcome.as_str(), held[0].detail.as_deref()),
        ("held", Some("Read-only"))
    );
    assert!(held[0].event_id.is_some() && held[0].evidence.is_some());
    // The off rule watches: what it would have sent, for every file it matches.
    let shadow = log(watched.id).await;
    assert_eq!(shadow.len(), 3);
    assert!(
        shadow
            .iter()
            .all(|e| e.outcome == "shadow" && e.detail.is_none())
    );
    // What processing made is never sent again.
    let made = log(opus.id).await;
    assert_eq!(made.len(), 2);
    assert!(
        made.iter()
            .all(|e| e.outcome == "skipped" && e.detail.as_deref() == Some("Processing result"))
    );

    // Write access, and the same file imported again: the on rule sends it, as itself.
    st.engines
        .access
        .set(&t.pool, &journal, owner, "hush", Access::Act)
        .await
        .unwrap();
    let kingdom: i64 = sqlx::query_scalar("SELECT id FROM copies WHERE relative_path = $1")
        .bind(KINGDOM_FILE)
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let import = |copy: i64| {
        let pool = t.pool.clone();
        let journal = journal.clone();
        async move {
            let mut tx = journal.begin(&pool).await.unwrap();
            tx.append_one(event("copy_added", "copy", copy, "sync:radarr", json!({})))
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
    };
    import(kingdom).await;
    let sent = pass().await.unwrap();
    assert_eq!((sent.imports, sent.sent), (1, 1));
    deliver_acts(&st).await;
    assert_eq!(staged_calls(&recorded), 1);
    let staged = recorded
        .calls_sent()
        .into_iter()
        .rfind(|c| c.method == Method::Post && c.url.starts_with(HUSH))
        .unwrap();
    assert!(
        matches!(&staged.body, Sent::Json(body) if body.contains(KINGDOM_FILE)),
        "{:?}",
        staged.body
    );
    let newest = &log(atmos.id).await[0];
    assert_eq!(
        (newest.outcome.as_str(), newest.detail.as_deref()),
        ("sent", None)
    );
    // The send is a journal event in the rule's name; the off rule sent nothing.
    let by_rule = core_store::find_events(
        &t.pool,
        &["processing_sent".to_string()],
        &[format!("rule:{}", atmos.id)],
        None,
        None,
        10,
    )
    .await
    .unwrap();
    assert_eq!(by_rule.len(), 1);
    assert_eq!(log(watched.id).await[0].outcome, "shadow");

    // Imported once more: a file is sent once.
    import(kingdom).await;
    assert_eq!(pass().await.unwrap().sent, 0);
    assert_eq!(staged_calls(&recorded), 1);
    let newest = &log(atmos.id).await[0];
    assert_eq!(
        (newest.outcome.as_str(), newest.detail.as_deref()),
        ("skipped", Some("Sent before"))
    );
    // A title its owner protects in its manager is never sent, whatever else holds.
    sqlx::query("UPDATE title_engines SET tags = '{hush-keep}' WHERE path = $1")
        .bind(KINGDOM_FOLDER)
        .execute(&t.pool)
        .await
        .unwrap();
    import(kingdom).await;
    assert_eq!(pass().await.unwrap().sent, 0);
    let newest = &log(atmos.id).await[0];
    assert_eq!(
        (newest.outcome.as_str(), newest.detail.as_deref()),
        ("skipped", Some("Protected by tag hush-keep"))
    );
    assert_eq!(staged_calls(&recorded), 1);
    let sent_once: i64 =
        sqlx::query_scalar("SELECT count(*) FROM processing_rule_log WHERE outcome = 'sent'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(sent_once, 1);

    // On request: the files the rule matches now, each decided like an import.
    let mut conn = t.pool.acquire().await.unwrap();
    let asked = processing_store::insert_rule(
        &mut conn,
        &rule(
            "Opus on request",
            true,
            "request",
            json!({"audio_codec": ["Opus"]}),
            "audio",
        ),
        owner,
    )
    .await
    .unwrap();
    drop(conn);
    let ran = rules::request_run(
        &t.pool,
        &journal,
        &st.engines,
        &policy,
        &asked,
        true,
        &outbox,
    )
    .await
    .unwrap();
    assert_eq!((ran.matched, ran.logged, ran.sent, ran.left), (2, 2, 0, 0));
    // A rule on request is no import rule: the imports pass it by.
    import(kingdom).await;
    pass().await.unwrap();
    assert_eq!(log(asked.id).await.len(), 2);
    assert_eq!(staged_calls(&recorded), 1);
    t.drop_db().await;
}

/// The worker's own page, and the processing rules through the API: a draft previewed, a rule
/// added and saved again (each a version, each journaled), its log with what the service did
/// on its own for the same file, a run on request refused while the service is read-only or
/// the rule is off, and the rule removed. Changes are the owner's alone.
#[tokio::test]
async fn the_worker_page_and_the_rules_routes_answer() {
    let Some(t) = TestDb::new("rules_api").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::journal::event;
    use singularr::processing::rules;
    let journal = Journal::new();
    let recorded = Recorded::new();
    record_processing(&recorded, "hush_health_live.json");
    ledger_catalog(&t.pool, &journal, &recorded).await;
    let out = outbound(recorded.clone());
    let services = [("HUSH_URL", HUSH), ("TDARR_URL", TDARR)];
    let st = state_over(&t.pool, journal.clone(), &services, &out);
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    for id in ["hush", "tdarr"] {
        let p = st.engines.processor(id).unwrap().clone();
        processing::pass(&t.pool, &p, processing::Read::Whole)
            .await
            .unwrap();
    }

    // The worker as it answers now: its node with its limits and no worker busy, its empty
    // queue, its libraries; what it does not give is named.
    let page = get_ok(&app, "/api/v1/processing/workers/tdarr").await;
    assert_eq!(page["access"], "read");
    let actions = page["actions"].as_array().unwrap();
    assert_eq!(actions.len(), 7, "{actions:?}");
    assert!(actions.iter().all(|a| a["access"] == "read"));
    assert_eq!(page["service"]["id"], "tdarr");
    assert_eq!(
        page["service"]["libraries"].as_array().map(Vec::len),
        Some(7)
    );
    let node = &page["nodes"][0];
    assert_eq!(
        (
            &node["id"],
            &node["limits"]["transcodeGpu"],
            &node["workers"]
        ),
        (&json!("TNUn37R"), &json!(4), &json!([]))
    );
    assert_eq!(page["queue"]["total"], 0);
    assert_eq!(page["unread"], json!(["Resources", "History"]));
    for path in [
        "/api/v1/processing/workers/hush",
        "/api/v1/processing/workers/nothing",
    ] {
        let (status, _, _) = call(&app, "GET", path, &session, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
    }

    // A draft: what it matches now, with a count, and why.
    let draft = json!({"conditions": {"file": {"audio_codec": ["Eac3"], "atmos": true}}});
    let (status, _, preview) = call(
        &app,
        "POST",
        "/api/v1/processing/rules/preview",
        &session,
        Some(&draft.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["count"], 1);
    let first = &preview["files"][0];
    assert_eq!(
        (&first["file"], &first["title"]["id"]),
        (&json!(KINGDOM_FILE), &json!("movie-1495"))
    );
    assert!(
        first["evidence"]
            .as_str()
            .is_some_and(|e| e.contains("Atmos"))
    );

    // Adding is the owner's, and a rule is checked first.
    let body = |patch: serde_json::Value| {
        let mut b = json!({
            "name": "Atmos audio", "enabled": false, "when": "import",
            "conditions": {"file": {"audio_codec": ["Eac3"], "atmos": true}},
            "service": "hush", "flow": "audio"
        });
        b.as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        b.to_string()
    };
    let rules_at = "/api/v1/processing/rules";
    for (patch, why) in [
        (json!({"conditions": {}}), "no condition"),
        (json!({"flow": "nothing"}), "no such flow"),
        (json!({"service": "tdarr"}), "a service without flows"),
        (json!({"when": "later"}), "no such moment"),
        (json!({"name": " "}), "no name"),
        (
            json!({"conditions": {"traits": ["nothing"]}}),
            "no such trait",
        ),
        (
            json!({"conditions": {"file": {"codec": ["H264"]}}}),
            "no such field",
        ),
    ] {
        let (status, _, problem) = call(&app, "POST", rules_at, &session, Some(&body(patch))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{why}: {problem}");
    }
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let key = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "everything".into(),
            scopes: vec![Scope::Admin],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", key.secret);
    let with_key: [(&str, &str); 1] = [("authorization", bearer.as_str())];
    let (status, _, _) = call(&app, "POST", rules_at, &with_key, Some(&body(json!({})))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, added) = call(&app, "POST", rules_at, &session, Some(&body(json!({})))).await;
    assert_eq!(status, StatusCode::CREATED, "{added}");
    let id = added["id"].as_i64().unwrap();
    assert_eq!(
        (
            &added["version"],
            &added["serviceName"],
            &added["flowLabel"],
            &added["when"]
        ),
        (
            &json!(1),
            &json!("HUSH"),
            &json!("Rebuild audio"),
            &json!("import")
        )
    );
    let one = format!("{rules_at}/{id}");
    let (status, _, saved) = call(
        &app,
        "PUT",
        &one,
        &session,
        Some(&body(json!({"name": "Atmos audio, all of it"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["version"], 2);
    let detail = get_ok(&app, &one).await;
    assert_eq!(detail["versions"].as_array().map(Vec::len), Some(2));
    assert_eq!(detail["versions"][1]["body"]["name"], "Atmos audio");
    assert_eq!(
        get_ok(&app, rules_at).await["rules"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );

    // An import the rule watches: the log says what it would have sent, next to what the
    // service did on its own for the same file.
    let policy = ActivePolicy::builtin().policy;
    rules::import_pass(&t.pool, &journal, &st.engines, &policy, true, &st.outbox())
        .await
        .unwrap();
    let kingdom: i64 = sqlx::query_scalar("SELECT id FROM copies WHERE relative_path = $1")
        .bind(KINGDOM_FILE)
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let mut tx = journal.begin(&t.pool).await.unwrap();
    tx.append_one(event(
        "copy_added",
        "copy",
        kingdom,
        "sync:radarr",
        json!({}),
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    rules::import_pass(&t.pool, &journal, &st.engines, &policy, true, &st.outbox())
        .await
        .unwrap();
    let log = get_ok(&app, &format!("{one}/log")).await;
    assert_eq!(log["counts"]["shadow"], 1);
    let entry = &log["entries"][0];
    assert_eq!(
        (
            &entry["outcome"],
            &entry["file"],
            &entry["version"],
            &entry["title"]["id"]
        ),
        (
            &json!("shadow"),
            &json!(KINGDOM_FILE),
            &json!(2),
            &json!("movie-1495")
        )
    );
    assert_eq!(
        (&entry["serviceDid"]["run"], &entry["serviceDid"]["trigger"]),
        (
            &json!("hush:20261115T001015Z-cb34e7ec"),
            &json!("tag:hush-audio")
        )
    );
    assert_eq!(
        get_ok(&app, rules_at).await["rules"][0]["counts"]["shadow"],
        1
    );

    // Run on request: an off rule is not run, nor one whose service is read-only.
    let run = format!("{one}/run");
    let (status, _, off) = call(&app, "POST", &run, &session, None).await;
    assert_eq!(
        (status, &off["code"]),
        (StatusCode::CONFLICT, &json!("singularr.conflict"))
    );
    let (status, _, _) = call(
        &app,
        "PUT",
        &one,
        &session,
        Some(&body(json!({"enabled": true, "when": "request"}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, read_only) = call(&app, "POST", &run, &session, None).await;
    assert_eq!(
        (status, &read_only["code"], &read_only["detail"]),
        (
            StatusCode::FORBIDDEN,
            &json!("singularr.not_allowed"),
            &json!("Send a file to a flow is read-only on HUSH.")
        )
    );
    assert_eq!(staged_calls(&recorded), 0);

    // Removed: gone from the list, its log kept; every change journaled in the owner's name.
    let (status, _, _) = call(&app, "DELETE", &one, &with_key, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(&app, "DELETE", &one, &session, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = call(&app, "GET", &one, &session, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(get_ok(&app, rules_at).await["rules"], json!([]));
    let kinds = kinds(&t.pool, 0).await;
    assert_eq!(count(&kinds, "processing_rule_saved"), 3);
    assert_eq!(count(&kinds, "processing_rule_removed"), 1);
    let page = get_ok(&app, "/api/v1/journal?kind=rule.removed").await;
    assert!(
        page.to_string().contains("Rule Atmos audio removed"),
        "{page}"
    );
    t.drop_db().await;
}

// Search, grab and import ------------------------------------------------------------------------

/// The download Radarr refused to import on 24 Nov (`radarr_queue_import_refused.json`).
const REFUSED_HASH: &str = "147E6690379BCAC2718AAA6A0D4089BFED6737B2";

/// The way out of `outbound()` with the managers listed as production lists them: GET, and
/// their acts.
fn acting_outbound(recorded: Arc<Recorded>) -> Outbound {
    let mut allow = Allowlist::with_metadata_providers();
    allow
        .allow_integration(
            "radarr",
            RADARR,
            singularr::engines::arr::calls(ArrKind::Radarr),
        )
        .unwrap();
    allow
        .allow_integration(
            "sonarr",
            SONARR,
            singularr::engines::arr::calls(ArrKind::Sonarr),
        )
        .unwrap();
    allow
        .allow_engine("qbittorrent", QBIT, &["/api/v2/auth/login"])
        .unwrap();
    allow.allow_engine("seerr", SEERR, &[]).unwrap();
    Outbound::with_transport(allow, recorded)
}

/// Every call that is no GET, as (method, path and query, body).
fn acts_sent(recorded: &Recorded) -> Vec<(Method, String, Option<serde_json::Value>)> {
    recorded
        .calls_sent()
        .into_iter()
        .filter(|c| c.method != Method::Get)
        .map(|c| {
            let body = match &c.body {
                Sent::Json(text) => serde_json::from_str(text).ok(),
                _ => None,
            };
            let at = c
                .url
                .trim_start_matches(RADARR)
                .trim_start_matches(SONARR)
                .to_string();
            (c.method, at, body)
        })
        .collect()
}

/// Search, grab and import are the owner's, each asked of the title's manager: the
/// interactive search is a read and answers under read access; every act answers 403
/// `singularr.not_allowed` under read access with nothing sent, goes in its one form under write
/// access, and is journaled with the title and the release. A second search of a title while
/// one runs answers 409. A download the manager refused carries the refusal in Singularr's words.
#[tokio::test]
async fn the_managers_are_asked_by_the_owner_only_with_write_access() {
    let Some(t) = TestDb::new("acquire").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::outbound::Access;
    let journal = Journal::new();
    let recorded = Recorded::new();
    ledger_catalog(&t.pool, &journal, &recorded).await;
    let damsel = title_with_tmdb(&t.pool, 763215).await;
    // What the managers answer.
    recorded.answer(
        Method::Get,
        &format!("{RADARR}/api/v3/release"),
        200,
        &fixture("radarr_release.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{SONARR}/api/v3/release"),
        200,
        &fixture("sonarr_release.json"),
    );
    recorded.answer(
        Method::Get,
        &format!("{SONARR}/api/v3/episode"),
        200,
        r#"[{"id": 16846, "seriesId": 7, "seasonNumber": 1, "episodeNumber": 1, "hasFile": true, "monitored": true},
            {"id": 16847, "seriesId": 7, "seasonNumber": 1, "episodeNumber": 2, "hasFile": false, "monitored": true}]"#,
    );
    let queue = format!(
        r#"{{"page": 1, "pageSize": 500, "totalRecords": 1, "records": {}}}"#,
        fixture("radarr_queue_import_refused.json")
    );
    recorded.answer(Method::Get, &format!("{RADARR}/api/v3/queue"), 200, &queue);
    recorded.answer(
        Method::Get,
        &format!("{RADARR}/api/v3/manualimport"),
        200,
        &fixture("radarr_manualimport.json"),
    );
    recorded.answer(
        Method::Post,
        &format!("{RADARR}/api/v3/release"),
        200,
        r#"{"guid": "g"}"#,
    );
    recorded.answer(
        Method::Post,
        &format!("{RADARR}/api/v3/command"),
        201,
        r#"{"id": 501, "status": "queued"}"#,
    );
    recorded.answer(
        Method::Post,
        &format!("{SONARR}/api/v3/command"),
        201,
        r#"{"id": 601, "status": "queued"}"#,
    );
    recorded.answer(Method::Delete, &format!("{RADARR}/api/v3/queue/"), 200, "");

    // The refused download, as the downloads pass stores it: Radarr's, for Damsel, stuck.
    let message = "Not a Custom Format upgrade for existing movie file(s). New: [5.1 Surround, Bluray, DD+, Open Matte, x264] (300) do not improve on Existing: [5.1 Surround, HUSH SHH, Opus, x265 (HD)] (600)";
    let torrent = singularr::engines::ClientDownload {
        id: REFUSED_HASH.to_ascii_lowercase(),
        download_id: REFUSED_HASH.to_string(),
        protocol: singularr::engines::Protocol::Torrent,
        name: "The.Devil.Wears.Prada.2006.Open.Matte.BRRip.1080p.x264.DD5.1.gerald99".to_string(),
        category: Some("radarr".to_string()),
        status: singularr::engines::DownloadStatus::Seeding,
        state: "uploading".to_string(),
        progress: 1.0,
        size_bytes: 7_911_404_354,
        downloaded_bytes: None,
        added_at: None,
        completed_at: None,
        save_path: Some("/downloads".to_string()),
        speed: None,
        upload_speed: None,
        eta: None,
        tracker: None,
        seeds: None,
        peers: None,
        ratio: None,
        seeding_seconds: 60,
    };
    let mut claims = Claims::default();
    claims.queues.insert(
        "radarr".to_string(),
        vec![singularr::engines::QueueItem {
            queue_id: 1374604330,
            engine_title_id: Some(500),
            title: torrent.name.clone(),
            download_id: Some(REFUSED_HASH.to_string()),
            status: Some("completed".to_string()),
            tracked_state: Some("importPending".to_string()),
            tracked_status: Some("warning".to_string()),
            messages: vec![message.to_string()],
            indexer: Some("TorrentDownload (Prowlarr)".to_string()),
        }],
    );
    reconcile(&t.pool, &journal, "qbittorrent", vec![torrent], &claims)
        .await
        .unwrap();

    let out = acting_outbound(recorded.clone());
    let st = state_over(
        &t.pool,
        journal.clone(),
        &[("RADARR_API_KEY", "k"), ("SONARR_API_KEY", "k")],
        &out,
    );
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let row = format!("q-{}", REFUSED_HASH.to_ascii_lowercase());
    let grab = json!({"guid": "https://indexer.example.org/guid/9af2921d9d47", "indexerId": 17});
    let acts: Vec<(&str, String, Option<String>)> = vec![
        (
            "POST",
            "/api/v1/titles/movie-763215/grab".to_string(),
            Some(grab.to_string()),
        ),
        (
            "POST",
            "/api/v1/titles/movie-763215/search".to_string(),
            Some("{}".to_string()),
        ),
        (
            "POST",
            "/api/v1/titles/show-82452/search".to_string(),
            Some(json!({"season": 1, "episodes": [1]}).to_string()),
        ),
        ("POST", format!("/api/v1/activity/queue/{row}/import"), None),
        (
            "DELETE",
            format!("/api/v1/activity/queue/{row}?blocklist=true"),
            None,
        ),
        (
            "POST",
            "/api/v1/search".to_string(),
            Some(json!({"items": [{"title": "movie-763215"}]}).to_string()),
        ),
    ];

    // The refusal reads in Singularr's words on the queue row and in the bell, the manager's
    // sentence beside it.
    let queued = get_ok(&app, "/api/v1/activity/queue").await;
    let refused = queued["queue"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["id"] == row)
        .cloned()
        .expect("the refused download is a queue row");
    assert_eq!(
        refused["refusal"],
        json!({
            "by": "Radarr",
            "reason": "Not a custom format upgrade",
            "new": {"score": 300, "formats": ["5.1 Surround", "Bluray", "DD+", "Open Matte", "x264"]},
            "held": {"score": 600, "formats": ["5.1 Surround", "HUSH SHH", "Opus", "x265 (HD)"]}
        })
    );
    assert_eq!(
        refused["attention"]["reason"],
        "Radarr refused the import \u{b7} 300 against 600"
    );
    assert_eq!(refused["attention"]["message"], message);
    let ws = get_ok(&app, "/api/v1/workspace").await;
    let bell = ws["attention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == row)
        .cloned()
        .expect("the refused download is in the bell");
    assert_eq!(
        bell["detail"],
        "Radarr refused the import \u{b7} 300 against 600"
    );

    // Read access: every act says so, and nothing at all is asked of a manager.
    let before = recorded.calls_sent().len();
    for (method, path, body) in &acts {
        let (status, content_type, problem) =
            call(&app, method, path, &session, body.as_deref()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {problem}");
        assert!(content_type.starts_with("application/problem+json"));
        assert_eq!(problem["code"], "singularr.not_allowed", "{path}");
        assert!(
            problem["detail"]
                .as_str()
                .unwrap()
                .contains(" is read-only on "),
            "{problem}"
        );
    }
    assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");

    // A key, whatever its scopes, asks nothing: only the owner does, the search included.
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let admin = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "everything".into(),
            scopes: vec![Scope::Admin],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", admin.secret);
    let with_key: [(&str, &str); 1] = [("authorization", bearer.as_str())];
    let (status, _, problem) = call(
        &app,
        "GET",
        "/api/v1/titles/movie-763215/releases",
        &with_key,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{problem}");
    assert_eq!(problem["code"], "singularr.forbidden");
    for (method, path, body) in &acts {
        let (status, _, problem) = call(&app, method, path, &with_key, body.as_deref()).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {problem}");
        assert_ne!(problem["code"], "singularr.not_allowed", "{path}");
    }
    assert_eq!(recorded.calls_sent().len(), before, "nothing was sent");

    // The interactive search is a read: it answers under read access, with both verdicts.
    let list = get_ok(&app, "/api/v1/titles/movie-763215/releases").await;
    assert_eq!(list["releases"].as_array().unwrap().len(), 205);
    assert_eq!(
        (&list["manager"]["id"], &list["manager"]["access"]),
        (&json!("radarr"), &json!("read"))
    );
    assert_eq!(list["counts"]["total"], 205);
    assert_eq!(list["counts"]["managerAccepted"], 0);
    assert_eq!(list["profile"]["id"], "movies-hd");
    assert_eq!(
        list["held"].as_array().unwrap().len(),
        1,
        "{}",
        list["held"]
    );
    assert!(
        list["held"][0]["file"]
            .as_str()
            .unwrap()
            .starts_with("Damsel (2024)")
    );
    let first = &list["releases"][0];
    for key in [
        "singularr",
        "manager",
        "guid",
        "indexerId",
        "name",
        "quality",
        "size",
        "age",
        "seeders",
    ] {
        assert!(first.get(key).is_some(), "a release has no `{key}`");
    }
    assert!(list["searched"].is_string() && list["took"].is_number());
    let searched = recorded.calls_sent();
    let asked = searched.last().unwrap();
    assert_eq!(asked.method, Method::Get);
    assert!(
        asked.url.ends_with("/api/v3/release?movieId=500"),
        "{}",
        asked.url
    );
    assert_eq!(asked.timeout, Some(Duration::from_secs(120)));
    // A show is searched by season or episode.
    let (status, _, problem) = call(
        &app,
        "GET",
        "/api/v1/titles/show-82452/releases",
        &session,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    let episode = get_ok(
        &app,
        "/api/v1/titles/show-82452/releases?season=1&episode=1",
    )
    .await;
    assert_eq!(episode["releases"].as_array().unwrap().len(), 110);
    assert_eq!(episode["scope"], json!({"season": 1, "episode": 1}));
    assert!(
        recorded
            .urls_seen()
            .iter()
            .any(|u| u.ends_with("/api/v3/release?episodeId=16846"))
    );
    // While a search of a title runs, a second one waits.
    {
        let _running = st
            .engines
            .searches
            .hold(&[format!("title:{damsel}")])
            .unwrap();
        let sent = recorded.calls_sent().len();
        let (status, _, problem) = call(
            &app,
            "GET",
            "/api/v1/titles/movie-763215/releases",
            &session,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{problem}");
        assert_eq!(problem["code"], "singularr.busy");
        assert_eq!(recorded.calls_sent().len(), sent, "nothing was sent");
    }
    let after_reads = core_store::latest_event_id(&t.pool).await.unwrap();
    assert!(
        acts_sent(&recorded).is_empty(),
        "no act left while the access was read"
    );

    // Write access to both managers.
    for manager in ["radarr", "sonarr"] {
        st.engines
            .access
            .set(
                &t.pool,
                &journal,
                "user:owner@example.com",
                manager,
                Access::Act,
            )
            .await
            .unwrap();
    }
    let after_access = core_store::latest_event_id(&t.pool).await.unwrap();
    assert!(after_access > after_reads);

    // A release the last search did not list is not grabbed; one it listed is.
    let unknown = json!({"guid": "https://indexer.example.org/guid/unknown", "indexerId": 17});
    let (status, _, problem) = call(
        &app,
        "POST",
        "/api/v1/titles/movie-763215/grab",
        &session,
        Some(&unknown.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{problem}");
    assert!(acts_sent(&recorded).is_empty());
    let mut answers = Vec::new();
    for (method, path, body) in &acts {
        let (status, _, answer) = call(&app, method, path, &session, body.as_deref()).await;
        assert_eq!(status, StatusCode::OK, "{method} {path}: {answer}");
        answers.push(answer);
    }
    assert_eq!(answers[0]["act"], "grab");
    assert_eq!(
        answers[0]["detail"],
        "The Devil Wears Prada 2006 BluRay 1080p DTS-HD MA 5 1 AVC REMUX-FraMeSToR"
    );
    assert_eq!(
        (&answers[1]["detail"], &answers[1]["command"]),
        (&json!("MoviesSearch"), &json!(501))
    );
    assert_eq!(answers[2]["detail"], "EpisodeSearch");
    assert_eq!(answers[3]["act"], "import");
    assert_eq!(answers[4]["act"], "remove");
    assert_eq!(answers[5]["sent"][0]["detail"], "MoviesSearch");
    // Each in its one form.
    let import = json!({"name": "ManualImport", "importMode": "copy", "files": [{
        "path": "/downloads/The Devil Wears Prada.mkv",
        "movieId": 4806,
        "quality": {
            "quality": {"id": 7, "name": "Bluray-1080p", "source": "bluray", "resolution": 1080, "modifier": "none"},
            "revision": {"version": 1, "real": 0, "isRepack": false}
        },
        "languages": [{"id": 1, "name": "English"}],
        "downloadId": REFUSED_HASH,
        "indexerFlags": 1
    }]});
    let post = |body: serde_json::Value| (Method::Post, "/api/v3/command".to_string(), Some(body));
    assert_eq!(
        acts_sent(&recorded),
        vec![
            (Method::Post, "/api/v3/release".to_string(), Some(grab)),
            post(json!({"name": "MoviesSearch", "movieIds": [500]})),
            post(json!({"name": "EpisodeSearch", "episodeIds": [16846]})),
            post(import),
            (
                Method::Delete,
                "/api/v3/queue/1374604330?removeFromClient=true&blocklist=true".to_string(),
                None
            ),
            post(json!({"name": "MoviesSearch", "movieIds": [500]})),
        ]
    );
    // Each one journaled, naming the title and the release.
    let events = core_store::events_after(&t.pool, after_access, 100)
        .await
        .unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "release_grabbed",
            "search_sent",
            "search_sent",
            "download_import_forced",
            "download_queue_removed",
            "search_sent"
        ]
    );
    let grabbed = &events[0];
    assert_eq!(
        (grabbed.subject_type.as_str(), grabbed.subject_id.clone()),
        ("title", damsel.to_string())
    );
    assert_eq!(grabbed.actor, "user:owner@example.com");
    assert_eq!(grabbed.body["title"], "Damsel");
    assert_eq!(
        grabbed.body["release"],
        "The Devil Wears Prada 2006 BluRay 1080p DTS-HD MA 5 1 AVC REMUX-FraMeSToR"
    );
    assert_eq!(events[2].body["episodes"], json!([1]));
    let forced = &events[3];
    assert_eq!(
        (forced.subject_type.as_str(), forced.subject_id.as_str()),
        ("download", REFUSED_HASH)
    );
    assert_eq!(forced.body["title"], "Damsel");
    assert_eq!(forced.body["mode"], "copy");
    assert_eq!(forced.body["files"], json!(["The Devil Wears Prada.mkv"]));
    assert_eq!(events[4].body["blocklist"], true);
    assert_eq!(events[5].subject_type, "engine");
    assert_eq!(events[5].body["titles"], json!(["Damsel"]));
    // The journal reads them in words.
    let page = get_ok(&app, "/api/v1/journal?kind=release.grabbed").await;
    assert!(page.to_string().contains("Grabbed by Radarr"), "{page}");

    // A second search of a title while one runs answers 409, and nothing is sent.
    {
        let _running = st
            .engines
            .searches
            .hold(&[format!("title:{damsel}")])
            .unwrap();
        let sent = recorded.calls_sent().len();
        for (path, body) in [
            ("/api/v1/titles/movie-763215/search", json!({})),
            (
                "/api/v1/search",
                json!({"items": [{"title": "movie-763215"}]}),
            ),
        ] {
            let (status, _, problem) =
                call(&app, "POST", path, &session, Some(&body.to_string())).await;
            assert_eq!(status, StatusCode::CONFLICT, "{path}: {problem}");
            assert_eq!(problem["code"], "singularr.busy");
        }
        assert_eq!(recorded.calls_sent().len(), sent, "nothing was sent");
    }

    // The wanted list: one command per manager, the episodes by the manager's ids.
    let before = acts_sent(&recorded).len();
    let items = json!({"items": [
        {"title": "movie-763215"}, {"title": "movie-1495"},
        {"title": "show-82452", "season": 1, "episode": 2}
    ]});
    let (status, _, many) = call(
        &app,
        "POST",
        "/api/v1/search",
        &session,
        Some(&items.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{many}");
    assert_eq!(many["count"], 3);
    assert_eq!(
        acts_sent(&recorded)[before..].to_vec(),
        vec![
            post(json!({"name": "MoviesSearch", "movieIds": [500, 501]})),
            post(json!({"name": "EpisodeSearch", "episodeIds": [16847]})),
        ]
    );
    // What cannot be asked: more than 50, none, a show without its episode, a movie's season,
    // a title the library does not hold, an id that is none, a grab without its indexer.
    let too_many: Vec<serde_json::Value> =
        (0..51).map(|_| json!({"title": "movie-763215"})).collect();
    for (path, body, expected) in [
        (
            "/api/v1/search",
            json!({"items": too_many}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/search",
            json!({"items": []}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/search",
            json!({"items": [{"title": "show-82452"}]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/search",
            json!({"items": [{"title": "movie-763215", "season": 1}]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/search",
            json!({"items": [{"title": "movie-1"}]}),
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/v1/titles/movie-763215/search",
            json!({"season": 1}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/titles/show-82452/search",
            json!({"episodes": [1]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/titles/show-82452/search",
            json!({"season": 1, "episodes": [9]}),
            StatusCode::NOT_FOUND,
        ),
        (
            "/api/v1/titles/nothing/search",
            json!({}),
            StatusCode::BAD_REQUEST,
        ),
        (
            "/api/v1/titles/movie-763215/grab",
            json!({"guid": "g"}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let sent = acts_sent(&recorded).len();
        let (status, _, problem) =
            call(&app, "POST", path, &session, Some(&body.to_string())).await;
        assert_eq!(status, expected, "{path} {body}: {problem}");
        assert_eq!(
            acts_sent(&recorded).len(),
            sent,
            "{path} {body}: nothing was sent"
        );
    }
    // The manager says no in its own words.
    recorded.answer(
        Method::Post,
        &format!("{RADARR}/api/v3/command"),
        400,
        r#"[{"propertyName": "MovieIds", "errorMessage": "Movie does not exist"}]"#,
    );
    let (status, _, problem) = call(
        &app,
        "POST",
        "/api/v1/titles/movie-763215/search",
        &session,
        Some("{}"),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{problem}");
    assert_eq!(problem["code"], "singularr.refused");
    assert_eq!(problem["detail"], "Radarr refused: Movie does not exist");
    t.drop_db().await;
}

/// A usenet download keeps its id in its client's case (SABnzbd's `nzo_id`). The import of one
/// its manager refused asks the manual import and sends the command under the id the manager
/// wrote. Its files are moved.
#[tokio::test]
async fn a_refused_usenet_download_is_imported_under_the_id_its_manager_wrote() {
    let Some(t) = TestDb::new("usenet_import").await else {
        return;
    };
    use serde_json::json;
    use singularr::core::outbound::Access;
    const NZO: &str = "SABnzbd_nzo_x7f3k2ab";
    let journal = Journal::new();
    let recorded = Recorded::new();
    let mut records: serde_json::Value =
        serde_json::from_str(&fixture("radarr_queue_import_refused.json")).unwrap();
    records[0]["downloadId"] = json!(NZO);
    records[0]["protocol"] = json!("usenet");
    records[0]["downloadClient"] = json!("SABnzbd");
    let queue = json!({"page": 1, "pageSize": 500, "totalRecords": 1, "records": records});
    recorded.answer(
        Method::Get,
        &format!("{RADARR}/api/v3/queue"),
        200,
        &queue.to_string(),
    );
    // The offer names no download: the import gives its files the queue entry's id.
    let mut offer: serde_json::Value =
        serde_json::from_str(&fixture("radarr_manualimport.json")).unwrap();
    offer[0].as_object_mut().unwrap().remove("downloadId");
    recorded.answer(
        Method::Get,
        &format!("{RADARR}/api/v3/manualimport"),
        200,
        &offer.to_string(),
    );
    recorded.answer(
        Method::Post,
        &format!("{RADARR}/api/v3/command"),
        201,
        r#"{"id": 502, "status": "queued"}"#,
    );

    // The job as the SABnzbd adapter lists it, claimed by Radarr's queue.
    let job = singularr::engines::ClientDownload {
        id: NZO.to_string(),
        download_id: NZO.to_ascii_uppercase(),
        protocol: singularr::engines::Protocol::Usenet,
        name: "The.Devil.Wears.Prada.2006.1080p.BluRay.x264-GRP".to_string(),
        category: Some("movies".to_string()),
        status: singularr::engines::DownloadStatus::Completed,
        state: "Completed".to_string(),
        progress: 1.0,
        size_bytes: 7_911_404_354,
        downloaded_bytes: Some(7_911_404_354),
        added_at: None,
        completed_at: None,
        save_path: Some("/downloads/complete".to_string()),
        speed: None,
        upload_speed: None,
        eta: None,
        tracker: None,
        seeds: None,
        peers: None,
        ratio: None,
        seeding_seconds: 0,
    };
    let mut claims = Claims::default();
    claims.queues.insert(
        "radarr".to_string(),
        vec![singularr::engines::QueueItem {
            queue_id: 1374604330,
            engine_title_id: Some(4806),
            title: job.name.clone(),
            download_id: Some(NZO.to_ascii_uppercase()),
            status: Some("completed".to_string()),
            tracked_state: Some("importPending".to_string()),
            tracked_status: Some("warning".to_string()),
            messages: Vec::new(),
            indexer: None,
        }],
    );
    reconcile(&t.pool, &journal, "sabnzbd", vec![job], &claims)
        .await
        .unwrap();

    let out = acting_outbound(recorded.clone());
    let st = state_over(&t.pool, journal.clone(), &[("RADARR_API_KEY", "k")], &out);
    st.engines
        .access
        .set(
            &t.pool,
            &journal,
            "user:owner@example.com",
            "radarr",
            Access::Act,
        )
        .await
        .unwrap();
    let app = routes::router(st.clone());
    let session: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];
    let path = format!(
        "/api/v1/activity/queue/q-{}/import",
        NZO.to_ascii_lowercase()
    );
    let (status, _, answer) = call(&app, "POST", &path, &session, None).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert_eq!(answer["act"], "import");
    let asked = recorded.urls_seen();
    assert!(
        asked
            .iter()
            .any(|u| u.ends_with("/api/v3/manualimport?downloadId=SABnzbd_nzo_x7f3k2ab")),
        "{asked:?}"
    );
    let acts = acts_sent(&recorded);
    assert_eq!(acts.len(), 1, "{acts:?}");
    let body = acts[0].2.clone().unwrap();
    assert_eq!(
        (&body["importMode"], &body["files"][0]["downloadId"]),
        (&json!("move"), &json!(NZO))
    );
    t.drop_db().await;
}
