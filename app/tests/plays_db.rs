//! Database-backed tests of play statistics: the three sources (the media server's sessions and
//! history, and Tautulli) written as one row per play, and the statistics routes end to end.
//!
//! Like `db.rs`, they run only when `SINGULARR_TEST_DATABASE_URL` names a server the tests may
//! create databases on, and say so and pass otherwise. Each test makes a database of its own
//! and drops it afterwards. The media server and Tautulli are recorded answers
//! (`tests/fixtures`), sent through the real outbound allowlist.
#![cfg(feature = "ssr")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Datelike, Utc};
use serde_json::{Value, json};
use singularr::api::routes;
use singularr::api::state::{AppState, Inner};
use singularr::core::auth::{self, Caller};
use singularr::core::config::Config;
use singularr::core::journal::Journal;
use singularr::core::model::{NewApiKey, Scope};
use singularr::core::outbound::recorded::Recorded;
use singularr::core::outbound::{Method, Outbound};
use singularr::core::{db, store as core_store};
use singularr::engines::service::{EngineSet, allowlist, statuses};
use singularr::homemovies::service::HomeMovies;
use singularr::metadata::service::ProviderSet;
use singularr::music::service::MusicLibrary;
use singularr::plays::model::{PlayRecord, from_history_play, from_server_play};
use singularr::plays::service::{self as plays, PlaysDeps};
use singularr::policy::ActivePolicy;
use singularr::requests::service::Requests;
use singularr::settings::Settings;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const PLEX: &str = "http://10.0.0.30:32400";
const TAUTULLI: &str = "http://10.0.0.50:8181";
const KEY: &str = "f00dfeedf00dfeedf00dfeedf00dfeed";
/// The id Tautulli knows the media server's owner by, in the fixtures.
const OWNER_IN_TAUTULLI: i64 = 16_289_551;

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
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(
            "mq_{test}_{:x}",
            nanos ^ ((std::process::id() as u128) << 64)
        );
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
        db::migrate(&pool).await.expect("the migrations apply");
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

fn env(extra: &[(&'static str, &'static str)]) -> BTreeMap<&'static str, &'static str> {
    let mut env = BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("SINGULARR_OWNER_EMAILS", "owner@example.com"),
        ("PLEX_URL", PLEX),
        ("PLEX_TOKEN", "plex-token"),
        ("TAUTULLI_URL", TAUTULLI),
        ("TAUTULLI_API_KEY", KEY),
    ]);
    env.extend(extra.iter().copied());
    env
}

/// The plays passes over a test database, the media server and Tautulli answering from
/// `recorded`.
fn make_deps(pool: &PgPool, recorded: &Arc<Recorded>) -> PlaysDeps {
    let env = env(&[]);
    let config = Config::from(&env).unwrap();
    let out = Outbound::with_transport(allowlist(&config).unwrap(), recorded.clone());
    PlaysDeps::new(
        pool.clone(),
        Journal::new(),
        Arc::new(EngineSet::from_config(&config, &out)),
        Arc::new(Settings::new(&config, &env, None)),
        Duration::ZERO,
    )
}

fn record_server(recorded: &Recorded) {
    let at = |path: &str| format!("{PLEX}{path}");
    recorded.answer(
        Method::Get,
        &at("/status/sessions/history/all"),
        200,
        &fixture("plex_history.json"),
    );
    recorded.answer(
        Method::Get,
        &at("/accounts"),
        200,
        &fixture("plex_accounts.json"),
    );
    recorded.answer(
        Method::Get,
        &at("/devices"),
        200,
        &fixture("plex_devices.json"),
    );
    recorded.answer(
        Method::Get,
        &at("/status/sessions"),
        200,
        &fixture("plex_sessions_idle.json"),
    );
}

fn record_tautulli(recorded: &Recorded) {
    recorded.answer(
        Method::Get,
        "cmd=get_tautulli_info",
        200,
        "{\"response\": {\"result\": \"success\", \"message\": null, \"data\": {\"tautulli_version\": \"v2.15.2\"}}}",
    );
    recorded.answer(
        Method::Get,
        "cmd=get_users",
        200,
        &fixture("tautulli_users.json"),
    );
    recorded.answer(
        Method::Get,
        "cmd=get_history",
        200,
        &fixture("tautulli_history.json"),
    );
}

async fn count(pool: &PgPool) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM plays")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// One play by a source's id: (history id, Tautulli id, account, name, started, watched,
/// decision, local, player).
type Row = (
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    i64,
    Option<i32>,
    Option<String>,
    Option<bool>,
    Option<String>,
);

