//! The saved model of quality profiles, lists and naming conventions: versions, undo, where the
//! policy in force comes from, and the routes that read and change it.
//!
//! They run only when `SINGULARR_TEST_DATABASE_URL` names a server the tests may create databases
//! on (tools/gate.sh starts one); otherwise each test says so and returns.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use singularr::api::routes;
use singularr::api::state::{AppState, Inner};
use singularr::core::auth::{self, Caller};
use singularr::core::config::Config;
use singularr::core::db;
use singularr::core::journal::Journal;
use singularr::core::model::{NewApiKey, Scope};
use singularr::core::outbound::recorded::Recorded;
use singularr::core::outbound::{Allowlist, Outbound};
use singularr::engines::service::EngineSet;
use singularr::homemovies::service::HomeMovies;
use singularr::metadata::service::ProviderSet;
use singularr::music::service::MusicLibrary;
use singularr::policy::saved::{self, Change, Origin, SaveError};
use singularr::policy::{ActivePolicy, PolicyCell, PolicySource};
use singularr::requests::service::Requests;
use singularr::settings::Settings;
use singularr_core::profile::{List, ListKind, Model, Stance};
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
        let name = format!("mq_{test}_{nanos:x}");
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

fn env<'a>(extra: &[(&'a str, &'a str)]) -> BTreeMap<&'a str, &'a str> {
    let mut env = BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("SINGULARR_OWNER_EMAILS", "owner@example.com"),
    ]);
    env.extend(extra.iter().copied());
    env
}

const OWNER: &str = "user:owner@example.com";

fn what<'a>(action: &'a str, subject: &'a str) -> Change<'a> {
    Change {
        action,
        subject,
        actor: OWNER,
    }
}

fn group_list(id: &str, name: &str, members: &[&str]) -> List {
    List {
        id: id.to_string(),
        name: name.to_string(),
        kind: ListKind::Group,
        members: members.iter().map(|m| m.to_string()).collect(),
        ..List::default()
    }
}

