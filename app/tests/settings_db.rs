//! Database-backed tests of the configuration routes: the settings store (sources, checks,
//! a person's own values, the journal), and the routes of the Settings, Rules and Journal
//! pages end to end.
//!
//! Like `db.rs`, they run only when `SINGULARR_TEST_DATABASE_URL` names a server the tests may
//! create databases on, and say so and pass otherwise. Each test makes a database of its own
//! and drops it afterwards. No test talks to a real integration or provider.
#![cfg(feature = "ssr")]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use singularr::api::routes;
use singularr::api::state::{AppState, Inner};
use singularr::core::auth::{self, Caller};
use singularr::core::config::Config;
use singularr::core::journal::Journal;
use singularr::core::model::{EventQuery, NewApiKey, Paging, Scope};
use singularr::core::outbound::recorded::Recorded;
use singularr::core::outbound::{Allowlist, Outbound};
use singularr::core::{db, store as core_store};
use singularr::engines::service::EngineSet;
use singularr::homemovies::service::HomeMovies;
use singularr::metadata::service::ProviderSet;
use singularr::music::service::MusicLibrary;
use singularr::policy::{ActivePolicy, PolicyCell};
use singularr::requests::service::Requests;
use singularr::settings::Settings;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::sync::Arc;
use tower::ServiceExt;

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

const OWNER: (&str, &str) = ("x-auth-request-email", "owner@example.com");
const OTHER: (&str, &str) = ("x-auth-request-email", "other@example.com");

fn env(extra: &[(&'static str, &'static str)]) -> BTreeMap<&'static str, &'static str> {
    let mut env = BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        (
            "SINGULARR_OWNER_EMAILS",
            "owner@example.com,other@example.com",
        ),
    ]);
    env.extend(extra.iter().copied());
    env
}

/// The application state over a test database, its metadata asked in the settings' locale as
/// at start.
fn state(pool: &PgPool, journal: Journal, extra: &[(&'static str, &'static str)]) -> AppState {
    let env = env(extra);
    let config = Config::from(&env).unwrap();
    let out = Outbound::with_transport(Allowlist::with_metadata_providers(), Recorded::new());
    let settings = Arc::new(Settings::new(&config, &env, None));
    AppState::new(Inner {
        engines: Arc::new(EngineSet::from_config(&config, &out)),
        metadata: Arc::new(ProviderSet::from_config(&config, &out).with_locale(settings.locale())),
        owners: singularr::core::identity::Allowlist::parse(&config.bootstrap.owner_emails),
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
        journal,
        policy: PolicyCell::handle(ActivePolicy::builtin()),
    })
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(res.into_body(), 8 * 1024 * 1024)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// One field of a settings answer.
fn field<'a>(view: &'a Value, key: &str) -> &'a Value {
    view["sections"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["groups"].as_array().unwrap().iter())
        .flat_map(|g| g["fields"].as_array().unwrap().iter())
        .find(|f| f["key"] == key)
        .unwrap_or_else(|| panic!("{key} is not in the answer"))
}

async fn key(pool: &PgPool, journal: &Journal, name: &str, scopes: Vec<Scope>) -> String {
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let created = auth::create_key(
        pool,
        journal,
        &owner,
        &NewApiKey {
            name: name.into(),
            scopes,
        },
    )
    .await
    .unwrap();
    format!("Bearer {}", created.secret)
}