async fn by_history(pool: &PgPool, id: i64) -> Row {
    sqlx::query_as(
        "SELECT history_id, tautulli_id, account_id, account_name,
                EXTRACT(EPOCH FROM started_at)::bigint, watched_secs, decision, local, player
         FROM plays WHERE history_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn by_tautulli(pool: &PgPool, id: i64) -> Row {
    sqlx::query_as(
        "SELECT history_id, tautulli_id, account_id, account_name,
                EXTRACT(EPOCH FROM started_at)::bigint, watched_secs, decision, local, player
         FROM plays WHERE tautulli_id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn the_server_history_and_tautulli_make_one_row_per_play() {
    let Some(t) = TestDb::new("plays_sources").await else {
        return;
    };
    let recorded = Recorded::new();
    record_server(&recorded);
    record_tautulli(&recorded);
    let deps = make_deps(&t.pool, &recorded);
    let server = deps.engines.media_server().cloned().unwrap();
    let history = deps.engines.play_history.clone().unwrap();

    // The media server's history: every entry a play, without durations or delivery.
    let said = plays::server_history_pass(&deps, &server).await.unwrap();
    assert_eq!(said, "40 entries, 40 new plays");
    assert_eq!(count(&t.pool).await, 40);
    let odyssey = by_history(&t.pool, 8692).await;
    assert_eq!(
        (odyssey.2, odyssey.4, odyssey.5, odyssey.6.clone()),
        (Some(108_218_570), 1_795_488_084, None, None)
    );
    // The name is the media server's, the player its device's.
    assert!(odyssey.3.is_some());
    assert!(odyssey.8.is_some());
    // Read again, nothing is added: the newest entry read is kept, and an entry read anew is
    // the play it was.
    assert_eq!(
        plays::server_history_pass(&deps, &server).await.unwrap(),
        "No new entries"
    );
    sqlx::query("DELETE FROM play_sources")
        .execute(&t.pool)
        .await
        .unwrap();
    plays::server_history_pass(&deps, &server).await.unwrap();
    assert_eq!(count(&t.pool).await, 40);

    // Tautulli: 27 of its 40 plays are plays the server's history knows, and stay one row
    // each; 13 are plays the history does not list (not finished, or older).
    let ok = plays::tautulli_pass(&deps, &history, true).await.unwrap();
    assert_eq!(ok.version.as_deref(), Some("v2.15.2"));
    assert_eq!(count(&t.pool).await, 53);
    assert_eq!(ok.counts["plays"], 40);
    assert_eq!(ok.counts["new"], 13);
    // One play known to both: Tautulli's times and delivery, the server's name for the person.
    let both = by_history(&t.pool, 8692).await;
    assert_eq!(
        (both.1, both.4, both.5, both.6.as_deref(), both.7),
        (
            Some(6679),
            1_795_395_674,
            Some(10_143),
            Some("direct play"),
            Some(false)
        )
    );
    assert_eq!(both.3, odyssey.3, "the server's name stays");
    // The owner as Tautulli names them is the server's owner account, and their play is one.
    let owner = by_tautulli(&t.pool, 6671).await;
    assert_eq!((owner.0, owner.2), (Some(8690), Some(1)));
    // A play Tautulli alone knows: not finished, so the server never marked it played.
    let open = by_tautulli(&t.pool, 6680).await;
    assert_eq!((open.0, open.6.as_deref()), (None, Some("transcode")));
    // No address is kept: the setting is off by default.
    let kept: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM plays WHERE address IS NOT NULL OR raw #> '{tautulli,ip_address}' IS NOT NULL",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(kept, 0);
    // The import run again adds nothing and changes nothing.
    let again = plays::tautulli_pass(&deps, &history, true).await.unwrap();
    assert_eq!(again.counts["new"], 0);
    assert_eq!(count(&t.pool).await, 53);
    // An ordinary pass reads the newest days only.
    plays::tautulli_pass(&deps, &history, false).await.unwrap();
    assert!(
        recorded
            .urls_seen()
            .iter()
            .any(|u| u.contains("cmd=get_history") && u.contains("after=2026-11-"))
    );
    assert_eq!(count(&t.pool).await, 53);
    // Toward both, GET alone; Tautulli's key travels only in its own calls.
    assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
    assert!(
        recorded
            .urls_seen()
            .iter()
            .all(|u| u.starts_with(TAUTULLI) || !u.contains(KEY))
    );
    t.drop_db().await;
}

/// A session of the media server, playing or paused, `offset` seconds in.
fn session(state: &str, offset: i64) -> String {
    serde_json::json!({"MediaContainer": {"size": 1, "Metadata": [{
        "type": "movie", "title": "National Treasure: Book of Secrets", "ratingKey": "72171",
        "year": 2007, "librarySectionID": "1", "sessionKey": "12", "duration": 7_480_000,
        "viewOffset": offset * 1000,
        "User": {"id": "1", "title": "person-27"},
        "Player": {"title": "device-446", "state": state, "device": "SHIELD Android TV",
                   "platform": "Android", "product": "Plex for Android (TV)", "local": false,
                   "address": "10.0.0.9", "remotePublicAddress": "198.51.100.98"},
        "Session": {"id": "abc-123", "bandwidth": 20000, "location": "wan"},
        "Media": [{"videoResolution": "1080", "bitrate": 18000, "videoCodec": "h264",
                   "audioCodec": "eac3", "selected": true}]
    }]}})
    .to_string()
}

#[tokio::test]
async fn a_session_is_one_play_from_start_through_a_pause_to_its_end() {
    let Some(t) = TestDb::new("plays_live").await else {
        return;
    };
    let recorded = Recorded::new();
    record_server(&recorded);
    let sessions = format!("{PLEX}/status/sessions");
    recorded.answer(Method::Get, &sessions, 200, &session("playing", 60));
    let deps = make_deps(&t.pool, &recorded);
    let server = deps.engines.media_server().cloned().unwrap();
    let mut follower = singularr::plays::model::Follower::default();
    let mut files = std::collections::HashMap::new();

    assert_eq!(
        plays::live_pass(&deps, &server, &mut follower, &mut files)
            .await
            .unwrap(),
        1
    );
    type Live = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<bool>,
        Option<String>,
        bool,
        Option<f64>,
    );
    let read = || async {
        sqlx::query_as::<_, Live>(
            "SELECT session_id, decision, source_resolution, local, address,
                    stopped_at IS NOT NULL, progress
             FROM plays",
        )
        .fetch_all(&t.pool)
        .await
        .unwrap()
    };
    let rows = read().await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].0.as_deref(),
            rows[0].1.as_deref(),
            rows[0].2.as_deref(),
            rows[0].3,
            rows[0].4.clone(),
            rows[0].5
        ),
        (
            Some("abc-123"),
            Some("direct play"),
            Some("1080p"),
            Some(false),
            None,
            false
        )
    );
    // The sessions route says how the stream is delivered, and what it takes.
    let routes = app(&t.pool, &deps.journal, &recorded);
    let now = owner_get(&routes, "/api/v1/server/sessions").await;
    let first = &now["sessions"][0];
    assert_eq!(first["delivery"]["video"], "direct play");
    assert_eq!(first["delivery"]["stream"]["resolution"], "1080p");
    assert_eq!(
        (first["bandwidth"].clone(), first["network"].clone()),
        (json!(20000), json!("outside"))
    );
    // Paused, further in: the same play.
    recorded.answer(Method::Get, &sessions, 200, &session("paused", 600));
    plays::live_pass(&deps, &server, &mut follower, &mut files)
        .await
        .unwrap();
    let rows = read().await;
    assert_eq!((rows.len(), rows[0].5), (1, false));
    assert!(rows[0].6.unwrap() > 7.0);
    // Gone: the play ended.
    recorded.answer(
        Method::Get,
        &sessions,
        200,
        &fixture("plex_sessions_idle.json"),
    );
    assert_eq!(
        plays::live_pass(&deps, &server, &mut follower, &mut files)
            .await
            .unwrap(),
        0
    );
    let rows = read().await;
    assert_eq!((rows.len(), rows[0].5), (1, true));
    // The session's own record is kept, without the player's addresses.
    let raw: serde_json::Value = sqlx::query_scalar("SELECT raw FROM plays")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(raw["live"]["Session"]["id"], "abc-123");
    assert!(raw["live"]["Player"].get("address").is_none());
    assert!(raw["live"]["Player"].get("remotePublicAddress").is_none());

    // Tautulli tells of the same sitting later: still one play, with its times.
    let (start, stop): (i64, i64) = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM started_at)::bigint, EXTRACT(EPOCH FROM stopped_at)::bigint
         FROM plays",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let row = serde_json::json!({
        "id": 9001, "reference_id": 9001, "user_id": OWNER_IN_TAUTULLI, "friendly_name": "owner",
        "rating_key": 72171, "media_type": "movie", "title": "National Treasure: Book of Secrets",
        "started": start - 20, "stopped": stop + 20, "play_duration": 540, "paused_counter": 40,
        "percent_complete": 8, "player": "device-446", "platform": "Android",
        "transcode_decision": "direct play", "location": "wan", "ip_address": "198.51.100.98"
    });
    let play = singularr::engines::tautulli::history_play(&row).unwrap();
    let done = plays::absorb_all(
        &deps,
        vec![from_history_play("plex", &play, Some(OWNER_IN_TAUTULLI))],
    )
    .await
    .unwrap();
    assert_eq!((done.new, done.changed), (0, 1));
    assert_eq!(count(&t.pool).await, 1);
    let merged = by_tautulli(&t.pool, 9001).await;
    assert_eq!((merged.4, merged.5), (start - 20, Some(540)));
    t.drop_db().await;
}

