//! Database-backed tests of the media server layer: a Jellyfin server read through the port
//! Plex is read through. Its libraries and items reach the catalog and the workspace, its
//! sessions become plays, and the statistics count them with no history of the server's own and
//! no Tautulli.
//!
//! Like `db.rs`, they run only when `SINGULARR_TEST_DATABASE_URL` names a server the tests may
//! create databases on, and say so and pass otherwise. Each test makes a database of its own and
//! drops it afterwards. The server is recorded answers (`tests/fixtures/jellyfin`), sent through
//! the real outbound allowlist.
#![cfg(feature = "ssr")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};
use singularr::api::routes;
use singularr::api::state::{AppState, Inner};
use singularr::catalog::service as catalog;
use singularr::core::config::Config;
use singularr::core::db;
use singularr::core::journal::Journal;
use singularr::core::outbound::recorded::Recorded;
use singularr::core::outbound::{Method, Outbound};
use singularr::engines::ServerSession;
use singularr::engines::service::{EngineSet, allowlist};
use singularr::homemovies::service::HomeMovies;
use singularr::lifecycle::server::server_pass;
use singularr::lifecycle::sync::SyncDeps;
use singularr::metadata::service::ProviderSet;
use singularr::music::service::MusicLibrary;
use singularr::plays::model::{Follower, from_session};
use singularr::plays::service::{self as plays, PlaysDeps};
use singularr::policy::ActivePolicy;
use singularr::releases::assess::Assessor;
use singularr::requests::service::Requests;
use singularr::settings::Settings;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;

const JELLYFIN: &str = "http://10.0.0.40:8096";
const KEY: &str = "0123456789abcdef0123456789abcdef";
/// The demo's libraries, by their ids.
const MOVIES: &str = "f137a2dd21bbc1b99aa5c0f6bf02a805";
const SHOWS: &str = "a656b907eb3a73532e40e44b968d0225";
const MUSIC: &str = "7e64e319657a9516ec78490da03edccb";
/// The movie the transcoding session plays, and the demo's administrator by account number.
const LLAMA: &str = "c541e7545e58b28cd4d0da7aac89d7e8";
const ADMIN_NUMBER: i64 = 6_004_263_143_739_604;
const OWNER: (&str, &str) = ("x-auth-request-email", "owner@example.com");

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

fn env() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("SINGULARR_OWNER_EMAILS", "owner@example.com"),
        ("JELLYFIN_URL", JELLYFIN),
        ("JELLYFIN_API_KEY", KEY),
    ])
}

/// The demo server's answers, each for the call the adapter makes.
fn record_jellyfin(recorded: &Recorded) {
    let items = |library: &str, types: &str| {
        format!("{JELLYFIN}/Items?ParentId={library}&IncludeItemTypes={types}&")
    };
    for (part, file) in [
        (format!("{JELLYFIN}/System/Info"), "system_info.json"),
        (
            format!("{JELLYFIN}/Library/VirtualFolders"),
            "virtual_folders.json",
        ),
        (format!("{JELLYFIN}/Users"), "users.json"),
        (items(MOVIES, "Movie"), "items_movies.json"),
        (items(SHOWS, "Series"), "items_shows.json"),
        (items(MUSIC, "MusicArtist"), "items_artists.json"),
        (items(MUSIC, "MusicAlbum"), "items_albums.json"),
        (items(MUSIC, "Audio"), "items_tracks.json"),
        (format!("{JELLYFIN}/Sessions"), "sessions_transcode.json"),
    ] {
        recorded.answer(
            Method::Get,
            &part,
            200,
            &fixture(&format!("jellyfin/{file}")),
        );
    }
}

/// The application over a test database, its integrations answering from `recorded` through the
/// real allowlist: the Jellyfin server of `env()`.
fn app(pool: &PgPool, journal: &Journal, engines: Arc<EngineSet>, out: &Outbound) -> axum::Router {
    app_of(&env(), pool, journal, engines, out)
}