#[tokio::test]
async fn settings_have_sources_checks_own_values_and_a_journal() {
    let Some(t) = TestDb::new("settings").await else {
        return;
    };
    let journal = Journal::new();
    let state = state(
        &t.pool,
        journal.clone(),
        &[("SINGULARR_PORTAL_NAME", "Family Requests")],
    );
    let app = routes::router(state.clone());

    // The schema with its values and their sources.
    let (status, view) = call(&app, "GET", "/api/v1/settings", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = view["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 10);
    let name = field(&view, "general.name");
    assert_eq!(
        (&name["value"], &name["source"], &name["editable"]),
        (&json!("Singularr"), &json!("default"), &json!(true))
    );
    let portal = field(&view, "portal.name");
    assert_eq!(
        (&portal["source"], &portal["env"], &portal["editable"]),
        (
            &json!("environment"),
            &json!("SINGULARR_PORTAL_NAME"),
            &json!(false)
        )
    );

    // A save: in force at once, one journal event per change.
    let (status, saved) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {
            "general.name": "Home Theater",
            "general.language": "de-de",
            "general.signOutUrl": "/oauth2/sign_out"
        }})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved,
        json!({"saved": ["general.language", "general.name", "general.signOutUrl"], "unchanged": [], "restart": false})
    );
    assert_eq!(
        state.settings.text("general.name").as_deref(),
        Some("Home Theater")
    );
    assert_eq!(state.metadata.locale.language(), "de-DE");
    let changed = |pool: PgPool| async move {
        core_store::list_events(
            &pool,
            &EventQuery {
                kind: Some("setting_changed".into()),
                ..EventQuery::default()
            },
            Paging::new(None, None),
        )
        .await
        .unwrap()
    };
    let (events, total) = changed(t.pool.clone()).await;
    assert_eq!(total, 3);
    let renamed = events
        .iter()
        .find(|e| e.subject_id == "general.name")
        .unwrap();
    assert_eq!(renamed.subject_type, "setting");
    assert_eq!(renamed.actor, "user:owner@example.com");
    assert_eq!(
        (
            &renamed.body["from"],
            &renamed.body["to"],
            &renamed.body["scope"]
        ),
        (
            &json!("Singularr"),
            &json!("Home Theater"),
            &json!("instance")
        )
    );
    let (_, view) = call(&app, "GET", "/api/v1/settings", &[OWNER], None).await;
    assert_eq!(field(&view, "general.name")["source"], "saved");
    assert_eq!(field(&view, "general.language")["value"], "de-DE");

    // The same value again changes nothing and writes no event.
    let (status, again) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {"general.name": "Home Theater"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["unchanged"], json!(["general.name"]));
    assert_eq!(changed(t.pool.clone()).await.1, 3);

    // All or nothing: a fixed value and a bad one refuse the whole save, with the reasons.
    let (status, refused) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {
            "portal.name": "Ours",
            "general.timeZone": "Mars",
            "general.dateFormat": "YYYY-MM-DD",
            "general.nothing": 1,
            "jobs.titles": 60,
            "integrations.tmdbToken": "x"
        }})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let errors = refused["errors"].as_object().unwrap();
    assert!(
        errors["portal.name"]
            .as_str()
            .unwrap()
            .contains("SINGULARR_PORTAL_NAME")
    );
    assert!(errors.contains_key("general.timeZone"));
    assert!(errors.contains_key("general.nothing"));
    assert!(
        errors["jobs.titles"]
            .as_str()
            .unwrap()
            .contains("SINGULARR_SYNC_TITLES_SECS")
    );
    assert!(errors.contains_key("integrations.tmdbToken"));
    assert!(!errors.contains_key("general.dateFormat"));
    assert_eq!(
        state.settings.value("general.dateFormat"),
        json!("D MMM YYYY")
    );
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {}, "colour": 1})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown fields are refused"
    );

    // A reader key may read and not save; a key with `settings` may save the installation's
    // values but has no own values.
    let reader = key(&t.pool, &journal, "dashboard", vec![Scope::Read]).await;
    let (status, view) = call(
        &app,
        "GET",
        "/api/v1/settings",
        &[("authorization", &reader)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(field(&view, "people.ownerEmails")["withheld"], true);
    assert!(!view.to_string().contains("owner@example.com"));
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[("authorization", &reader)],
        Some(json!({"values": {"general.name": "Nope"}})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let admin = key(
        &t.pool,
        &journal,
        "setup",
        vec![Scope::Read, Scope::Settings],
    )
    .await;
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[("authorization", &admin)],
        Some(json!({"values": {"general.timeFormat": "12h"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[("authorization", &admin)],
        Some(json!({"values": {"appearance.theme": "graphite"}, "scope": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (events, _) = changed(t.pool.clone()).await;
    assert!(events.iter().any(|e| e.actor == "key:setup"));

    // A person's own values: theirs alone, the installation's value the default for everyone.
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {"appearance.theme": "graphite", "appearance.density": "compact"}, "scope": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, mine) = call(&app, "GET", "/api/v1/settings", &[OWNER], None).await;
    let theme = field(&mine, "appearance.theme");
    assert_eq!(
        (&theme["value"], &theme["source"], &theme["scope"]),
        (&json!("graphite"), &json!("saved"), &json!("user"))
    );
    assert_eq!(theme["instance"]["value"], "horizon");
    let (_, theirs) = call(&app, "GET", "/api/v1/settings", &[OTHER], None).await;
    assert_eq!(field(&theirs, "appearance.theme")["value"], "horizon");
    assert_eq!(state.settings.value("appearance.theme"), json!("horizon"));
    // Without a scope a person's own field is the caller's own; `scope: instance` sets the
    // default for everyone, and a key has no own values, so its save is the installation's.
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OTHER],
        Some(json!({"values": {"appearance.sidebar": "rail"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, theirs) = call(&app, "GET", "/api/v1/settings", &[OTHER], None).await;
    assert_eq!(field(&theirs, "appearance.sidebar")["value"], "rail");
    assert_eq!(state.settings.value("appearance.sidebar"), json!("full"));
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {"appearance.startPage": "discover"}, "scope": "instance"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        state.settings.value("appearance.startPage"),
        json!("discover")
    );
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[("authorization", &admin)],
        Some(json!({"values": {"appearance.density": "compact"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state.settings.value("appearance.density"), json!("compact"));
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {"appearance.density": null}, "scope": "instance"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // An installation's setting is not a person's own.
    let (status, refused) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {"general.name": "Mine"}, "scope": "user"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(refused["errors"]["general.name"].is_string());

    // `null` goes back to the default, and says so in the journal.
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {"general.signOutUrl": null}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(state.settings.value("general.signOutUrl"), Value::Null);
    let (events, _) = changed(t.pool.clone()).await;
    let reset = events
        .iter()
        .find(|e| e.subject_id == "general.signOutUrl" && e.body["reset"] == true)
        .unwrap();
    assert_eq!(reset.body["to"], Value::Null);

    // A new start reads the saved values back.
    let again = state_restart(&t.pool, &journal).await;
    assert_eq!(
        again.settings.text("general.name").as_deref(),
        Some("Home Theater")
    );
    assert_eq!(again.metadata.locale.language(), "de-DE");
    assert_eq!(again.settings.value("general.timeFormat"), json!("12h"));
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    assert_eq!(
        again.settings.value_for(&owner, "appearance.density"),
        json!("compact")
    );
    t.drop_db().await;
}

/// The state as a new start builds it: the settings read from the database.
async fn state_restart(pool: &PgPool, journal: &Journal) -> AppState {
    let state = state(pool, journal.clone(), &[]);
    state.settings.reload(pool).await.unwrap();
    state
}

#[tokio::test]
async fn the_workspace_address_reaches_the_portal_s_table() {
    let Some(t) = TestDb::new("settings_url").await else {
        return;
    };
    let state = state(&t.pool, Journal::new(), &[]);
    let app = routes::router(state.clone());
    let (status, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &[OWNER],
        Some(json!({"values": {"general.url": "https://media.example.org/"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let url: Option<String> =
        sqlx::query_scalar("SELECT owner_url FROM face_settings WHERE id = 1")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(url.as_deref(), Some("https://media.example.org"));
    t.drop_db().await;
}

#[tokio::test]
async fn the_rules_page_reads_the_policy_in_force() {
    let Some(t) = TestDb::new("rules").await else {
        return;
    };
    let state = state(
        &t.pool,
        Journal::new(),
        &[("SINGULARR_REQUEST_MAX_SEASONS", "3")],
    );
    let app = routes::router(state);
    let (status, rules) = call(&app, "GET", "/api/v1/rules", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK, "{rules}");
    assert_eq!(rules["profiles"].as_array().unwrap().len(), 4);
    assert_eq!(rules["profiles"][0]["titles"], 0);
    assert_eq!(rules["source"], "default");
    assert_eq!(rules["editable"], false);
    assert!(!rules["editions"].as_array().unwrap().is_empty());
    let gate = &rules["requestRules"][0];
    assert_eq!(
        (&gate["value"], &gate["source"], &gate["env"]),
        (
            &json!(3),
            &json!("environment"),
            &json!("SINGULARR_REQUEST_MAX_SEASONS")
        )
    );
    assert_eq!(
        rules["protections"][0]["current"]["edition"][0],
        "Open Matte"
    );
    let (status, _) = call(&app, "GET", "/api/v1/rules", &[], None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    t.drop_db().await;
}

#[tokio::test]
async fn the_journal_page_reads_events_in_words_paged_and_filtered() {
    let Some(t) = TestDb::new("journal").await else {
        return;
    };
    let journal = Journal::new();
    let state = state(&t.pool, journal.clone(), &[]);
    let app = routes::router(state);
    let reader = key(&t.pool, &journal, "dashboard", vec![Scope::Read]).await;
    for name in ["Home Theater", "Den"] {
        let (status, _) = call(
            &app,
            "PATCH",
            "/api/v1/settings",
            &[OWNER],
            Some(json!({"values": {"general.name": name}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, page) = call(&app, "GET", "/api/v1/journal?limit=2", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let events = page["events"].as_array().unwrap();
    assert_eq!(events.len(), 2);
    let newest = &events[0];
    assert_eq!(newest["kind"], "setting.changed");
    assert_eq!(newest["code"], "setting_changed");
    assert_eq!(newest["summary"], "Changed from Home Theater to Den");
    assert_eq!(
        newest["actor"],
        json!({"kind": "user", "id": "owner@example.com", "name": "owner@example.com"})
    );
    assert_eq!(newest["subject"]["type"], "setting");
    assert!(newest["id"].as_str().unwrap().starts_with("e-"));
    let next = page["next"].as_str().unwrap().to_string();

    // The older page holds the key's creation.
    let (_, older) = call(
        &app,
        "GET",
        &format!("/api/v1/journal?limit=2&before={next}"),
        &[OWNER],
        None,
    )
    .await;
    let older = older["events"].as_array().unwrap();
    assert_eq!(older.len(), 1);
    assert_eq!(older[0]["kind"], "key.created");
    assert_eq!(older[0]["summary"], "Created with scopes read");

    // Filters: the page's words or the stored kinds, the actor, words.
    let count = |path: String| {
        let app = app.clone();
        let reader = reader.clone();
        async move {
            let (status, page) =
                call(&app, "GET", &path, &[("authorization", &reader)], None).await;
            assert_eq!(status, StatusCode::OK, "{path}: {page}");
            page["events"].as_array().unwrap().len()
        }
    };
    assert_eq!(
        count("/api/v1/journal?kind=setting.changed".into()).await,
        2
    );
    assert_eq!(count("/api/v1/journal?kind=key_created".into()).await, 1);
    assert_eq!(
        count("/api/v1/journal?kind=key.created,setting.changed".into()).await,
        3
    );
    assert_eq!(count("/api/v1/journal?q=theater".into()).await, 2);
    assert_eq!(count("/api/v1/journal?q=nothing-like-this".into()).await, 0);
    assert_eq!(
        count("/api/v1/journal?actor=user:owner@example.com".into()).await,
        3
    );
    assert_eq!(
        count("/api/v1/journal?actor=integration:radarr".into()).await,
        0
    );
    let (status, _) = call(&app, "GET", "/api/v1/journal?before=x", &[OWNER], None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        "GET",
        "/api/v1/journal?kind=file.nothing",
        &[OWNER],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    t.drop_db().await;
}

#[tokio::test]
async fn the_settings_panels_answer_in_the_brief_s_shapes() {
    let Some(t) = TestDb::new("panels").await else {
        return;
    };
    let journal = Journal::new();
    // Radarr is listed without a key, so it is not asked; Prowlarr is a type this version
    // does not read.
    let state = state(
        &t.pool,
        journal.clone(),
        &[
            ("RADARR_URL", "http://10.0.0.61:7878"),
            ("PROWLARR_URL", "http://10.0.0.62:9696"),
        ],
    );
    let app = routes::router(state);
    let reader = key(&t.pool, &journal, "dashboard", vec![Scope::Read]).await;

    let (status, list) = call(&app, "GET", "/api/v1/integrations", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let radarr = list["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "radarr")
        .unwrap();
    assert_eq!(radarr["address"], "http://10.0.0.61:7878");
    assert_eq!(radarr["source"], "environment");
    assert_eq!(radarr["env"], "RADARR_URL");
    assert_eq!(radarr["state"], "warn");
    assert_eq!(radarr["configured"], false);
    // What could be changed in a manager, once it is allowed.
    assert_eq!(
        radarr["writes"],
        json!([
            "Search for releases",
            "Grab a release",
            "Import a refused download",
            "Remove a download"
        ])
    );
    let (status, one) = call(&app, "GET", "/api/v1/integrations/radarr", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK, "{one}");
    let secret = one["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["key"] == "integrations.radarr.apiKey")
        .unwrap();
    assert_eq!(
        (&secret["set"], &secret["env"], secret.get("value")),
        (&json!(false), &json!("RADARR_API_KEY"), None)
    );
    assert!(one["log"].is_array());
    let (status, test) = call(
        &app,
        "POST",
        "/api/v1/integrations/radarr/test",
        &[OWNER],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(test["ok"], false);
    assert!(test["detail"].as_str().unwrap().contains("RADARR_API_KEY"));
    let (_, test) = call(
        &app,
        "POST",
        "/api/v1/integrations/prowlarr/test",
        &[OWNER],
        None,
    )
    .await;
    assert_eq!(test["ok"], false);
    let (status, _) = call(&app, "GET", "/api/v1/integrations/nothing", &[OWNER], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // No loop reads Radarr in this process: nothing to start.
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/integrations/radarr/sync",
        &[OWNER],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/integrations/radarr/sync",
        &[("authorization", &reader)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, libs) = call(&app, "GET", "/api/v1/libraries", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK, "{libs}");
    assert!(libs["libraries"].is_array());

    let (status, people) = call(&app, "GET", "/api/v1/people", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK, "{people}");
    let all = people["people"].as_array().unwrap();
    assert_eq!(all[0]["role"], "owner");
    assert_eq!(all[0]["name"], "owner@example.com");
    assert!(
        all.iter()
            .any(|p| p["kind"] == "program" && p["name"] == "dashboard")
    );
    assert_eq!(people["roles"][0]["id"], "owner");
    assert!(!people["permissions"].as_array().unwrap().is_empty());
    let (_, hidden) = call(
        &app,
        "GET",
        "/api/v1/people",
        &[("authorization", &reader)],
        None,
    )
    .await;
    assert!(!hidden.to_string().contains("owner@example.com"));

    // A schedule of the job runtime is listed and can be asked for a run. While its job is open
    // a second ask starts nothing.
    let hour = std::time::Duration::from_secs(3600);
    singularr::core::runtime::Runtime::reconcile(
        &t.pool,
        &[singularr::core::runtime::Schedule {
            key: "folder:music".to_string(),
            kind: "scan.music".to_string(),
            integration: None,
            queue: "scan".to_string(),
            every: hour,
            floor: std::time::Duration::from_secs(600),
            first: hour,
            jitter: std::time::Duration::ZERO,
            on_start: false,
            lease: std::time::Duration::from_secs(120),
            source: singularr::core::runtime::Source::Default,
        }],
    )
    .await
    .unwrap();
    let (status, jobs) = call(&app, "GET", "/api/v1/jobs", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["id"] == "folder:music" && j["every"] == 3600)
    );
    let (status, started) = call(
        &app,
        "POST",
        "/api/v1/jobs/folder:music/run",
        &[OWNER],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(started["started"], true);
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/jobs/folder:music/run",
        &[OWNER],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "POST", "/api/v1/jobs/nothing/run", &[OWNER], None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, system) = call(&app, "GET", "/api/v1/system", &[OWNER], None).await;
    assert_eq!(status, StatusCode::OK, "{system}");
    assert_eq!(system["mode"], "observe");
    let ids: Vec<&str> = system["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["database", "search", "cache", "artwork"]);
    assert_eq!(system["services"][0]["state"], "ok");
    assert!(
        system["services"][0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("PostgreSQL ")
    );
    let checks = system["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|c| c["id"] == "migrations" && c["state"] == "ok")
    );
    assert!(checks.iter().any(|c| c["id"] == "integrations"));
    assert_eq!(system["links"]["reference"], "/api/docs");
    t.drop_db().await;
}