#[tokio::test]
async fn the_tautulli_key_is_never_in_an_error_or_the_journal() {
    let Some(t) = TestDb::new("plays_key").await else {
        return;
    };
    let recorded = Recorded::new();
    recorded.answer(
        Method::Get,
        "cmd=get_tautulli_info",
        200,
        &format!(
            "{{\"response\": {{\"result\": \"error\", \"message\": \"Invalid apikey {KEY}\", \"data\": {{}}}}}}"
        ),
    );
    let deps = make_deps(&t.pool, &recorded);
    let history = deps.engines.play_history.clone().unwrap();
    let outcome = plays::tautulli_pass(&deps, &history, true).await;
    let error = outcome.clone().unwrap_err();
    assert!(!error.contains(KEY), "{error}");
    deps.engines
        .report(&t.pool, &deps.journal, history.id(), "plays", outcome)
        .await
        .unwrap();
    // A failing answer, then no answer at all.
    let failing = Recorded::new();
    failing.answer(Method::Get, "cmd=get_tautulli_info", 502, "bad gateway");
    let other = make_deps(&t.pool, &failing);
    let other_history = other.engines.play_history.clone().unwrap();
    let again = plays::tautulli_pass(&other, &other_history, true).await;
    assert!(!again.clone().unwrap_err().contains(KEY));
    deps.engines
        .report(&t.pool, &deps.journal, history.id(), "plays", again)
        .await
        .unwrap();
    let events = core_store::events_after(&t.pool, 0, 1000).await.unwrap();
    assert!(events.iter().any(|e| e.kind == "engine_down"));
    for e in &events {
        let text = serde_json::to_string(e).unwrap();
        assert!(!text.contains(KEY), "{text}");
    }
    let state = statuses(&t.pool, &deps.engines).await.unwrap();
    let tautulli = state.iter().find(|s| s.engine == "tautulli").unwrap();
    let last = tautulli.last_error.clone().unwrap_or_default();
    assert!(!last.is_empty() && !last.contains(KEY), "{last}");
    t.drop_db().await;
}