/// The application of one environment.
fn app_of(
    env: &BTreeMap<&'static str, &'static str>,
    pool: &PgPool,
    journal: &Journal,
    engines: Arc<EngineSet>,
    out: &Outbound,
) -> axum::Router {
    let config = Config::from(env).unwrap();
    let settings = Arc::new(Settings::new(&config, env, None));
    routes::router(AppState::new(Inner {
        engines,
        metadata: Arc::new(ProviderSet::from_config(&config, out).with_locale(settings.locale())),
        owners: singularr::core::identity::Allowlist::parse(&config.bootstrap.owner_emails),
        requests: Arc::new(Requests::new(&config.request_rules)),
        music: Arc::new(MusicLibrary::new(&config.music.root)),
        home_movies: Arc::new(HomeMovies::new(&config.home_movies.root)),
        settings,
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
        journal: journal.clone(),
        policy: singularr::policy::PolicyCell::handle(ActivePolicy::builtin()),
    }))
}

async fn owner_get(app: &axum::Router, path: &str) -> Value {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .header(OWNER.0, OWNER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

#[tokio::test]
async fn a_jellyfin_server_stands_in_for_plex_from_its_libraries_to_its_statistics() {
    let Some(t) = TestDb::new("jellyfin").await else {
        return;
    };
    // A movie library of a manager, whose folder the server's movie library names, and a movie
    // of it that the server holds.
    sqlx::query(
        "INSERT INTO libraries (id, name, kind, integration, root_folders)
         VALUES ('radarr', 'Radarr', 'movie', 'radarr', '{/data/Movies}')",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let title_id: i64 = sqlx::query_scalar(
        "INSERT INTO titles (ext_key, kind, tmdb_id, imdb_id, title, sort_title, year)
         VALUES ('movie:253777', 'movie', 253777, 'tt6059374', 'Caminandes: Llama Drama',
                 'caminandes llama drama', 2013)
         RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();

    let recorded = Recorded::new();
    record_jellyfin(&recorded);
    let config = Config::from(&env()).unwrap();
    let out = Outbound::with_transport(allowlist(&config).unwrap(), recorded.clone());
    let engines = Arc::new(EngineSet::from_config(&config, &out));
    let server = engines.media_server_of("jellyfin").cloned().unwrap();
    assert_eq!(server.kind(), "jellyfin");
    let journal = Journal::new();
    let sync = SyncDeps {
        config: config.sync.clone(),
        pool: t.pool.clone(),
        journal: journal.clone(),
        engines: engines.clone(),
        assess: Arc::new(Assessor::new(
            Arc::new(ActivePolicy::builtin()),
            "test-build",
        )),
        policy_event: None,
    };

    // The server pass: its libraries, its three movies and one show with the movie the catalog
    // holds matched by its ids, its music.
    let ok = server_pass(&sync, &server).await.unwrap();
    assert_eq!(ok.version.as_deref(), Some("12.4.0"));
    assert_eq!(
        (
            ok.counts["sections"].clone(),
            ok.counts["items"].clone(),
            ok.counts["titles_matched"].clone()
        ),
        (json!(3), json!(4), json!(1))
    );
    assert_eq!(
        (ok.counts["albums"].clone(), ok.counts["tracks"].clone()),
        (json!(2), json!(1))
    );
    let libraries = catalog::libraries(&t.pool).await.unwrap();
    let movies = libraries
        .iter()
        .find(|l| l.id == format!("jellyfin-{MOVIES}"))
        .unwrap();
    assert_eq!(
        (
            movies.kind.as_deref(),
            movies.integration.as_deref(),
            movies.managed_by.as_deref()
        ),
        (Some("movie"), Some("jellyfin"), Some("radarr"))
    );
    assert_eq!(movies.locations, vec!["/media/Movies".to_string()]);
    engines
        .report(&t.pool, &journal, "jellyfin", "server", Ok(ok))
        .await
        .unwrap();

    // Its sessions are plays: a transcode on the home network, linked to the catalog's title.
    let deps = PlaysDeps::new(
        t.pool.clone(),
        journal.clone(),
        engines.clone(),
        Arc::new(Settings::new(&config, &env(), None)),
        Duration::ZERO,
    );
    let mut follower = Follower::default();
    let mut files = HashMap::new();
    assert_eq!(
        plays::live_pass(&deps, &server, &mut follower, &mut files)
            .await
            .unwrap(),
        1
    );
    recorded.answer(
        Method::Get,
        &format!("{JELLYFIN}/Sessions"),
        200,
        &fixture("jellyfin/sessions_idle.json"),
    );
    assert_eq!(
        plays::live_pass(&deps, &server, &mut follower, &mut files)
            .await
            .unwrap(),
        0
    );
    type Row = (
        String,
        Option<String>,
        Option<i64>,
        Option<String>,
        Option<String>,
        Option<bool>,
        Option<i64>,
        bool,
    );
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT server, session_id, account_id, decision, stream_resolution, local, title_id,
                stopped_at IS NOT NULL
         FROM plays",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0],
        (
            "jellyfin".to_string(),
            Some(format!("0f4c1d2e3b5a69788796a5b4c3d2e1f0-{LLAMA}")),
            Some(ADMIN_NUMBER),
            Some("transcode".to_string()),
            Some("720p".to_string()),
            Some(true),
            Some(title_id),
            true
        )
    );
    // The player's address is kept for the owner, in the play and in the session's record.
    let (address, raw): (Option<String>, Value) = sqlx::query_as("SELECT address, raw FROM plays")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(address.as_deref(), Some("10.0.0.20"));
    assert_eq!(raw["live"]["RemoteEndPoint"], "10.0.0.20");
    assert_eq!(raw["live"]["Client"], "Jellyfin Android TV");
    // The server's kind, the app's version, the device, the file and what was sent of it, and
    // why the server transcoded.
    type Facts = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i32>,
        Option<String>,
        Vec<String>,
    );
    let facts: Facts = sqlx::query_as(
        "SELECT server_kind, product_version, device_id, source_video_codec, source_audio_codec,
                source_audio_channels, source_container, stream_video_codec, stream_audio_codec,
                stream_audio_channels, stream_container, transcode_reasons
         FROM plays",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        facts,
        (
            Some("jellyfin".to_string()),
            Some("0.18.11".to_string()),
            Some("fixtures-device-4".to_string()),
            Some("h264".to_string()),
            Some("aac".to_string()),
            Some(2),
            Some("mp4".to_string()),
            Some("h264".to_string()),
            Some("aac".to_string()),
            Some(2),
            Some("ts".to_string()),
            vec!["VideoBitrateNotSupported".to_string()]
        )
    );

    // The pages read it as they read Plex: the workspace's libraries and server, the sessions,
    // and the statistics with this server as their source.
    let app = app(&t.pool, &journal, engines.clone(), &out);
    let ws = owner_get(&app, "/api/v1/workspace").await;
    assert_eq!(ws["server"]["name"], "Stable Demo", "{ws}");
    let ids: Vec<&str> = ws["libraries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&format!("jellyfin-{MOVIES}").as_str())
            && ids.contains(&format!("jellyfin-{MUSIC}").as_str()),
        "{ids:?}"
    );
    recorded.answer(
        Method::Get,
        &format!("{JELLYFIN}/Sessions"),
        200,
        &fixture("jellyfin/sessions_transcode.json"),
    );
    let now = owner_get(&app, "/api/v1/server/sessions").await;
    assert_eq!(now["configured"], true);
    let playing = &now["sessions"][0];
    assert_eq!(
        (
            playing["title"].clone(),
            playing["person"].clone(),
            playing["decision"].clone(),
            playing["network"].clone()
        ),
        (
            json!("Caminandes: Llama Drama"),
            json!("admin"),
            json!("transcode"),
            json!("home")
        )
    );
    assert_eq!(playing["delivery"]["stream"]["resolution"], "720p");
    assert_eq!(playing["delivery"]["source"]["resolution"], "1080p");
    let stats = owner_get(&app, "/api/v1/stats/overview?days=all").await;
    assert_eq!(stats["totals"]["plays"], 1, "{stats}");
    assert_eq!(stats["delivery"]["transcode"], 1);
    assert_eq!(stats["network"]["home"], 1);
    assert_eq!(
        stats["sources"],
        json!([{"id": "jellyfin", "kind": "server", "name": "Jellyfin"}])
    );
    let library = owner_get(
        &app,
        &format!("/api/v1/stats/overview?days=all&library=jellyfin-{MOVIES}"),
    )
    .await;
    assert_eq!(library["totals"]["plays"], 1);
    let person = owner_get(
        &app,
        &format!("/api/v1/stats/overview?days=all&person={ADMIN_NUMBER}"),
    )
    .await;
    assert_eq!(person["totals"]["plays"], 1);
    let history = owner_get(&app, "/api/v1/stats/plays").await;
    let play = &history["plays"][0];
    assert_eq!(
        (
            play["person"]["id"].clone(),
            play["person"]["name"].clone(),
            play["sources"].clone()
        ),
        (json!(ADMIN_NUMBER), json!("admin"), json!(["live"]))
    );
    // The history names the media server and the player, the address with its place (a home
    // address is the local network, with or without a location database), and the stream.
    assert_eq!(
        play["server"],
        json!({"id": "jellyfin", "kind": "jellyfin", "name": "Jellyfin"})
    );
    assert_eq!(
        (
            play["player"].clone(),
            play["product"].clone(),
            play["productVersion"].clone(),
            play["deviceId"].clone()
        ),
        (
            json!("Living Room"),
            json!("Jellyfin Android TV"),
            json!("0.18.11"),
            json!("fixtures-device-4")
        )
    );
    assert_eq!(play["address"], "10.0.0.20");
    assert_eq!(
        (
            play["location"]["label"].clone(),
            play["location"]["local"].clone()
        ),
        (json!("Local network"), json!(true))
    );
    assert_eq!(play["secure"], Value::Null);
    assert_eq!(
        play["delivery"]["reasons"],
        json!(["VideoBitrateNotSupported"])
    );
    assert_eq!(
        (
            play["delivery"]["source"]["container"].clone(),
            play["delivery"]["stream"]["container"].clone(),
            play["delivery"]["stream"]["videoCodec"].clone()
        ),
        (json!("mp4"), json!("ts"), json!("h264"))
    );
    let options = owner_get(&app, "/api/v1/stats/options").await;
    assert_eq!(
        options["servers"],
        json!([{"id": "jellyfin", "kind": "jellyfin", "name": "Jellyfin"}])
    );
    assert_eq!(options["products"], json!(["Jellyfin Android TV"]));
    assert_eq!(options["locations"]["database"], Value::Null);
    assert_eq!(options["locations"]["credit"], "IP geolocation by DB-IP");

    // An album's cover is made by the server that holds the album.
    recorded.answer_with(
        Method::Get,
        &format!("{JELLYFIN}/Items/f7e6d83954e06a0d46724a6068fa21ff/Images/Primary"),
        200,
        "jpeg",
        &[("content-type", "image/jpeg")],
    );
    let cover = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/v1/music/albums/album-f7e6d83954e06a0d46724a6068fa21ff/cover?size=300")
                .header(OWNER.0, OWNER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cover.status(), StatusCode::OK);
    assert_eq!(cover.headers()["content-type"], "image/jpeg");

    // The server is asked with GET alone, the key in its header and never in an address.
    assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
    assert!(recorded.urls_seen().iter().all(|u| !u.contains(KEY)));
    t.drop_db().await;
}

/// One reading of the sessions at `now`, written as the live pass writes it: the plays that go
/// on and the plays that ended.
async fn read_sessions(
    deps: &PlaysDeps,
    follower: &mut Follower,
    now: DateTime<Utc>,
    sessions: &[ServerSession],
) {
    let reading = follower.observe(
        now,
        sessions
            .iter()
            .filter_map(|s| Some((from_session("jellyfin", s, now)?, s.state.clone())))
            .collect(),
    );
    let mut all = reading.open;
    all.extend(reading.ended);
    plays::absorb_all(deps, all).await.unwrap();
}

/// A Jellyfin session names its play by the session and the item, and the pair comes back when
/// the item is played again on that player. Read after its play ended, it is a second play, and
/// the first keeps its times.
#[tokio::test]
async fn a_session_read_again_after_its_play_ended_is_a_second_play() {
    let Some(t) = TestDb::new("jellyfin_replay").await else {
        return;
    };
    let recorded = Recorded::new();
    record_jellyfin(&recorded);
    let config = Config::from(&env()).unwrap();
    let out = Outbound::with_transport(allowlist(&config).unwrap(), recorded.clone());
    let engines = Arc::new(EngineSet::from_config(&config, &out));
    let server = engines.media_server_of("jellyfin").cloned().unwrap();
    let deps = PlaysDeps::new(
        t.pool.clone(),
        Journal::new(),
        engines.clone(),
        Arc::new(Settings::new(&config, &env(), None)),
        Duration::ZERO,
    );
    let playing = server.sessions().await.unwrap();
    assert_eq!(playing.len(), 1);
    let id = playing[0].session_id.clone().unwrap();
    assert_eq!(id, format!("0f4c1d2e3b5a69788796a5b4c3d2e1f0-{LLAMA}"));
    let start = Utc
        .timestamp_opt(Utc::now().timestamp() - 2 * 86_400, 0)
        .unwrap();
    let at = |secs: i64| start + chrono::Duration::seconds(secs);
    let mut follower = Follower::default();
    // The first sitting: ten minutes, then the session shows nothing.
    read_sessions(&deps, &mut follower, at(0), &playing).await;
    read_sessions(&deps, &mut follower, at(600), &playing).await;
    read_sessions(&deps, &mut follower, at(610), &[]).await;
    // A day later the same item plays on the same player: the same session and item.
    read_sessions(&deps, &mut follower, at(86_400), &playing).await;
    read_sessions(&deps, &mut follower, at(87_000), &playing).await;
    read_sessions(&deps, &mut follower, at(87_010), &[]).await;
    let rows: Vec<(Option<String>, i64, Option<i64>)> = sqlx::query_as(
        "SELECT session_id, EXTRACT(EPOCH FROM started_at)::bigint,
                EXTRACT(EPOCH FROM stopped_at)::bigint
         FROM plays ORDER BY started_at",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    let secs = |s: i64| at(s).timestamp();
    assert_eq!(
        rows,
        vec![
            (Some(id.clone()), secs(0), Some(secs(600))),
            (Some(id.clone()), secs(86_400), Some(secs(87_000))),
        ]
    );
    t.drop_db().await;
}

const PLEX: &str = "http://10.0.0.30:32400";
const EMBY: &str = "http://10.0.0.41:8096";

/// The status and the bytes of a GET as the owner.
async fn owner_bytes(app: &axum::Router, path: &str) -> (StatusCode, Vec<u8>) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header(OWNER.0, OWNER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

/// Plex and Emby both number their items: one key can name an album on each. Each album and
/// artist stays its server's: in the music list, an album's own route and cover, an artist's
/// discography and the search index. The first server's ids keep the short form they had.
#[tokio::test]
async fn two_servers_that_hold_one_key_keep_their_music_apart() {
    let Some(t) = TestDb::new("two_servers").await else {
        return;
    };
    for sql in [
        "INSERT INTO server_artists (server, rating_key, name, thumb)
         VALUES ('plex', '50', 'Plex Artist', '/library/metadata/50/thumb/1'),
                ('emby', '50', 'Emby Artist', '/Items/50/Images/Primary')",
        "INSERT INTO server_albums (server, rating_key, artist_key, artist, title, year, thumb)
         VALUES ('plex', '100', '50', 'Plex Artist', 'On Plex', 2001,
                 '/library/metadata/100/thumb/1'),
                ('emby', '100', '50', 'Emby Artist', 'On Emby', 2002, '/Items/100/Images/Primary')",
        "INSERT INTO server_tracks (server, rating_key, album_key, number, disc, title,
                                    duration_ms, codec, bitrate, size_bytes)
         VALUES ('plex', '101', '100', 1, 1, 'Plex Track', 200000, 'flac', 900, 10),
                ('emby', '101', '100', 1, 1, 'Emby Track One', 200000, 'mp3', 320, 5),
                ('emby', '102', '100', 2, 1, 'Emby Track Two', 200000, 'mp3', 320, 5)",
    ] {
        sqlx::query(sql).execute(&t.pool).await.unwrap();
    }
    let env = BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("SINGULARR_OWNER_EMAILS", "owner@example.com"),
        ("PLEX_URL", PLEX),
        ("PLEX_TOKEN", "plex-token"),
        ("EMBY_URL", EMBY),
        ("EMBY_API_KEY", KEY),
    ]);
    let config = Config::from(&env).unwrap();
    assert_eq!(config.first_media_server(), Some("plex"));
    let recorded = Recorded::new();
    recorded.answer_with(
        Method::Get,
        "/photo/:/transcode",
        200,
        "plex-cover",
        &[("content-type", "image/jpeg")],
    );
    recorded.answer_with(
        Method::Get,
        &format!("{EMBY}/emby/Items/100/Images/Primary"),
        200,
        "emby-cover",
        &[("content-type", "image/jpeg")],
    );
    let out = Outbound::with_transport(allowlist(&config).unwrap(), recorded.clone());
    let engines = Arc::new(EngineSet::from_config(&config, &out));
    let app = app_of(&env, &t.pool, &Journal::new(), engines, &out);

    let music = owner_get(&app, "/api/v1/music").await;
    let album = |id: &str| {
        music["albums"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id}: {music}"))
    };
    let (plex, emby) = (album("album-100"), album("album-emby_100"));
    assert_eq!(
        (
            plex["title"].clone(),
            plex["artistId"].clone(),
            plex["cover"].clone()
        ),
        (
            json!("On Plex"),
            json!("artist-50"),
            json!("/api/v1/music/albums/album-100/cover")
        )
    );
    assert_eq!(
        (
            emby["title"].clone(),
            emby["artistId"].clone(),
            emby["cover"].clone()
        ),
        (
            json!("On Emby"),
            json!("artist-emby_50"),
            json!("/api/v1/music/albums/album-emby_100/cover")
        )
    );
    let artists: Vec<&str> = music["artists"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(artists, ["artist-emby_50", "artist-50"]);

    // Each album with its own server's tracks.
    let on_emby = owner_get(&app, "/api/v1/music/albums/album-emby_100").await;
    let titles: Vec<&str> = on_emby["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        (on_emby["title"].clone(), titles),
        (json!("On Emby"), vec!["Emby Track One", "Emby Track Two"])
    );
    let on_plex = owner_get(&app, "/api/v1/music/albums/album-100").await;
    assert_eq!(on_plex["title"], "On Plex");
    assert_eq!(on_plex["tracks"].as_array().unwrap().len(), 1);
    let artist = owner_get(&app, "/api/v1/music/artists/artist-emby_50").await;
    assert_eq!(artist["name"], "Emby Artist");
    let discography: Vec<&str> = artist["discography"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    assert_eq!(discography, ["album-emby_100"]);

    // A cover is made by the server that holds the album.
    let (status, cover) =
        owner_bytes(&app, "/api/v1/music/albums/album-emby_100/cover?size=300").await;
    assert_eq!(
        (status, cover.as_slice()),
        (StatusCode::OK, b"emby-cover".as_slice())
    );
    let (status, cover) = owner_bytes(&app, "/api/v1/music/albums/album-100/cover").await;
    assert_eq!(
        (status, cover.as_slice()),
        (StatusCode::OK, b"plex-cover".as_slice())
    );
    let (status, _) = owner_bytes(&app, "/api/v1/music/albums/album-jellyfin_100/cover").await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // The search index names them as the API does.
    let search = singularr::search::Search::new(t.pool.clone(), &config, &out);
    assert_eq!(search.first_server(), Some("plex"));
    let docs = singularr::search::index::documents(&t.pool, search.first_server())
        .await
        .unwrap();
    let mut music_docs: Vec<(String, Option<String>)> = docs
        .iter()
        .filter(|d| d.kind == "album" || d.kind == "artist")
        .map(|d| (d.id.clone(), d.parent.clone()))
        .collect();
    music_docs.sort();
    assert_eq!(
        music_docs,
        vec![
            ("album-100".to_string(), Some("artist-50".to_string())),
            (
                "album-emby_100".to_string(),
                Some("artist-emby_50".to_string())
            ),
            ("artist-50".to_string(), None),
            ("artist-emby_50".to_string(), None),
        ]
    );
    t.drop_db().await;
}