#[tokio::test]
async fn every_save_is_a_version_and_an_earlier_one_can_be_restored() {
    let Some(t) = TestDb::new("profiles_versions").await else {
        return;
    };
    let journal = Journal::new();
    let config = Config::from(&env(&[])).unwrap();
    let file = ActivePolicy::builtin();
    let seed = saved::imported(&file.policy, &config, None);
    let (_, v1, _) = saved::change(
        &t.pool,
        &journal,
        what("seeded", "policy"),
        || seed.clone(),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(v1.version, 1);
    assert_eq!(v1.model.profiles.len(), file.policy.lanes.len());

    let (_, v2, active) = saved::change(
        &t.pool,
        &journal,
        what("list_saved", "list:tier-1"),
        Model::default,
        |m| {
            m.lists.push(group_list("tier-1", "Tier 1", &["AAA"]));
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(v2.version, 2);
    assert_eq!(active.source, PolicySource::Saved(2));
    assert!(active.policy.traits.iter().any(|t| t.id == "list.tier-1"));
    assert!(v2.model.profiles.iter().all(|p| {
        p.lists
            .ranked
            .iter()
            .any(|x| x.key == "tier-1" && x.stance == Stance::Allow)
    }));

    // Undo: version 1 again, as version 3.
    let (v3, _) = saved::restore(&t.pool, &journal, 1, OWNER).await.unwrap();
    assert_eq!(v3.version, 3);
    assert_eq!(v3.model, v1.model);
    let latest = saved::latest(&t.pool).await.unwrap().unwrap();
    assert_eq!((latest.version, latest.action.as_str()), (3, "restored"));
    assert_eq!(latest.subject, "version:1");
    assert!(matches!(
        saved::restore(&t.pool, &journal, 99, OWNER).await,
        Err(SaveError::NotFound(_))
    ));

    // A change that does not compile is not saved.
    let refused = saved::change(
        &t.pool,
        &journal,
        what("list_saved", "list:tier-1"),
        Model::default,
        |m| {
            m.lists.push(group_list("tier-1", "Tier 1 again", &["BBB"]));
            m.lists.push(group_list("tier-1", "Tier 1 twice", &["CCC"]));
            Ok(())
        },
    )
    .await;
    assert!(matches!(refused, Err(SaveError::Invalid(_))), "{refused:?}");
    assert_eq!(saved::latest(&t.pool).await.unwrap().unwrap().version, 3);

    // One journal event per save, naming its version.
    let events: Vec<(String, serde_json::Value)> = sqlx::query_as(
        "SELECT subject_id, body FROM events WHERE kind = 'policy_model_saved' ORDER BY id",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2].0, "3");
    assert_eq!(events[2].1["action"], "restored");
    assert_eq!(events[2].1["previous"], 2);

    // Putting a version in force swaps the policy every reader gets next.
    let cell = PolicyCell::handle(file.clone());
    let loaded = saved::put_in_force(&t.pool, &journal, &cell, active)
        .await
        .unwrap();
    assert!(loaded.is_some());
    assert_eq!(cell.current().source, PolicySource::Saved(2));
    t.drop_db().await;
}

#[tokio::test]
async fn the_source_is_the_file_while_one_is_pinned_and_else_the_saved_model() {
    let Some(t) = TestDb::new("profiles_source").await else {
        return;
    };
    let journal = Journal::new();
    let file = ActivePolicy::builtin();

    // SINGULARR_POLICY pins a file: it stays in force, and nothing is saved.
    let pinned = env(&[("SINGULARR_POLICY", "/nonexistent/policy.toml")]);
    let config = Config::from(&pinned).unwrap();
    let settings = Settings::new(&config, &pinned, None);
    assert_eq!(saved::origin(&settings), Origin::File);
    let active = saved::starting_policy(&t.pool, &journal, &settings, &config, file.clone())
        .await
        .unwrap();
    assert_eq!(active.source, PolicySource::Builtin);
    assert!(saved::latest(&t.pool).await.unwrap().is_none());

    // No file pinned: the saved model, which starts as the file imported.
    let open = env(&[]);
    let config = Config::from(&open).unwrap();
    let settings = Settings::new(&config, &open, None);
    assert_eq!(saved::origin(&settings), Origin::Saved);
    let first = saved::starting_policy(&t.pool, &journal, &settings, &config, file.clone())
        .await
        .unwrap();
    assert_eq!(first.source, PolicySource::Saved(1));
    assert_eq!(first.policy.lanes.len(), file.policy.lanes.len());
    let again = saved::starting_policy(&t.pool, &journal, &settings, &config, file.clone())
        .await
        .unwrap();
    assert_eq!(
        (again.source, again.hash),
        (PolicySource::Saved(1), first.hash)
    );

    // The environment can fix the source either way.
    let fixed = env(&[
        ("SINGULARR_POLICY", "/nonexistent/policy.toml"),
        ("SINGULARR_POLICY_SOURCE", "saved"),
    ]);
    let config = Config::from(&fixed).unwrap();
    let settings = Settings::new(&config, &fixed, None);
    assert_eq!(saved::origin(&settings), Origin::Saved);
    t.drop_db().await;
}

const OWNER_HEADER: (&str, &str) = ("x-auth-request-email", "owner@example.com");

/// The application state over a test database, with the built-in policy in force.
fn app_state(pool: &PgPool, journal: Journal, extra: &[(&'static str, &'static str)]) -> AppState {
    let env = env(extra);
    let config = Config::from(&env).unwrap();
    let out = Outbound::with_transport(Allowlist::with_metadata_providers(), Recorded::new());
    let settings = Arc::new(Settings::new(&config, &env, None));
    AppState::new(Inner {
        engines: Arc::new(EngineSet::from_config(&config, &out)),
        metadata: Arc::new(ProviderSet::from_config(&config, &out).with_locale(settings.locale())),
        owners: singularr::core::identity::Allowlist::parse(&config.owner_emails),
        requests: Arc::new(Requests::new(&config.request_rules)),
        music: Arc::new(MusicLibrary::new(&config.music.root)),
        home_movies: Arc::new(HomeMovies::new(&config.home_movies.root)),
        settings,
        pages: Default::default(),
        search: Arc::new(singularr::search::Search::new(
            pool.clone(),
            &config.search,
            &out,
        )),
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

/// A request to the router: the status, the body as JSON (null when it is not), and as text.
async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value, String) {
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
    let text = String::from_utf8_lossy(&bytes).to_string();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        text,
    )
}

async fn reader_key(pool: &PgPool, journal: &Journal) -> String {
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let created = auth::create_key(
        pool,
        journal,
        &owner,
        &NewApiKey {
            name: "pages".into(),
            scopes: vec![Scope::Read, Scope::Settings],
        },
    )
    .await
    .unwrap();
    format!("Bearer {}", created.secret)
}

fn product_policy() -> &'static str {
    Box::leak(format!("{}/../rules/policy.toml", env!("CARGO_MANIFEST_DIR")).into_boxed_str())
}

#[tokio::test]
async fn the_routes_read_change_and_refuse_as_the_source_says() {
    let Some(t) = TestDb::new("profiles_routes").await else {
        return;
    };
    let journal = Journal::new();
    let file = product_policy();
    let state = app_state(&t.pool, journal.clone(), &[("SINGULARR_POLICY", file)]);
    let app = routes::router(state.clone());
    let owner = [OWNER_HEADER];

    // A pinned file: read into the model, read-only.
    let (status, view, _) = call(&app, "GET", "/api/v1/profiles", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["source"]["kind"], "file");
    assert_eq!(view["source"]["fileEnv"], "SINGULARR_POLICY");
    assert_eq!(view["editable"], false);
    let profiles = view["profiles"].as_array().unwrap();
    assert!(!profiles.is_empty());
    assert!(
        profiles
            .iter()
            .all(|p| p["summary"].as_str().is_some_and(|s| !s.is_empty()))
    );
    assert!(view["options"]["groups"].as_array().unwrap().len() >= 7);
    let first = profiles[0]["id"].as_str().unwrap().to_string();

    // A change is refused with a problem that names the file.
    let (status, problem, _) = call(
        &app,
        "PUT",
        &format!("/api/v1/profiles/{first}"),
        &owner,
        Some(profiles[0].clone()),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{problem}");
    assert!(
        problem["detail"].as_str().unwrap().contains(file),
        "{problem}"
    );

    // Only the owner changes anything; a key that reads may not import.
    let key = reader_key(&t.pool, &journal).await;
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/v1/policy/import",
        &[("authorization", key.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = call(
        &app,
        "GET",
        "/api/v1/profiles",
        &[("authorization", key.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // A dry run saves nothing; it says how many lists each way of gathering them makes and what
    // that changes on the library's files (none here).
    let (status, tiers, _) = call(
        &app,
        "POST",
        "/api/v1/policy/import?dryRun=true",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tiers}");
    assert_eq!(
        (tiers["dryRun"].clone(), tiers["mode"].clone()),
        (json!(true), json!("tiers"))
    );
    assert_eq!(
        (tiers["releases"].clone(), tiers["acceptChanged"].clone()),
        (json!(0), json!(0))
    );
    let (status, exact, _) = call(
        &app,
        "POST",
        "/api/v1/policy/import?mode=exact&dryRun=true",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exact}");
    assert_eq!(exact["mode"], "exact");
    assert!(exact["lists"].as_u64().unwrap() >= tiers["lists"].as_u64().unwrap());
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/v1/policy/import?mode=closest",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, view, _) = call(&app, "GET", "/api/v1/profiles", &owner, None).await;
    assert_eq!(view["source"]["kind"], "file");

    // Import: the file becomes version 1 of the saved model, which is the source from now on.
    let (status, imported, _) = call(&app, "POST", "/api/v1/policy/import", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["version"], 1);
    assert_eq!(imported["source"]["kind"], "saved");
    assert_eq!(imported["source"]["settingSource"], "saved");
    let (_, view, _) = call(&app, "GET", "/api/v1/profiles", &owner, None).await;
    assert_eq!(
        (view["editable"].clone(), view["version"].clone()),
        (json!(true), json!(1))
    );
    let (_, policy, _) = call(&app, "GET", "/api/v1/policy", &owner, None).await;
    assert_eq!(policy["source"], "saved:1");

    // A list: members trimmed, once each; every profile has it, allowed.
    let (status, list, _) = call(
        &app,
        "POST",
        "/api/v1/lists",
        &owner,
        Some(json!({"name": "Tier 1", "kind": "group", "members": ["AAA", " AAA ", "", "BBB"]})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{list}");
    assert_eq!(list["id"], "tier-1");
    assert_eq!(list["members"], json!(["AAA", "BBB"]));
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/v1/lists",
        &owner,
        Some(json!({"members": ["X"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // The profile ranks the list first; its line says so; the compiled numbers come on request.
    let (_, profile, _) = call(
        &app,
        "GET",
        &format!("/api/v1/profiles/{first}"),
        &owner,
        None,
    )
    .await;
    assert!(profile["compiled"]["ladder"].is_array(), "{profile}");
    let mut edited = profile.clone();
    let ranked = edited["lists"]["ranked"].as_array_mut().unwrap();
    for pick in ranked.iter_mut() {
        if pick["key"] == "tier-1" {
            pick["stance"] = json!("prefer");
        }
    }
    let (status, saved, _) = call(
        &app,
        "PUT",
        &format!("/api/v1/profiles/{first}"),
        &owner,
        Some(edited.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(
        saved["summary"].as_str().unwrap().contains("Tier 1 first"),
        "{saved}"
    );
    assert_eq!(saved["rev"], profile["rev"].as_i64().unwrap() + 1);

    // Validation: Stop at below Accept from; an option no group has.
    let mut wrong = edited.clone();
    wrong["target"]["sources"] = json!([["Remux"], ["BluRay"], ["WebDl"], ["WebRip"], ["Tv"]]);
    wrong["target"]["acceptFrom"] = json!("BluRay");
    wrong["target"]["stopAt"] = json!("WebDl");
    let (status, problem, _) = call(
        &app,
        "PUT",
        &format!("/api/v1/profiles/{first}"),
        &owner,
        Some(wrong),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    assert!(
        problem["detail"].as_str().unwrap().contains("Stop at"),
        "{problem}"
    );
    let (status, _, _) = call(
        &app,
        "POST",
        "/api/v1/profiles",
        &owner,
        Some(json!({"name": "Odd", "picture": {"range": [{"key": "nope", "stance": "prefer"}]}})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A new profile from a name alone, and a copy of it.
    let (status, created, _) = call(
        &app,
        "POST",
        "/api/v1/profiles",
        &owner,
        Some(json!({"name": "Movies 4K", "target": {"kind": "movie", "resolution": "2160p"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["id"], "movies-4k");
    assert_eq!(created["target"]["resolution"], "2160p");
    let sdr = created["picture"]["range"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "sdr")
        .unwrap();
    assert_eq!(sdr["stance"], "avoid");
    let (status, copy, _) = call(
        &app,
        "POST",
        "/api/v1/profiles/movies-4k/duplicate",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{copy}");
    assert_eq!(copy["id"], "movies-4k-copy");
    let (status, _, _) = call(
        &app,
        "DELETE",
        "/api/v1/profiles/movies-4k-copy",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // The preview ranks under the draft, with the reasons in the profile's words.
    let (status, preview, _) = call(
        &app,
        "POST",
        "/api/v1/profiles/preview",
        &owner,
        Some(json!({
            "profile": edited,
            "releases": ["Movie.2020.1080p.BluRay.x264-NOBODY", "Movie.2020.1080p.BluRay.x264-BBB", "Movie.2020.HDCAM.x264-BBB"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    let ranking = preview["ranking"].as_array().unwrap();
    assert_eq!(ranking.len(), 3);
    assert!(
        ranking[0]["name"].as_str().unwrap().ends_with("-BBB"),
        "{preview}"
    );
    let reasons: Vec<&str> = ranking[0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["text"].as_str().unwrap())
        .collect();
    assert!(reasons.contains(&"Release groups: Tier 1"), "{reasons:?}");
    assert_eq!(ranking[2]["accepted"], false);

    // Unlisted: what the download client holds that no list names.
    sqlx::query(
        "INSERT INTO downloads (hash, name, state) VALUES
           ('h1', 'Movie.2020.1080p.WEB-DL.DDP5.1.H.264-SMURF', 'downloading'),
           ('h2', 'Movie.2021.1080p.WEB-DL.DDP5.1.H.264-AAA', 'downloading')",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let (status, unlisted, _) = call(&app, "GET", "/api/v1/lists/unlisted", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{unlisted}");
    let names: Vec<&str> = unlisted["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"SMURF") && !names.contains(&"AAA"),
        "{names:?}"
    );
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/v1/lists/tier-1",
        &owner,
        Some(json!({"name": "Tier 1", "kind": "group", "members": ["AAA", "BBB", "SMURF"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, unlisted, _) = call(&app, "GET", "/api/v1/lists/unlisted", &owner, None).await;
    assert!(
        !unlisted["groups"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["name"] == "SMURF"),
        "{unlisted}"
    );
    let (_, lists, _) = call(&app, "GET", "/api/v1/lists", &owner, None).await;
    let tier = &lists["lists"][0];
    assert_eq!(tier["usedBy"][0]["profile"], first.as_str(), "{lists}");

    // Versions: every change is one; restoring 1 undoes them; the export loads as a policy.
    let (_, versions, _) = call(&app, "GET", "/api/v1/policy/versions", &owner, None).await;
    let list = versions["versions"].as_array().unwrap();
    assert!(list.len() >= 6, "{versions}");
    assert_eq!(list[0]["inForce"], true);
    let (status, restored, _) = call(
        &app,
        "POST",
        "/api/v1/policy/versions/1/restore",
        &owner,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{restored}");
    let (_, profile, _) = call(
        &app,
        "GET",
        &format!("/api/v1/profiles/{first}"),
        &owner,
        None,
    )
    .await;
    assert!(
        !profile["summary"].as_str().unwrap().contains("Tier 1"),
        "{profile}"
    );
    let (status, _, text) = call(&app, "GET", "/api/v1/policy/export", &owner, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(singularr_core::Policy::from_toml(&text).is_ok(), "{text}");

    // Back to the file: the policy in force follows the setting at once, and changes are
    // refused again.
    let (status, _, _) = call(
        &app,
        "PATCH",
        "/api/v1/settings",
        &owner,
        Some(json!({"values": {"system.policySource": "file"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, policy, _) = call(&app, "GET", "/api/v1/policy", &owner, None).await;
    assert!(
        policy["source"].as_str().unwrap().starts_with("file:"),
        "{policy}"
    );
    let (status, _, _) = call(&app, "DELETE", "/api/v1/lists/tier-1", &owner, None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    t.drop_db().await;
}

#[tokio::test]
async fn naming_is_saved_with_a_version_and_previewed_on_the_library_s_files() {
    let Some(t) = TestDb::new("profiles_naming").await else {
        return;
    };
    let journal = Journal::new();
    let state = app_state(
        &t.pool,
        journal.clone(),
        &[("SINGULARR_POLICY", product_policy())],
    );
    let app = routes::router(state.clone());
    let owner = [OWNER_HEADER];

    // One movie file of the library, named the detailed way.
    let name = "10 Cloverfield Lane (2016) {imdb-tt1179933} [Bluray-1080p][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv";
    let held = serde_json::to_value(singularr_core::naming::parse_library_name(name).to_release())
        .unwrap();
    sqlx::query(
        "INSERT INTO libraries (id, name) VALUES ('test-movies', 'Movies') ON CONFLICT DO NOTHING",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let title: i64 = sqlx::query_scalar(
        "INSERT INTO titles (ext_key, kind, tmdb_id, imdb_id, title, sort_title, year)
         VALUES ('movie:tmdb:333371', 'movie', 333371, 'tt1179933', '10 Cloverfield Lane', '10 cloverfield lane', 2016)
         RETURNING id",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO copies (title_id, library_id, engine, engine_title_id, engine_file_id, relative_path,
                             release_name, name_source, parsed, held)
         VALUES ($1, 'test-movies', 'radarr', 1, 1, $2, $2, 'file', $3, $3)",
    )
    .bind(title)
    .bind(name)
    .bind(&held)
    .execute(&t.pool)
    .await
    .unwrap();

    let (status, naming, _) = call(&app, "GET", "/api/v1/naming", &owner, None).await;
    assert_eq!(status, StatusCode::OK, "{naming}");
    assert_eq!(naming["conventions"].as_array().unwrap().len(), 5);
    assert!(naming["presets"].as_array().unwrap().len() >= 5);
    let detailed = naming["conventions"][1]["template"]
        .as_str()
        .unwrap()
        .to_string();

    // The library's file is named the detailed way; the Plex way differs.
    let (status, preview, _) = call(
        &app,
        "POST",
        "/api/v1/naming/preview",
        &owner,
        Some(json!({"kind": "movieFile", "template": detailed})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["mismatches"]["total"], 1);
    assert_eq!(preview["mismatches"]["count"], 0, "{preview}");
    assert_eq!(preview["items"][0]["matches"], true);
    let plex = "{Title} ({Year}) {edition-{Edition}}";
    let (_, preview, _) = call(
        &app,
        "POST",
        "/api/v1/naming/preview",
        &owner,
        Some(json!({"kind": "movieFile", "template": plex, "titles": ["movie-333371"]})),
    )
    .await;
    assert_eq!(preview["mismatches"]["count"], 1);
    assert_eq!(preview["items"][0]["result"], "10 Cloverfield Lane (2016)");

    // Saved by the owner, with a version, while the policy file stays in force.
    let (status, problem, _) = call(
        &app,
        "PUT",
        "/api/v1/naming/movieFile",
        &owner,
        Some(json!({"template": "{Titel} ({Year})"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{problem}");
    let (status, saved, _) = call(
        &app,
        "PUT",
        "/api/v1/naming/movieFile",
        &owner,
        Some(json!({"template": plex, "preset": "plex"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        (saved["preset"].clone(), saved["version"].clone()),
        (json!("plex"), json!(1))
    );
    let (_, naming, _) = call(&app, "GET", "/api/v1/naming", &owner, None).await;
    assert_eq!(naming["conventions"][1]["template"], plex);
    let (_, policy, _) = call(&app, "GET", "/api/v1/policy", &owner, None).await;
    assert!(
        !policy["source"].as_str().unwrap().starts_with("saved"),
        "{policy}"
    );
    let (status, _, _) = call(
        &app,
        "PUT",
        "/api/v1/naming/nothing",
        &owner,
        Some(json!({"template": plex})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    t.drop_db().await;
}