// The routes ---------------------------------------------------------------------------------------

const OWNER: (&str, &str) = ("x-auth-request-email", "owner@example.com");

/// The application state over a test database, its integrations answering from `recorded`.
fn app(pool: &PgPool, journal: &Journal, recorded: &Arc<Recorded>) -> axum::Router {
    let env = env(&[("SINGULARR_TMDB_TOKEN", "tmdb-token")]);
    let config = Config::from(&env).unwrap();
    let out = Outbound::with_transport(allowlist(&config).unwrap(), recorded.clone());
    let settings = Arc::new(Settings::new(&config, &env, None));
    routes::router(AppState::new(Inner {
        engines: Arc::new(EngineSet::from_config(&config, &out)),
        metadata: Arc::new(ProviderSet::from_config(&config, &out).with_locale(settings.locale())),
        owners: singularr::core::identity::Allowlist::parse(&config.owner_emails),
        requests: Arc::new(Requests::new(&config.request_rules)),
        music: Arc::new(MusicLibrary::new(&config.music.root)),
        home_movies: Arc::new(HomeMovies::new(&config.home_movies.root)),
        settings,
        pages: Default::default(),
        search: Arc::new(singularr::search::Search::new(pool.clone(), &config, &out)),
        notify: singularr::notify::service::Notifier::new(
            pool.clone(),
            journal.clone(),
            out.clone(),
            singularr::core::smtp::recorded::Recorded::new(),
        ),
        config,
        pool: pool.clone(),
        journal: journal.clone(),
        policy: singularr::policy::PolicyCell::handle(ActivePolicy::builtin()),
    }))
}

async fn get(app: &axum::Router, path: &str, headers: &[(&str, &str)]) -> (StatusCode, Value) {
    let mut req = Request::builder().method("GET").uri(path);
    for (n, v) in headers {
        req = req.header(*n, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn owner_get(app: &axum::Router, path: &str) -> Value {
    let (status, body) = get(app, path, &[OWNER]).await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

/// A play as Tautulli would tell it: `minutes` long from `start`, every minute watched.
#[allow(clippy::too_many_arguments)]
fn sitting(
    id: i64,
    account: i64,
    name: &str,
    item: (&str, &str, &str),
    show: Option<(&str, &str, i32, i32)>,
    start: DateTime<Utc>,
    minutes: i64,
    how: (&str, &str, &str, &str),
) -> PlayRecord {
    let (kind, key, title) = item;
    let (decision, location, platform, player) = how;
    let mut row = json!({
        "id": id, "reference_id": id, "user_id": account, "friendly_name": name,
        "rating_key": key, "media_type": kind, "title": title,
        "started": start.timestamp(), "stopped": start.timestamp() + minutes * 60,
        "play_duration": minutes * 60, "paused_counter": 0, "percent_complete": 90,
        "player": player, "platform": platform, "transcode_decision": decision,
        "location": location
    });
    if let Some((show_title, show_key, season, number)) = show {
        row["grandparent_title"] = json!(show_title);
        row["grandparent_rating_key"] = json!(show_key);
        row["parent_media_index"] = json!(season);
        row["media_index"] = json!(number);
    }
    from_history_play(
        "plex",
        &singularr::engines::tautulli::history_play(&row).unwrap(),
        None,
    )
}

/// Six plays: two people on one movie at once yesterday, two episodes of a show three days
/// ago, the movie in the server's history alone ten days ago, and a movie forty days ago.
async fn known_plays(deps: &PlaysDeps) -> DateTime<Utc> {
    let today = Utc::now().date_naive();
    let at = |days: i64, hour: u32| {
        (today - chrono::Duration::days(days))
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
    };
    let movie = ("movie", "100", "Movie A");
    let entry = json!({
        "historyKey": "/status/sessions/history/77", "ratingKey": "100", "type": "movie",
        "title": "Movie A", "viewedAt": at(10, 12).timestamp(), "accountID": 1
    });
    let names = HashMap::from([(1, "owner".to_string())]);
    let records = vec![
        sitting(
            1,
            1,
            "owner",
            movie,
            None,
            at(1, 20),
            120,
            ("direct play", "lan", "Android", "TV"),
        ),
        sitting(
            2,
            2,
            "friend",
            movie,
            None,
            at(1, 21),
            120,
            ("transcode", "wan", "Roku", "Roku TV"),
        ),
        sitting(
            3,
            2,
            "friend",
            ("episode", "201", "Episode 1"),
            Some(("Show B", "200", 1, 1)),
            at(3, 10),
            45,
            ("copy", "wan", "Roku", "Roku TV"),
        ),
        sitting(
            4,
            2,
            "friend",
            ("episode", "202", "Episode 2"),
            Some(("Show B", "200", 1, 2)),
            at(3, 11),
            45,
            ("transcode", "wan", "Roku", "Roku TV"),
        ),
        sitting(
            5,
            3,
            "guest",
            ("movie", "300", "Movie C"),
            None,
            at(40, 12),
            60,
            ("direct play", "lan", "Android", "Phone"),
        ),
        from_server_play(
            "plex",
            &singularr::engines::plex::server_play(&entry).unwrap(),
            &names,
            &HashMap::new(),
        ),
    ];
    let done = plays::absorb_all(deps, records).await.unwrap();
    assert_eq!(done.new, 6);
    at(1, 21)
}

#[tokio::test]
async fn the_overview_adds_up_a_known_set_of_plays() {
    let Some(t) = TestDb::new("plays_overview").await else {
        return;
    };
    let recorded = Recorded::new();
    let deps = make_deps(&t.pool, &recorded);
    let both = known_plays(&deps).await;
    let app = app(&t.pool, &deps.journal, &recorded);

    let o = owner_get(&app, "/api/v1/stats/overview?days=30").await;
    assert_eq!(o["range"]["days"], 30);
    assert_eq!(o["range"]["timeZone"], "UTC");
    assert_eq!(
        o["totals"],
        json!({"plays": 5, "hours": 5.5, "people": 2, "titles": 2})
    );
    assert_eq!(
        o["delivery"],
        json!({"directPlay": 1, "directStream": 1, "transcode": 2, "other": 1})
    );
    assert_eq!(o["network"], json!({"home": 1, "outside": 3, "other": 1}));
    // Two people on the movie at once, from the second one's start.
    assert_eq!(o["concurrent"]["streams"], 2);
    assert_eq!(
        o["concurrent"]["at"].as_str().unwrap()[..16],
        both.to_rfc3339()[..16]
    );
    // One bar a day, the days without plays at zero.
    let points = o["series"]["points"].as_array().unwrap();
    assert_eq!(
        (o["series"]["bucket"].as_str(), points.len()),
        (Some("day"), 30)
    );
    let day = |n: i64| {
        let d = (Utc::now().date_naive() - chrono::Duration::days(n)).to_string();
        points
            .iter()
            .find(|p| p["at"] == d.as_str())
            .unwrap()
            .clone()
    };
    assert_eq!(
        (
            day(1)["plays"].clone(),
            day(1)["directPlay"].clone(),
            day(1)["transcode"].clone()
        ),
        (json!(2), json!(1), json!(1))
    );
    assert_eq!(day(1)["hours"], 4.0);
    assert_eq!(
        (day(3)["directStream"].clone(), day(10)["other"].clone()),
        (json!(1), json!(1))
    );
    assert_eq!(day(0)["plays"], 0);
    // By hour and weekday.
    let hours = o["hours"].as_array().unwrap();
    assert_eq!(
        (hours[20].clone(), hours[21].clone(), hours[10].clone()),
        (json!(1), json!(1), json!(1))
    );
    let weekday = (Utc::now().date_naive() - chrono::Duration::days(1))
        .weekday()
        .num_days_from_monday() as usize;
    assert_eq!(o["grid"][weekday][20], 1);
    assert_eq!(
        o["weekdays"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_i64().unwrap())
            .sum::<i64>(),
        5
    );
    // The most played.
    assert_eq!(o["top"]["movies"][0]["title"]["title"], "Movie A");
    assert_eq!(
        (
            o["top"]["movies"][0]["plays"].clone(),
            o["top"]["movies"][0]["people"].clone()
        ),
        (json!(3), json!(2))
    );
    assert_eq!(o["top"]["movies"][0]["title"]["kind"], "movie");
    assert_eq!(o["top"]["shows"][0]["title"]["title"], "Show B");
    assert_eq!(o["top"]["shows"][0]["title"]["kind"], "show");
    assert_eq!(o["top"]["shows"][0]["plays"], 2);
    assert_eq!(o["top"]["people"][0]["id"], 2);
    assert_eq!(o["top"]["people"][0]["plays"], 3);
    assert_eq!(o["top"]["platforms"][0]["name"], "Roku");
    assert_eq!(
        (
            o["top"]["platforms"][0]["plays"].clone(),
            o["top"]["platforms"][0]["hours"].clone()
        ),
        (json!(3), json!(3.5))
    );
    assert_eq!(o["top"]["players"][0]["name"], "Roku TV");
    assert_eq!(o["sources"][0]["kind"], "server");
    assert_eq!(o["sources"][1]["kind"], "history");
    // Every day, one kind, one person.
    let all = owner_get(&app, "/api/v1/stats/overview?days=all").await;
    assert_eq!(
        (all["totals"]["plays"].clone(), all["range"]["days"].clone()),
        (json!(6), Value::Null)
    );
    assert_eq!(all["series"]["points"].as_array().unwrap().len(), 41);
    let shows = owner_get(&app, "/api/v1/stats/overview?days=30&kind=show").await;
    assert_eq!(shows["totals"]["plays"], 2);
    assert!(shows["top"]["movies"].as_array().unwrap().is_empty());
    // A library the plays name no section of, by its id: none of them.
    let library = owner_get(&app, "/api/v1/stats/overview?days=all&library=plex-9").await;
    assert_eq!(library["totals"]["plays"], 0);
    let owner = owner_get(&app, "/api/v1/stats/overview?days=30&person=1").await;
    assert_eq!(owner["totals"]["plays"], 2);
    let (status, body) = get(&app, "/api/v1/stats/overview?days=week", &[OWNER]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    t.drop_db().await;
}

#[tokio::test]
async fn the_history_pages_and_filters_and_the_people_answer() {
    let Some(t) = TestDb::new("plays_history").await else {
        return;
    };
    let recorded = Recorded::new();
    let deps = make_deps(&t.pool, &recorded);
    known_plays(&deps).await;
    let app = app(&t.pool, &deps.journal, &recorded);

    let titles = |page: &Value| -> Vec<String> {
        page["plays"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                format!(
                    "{}{}",
                    p["title"]["title"].as_str().unwrap(),
                    p["episode"]["number"]
                        .as_i64()
                        .map(|n| format!(" {n}"))
                        .unwrap_or_default()
                )
            })
            .collect()
    };
    let first = owner_get(&app, "/api/v1/stats/plays?limit=2").await;
    assert_eq!(
        (first["total"].clone(), first["limit"].clone()),
        (json!(6), json!(2))
    );
    assert_eq!(titles(&first), vec!["Movie A", "Movie A"]);
    assert_eq!(
        first["plays"][0]["person"],
        json!({"id": 2, "name": "friend"})
    );
    assert_eq!(first["plays"][0]["network"], "outside");
    assert_eq!(first["plays"][0]["delivery"]["decision"], "transcode");
    assert_eq!(first["plays"][0]["sources"], json!(["tautulli"]));
    assert_eq!(first["plays"][0]["watchedSeconds"], 7200);
    assert_eq!(first["plays"][0]["playing"], false);
    assert!(first["plays"][0]["title"]["id"].is_null());
    let second = owner_get(
        &app,
        &format!("/api/v1/stats/plays?limit=2&before={}", first["next"]),
    )
    .await;
    assert_eq!(titles(&second), vec!["Show B 2", "Show B 1"]);
    assert_eq!(
        second["plays"][0]["episode"],
        json!({"season": 1, "number": 2, "title": "Episode 2"})
    );
    assert_eq!(second["plays"][0]["title"]["kind"], "show");
    let third = owner_get(
        &app,
        &format!("/api/v1/stats/plays?limit=2&before={}", second["next"]),
    )
    .await;
    assert_eq!(titles(&third), vec!["Movie A", "Movie C"]);
    assert!(third["next"].is_null());
    assert_eq!(third["plays"][0]["sources"], json!(["server"]));
    for (query, total) in [
        ("person=2", 3),
        ("kind=show", 2),
        ("kind=movie", 4),
        ("decision=transcode", 2),
        ("decision=direct-stream", 1),
        ("platform=Roku", 3),
        ("days=30", 5),
        ("days=30&person=1", 2),
        ("title=movie-999", 0),
    ] {
        let page = owner_get(&app, &format!("/api/v1/stats/plays?{query}")).await;
        assert_eq!(page["total"], total, "{query}");
        assert_eq!(
            page["plays"].as_array().unwrap().len() as i64,
            total,
            "{query}"
        );
    }
    let (status, body) = get(&app, "/api/v1/stats/plays?decision=sideways", &[OWNER]).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("decision"),
        "{body}"
    );

    // The people, the most active first, and one of them.
    let people = owner_get(&app, "/api/v1/stats/people?days=all").await;
    let ids: Vec<i64> = people["people"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, vec![2, 1, 3]);
    assert_eq!(people["people"][0]["name"], "friend");
    let friend = owner_get(&app, "/api/v1/stats/people/2?days=30").await;
    assert_eq!(
        (
            friend["person"]["plays"].clone(),
            friend["person"]["hours"].clone()
        ),
        (json!(3), json!(3.5))
    );
    assert_eq!(friend["top"][0]["title"]["title"], "Show B");
    assert_eq!(friend["devices"][0]["name"], "Roku TV");
    assert_eq!(friend["devices"][0]["plays"], 3);
    assert_eq!(friend["recent"].as_array().unwrap().len(), 3);
    assert_eq!(friend["delivery"]["transcode"], 2);
    let (status, body) = get(&app, "/api/v1/stats/people/99", &[OWNER]).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    t.drop_db().await;
}

#[tokio::test]
async fn statistics_are_the_owner_s_and_a_title_carries_its_plays() {
    let Some(t) = TestDb::new("plays_owner").await else {
        return;
    };
    let recorded = Recorded::new();
    recorded.answer(
        Method::Get,
        "https://api.themoviedb.org/3/movie/603?append_to_response=",
        200,
        &fixture("tmdb_movie_603.json"),
    );
    let deps = make_deps(&t.pool, &recorded);
    let app = app(&t.pool, &deps.journal, &recorded);
    // A key, even one that may do everything else, reads no statistics; nobody reads them
    // without a sign-in.
    let created = auth::create_key(
        &t.pool,
        &deps.journal,
        &Caller::Owner {
            email: "owner@example.com".into(),
        },
        &NewApiKey {
            name: "script".into(),
            scopes: vec![Scope::Admin],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", created.secret);
    for path in [
        "/api/v1/stats/overview",
        "/api/v1/stats/plays",
        "/api/v1/stats/people",
        "/api/v1/stats/people/1",
    ] {
        let (status, body) = get(&app, path, &[("authorization", bearer.as_str())]).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
        assert_eq!(body["code"], "singularr.forbidden", "{path}: {body}");
        let (status, _) = get(&app, path, &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }

    // The Matrix, in the catalog, played twice by one person and once by another.
    let title: i64 = sqlx::query_scalar(
        "INSERT INTO titles (ext_key, kind, tmdb_id, title, sort_title, year)
         VALUES ('test:the-matrix', 'movie', 603, 'The Matrix', 'matrix', 1999) RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    let start = Utc::now() - chrono::Duration::days(2);
    let mut records = vec![
        sitting(
            11,
            1,
            "owner",
            ("movie", "603", "The Matrix"),
            None,
            start,
            130,
            ("direct play", "lan", "Android", "TV"),
        ),
        sitting(
            12,
            2,
            "friend",
            ("movie", "603", "The Matrix"),
            None,
            start + chrono::Duration::days(1),
            60,
            ("transcode", "wan", "Roku", "Roku TV"),
        ),
        sitting(
            13,
            1,
            "owner",
            ("movie", "603", "The Matrix"),
            None,
            start - chrono::Duration::days(30),
            136,
            ("direct play", "lan", "Android", "TV"),
        ),
    ];
    for r in &mut records {
        r.title_id = Some(title);
    }
    plays::absorb_all(&deps, records).await.unwrap();
    let record = owner_get(&app, "/api/v1/metadata/titles/movie/603").await;
    let plays = &record["plays"];
    assert_eq!(
        (
            plays["count"].clone(),
            plays["people"].clone(),
            plays["hours"].clone()
        ),
        (json!(3), json!(2), json!(5.4))
    );
    assert_eq!(plays["recent"].as_array().unwrap().len(), 3);
    assert_eq!(
        plays["recent"][0]["person"],
        json!({"id": 2, "name": "friend"})
    );
    assert_eq!(plays["recent"][0]["decision"], "transcode");
    // The field it had stays.
    assert!(
        record["library"].get("plays").is_some(),
        "{}",
        record["library"]
    );
    // A key reads the counts and nobody's plays.
    let (status, keyed) = get(
        &app,
        "/api/v1/metadata/titles/movie/603",
        &[("authorization", bearer.as_str())],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{keyed}");
    assert_eq!(keyed["plays"]["count"], 3);
    assert!(keyed["plays"]["recent"].as_array().unwrap().is_empty());
    // The history filters by the title's id.
    let page = owner_get(&app, "/api/v1/stats/plays?title=movie-603").await;
    assert_eq!(page["total"], 3);
    assert_eq!(page["plays"][0]["title"]["id"], "movie-603");
    t.drop_db().await;
}

/// A play Tautulli told of while no Plex server was configured has Tautulli's id for its server.
/// Its keys are the Plex server's. It links through the Plex server Tautulli reads now: as it is
/// written, and in the housekeeping for one written before. Another server's item of the
/// same key is another item.
#[tokio::test]
async fn a_tautulli_play_links_through_the_plex_server() {
    let Some(t) = TestDb::new("plays_tautulli_link").await else {
        return;
    };
    let recorded = Recorded::new();
    let deps = make_deps(&t.pool, &recorded);
    let title: i64 = sqlx::query_scalar(
        "INSERT INTO titles (ext_key, kind, tmdb_id, title, sort_title, year)
         VALUES ('test:the-matrix', 'movie', 603, 'The Matrix', 'matrix', 1999) RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO title_server (server, rating_key, title_id) VALUES ('plex', '603', $1)",
    )
    .bind(title)
    .execute(&t.pool)
    .await
    .unwrap();
    let start = Utc::now() - chrono::Duration::days(2);
    let play = |id: i64, server: &str, hours: i64| {
        let mut r = sitting(
            id,
            1,
            "owner",
            ("movie", "603", "The Matrix"),
            None,
            start + chrono::Duration::hours(hours),
            60,
            ("direct play", "lan", "Android", "TV"),
        );
        r.server = server.to_string();
        r
    };
    let tautulli = deps.engines.play_history.clone().unwrap();
    assert_eq!(tautulli.id(), "tautulli");
    plays::absorb_all(&deps, vec![play(21, "tautulli", 0), play(22, "emby", 3)])
        .await
        .unwrap();
    let linked = || async {
        sqlx::query_as::<_, (String, Option<i64>)>(
            "SELECT server, title_id FROM plays ORDER BY tautulli_id",
        )
        .fetch_all(&t.pool)
        .await
        .unwrap()
    };
    let expected = vec![
        ("tautulli".to_string(), Some(title)),
        ("emby".to_string(), None),
    ];
    assert_eq!(linked().await, expected);
    // Written before the Plex server was configured, and linked by the housekeeping.
    sqlx::query("UPDATE plays SET title_id = NULL")
        .execute(&t.pool)
        .await
        .unwrap();
    plays::housekeeping(&deps).await.unwrap();
    assert_eq!(linked().await, expected);
    t.drop_db().await;
}
