//! The platform's tests that need a PostgreSQL: API keys, the journal, the migration runner, the
//! settings, the access book, the job runtime and the markers.
//!
//! They run only when `SINGULARR_TEST_DATABASE_URL` names a server the tests may create
//! databases on. Without it each test says so and passes. The gate (`tools/gate.sh`) starts a
//! throwaway PostgreSQL 18 and runs them with the workspace and on their own. Every test
//! creates a database of its own (`platform_<test>_<id>`) and drops it afterwards.
//!
//! The platform has no migrations of its own. Its tables come from the app's
//! (`app/migrations`), read from the repository when the tests start.

use async_trait::async_trait;
use serde_json::json;
use singularr_platform::access::{AccessBook, Action, Listed};
use singularr_platform::auth::{self, Caller};
use singularr_platform::db;
use singularr_platform::identity::Allowlist;
use singularr_platform::journal::{Journal, event};
use singularr_platform::markers;
use singularr_platform::model::{EventQuery, NewApiKey, Paging, Scope};
use singularr_platform::outbound::{Access, Allowlist as Hosts};
use singularr_platform::runtime::{
    self, Cx, Done, Failed, Handler, Lease, Retry, RunNow, Runtime, Schedule, Source, Timing,
};
use singularr_platform::settings::model::SettingsPatch;
use singularr_platform::settings::registry::{Fallback, Field, Group, Kind, Section};
use singularr_platform::settings::{Described, PatchError, Settings};
use singularr_platform::store;
use sqlx::PgPool;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, oneshot};

/// The app's migrations, where the platform's tables are created.
const MIGRATIONS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../app/migrations");

/// The app's migrations as a migrator, read once.
async fn migrator() -> &'static Migrator {
    static MIGRATOR: OnceLock<&'static Migrator> = OnceLock::new();
    if let Some(m) = MIGRATOR.get() {
        return m;
    }
    let read = Migrator::new(Path::new(MIGRATIONS))
        .await
        .expect("the app's migrations are read");
    MIGRATOR.get_or_init(|| Box::leak(Box::new(read)))
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
        db::migrate(&t.pool, migrator().await)
            .await
            .expect("the migrations apply");
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
        let name = format!("platform_{}_{}", test, uuid_like());
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

async fn kinds(pool: &PgPool, after: i64) -> Vec<String> {
    store::events_after(pool, after, 10_000)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.kind)
        .collect()
}

/// The bodies of the journal's events of one kind, oldest first.
async fn bodies(pool: &PgPool, kind: &str) -> Vec<serde_json::Value> {
    sqlx::query_scalar("SELECT body FROM events WHERE kind = $1 ORDER BY id")
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// The runner over the app's migrations: a database without them is not ready and lists each
/// one as pending. After the run none is pending and the check passes. A second run changes
/// nothing.
#[tokio::test]
async fn the_runner_applies_the_migrations_it_is_given_and_reports_ready() {
    let Some(t) = TestDb::bare("runner").await else {
        return;
    };
    let migrator = migrator().await;
    let pending = db::pending_migrations(&t.pool, migrator).await.unwrap();
    assert_eq!(pending.len(), migrator.iter().count());
    let ready = db::ready_check(t.pool.clone(), migrator);
    assert!(ready().await.is_err(), "not ready before the migrations");
    db::migrate(&t.pool, migrator).await.unwrap();
    assert!(
        db::pending_migrations(&t.pool, migrator)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(ready().await.is_ok());
    db::migrate(&t.pool, migrator).await.unwrap();
    assert!(ready().await.is_ok());
    t.drop_db().await;
}

/// The journal: events appended in a transaction are stored and published after its commit,
/// in id order. A transaction dropped without a commit stores and publishes nothing. The store
/// reads the events back after an id, by kind, by subject and as a title's history.
#[tokio::test]
async fn the_journal_publishes_what_commits_in_id_order() {
    let Some(t) = TestDb::new("journal").await else {
        return;
    };
    let journal = Journal::new();
    let mut live = journal.subscribe();

    let mut tx = journal.begin(&t.pool).await.unwrap();
    let added = tx
        .append_one(event(
            "title_added",
            "title",
            7,
            "sync:radarr",
            json!({"name": "Alien"}),
        ))
        .await
        .unwrap();
    let copy = tx
        .append_one(
            event(
                "copy_added",
                "copy",
                12,
                "sync:radarr",
                json!({"title_id": 7}),
            )
            .caused_by(Some(added.id)),
        )
        .await
        .unwrap();
    assert_eq!(tx.pending(), 2);
    assert!(
        live.try_recv().is_err(),
        "nothing is published before the commit"
    );
    tx.commit().await.unwrap();
    let first = live.try_recv().unwrap();
    let second = live.try_recv().unwrap();
    assert_eq!((first.id, second.id), (added.id, copy.id));
    assert!(added.id < copy.id);
    assert_eq!(second.cause, Some(added.id));

    // Dropped without a commit: nothing is stored and nothing is published.
    let mut tx = journal.begin(&t.pool).await.unwrap();
    tx.append_one(event("title_removed", "title", 7, "sync:radarr", json!({})))
        .await
        .unwrap();
    drop(tx);
    assert!(live.try_recv().is_err());
    assert_eq!(store::count_events(&t.pool).await.unwrap(), 2);
    assert_eq!(store::latest_event_id(&t.pool).await.unwrap(), copy.id);

    let after = store::events_after(&t.pool, added.id, 10).await.unwrap();
    assert_eq!(
        after.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![copy.id]
    );
    let all = Paging::new(None, None);
    let by_kind = EventQuery {
        kind: Some("copy_added".into()),
        ..EventQuery::default()
    };
    let (copies, total) = store::list_events(&t.pool, &by_kind, all).await.unwrap();
    assert_eq!((copies.len(), total), (1, 1));
    // A title's history holds its copies too.
    let of_title = EventQuery {
        subject: Some("title:7".into()),
        ..EventQuery::default()
    };
    let (history, total) = store::list_events(&t.pool, &of_title, all).await.unwrap();
    assert_eq!((history.len(), total), (2, 2));
    let subject = store::subject_events(&t.pool, "copy", &["12".to_string()], 10)
        .await
        .unwrap();
    assert_eq!(
        subject.iter().map(|e| e.id).collect::<Vec<_>>(),
        vec![copy.id]
    );
    assert_eq!(kinds(&t.pool, 0).await, vec!["title_added", "copy_added"]);
    t.drop_db().await;
}

#[tokio::test]
async fn keys_are_hashed_scoped_and_revocable() {
    let Some(t) = TestDb::new("keys").await else {
        return;
    };
    let journal = Journal::new();
    let owners = Allowlist::parse("owner@example.com");
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let created = auth::create_key(
        &t.pool,
        &journal,
        &owner,
        &NewApiKey {
            name: "reader".into(),
            scopes: vec![Scope::Read, Scope::Read],
        },
    )
    .await
    .unwrap();
    assert!(created.secret.starts_with("mq_"));
    assert_eq!(created.key.scopes, vec![Scope::Read]);
    let stored: (String, String) =
        sqlx::query_as("SELECT prefix, sha256 FROM api_keys WHERE id = $1")
            .bind(created.key.id)
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(stored.0, created.key.prefix);
    assert_ne!(stored.1, created.secret, "only the hash is stored");
    assert!(auth::key_matches(&created.secret, &stored.1));
    assert!(
        auth::create_key(
            &t.pool,
            &journal,
            &owner,
            &NewApiKey {
                name: " ".into(),
                scopes: vec![Scope::Read]
            }
        )
        .await
        .is_err()
    );
    assert!(
        auth::create_key(
            &t.pool,
            &journal,
            &owner,
            &NewApiKey {
                name: "x".into(),
                scopes: vec![]
            }
        )
        .await
        .is_err()
    );

    let bearer = |secret: &str| {
        let mut h = axum::http::HeaderMap::new();
        h.insert("authorization", format!("Bearer {secret}").parse().unwrap());
        h
    };
    let caller = auth::identify(&bearer(&created.secret), &t.pool, &owners)
        .await
        .unwrap();
    assert_eq!(
        caller,
        Caller::Key {
            id: created.key.id,
            name: "reader".into(),
            scopes: vec![Scope::Read]
        }
    );
    assert!(
        auth::list_keys(&t.pool).await.unwrap()[0]
            .last_used
            .is_some(),
        "use is recorded"
    );
    // A key with the right prefix and a wrong tail, and one that never existed.
    let forged = format!(
        "{}{}",
        &created.secret[..20],
        "0".repeat(created.secret.len() - 20)
    );
    assert!(
        auth::identify(&bearer(&forged), &t.pool, &owners)
            .await
            .is_err()
    );
    assert!(
        auth::identify(&bearer("mq_00000000000000000000"), &t.pool, &owners)
            .await
            .is_err()
    );
    assert!(
        auth::identify(&bearer("not-a-key"), &t.pool, &owners)
            .await
            .is_err()
    );

    auth::revoke_key(&t.pool, &journal, &owner, created.key.id)
        .await
        .unwrap();
    assert!(
        auth::identify(&bearer(&created.secret), &t.pool, &owners)
            .await
            .is_err(),
        "a revoked key is refused"
    );
    assert!(
        auth::revoke_key(&t.pool, &journal, &owner, created.key.id)
            .await
            .is_err()
    );
    assert_eq!(kinds(&t.pool, 0).await, vec!["key_created", "key_revoked"]);
    let events = store::events_after(&t.pool, 0, 10).await.unwrap();
    assert!(
        !events[0].body.to_string().contains(&created.secret),
        "the secret is not journaled"
    );
    t.drop_db().await;
}

/// A configuration of two fields of its own, for the settings' tests.
#[derive(Clone)]
struct Sample;

static SAMPLE: &[Section<Sample>] = &[Section {
    id: "general",
    label: "General",
    summary: "The sample's fields",
    groups: &[Group {
        id: "general",
        label: "General",
        help: "",
        fields: &[
            Field::new("general.name", "Name", Kind::Text, "The workspace's name")
                .by_default(Fallback::Text("Singularr"))
                .saves(),
            Field::new(
                "general.fixed",
                "Fixed",
                Kind::Text,
                "A value the environment fixes",
            )
            .env(&["SAMPLE_FIXED"])
            .saves(),
            Field::new(
                "general.slow",
                "Slow",
                Kind::Text,
                "A value read at the start",
            )
            .by_default(Fallback::Text("Slow"))
            .saves()
            .restart(),
        ],
    }],
    panels: &[],
}];

impl Described for Sample {
    fn sections() -> &'static [Section<Sample>] {
        SAMPLE
    }

    fn instance_path(&self) -> Option<&str> {
        None
    }
}

/// The settings over the fields a configuration describes: a save is stored with a
/// `setting_changed` event that says what changed. The next process reads it back. A value the
/// environment fixes is refused and stays the variable's. A reset goes back to the default.
#[tokio::test]
async fn a_saved_setting_is_journaled_and_read_back() {
    let Some(t) = TestDb::new("settings").await else {
        return;
    };
    let journal = Journal::new();
    let env = BTreeMap::from([("SAMPLE_FIXED", "from the environment")]);
    let owner = Caller::Owner {
        email: "owner@example.com".into(),
    };
    let patch = |key: &str, value: serde_json::Value| SettingsPatch {
        values: BTreeMap::from([(key.to_string(), value)]),
        scope: None,
    };
    let settings = Settings::new(&Sample, &env, None);
    assert_eq!(settings.value("general.name"), json!("Singularr"));
    let saved = settings
        .patch(
            &t.pool,
            &journal,
            &owner,
            &patch("general.name", json!("Home")),
        )
        .await
        .unwrap();
    assert_eq!(saved.saved, vec!["general.name"]);
    assert_eq!(settings.value("general.name"), json!("Home"));
    assert_eq!(kinds(&t.pool, 0).await, vec!["setting_changed"]);
    let name = |from: &str, to: &str, reset: bool| {
        json!({
            "key": "general.name",
            "label": "Name",
            "scope": "instance",
            "from": from,
            "to": to,
            "reset": reset,
            "restart": false,
        })
    };
    assert_eq!(
        bodies(&t.pool, "setting_changed").await,
        vec![name("Singularr", "Home", false)]
    );

    let next = Settings::new(&Sample, &env, None);
    assert_eq!(next.reload(&t.pool).await.unwrap(), 1);
    assert_eq!(next.value("general.name"), json!("Home"));

    let refused = settings
        .patch(
            &t.pool,
            &journal,
            &owner,
            &patch("general.fixed", json!("x")),
        )
        .await;
    assert!(matches!(refused, Err(PatchError::Refused(_))));
    assert_eq!(
        settings.value("general.fixed"),
        json!("from the environment")
    );
    assert_eq!(
        kinds(&t.pool, 0).await.len(),
        1,
        "nothing more is journaled"
    );

    // A field read at the start says so in its event. A reset goes back to the default.
    settings
        .patch(
            &t.pool,
            &journal,
            &owner,
            &patch("general.slow", json!("Fast")),
        )
        .await
        .unwrap();
    settings
        .patch(
            &t.pool,
            &journal,
            &owner,
            &patch("general.name", serde_json::Value::Null),
        )
        .await
        .unwrap();
    assert_eq!(settings.value("general.name"), json!("Singularr"));
    assert_eq!(
        bodies(&t.pool, "setting_changed").await,
        vec![
            name("Singularr", "Home", false),
            json!({
                "key": "general.slow",
                "label": "Slow",
                "scope": "instance",
                "from": "Slow",
                "to": "Fast",
                "reset": false,
                "restart": true,
            }),
            name("Home", "Singularr", true),
        ]
    );
    t.drop_db().await;
}

/// The access book over a list of integrations: an action saved at `act` acts, with one
/// `integration_access_changed` event. A row saved for every action (`*`) reads as every
/// action acting and is written again as one row per action.
#[tokio::test]
async fn the_access_book_saves_each_action_and_expands_an_old_row() {
    let Some(t) = TestDb::new("access").await else {
        return;
    };
    static ACTIONS: &[Action] = &[
        Action {
            id: "grab",
            label: "Grab a release",
        },
        Action {
            id: "remove",
            label: "Remove a download",
        },
    ];
    sqlx::query(
        "INSERT INTO integration_access (integration, action, access, saved_by)
         VALUES ('old', '*', 'act', 'user:owner@example.com')",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let listed = |id: &str| Listed {
        id: id.to_string(),
        name: id.to_uppercase(),
        actions: ACTIONS,
        fixed: None,
    };
    let book = AccessBook::new(vec![listed("manager"), listed("old")], &Hosts::default());
    let journal = Journal::new();
    assert_eq!(book.load(&t.pool).await.unwrap(), 1);
    assert_eq!(book.of("old").acting, vec!["grab", "remove"]);
    let changed = book
        .set_actions(
            &t.pool,
            &journal,
            "user:owner@example.com",
            "manager",
            &[("grab".to_string(), Access::Act)],
        )
        .await
        .unwrap();
    assert_eq!(changed, 1);
    assert!(book.allows("manager", "grab") && !book.allows("manager", "remove"));
    assert_eq!(kinds(&t.pool, 0).await, vec!["integration_access_changed"]);
    assert_eq!(
        bodies(&t.pool, "integration_access_changed").await,
        vec![json!({
            "integration": "manager",
            "name": "MANAGER",
            "action": "grab",
            "label": "Grab a release",
            "from": "read",
            "to": "act",
        })]
    );
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT integration, action FROM integration_access ORDER BY integration, action",
    )
    .fetch_all(&t.pool)
    .await
    .unwrap();
    let rows: Vec<(&str, &str)> = rows.iter().map(|(i, a)| (i.as_str(), a.as_str())).collect();
    assert_eq!(
        rows,
        vec![("manager", "grab"), ("old", "grab"), ("old", "remove")]
    );
    t.drop_db().await;
}

// The job runtime ------------------------------------------------------------------------------

fn every(key: &str, kind: &str, queue: &str, secs: u64, lease: u64) -> Schedule {
    Schedule {
        key: key.to_string(),
        kind: kind.to_string(),
        integration: None,
        queue: queue.to_string(),
        every: Duration::from_secs(secs),
        floor: Duration::from_secs(1),
        first: Duration::ZERO,
        jitter: Duration::ZERO,
        on_start: false,
        lease: Duration::from_secs(lease),
        source: Source::Default,
    }
}

/// The runtime's waits, short enough for a test.
fn quick() -> Timing {
    Timing {
        tick: Duration::from_millis(100),
        poll: Duration::from_millis(200),
        reap: Duration::from_millis(200),
        lease: Duration::from_secs(5),
        grace: Duration::from_secs(1),
    }
}

/// Sets its flag when it is dropped while armed: a run dropped before its end.
struct Armed(Option<Arc<AtomicBool>>);

impl Armed {
    fn disarm(mut self) {
        self.0 = None;
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        if let Some(flag) = &self.0 {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

/// What a test handler does on one run.
#[derive(Debug, Clone, Copy)]
enum Act {
    /// It ends at once.
    Ok,
    /// It fails at once.
    Fail,
    /// It holds its job for an hour.
    Hold,
    Panic,
}

/// A handler that counts its runs. Run `n` does `script[n]`, the last act once the script is
/// through. `dropped` is set when a run is dropped before its end.
struct Counting {
    runs: AtomicUsize,
    script: Vec<Act>,
    dropped: Arc<AtomicBool>,
    retry: Retry,
    limit: Duration,
}

fn counting(script: &[Act]) -> Counting {
    Counting {
        runs: AtomicUsize::new(0),
        script: script.to_vec(),
        dropped: Arc::new(AtomicBool::new(false)),
        retry: Retry::default(),
        limit: Duration::from_secs(3600),
    }
}

impl Counting {
    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    fn dropped(&self) -> bool {
        self.dropped.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Handler for Counting {
    async fn run(&self, _cx: Cx) -> Result<Done, Failed> {
        let n = self.runs.fetch_add(1, Ordering::SeqCst);
        let armed = Armed(Some(self.dropped.clone()));
        let act = self.script[n.min(self.script.len() - 1)];
        let outcome = match act {
            Act::Ok => Ok(Done::from(format!("run {}", n + 1))),
            Act::Fail => Err(Failed::from(format!("run {} failed", n + 1))),
            Act::Hold => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(Done::from("held"))
            }
            Act::Panic => panic!("the run panics"),
        };
        armed.disarm();
        outcome
    }

    fn limit(&self) -> Duration {
        self.limit
    }

    fn retry(&self) -> Retry {
        self.retry
    }
}

/// Wait until `ready` holds, `within` at most.
async fn until(within: Duration, ready: impl Fn() -> bool) -> bool {
    let end = Instant::now() + within;
    while Instant::now() < end {
        if ready() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    ready()
}

/// Wait until a job is in this state, `within` at most.
async fn a_job_is(pool: &PgPool, state: &str, within: Duration) -> bool {
    let end = Instant::now() + within;
    loop {
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state = $1")
            .bind(state)
            .fetch_one(pool)
            .await
            .unwrap();
        if n > 0 {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until a schedule has failed this many runs in a row, `within` at most.
async fn failures_are(pool: &PgPool, key: &str, failures: i32, within: Duration) -> bool {
    let end = Instant::now() + within;
    loop {
        let now: i32 = sqlx::query_scalar("SELECT failures FROM job_schedules WHERE key = $1")
            .bind(key)
            .fetch_one(pool)
            .await
            .unwrap();
        if now == failures {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// When the only schedule runs next.
async fn next_run_at(pool: &PgPool) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar("SELECT next_run_at FROM job_schedules")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Two runs of one schedule never overlap. While its job is open the tick queues none. The
/// unique index refuses a second.
#[tokio::test]
async fn one_schedule_never_has_two_open_jobs() {
    let Some(t) = TestDb::new("jobs_one").await else {
        return;
    };
    let music = every("folder:music", "scan.music", "scan", 3600, 60);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    assert_eq!(
        runtime::store::queue_due(&t.pool).await.unwrap(),
        vec!["scan"]
    );
    sqlx::query("UPDATE job_schedules SET next_run_at = now()")
        .execute(&t.pool)
        .await
        .unwrap();
    let due = next_run_at(&t.pool).await;
    assert!(
        runtime::store::queue_due(&t.pool).await.unwrap().is_empty(),
        "due again while its job is queued"
    );
    assert_eq!(
        next_run_at(&t.pool).await,
        due,
        "the tick leaves a schedule with an open job as it is"
    );
    let second = sqlx::query(
        "INSERT INTO jobs (kind, queue, schedule_key) VALUES ('scan.music', 'scan', 'folder:music')",
    )
    .execute(&t.pool)
    .await;
    assert!(second.is_err(), "the unique index holds one open job");
    let open: i64 =
        sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state IN ('queued', 'running')")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(open, 1);
    t.drop_db().await;
}

/// Run-now starts a job within a second by the wake-up alone: the tick and the poll wait 30 s.
/// Asked in this process, the queue wakes at once. Asked from any process, the channel wakes it.
/// Asked again while the job runs, nothing more starts.
#[tokio::test]
async fn run_now_starts_within_a_second() {
    let Some(t) = TestDb::new("jobs_now").await else {
        return;
    };
    let music = every("folder:music", "scan.music", "scan", 3600, 60);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    sqlx::query("UPDATE job_schedules SET next_run_at = now() + interval '1 hour'")
        .execute(&t.pool)
        .await
        .unwrap();
    let slow = Timing {
        tick: Duration::from_secs(30),
        poll: Duration::from_secs(30),
        ..quick()
    };
    let scans = Arc::new(counting(&[Act::Ok, Act::Hold]));
    let running = Runtime::new(t.pool.clone(), slow)
        .handle("scan.music", scans.clone())
        .workers("scan", 1)
        .start();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(scans.runs(), 0, "not due");
    let asked = Instant::now();
    assert_eq!(
        running.run_now("folder:music").await.unwrap(),
        RunNow::Started
    );
    assert!(until(Duration::from_secs(1), || scans.runs() == 1).await);
    assert!(asked.elapsed() < Duration::from_secs(1));
    assert!(a_job_is(&t.pool, "done", Duration::from_secs(1)).await);
    let asked = Instant::now();
    assert_eq!(
        runtime::run_now(&t.pool, "folder:music").await.unwrap(),
        RunNow::Started
    );
    assert!(
        until(Duration::from_secs(1), || scans.runs() == 2).await,
        "the channel woke the queue"
    );
    assert!(asked.elapsed() < Duration::from_secs(1));
    assert_eq!(
        running.run_now("folder:music").await.unwrap(),
        RunNow::Running
    );
    assert_eq!(running.run_now("nothing").await.unwrap(), RunNow::Unknown);
    running.stop().await;
    t.drop_db().await;
}

/// The heartbeat keeps the lease of a long run: a run held for three leases stays `running` at
/// its first attempt while its worker lives.
#[tokio::test]
async fn a_long_run_keeps_its_lease() {
    let Some(t) = TestDb::new("jobs_beat").await else {
        return;
    };
    let music = every("folder:music", "scan.music", "scan", 3600, 1);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    let held = Arc::new(counting(&[Act::Hold]));
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", held.clone())
        .workers("scan", 1)
        .start();
    assert!(until(Duration::from_secs(2), || held.runs() == 1).await);
    tokio::time::sleep(Duration::from_millis(3500)).await;
    let (state, attempts): (String, i32) = sqlx::query_as("SELECT state, attempts FROM jobs")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(
        (state.as_str(), attempts, held.runs(), held.dropped()),
        ("running", 1, 1, false)
    );
    running.stop().await;
    t.drop_db().await;
}

/// A run whose lease is taken away is dropped at its next heartbeat. Its end is not written: the
/// schedule has no last run, the job no result.
#[tokio::test]
async fn a_run_whose_lease_is_taken_is_dropped() {
    let Some(t) = TestDb::new("jobs_taken").await else {
        return;
    };
    let music = every("folder:music", "scan.music", "scan", 3600, 1);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    let held = Arc::new(counting(&[Act::Hold]));
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", held.clone())
        .workers("scan", 1)
        .start();
    assert!(until(Duration::from_secs(2), || held.runs() == 1).await);
    sqlx::query("UPDATE jobs SET locked_by = 'x'")
        .execute(&t.pool)
        .await
        .unwrap();
    assert!(
        until(Duration::from_secs(2), || held.dropped()).await,
        "dropped at the next heartbeat"
    );
    let (last_run, result): (
        Option<chrono::DateTime<chrono::Utc>>,
        Option<serde_json::Value>,
    ) = sqlx::query_as(
        "SELECT s.last_run_at, j.result FROM job_schedules s
             JOIN jobs j ON j.schedule_key = s.key ORDER BY j.id LIMIT 1",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!((last_run, result), (None, None));
    running.stop().await;
    t.drop_db().await;
}

/// A handler that panics fails its job. Its worker goes on: the next run takes the same worker.
/// No task of the runtime starts again. Failed runs in a row back the schedule off. A 1 s
/// schedule then waits 1, 2, then 4 s. A success resets the count.
#[tokio::test]
async fn a_panicking_handler_fails_its_job_and_its_worker_goes_on() {
    let Some(t) = TestDb::new("jobs_panic").await else {
        return;
    };
    let index = every("singularr:index", "search.index", "index", 1, 60);
    Runtime::reconcile(&t.pool, &[index]).await.unwrap();
    let runs = Arc::new(counting(&[Act::Panic, Act::Fail, Act::Fail, Act::Ok]));
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("search.index", runs.clone())
        .workers("index", 1)
        .start();
    assert!(failures_are(&t.pool, "singularr:index", 3, Duration::from_secs(8)).await);
    let next_in: f64 = sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM next_run_at - now())::float8 FROM job_schedules",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert!(
        (3.0..4.5).contains(&next_in),
        "backed off to 4 s: {next_in}"
    );
    let first: Option<String> =
        sqlx::query_scalar("SELECT last_error FROM jobs ORDER BY id LIMIT 1")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(first.as_deref(), Some("the handler panicked"));
    assert_eq!(
        (runs.runs(), running.restarts()),
        (3, 0),
        "the worker went on"
    );

    assert_eq!(
        running.run_now("singularr:index").await.unwrap(),
        RunNow::Started
    );
    assert!(failures_are(&t.pool, "singularr:index", 0, Duration::from_secs(2)).await);
    assert_eq!(running.restarts(), 0);
    running.stop().await;
    t.drop_db().await;
}

/// A worker killed mid-job: its job keeps its row until the lease runs out. The reaper then gives
/// it back. Another process runs it.
#[tokio::test]
async fn a_job_whose_worker_died_runs_again_after_its_lease() {
    let Some(t) = TestDb::new("jobs_lease").await else {
        return;
    };
    let music = every("folder:music", "scan.music", "scan", 3600, 1);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    let held = Arc::new(counting(&[Act::Hold]));
    let killed = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", held.clone())
        .workers("scan", 1)
        .start();
    assert!(until(Duration::from_secs(2), || held.runs() == 1).await);
    killed.abort();
    let state: String = sqlx::query_scalar("SELECT state FROM jobs")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(state, "running");

    let next = Arc::new(counting(&[Act::Ok]));
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", next.clone())
        .workers("scan", 1)
        .start();
    assert!(
        until(Duration::from_secs(4), || next.runs() == 1).await,
        "it runs again after its lease"
    );
    assert!(a_job_is(&t.pool, "done", Duration::from_secs(2)).await);
    let (state, attempts, jobs): (String, i32, i64) =
        sqlx::query_as("SELECT state, attempts, count(*) OVER () FROM jobs")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), attempts, jobs), ("done", 2, 1));
    running.stop().await;
    t.drop_db().await;
}

/// A job is dead after its last attempt. The reaper marks it so when its lease ran out. A one-off
/// job is dead after its kind's attempts: with two, it fails twice, its delay grown from its
/// kind's base, then is dead.
#[tokio::test]
async fn a_job_is_dead_after_its_last_attempt() {
    let Some(t) = TestDb::new("jobs_dead").await else {
        return;
    };
    sqlx::query(
        "INSERT INTO jobs (kind, queue, state, attempts, max_attempts, locked_by, locked_until)
         VALUES ('a', 'q', 'running', 2, 2, 'gone', now() - interval '1 second'),
                ('b', 'q', 'running', 1, 2, 'gone', now() - interval '1 second')",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let mut reaped = runtime::store::reap(&t.pool).await.unwrap();
    reaped.sort();
    assert_eq!(
        reaped,
        vec![("q".to_string(), false), ("q".to_string(), true)]
    );
    let states: Vec<(String, String)> =
        sqlx::query_as("SELECT kind, state FROM jobs ORDER BY kind")
            .fetch_all(&t.pool)
            .await
            .unwrap();
    let states: Vec<(&str, &str)> = states
        .iter()
        .map(|(k, s)| (k.as_str(), s.as_str()))
        .collect();
    assert_eq!(states, vec![("a", "dead"), ("b", "queued")]);

    sqlx::query("DELETE FROM jobs")
        .execute(&t.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO jobs (kind, queue) VALUES ('report.send', 'default')")
        .execute(&t.pool)
        .await
        .unwrap();
    let failing = Arc::new(Counting {
        retry: Retry {
            base: Duration::from_millis(200),
            cap: Duration::from_secs(1),
            attempts: 2,
        },
        ..counting(&[Act::Fail])
    });
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("report.send", failing.clone())
        .workers("default", 1)
        .start();
    assert!(a_job_is(&t.pool, "dead", Duration::from_secs(4)).await);
    let (attempts, max, class): (i32, i32, Option<String>) =
        sqlx::query_as("SELECT attempts, max_attempts, error_class FROM jobs")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(
        (attempts, max, class.as_deref(), failing.runs()),
        (2, 2, Some("failed"), 2)
    );
    running.stop().await;
    t.drop_db().await;
}

/// A stop lets a run end within the grace period. A run still going then is dropped. Its job goes
/// back to its queue with its attempt returned. The next process runs it at once: no lease to
/// wait out.
#[tokio::test]
async fn a_stop_gives_back_a_run_still_going_after_the_grace_period() {
    let Some(t) = TestDb::new("jobs_stop").await else {
        return;
    };
    let music = every("folder:music", "scan.music", "scan", 3600, 60);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    let scans = Arc::new(counting(&[Act::Hold, Act::Ok]));
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", scans.clone())
        .workers("scan", 1)
        .start();
    assert!(until(Duration::from_secs(2), || scans.runs() == 1).await);
    let stopping = Instant::now();
    running.stop().await;
    assert!(stopping.elapsed() < Duration::from_secs(3));
    assert!(scans.dropped());
    let (state, attempts, locked): (String, i32, Option<String>) =
        sqlx::query_as("SELECT state, attempts, locked_by FROM jobs")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), attempts, locked), ("queued", 0, None));

    let next = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", scans.clone())
        .workers("scan", 1)
        .start();
    assert!(
        until(Duration::from_secs(1), || scans.runs() == 2).await,
        "at once"
    );
    assert!(a_job_is(&t.pool, "done", Duration::from_secs(1)).await);
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM jobs")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(attempts, 1);
    next.stop().await;
    t.drop_db().await;
}

/// Reconcile writes the schedules of a process. A known one keeps its next run and whether it is
/// on. Its next run comes closer when its interval shrinks. Its source is written. A removed one
/// goes with its queued job cancelled. An empty list changes nothing.
#[tokio::test]
async fn reconcile_keeps_what_is_saved_and_cancels_the_queue_of_a_removed_schedule() {
    let Some(t) = TestDb::new("jobs_reconcile").await else {
        return;
    };
    let a = every("a", "k", "q", 3600, 60);
    Runtime::reconcile(&t.pool, &[a.clone(), every("b", "k", "q", 3600, 60)])
        .await
        .unwrap();
    sqlx::query(
        "UPDATE job_schedules SET next_run_at = now() + interval '2 hours', enabled = false
         WHERE key = 'a'",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO jobs (kind, queue, schedule_key) VALUES ('k', 'q', 'b')")
        .execute(&t.pool)
        .await
        .unwrap();
    let shorter = Schedule {
        every: Duration::from_secs(60),
        source: Source::Environment,
        ..a
    };
    Runtime::reconcile(&t.pool, &[shorter]).await.unwrap();
    let (every_secs, enabled, source, next_in): (i32, bool, String, f64) = sqlx::query_as(
        "SELECT every_secs, enabled, source, EXTRACT(EPOCH FROM next_run_at - now())::float8
         FROM job_schedules WHERE key = 'a'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        (every_secs, enabled, source.as_str()),
        (60, false, "environment")
    );
    assert!(next_in <= 60.0, "closer: {next_in}");
    let (state, key): (String, Option<String>) =
        sqlx::query_as("SELECT state, schedule_key FROM jobs")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!((state.as_str(), key), ("cancelled", None));
    Runtime::reconcile(&t.pool, &[]).await.unwrap();
    let schedules: i64 = sqlx::query_scalar("SELECT count(*) FROM job_schedules")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(schedules, 1, "an empty list changes nothing");
    t.drop_db().await;
}

/// A run that writes through a fenced journal: the first write commits under the lease. The
/// test then takes the lease away. The second write is refused.
struct FencedWrites {
    pool: PgPool,
    journal: Journal,
    wrote: Arc<Notify>,
    go: Arc<Notify>,
    refused: Arc<Mutex<Option<String>>>,
}

impl FencedWrites {
    /// One event through `journal`, committed.
    async fn write(&self, journal: &Journal, n: i64) -> Result<(), sqlx::Error> {
        let mut tx = journal.begin(&self.pool).await?;
        tx.append_one(event("fenced_write", "test", n, "test", json!({})))
            .await?;
        tx.commit().await.map(|_| ())
    }
}

#[async_trait]
impl Handler for FencedWrites {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let journal = self.journal.fenced(Arc::new(cx.lease.clone()));
        self.write(&journal, 1)
            .await
            .map_err(|e| Failed::from(e.to_string()))?;
        self.wrote.notify_one();
        self.go.notified().await;
        let refused = self.write(&journal, 2).await.err().map(|e| e.to_string());
        *self.refused.lock().unwrap() = refused.clone();
        Err(Failed::from(refused.unwrap_or_default()))
    }
}

/// A run that ends its job in its own transaction, with a marker of its data.
struct EndsInside {
    pool: PgPool,
}

#[async_trait]
impl Handler for EndsInside {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| Failed::from(e.to_string()))?;
        markers::put(&mut tx, "test", "seen", &json!(1))
            .await
            .map_err(|e| Failed::from(e.to_string()))?;
        cx.lease
            .finish(&mut tx, &Ok(Done::from("in its own commit")))
            .await
            .map_err(|e| Failed::from(e.to_string()))?;
        tx.commit().await.map_err(|e| Failed::from(e.to_string()))?;
        Ok(Done::from("returned"))
    }
}

/// A run commits only under its lease. A fenced journal commits the run's events while the lease
/// holds. Once the lease is taken it refuses them. The run's end is not written then. A run that
/// ends its job in its own transaction makes its data and its end one commit. The runtime writes
/// nothing over that end.
#[tokio::test]
async fn a_run_commits_only_under_its_lease() {
    let Some(t) = TestDb::new("jobs_fence").await else {
        return;
    };
    Runtime::reconcile(
        &t.pool,
        &[
            every("a", "write.fenced", "q", 3600, 60),
            every("b", "write.inside", "r", 3600, 60),
        ],
    )
    .await
    .unwrap();
    let fenced = Arc::new(FencedWrites {
        pool: t.pool.clone(),
        journal: Journal::new(),
        wrote: Arc::new(Notify::new()),
        go: Arc::new(Notify::new()),
        refused: Arc::new(Mutex::new(None)),
    });
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("write.fenced", fenced.clone())
        .handle(
            "write.inside",
            Arc::new(EndsInside {
                pool: t.pool.clone(),
            }),
        )
        .workers("q", 1)
        .workers("r", 1)
        .start();
    tokio::time::timeout(Duration::from_secs(3), fenced.wrote.notified())
        .await
        .expect("the first write");
    sqlx::query("UPDATE jobs SET locked_by = 'x' WHERE schedule_key = 'a'")
        .execute(&t.pool)
        .await
        .unwrap();
    fenced.go.notify_one();
    let refused = || fenced.refused.lock().unwrap().clone();
    assert!(until(Duration::from_secs(3), || refused().is_some()).await);
    let refused = refused().unwrap();
    assert!(refused.contains("lease is lost"), "{refused}");
    let written: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind = 'fenced_write'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(written, 1, "the second write is refused");
    let a_ran: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_run_at FROM job_schedules WHERE key = 'a'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(a_ran, None, "the run's end is not written");

    assert!(a_job_is(&t.pool, "done", Duration::from_secs(3)).await);
    let (job, schedule): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT j.result ->> 'detail', s.last_detail
         FROM jobs j JOIN job_schedules s ON s.key = j.schedule_key WHERE s.key = 'b'",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        (job.as_deref(), schedule.as_deref()),
        (Some("in its own commit"), Some("in its own commit"))
    );
    assert_eq!(
        markers::get(&t.pool, "test", "seen").await.unwrap(),
        Some(json!(1))
    );
    running.stop().await;
    t.drop_db().await;
}

/// A run that writes its events and ends its job in one transaction of a fenced journal.
struct JournalEnds {
    pool: PgPool,
    journal: Journal,
}

#[async_trait]
impl Handler for JournalEnds {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let failed = |e: &dyn std::fmt::Display| Failed::from(e.to_string());
        let journal = self.journal.fenced(Arc::new(cx.lease.clone()));
        let mut tx = journal.begin(&self.pool).await.map_err(|e| failed(&e))?;
        tx.append_one(event("journaled_end", "test", 1, "test", json!({})))
            .await
            .map_err(|e| failed(&e))?;
        tx.finish(&cx.lease, &Ok(Done::from("with its events")))
            .await
            .map_err(|e| failed(&e))?;
        tx.commit().await.map_err(|e| failed(&e))?;
        Ok(Done::from("returned"))
    }
}

/// A transaction of a fenced journal that ends its run commits the run's events and its end in
/// one commit. The end proves the lease: the commit checks no fence after it.
#[tokio::test]
async fn a_fenced_journal_ends_a_run_with_its_events() {
    let Some(t) = TestDb::new("jobs_journal_end").await else {
        return;
    };
    Runtime::reconcile(&t.pool, &[every("a", "write.journal", "q", 3600, 60)])
        .await
        .unwrap();
    let running = Runtime::new(t.pool.clone(), quick())
        .handle(
            "write.journal",
            Arc::new(JournalEnds {
                pool: t.pool.clone(),
                journal: Journal::new(),
            }),
        )
        .workers("q", 1)
        .start();
    assert!(a_job_is(&t.pool, "done", Duration::from_secs(3)).await);
    let (job, schedule): (Option<String>, Option<String>) = sqlx::query_as(
        "SELECT j.result ->> 'detail', s.last_detail
         FROM jobs j JOIN job_schedules s ON s.key = j.schedule_key",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert_eq!(
        (job.as_deref(), schedule.as_deref()),
        (Some("with its events"), Some("with its events"))
    );
    let events: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE kind = 'journaled_end'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(events, 1);
    running.stop().await;
    t.drop_db().await;
}

/// A handler that hands its lease to the test and holds its job.
struct Hands {
    lease: Mutex<Option<oneshot::Sender<Lease>>>,
}

#[async_trait]
impl Handler for Hands {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let to = self.lease.lock().unwrap().take();
        if let Some(to) = to {
            let _ = to.send(cx.lease.clone());
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Ok(Done::from("held"))
    }
}

/// The job's row is free while another transaction holds its schedule's row: an update of it
/// under a short lock timeout goes through.
async fn job_row_is_free(pool: &PgPool, job: i64) {
    let mut other = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL lock_timeout = '500ms'")
        .execute(&mut *other)
        .await
        .unwrap();
    sqlx::query("UPDATE jobs SET priority = priority WHERE id = $1")
        .bind(job)
        .execute(&mut *other)
        .await
        .expect("the job's row is not locked");
    other.rollback().await.unwrap();
}

/// A transaction that holds a schedule's row.
async fn holding(pool: &PgPool, key: &str) -> sqlx::Transaction<'static, sqlx::Postgres> {
    let mut holder = pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM job_schedules WHERE key = $1 FOR UPDATE")
        .bind(key)
        .execute(&mut *holder)
        .await
        .unwrap();
    holder
}

/// A proof of a lease and the end of a run lock the schedule's row before the job's, as the tick
/// and a run asked for now do. While another transaction holds the schedule, each one waits
/// without the job's row.
#[tokio::test]
async fn the_schedule_is_locked_before_its_job() {
    let Some(t) = TestDb::new("jobs_lock_order").await else {
        return;
    };
    Runtime::reconcile(&t.pool, &[every("a", "k", "q", 3600, 60)])
        .await
        .unwrap();
    let (to, from) = oneshot::channel();
    let running = Runtime::new(t.pool.clone(), quick())
        .handle(
            "k",
            Arc::new(Hands {
                lease: Mutex::new(Some(to)),
            }),
        )
        .workers("q", 1)
        .start();
    let lease = tokio::time::timeout(Duration::from_secs(3), from)
        .await
        .expect("the run started")
        .unwrap();
    // The proof.
    let holder = holding(&t.pool, "a").await;
    let proving = {
        let (pool, lease) = (t.pool.clone(), lease.clone());
        tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            lease.prove(&mut tx).await.unwrap();
            tx.commit().await.unwrap();
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!proving.is_finished(), "the proof waits for the schedule");
    job_row_is_free(&t.pool, lease.job()).await;
    holder.commit().await.unwrap();
    proving.await.unwrap();

    // The end.
    let holder = holding(&t.pool, "a").await;
    let ending = {
        let (pool, lease) = (t.pool.clone(), lease.clone());
        tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            lease
                .finish(&mut tx, &Ok(Done::from("ended")))
                .await
                .unwrap();
            tx.commit().await.unwrap();
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!ending.is_finished(), "the end waits for the schedule");
    job_row_is_free(&t.pool, lease.job()).await;
    holder.commit().await.unwrap();
    ending.await.unwrap();
    let detail: Option<String> = sqlx::query_scalar("SELECT result ->> 'detail' FROM jobs")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    assert_eq!(detail.as_deref(), Some("ended"));
    running.stop().await;
    t.drop_db().await;
}

/// A run whose renewals cannot go through is dropped at the edge of its lease. Here another
/// transaction holds the job's row: each renewal waits until the edge. The run's end is not
/// written.
#[tokio::test]
async fn a_run_whose_renewals_wait_is_dropped_at_its_edge() {
    let Some(t) = TestDb::new("jobs_edge").await else {
        return;
    };
    let music = every("folder:music", "scan.music", "scan", 3600, 1);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    let held = Arc::new(counting(&[Act::Hold]));
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", held.clone())
        .workers("scan", 1)
        .start();
    assert!(until(Duration::from_secs(2), || held.runs() == 1).await);
    let job: i64 = sqlx::query_scalar("SELECT id FROM jobs")
        .fetch_one(&t.pool)
        .await
        .unwrap();
    let mut holder = t.pool.begin().await.unwrap();
    sqlx::query("SELECT 1 FROM jobs WHERE id = $1 FOR UPDATE")
        .bind(job)
        .execute(&mut *holder)
        .await
        .unwrap();
    assert!(
        until(Duration::from_secs(3), || held.dropped()).await,
        "dropped at the edge"
    );
    holder.rollback().await.unwrap();
    let last_run: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT last_run_at FROM job_schedules")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(last_run, None, "the run's end is not written");
    running.stop().await;
    t.drop_db().await;
}

/// A handler that sets its schedule's next run and leaves a value for whoever waits on the run.
struct Soon;

#[async_trait]
impl Handler for Soon {
    async fn run(&self, _cx: Cx) -> Result<Done, Failed> {
        Ok(Done {
            detail: "soon".to_string(),
            value: Some(json!({ "n": 1 })),
            again: Some(Duration::from_secs(3)),
        })
    }
}

/// A handler may set its schedule's next run. Its value is kept with its detail. A run longer
/// than its kind's limit is dropped. Its job fails as a timeout.
#[tokio::test]
async fn a_handler_sets_its_next_run_and_a_run_over_its_limit_fails() {
    let Some(t) = TestDb::new("jobs_again").await else {
        return;
    };
    Runtime::reconcile(
        &t.pool,
        &[
            every("a", "soon", "q", 3600, 60),
            every("b", "slow", "r", 3600, 60),
        ],
    )
    .await
    .unwrap();
    let slow = Arc::new(Counting {
        limit: Duration::from_millis(300),
        ..counting(&[Act::Hold])
    });
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("soon", Arc::new(Soon))
        .handle("slow", slow.clone())
        .workers("q", 1)
        .workers("r", 1)
        .start();
    assert!(a_job_is(&t.pool, "done", Duration::from_secs(2)).await);
    let (next_in, result): (f64, Option<serde_json::Value>) = sqlx::query_as(
        "SELECT EXTRACT(EPOCH FROM s.next_run_at - now())::float8, j.result
         FROM job_schedules s JOIN jobs j ON j.schedule_key = s.key
         WHERE s.key = 'a' ORDER BY j.id LIMIT 1",
    )
    .fetch_one(&t.pool)
    .await
    .unwrap();
    assert!((1.5..3.5).contains(&next_in), "{next_in}");
    assert_eq!(
        result,
        Some(json!({ "detail": "soon", "value": { "n": 1 } }))
    );
    assert!(a_job_is(&t.pool, "failed", Duration::from_secs(2)).await);
    let class: Option<String> =
        sqlx::query_scalar("SELECT error_class FROM jobs WHERE schedule_key = 'b'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
    assert_eq!(class.as_deref(), Some("timeout"));
    assert!(slow.dropped());
    running.stop().await;
    t.drop_db().await;
}

/// A handler that leaves its run's number and its payload's actor for whoever waits on the run.
/// Each run takes 300 ms.
struct Reporting {
    runs: AtomicUsize,
}

#[async_trait]
impl Handler for Reporting {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let n = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        tokio::time::sleep(Duration::from_millis(300)).await;
        Ok(Done {
            detail: format!("run {n}"),
            value: Some(json!({ "run": n, "actor": cx.job.payload["actor"] })),
            again: None,
        })
    }
}

/// The database's clock now.
async fn db_now(pool: &PgPool) -> chrono::DateTime<chrono::Utc> {
    sqlx::query_scalar("SELECT now()")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A run asked for and waited on answers with its value. The payload reaches its handler. A run
/// asked for while one runs waits for that one. The schedule then runs again. The schedules'
/// states show the open job and count the runs. A key without a schedule answers nothing.
#[tokio::test]
async fn a_run_waited_on_answers_with_its_value() {
    let Some(t) = TestDb::new("jobs_wait").await else {
        return;
    };
    let since = db_now(&t.pool).await;
    let music = every("folder:music", "scan.music", "scan", 3600, 60);
    Runtime::reconcile(&t.pool, &[music]).await.unwrap();
    sqlx::query("UPDATE job_schedules SET next_run_at = now() + interval '1 hour'")
        .execute(&t.pool)
        .await
        .unwrap();
    let scans = Arc::new(Reporting {
        runs: AtomicUsize::new(0),
    });
    let running = Runtime::new(t.pool.clone(), quick())
        .handle("scan.music", scans.clone())
        .workers("scan", 1)
        .start();
    let ended = runtime::run_and_wait(&t.pool, "folder:music", json!({ "actor": "owner" }))
        .await
        .unwrap()
        .unwrap();
    assert!(ended.ok());
    assert_eq!(ended.detail.as_deref(), Some("run 1"));
    assert_eq!(ended.value, Some(json!({ "run": 1, "actor": "owner" })));

    assert_eq!(
        running.run_now("folder:music").await.unwrap(),
        RunNow::Started
    );
    assert!(a_job_is(&t.pool, "running", Duration::from_secs(2)).await);
    let states = runtime::states(&t.pool, since, &["folder:music"])
        .await
        .unwrap();
    assert_eq!(
        (states.len(), states[0].open.as_deref(), states[0].runs),
        (1, Some("running"), 1)
    );
    let ended = runtime::run_and_wait(&t.pool, "folder:music", json!({}))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        ended.value.map(|v| v["run"].clone()),
        Some(json!(3)),
        "the run after the one going"
    );
    let states = runtime::states(&t.pool, since, &["folder:music"])
        .await
        .unwrap();
    let s = &states[0];
    assert_eq!(
        (
            s.key.as_str(),
            s.open.as_deref(),
            s.runs,
            s.last_detail.as_deref()
        ),
        ("folder:music", None, 3, Some("run 3"))
    );
    assert_eq!(
        runtime::run_and_wait(&t.pool, "nothing", json!({}))
            .await
            .unwrap(),
        None
    );
    running.stop().await;
    t.drop_db().await;
}

/// How far away a schedule's next run is, in seconds.
async fn next_in(pool: &PgPool, key: &str) -> f64 {
    sqlx::query_scalar(
        "SELECT EXTRACT(EPOCH FROM next_run_at - now())::float8 FROM job_schedules WHERE key = $1",
    )
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A new schedule first runs after its first wait. A start keeps a known schedule's next run.
/// A schedule that runs after each start has its next run brought to its first wait. No next
/// run comes sooner than the first wait after a start.
#[tokio::test]
async fn a_start_brings_the_next_run_of_a_schedule_that_runs_on_start() {
    let Some(t) = TestDb::new("jobs_start").await else {
        return;
    };
    let later = Schedule {
        first: Duration::from_secs(600),
        ..every("a", "k", "q", 3600, 60)
    };
    let soon = Schedule {
        first: Duration::from_secs(5),
        on_start: true,
        ..every("b", "k", "q", 3600, 60)
    };
    Runtime::reconcile(&t.pool, &[later.clone(), soon.clone()])
        .await
        .unwrap();
    let a = next_in(&t.pool, "a").await;
    assert!((590.0..=600.0).contains(&a), "{a}");
    let b = next_in(&t.pool, "b").await;
    assert!((0.0..=5.0).contains(&b), "{b}");
    sqlx::query("UPDATE job_schedules SET next_run_at = now() + interval '1 hour'")
        .execute(&t.pool)
        .await
        .unwrap();
    Runtime::reconcile(&t.pool, &[later.clone(), soon])
        .await
        .unwrap();
    assert!(next_in(&t.pool, "a").await > 3500.0, "kept");
    assert!(
        next_in(&t.pool, "b").await <= 5.0,
        "brought to its first wait"
    );
    // A next run already due waits the first wait after a start all the same.
    sqlx::query(
        "UPDATE job_schedules SET next_run_at = now() - interval '1 minute' WHERE key = 'a'",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    Runtime::reconcile(&t.pool, &[later]).await.unwrap();
    let a = next_in(&t.pool, "a").await;
    assert!((590.0..=600.0).contains(&a), "the first wait: {a}");
    t.drop_db().await;
}

/// A done job goes 2 days after its end, a failed or dead one after 14. An open one stays.
#[tokio::test]
async fn jobs_that_ended_go_after_their_keep() {
    let Some(t) = TestDb::new("jobs_tidy").await else {
        return;
    };
    sqlx::query(
        "INSERT INTO jobs (kind, queue, state, finished_at) VALUES
             ('done-old', 'q', 'done', now() - interval '3 days'),
             ('done-new', 'q', 'done', now() - interval '1 day'),
             ('failed-old', 'q', 'failed', now() - interval '15 days'),
             ('failed-new', 'q', 'failed', now() - interval '3 days'),
             ('dead-old', 'q', 'dead', now() - interval '15 days'),
             ('open', 'q', 'queued', NULL)",
    )
    .execute(&t.pool)
    .await
    .unwrap();
    let removed = runtime::store::tidy(
        &t.pool,
        Duration::from_secs(2 * 86_400),
        Duration::from_secs(14 * 86_400),
    )
    .await
    .unwrap();
    assert_eq!(removed, 3);
    let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM jobs ORDER BY kind")
        .fetch_all(&t.pool)
        .await
        .unwrap();
    assert_eq!(kinds, vec!["done-new", "failed-new", "open"]);
    t.drop_db().await;
}

/// A marker keeps one value per scope and name across starts. Writing it again replaces it.
#[tokio::test]
async fn a_marker_keeps_its_value() {
    let Some(t) = TestDb::new("markers").await else {
        return;
    };
    assert_eq!(
        markers::get(&t.pool, "radarr", "history.loaded")
            .await
            .unwrap(),
        None
    );
    let mut conn = t.pool.acquire().await.unwrap();
    markers::put(&mut conn, "radarr", "history.loaded", &json!(true))
        .await
        .unwrap();
    markers::put(&mut conn, "radarr", "history.loaded", &json!(false))
        .await
        .unwrap();
    drop(conn);
    assert_eq!(
        markers::get(&t.pool, "radarr", "history.loaded")
            .await
            .unwrap(),
        Some(json!(false))
    );
    t.drop_db().await;
}

/// The act outbox over its tables. Each test sets the answers of its delivery.
mod outbox_tests {
    use super::*;
    use singularr_platform::journal::JournalTx;
    use singularr_platform::outbox::{
        self, Act, ConfirmBy, Confirmation, Deliver, Delivered, EnqueueError, Enqueued, Held,
        NewAct, Outbox, Plan, Resend,
    };

    /// A delivery that answers each send with the next of `sends` and each read with the next of
    /// `reads` (not seen yet when none is left). It notes each change of state it hears of.
    struct Scripted {
        allows: AtomicBool,
        sends: Mutex<Vec<Delivered>>,
        reads: Mutex<Vec<Confirmation>>,
        sent: AtomicUsize,
        plan: Plan,
        changes: Mutex<Vec<String>>,
    }

    impl Scripted {
        fn new(plan: Plan, sends: Vec<Delivered>, reads: Vec<Confirmation>) -> Arc<Scripted> {
            Arc::new(Scripted {
                allows: AtomicBool::new(true),
                sends: Mutex::new(sends),
                reads: Mutex::new(reads),
                sent: AtomicUsize::new(0),
                plan,
                changes: Mutex::new(Vec::new()),
            })
        }

        fn changes(&self) -> Vec<String> {
            self.changes.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Deliver for Scripted {
        fn plan(&self, _act: &Act) -> Plan {
            self.plan
        }

        fn held(&self, _act: &Act) -> Option<Held> {
            (!self.allows.load(Ordering::SeqCst)).then(|| Held::new("Read-only", outbox::ACCESS))
        }

        async fn send(&self, _act: &Act) -> Delivered {
            self.sent.fetch_add(1, Ordering::SeqCst);
            let mut sends = self.sends.lock().unwrap();
            if sends.is_empty() {
                Delivered::Unknown {
                    error: "no answer scripted".to_string(),
                }
            } else {
                sends.remove(0)
            }
        }

        async fn confirm(&self, _act: &Act) -> Result<Confirmation, String> {
            let mut reads = self.reads.lock().unwrap();
            Ok(if reads.is_empty() {
                Confirmation::NotYet
            } else {
                reads.remove(0)
            })
        }

        async fn changed(&self, _tx: &mut JournalTx, act: &Act) -> Result<(), sqlx::Error> {
            self.changes.lock().unwrap().push(act.state.clone());
            Ok(())
        }
    }

    /// Reads at once, one after the other.
    const READ_NOW: Plan = Plan {
        confirm: ConfirmBy::Read {
            first: Duration::ZERO,
            every: Duration::ZERO,
            window: Duration::from_secs(600),
        },
        resend: Resend::Never,
    };

    fn new_act(key: &str, path: &str) -> NewAct {
        NewAct {
            integration: "bridge".to_string(),
            action: "send".to_string(),
            key: key.to_string(),
            open_key: Some(format!("send:bridge:{path}:audio")),
            request: json!({"path": path, "flow": "audio"}),
            subject: Some(("title".to_string(), "7".to_string())),
            actor: "user:owner@example.org".to_string(),
            caused_by: None,
        }
    }

    async fn enqueue(
        pool: &PgPool,
        journal: &Journal,
        outbox: &Outbox,
        act: NewAct,
    ) -> Result<Enqueued, EnqueueError> {
        let mut tx = journal.begin(pool).await.unwrap();
        let enqueued = outbox.enqueue(&mut tx, act).await;
        tx.commit().await.unwrap();
        enqueued
    }

    /// The jobs of the acts queue run as its workers would, at once, their waits skipped, until
    /// none is queued.
    async fn run_acts(pool: &PgPool, outbox: &Outbox) {
        loop {
            let next: Option<(String, serde_json::Value)> = sqlx::query_as(
                "UPDATE jobs SET state = 'done', finished_at = now()
                 WHERE id = (SELECT id FROM jobs WHERE queue = 'acts' AND state = 'queued'
                             ORDER BY id LIMIT 1)
                 RETURNING kind, payload",
            )
            .fetch_optional(pool)
            .await
            .unwrap();
            let Some((kind, payload)) = next else {
                return;
            };
            let act = payload["act"].as_i64().unwrap();
            if kind == outbox::SEND_KIND {
                outbox.send(act).await.unwrap();
            } else {
                let send = i32::try_from(payload["send"].as_i64().unwrap()).unwrap();
                let read = u32::try_from(payload["read"].as_i64().unwrap()).unwrap();
                outbox.confirm(act, send, read).await.unwrap();
            }
        }
    }

    async fn state_of(pool: &PgPool, id: i64) -> Act {
        outbox::act(pool, id).await.unwrap().unwrap()
    }

    /// An act is a row with its send's job before anything is sent. Its key is the act for the
    /// same request and refused for another. Its open key is the open act. A worker sends it once.
    /// The answer leaves it sent. Reads confirm it: the first that finds it.
    #[tokio::test]
    async fn an_act_is_sent_once_and_confirmed_by_a_read() {
        let Some(t) = TestDb::new("outbox_send").await else {
            return;
        };
        let journal = Journal::new();
        let d = Scripted::new(
            READ_NOW,
            vec![Delivered::Answered(json!({"job": "j1"}))],
            vec![
                Confirmation::NotYet,
                Confirmation::Done(json!({"run": "bridge:j1"})),
            ],
        );
        let outbox = Outbox::new(t.pool.clone(), journal.clone()).deliver("send", d.clone());
        let Enqueued::New(act) = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k1", "/a.mkv"))
            .await
            .unwrap()
        else {
            panic!("a new act")
        };
        assert_eq!((act.state.as_str(), act.attempts), ("pending", 0));
        assert_eq!(kinds(&t.pool, 0).await, vec!["act_requested"]);
        let job: String = sqlx::query_scalar(
            "SELECT idempotency_key FROM jobs WHERE kind = 'act.send' AND queue = 'acts'",
        )
        .fetch_one(&t.pool)
        .await
        .unwrap();
        assert_eq!(job, format!("act:{}:1", act.id));
        // The key is that act for the same request. Another request with it is refused.
        assert!(matches!(
            enqueue(&t.pool, &journal, &outbox, new_act("api:o:k1", "/a.mkv")).await,
            Ok(Enqueued::Same(same)) if same.id == act.id
        ));
        assert!(matches!(
            enqueue(&t.pool, &journal, &outbox, new_act("api:o:k1", "/b.mkv")).await,
            Err(EnqueueError::Mismatch(other)) if other.id == act.id
        ));
        // Another key of the same file and flow is the open act.
        assert!(matches!(
            enqueue(&t.pool, &journal, &outbox, new_act("api:o:k2", "/a.mkv")).await,
            Ok(Enqueued::Open(open)) if open.id == act.id
        ));
        // The compare and set takes a pending act once.
        let (a, b) = tokio::join!(
            outbox::store::take(&t.pool, act.id),
            outbox::store::take(&t.pool, act.id)
        );
        let taken = [a.unwrap().is_some(), b.unwrap().is_some()];
        assert_eq!(taken.iter().filter(|x| **x).count(), 1);
        sqlx::query(
            "UPDATE acts SET state = 'pending', attempts = 0, sent_at = NULL WHERE id = $1",
        )
        .bind(act.id)
        .execute(&t.pool)
        .await
        .unwrap();
        run_acts(&t.pool, &outbox).await;
        assert_eq!(d.sent.load(Ordering::SeqCst), 1);
        let done = state_of(&t.pool, act.id).await;
        assert_eq!(
            (done.state.as_str(), done.attempts),
            ("confirmed", 1),
            "{done:?}"
        );
        assert_eq!(
            done.result,
            Some(json!({"job": "j1", "confirmed": {"run": "bridge:j1"}}))
        );
        assert!(done.sent_at.is_some() && done.settled_at.is_some());
        assert_eq!(d.changes(), vec!["sent", "confirmed"]);
        assert_eq!(
            kinds(&t.pool, 0).await,
            vec!["act_requested", "act_sent", "act_confirmed"]
        );
        // Its events name the act and its state.
        let confirmed = bodies(&t.pool, "act_confirmed").await;
        assert_eq!(
            (&confirmed[0]["act"], &confirmed[0]["state"]),
            (&json!(act.id), &json!("confirmed"))
        );
        // Settled, its job runs again to no effect: an act is sent once.
        assert_eq!(outbox.send(act.id).await.unwrap(), "Already confirmed");
        assert_eq!(d.sent.load(Ordering::SeqCst), 1);
        // Its open key is free once it is settled.
        assert!(matches!(
            enqueue(&t.pool, &journal, &outbox, new_act("api:o:k3", "/a.mkv")).await,
            Ok(Enqueued::New(_))
        ));
        // A sent act holds its open key no more: the service has the file. It refuses a second
        // copy itself.
        let sent = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k4", "/d.mkv"))
            .await
            .unwrap()
            .into_act();
        sqlx::query("UPDATE acts SET state = 'sent' WHERE id = $1")
            .bind(sent.id)
            .execute(&t.pool)
            .await
            .unwrap();
        assert!(matches!(
            enqueue(&t.pool, &journal, &outbox, new_act("api:o:k5", "/d.mkv")).await,
            Ok(Enqueued::New(_))
        ));
        t.drop_db().await;
    }

    /// No clear answer leaves an act unknown. No read that does not find it moves it: it is sent
    /// once. The owner settles it as not sent. An act its send's job finds sending has no answer
    /// recorded: it is unknown, never pending.
    #[tokio::test]
    async fn an_unclear_or_lost_send_is_unknown_and_never_sent_again() {
        let Some(t) = TestDb::new("outbox_unknown").await else {
            return;
        };
        let journal = Journal::new();
        let d = Scripted::new(
            READ_NOW,
            vec![Delivered::Unknown {
                error: "No clear answer".to_string(),
            }],
            Vec::new(),
        );
        let outbox = Outbox::new(t.pool.clone(), journal.clone()).deliver("send", d.clone());
        let act = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k1", "/a.mkv"))
            .await
            .unwrap()
            .into_act();
        run_acts(&t.pool, &outbox).await;
        let unknown = state_of(&t.pool, act.id).await;
        assert_eq!(
            (unknown.state.as_str(), unknown.error_class.as_deref()),
            ("unknown", Some("unknown"))
        );
        let reads: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'act.confirm'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(reads, i64::from(outbox::READS));
        assert_eq!(d.sent.load(Ordering::SeqCst), 1);
        // The owner's word: not sent. It fails. Only an unknown act takes that word.
        let settled = outbox
            .not_sent(act.id, "user:owner@example.org")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (settled.state.as_str(), settled.error_class.as_deref()),
            ("failed", Some(outbox::BY_HAND))
        );
        assert!(
            outbox
                .not_sent(act.id, "user:owner@example.org")
                .await
                .unwrap()
                .is_none()
        );
        // A send whose run recorded no answer: its job's next run finds it sending.
        let lost = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k2", "/b.mkv"))
            .await
            .unwrap()
            .into_act();
        assert!(
            outbox::store::take(&t.pool, lost.id)
                .await
                .unwrap()
                .is_some()
        );
        outbox.send(lost.id).await.unwrap();
        let after = state_of(&t.pool, lost.id).await;
        assert_eq!(
            (after.state.as_str(), after.error_class.as_deref()),
            ("unknown", Some(outbox::UNRECORDED))
        );
        assert_eq!(d.sent.load(Ordering::SeqCst), 1, "never sent again");
        t.drop_db().await;
    }

    /// A read that proves an act absent at its last read fails it. An action sent again when
    /// absent is sent again instead, three sends at most.
    #[tokio::test]
    async fn a_read_that_proves_an_act_absent_fails_it_or_sends_it_again() {
        let Some(t) = TestDb::new("outbox_absent").await else {
            return;
        };
        let journal = Journal::new();
        let absent = || vec![Confirmation::Absent; outbox::READS as usize];
        let never = Scripted::new(READ_NOW, vec![Delivered::Answered(json!({}))], absent());
        let outbox = Outbox::new(t.pool.clone(), journal.clone()).deliver("send", never.clone());
        let act = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k1", "/a.mkv"))
            .await
            .unwrap()
            .into_act();
        run_acts(&t.pool, &outbox).await;
        let failed = state_of(&t.pool, act.id).await;
        assert_eq!(
            (failed.state.as_str(), failed.error_class.as_deref()),
            ("failed", Some(outbox::ABSENT))
        );
        let again = Scripted::new(
            Plan {
                resend: Resend::WhenAbsent,
                ..READ_NOW
            },
            vec![
                Delivered::Answered(json!({})),
                Delivered::Answered(json!({})),
                Delivered::Answered(json!({})),
            ],
            [absent(), absent(), absent()].concat(),
        );
        let outbox = Outbox::new(t.pool.clone(), journal.clone()).deliver("send", again.clone());
        let act = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k2", "/b.mkv"))
            .await
            .unwrap()
            .into_act();
        run_acts(&t.pool, &outbox).await;
        let done = state_of(&t.pool, act.id).await;
        assert_eq!(
            (done.state.as_str(), done.attempts),
            ("failed", outbox::SENDS)
        );
        assert_eq!(again.sent.load(Ordering::SeqCst), 3);
        assert_eq!(
            bodies(&t.pool, "act_resent").await.len(),
            2,
            "sent again twice"
        );
        t.drop_db().await;
    }

    /// An act a read proved absent goes back to pending to be sent again. A newer act of its file
    /// and flow that took the open key while it was sent stands for it: it fails. The job of the
    /// read ends well. The newer act is left as it was.
    #[tokio::test]
    async fn an_absent_act_whose_open_key_a_newer_act_holds_fails_as_superseded() {
        let Some(t) = TestDb::new("outbox_superseded").await else {
            return;
        };
        let journal = Journal::new();
        let d = Scripted::new(
            Plan {
                resend: Resend::WhenAbsent,
                ..READ_NOW
            },
            vec![Delivered::Answered(json!({}))],
            vec![Confirmation::Absent; outbox::READS as usize],
        );
        let outbox = Outbox::new(t.pool.clone(), journal.clone()).deliver("send", d.clone());
        let old = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k1", "/a.mkv"))
            .await
            .unwrap()
            .into_act();
        assert_eq!(outbox.send(old.id).await.unwrap(), "Sent");
        // A sent act holds its open key no more: a newer act of the file and the flow takes it.
        let Enqueued::New(newer) =
            enqueue(&t.pool, &journal, &outbox, new_act("api:o:k2", "/a.mkv"))
                .await
                .unwrap()
        else {
            panic!("a new act")
        };
        // The old act's reads alone run: each proves it absent.
        let mut last = String::new();
        loop {
            let next: Option<(i64, i64)> = sqlx::query_as(
                "UPDATE jobs SET state = 'done', finished_at = now()
                 WHERE id = (SELECT id FROM jobs WHERE kind = 'act.confirm' AND state = 'queued'
                             AND (payload->>'act')::bigint = $1 ORDER BY id LIMIT 1)
                 RETURNING (payload->>'send')::bigint, (payload->>'read')::bigint",
            )
            .bind(old.id)
            .fetch_optional(&t.pool)
            .await
            .unwrap();
            let Some((send, read)) = next else {
                break;
            };
            last = outbox
                .confirm(
                    old.id,
                    i32::try_from(send).unwrap(),
                    u32::try_from(read).unwrap(),
                )
                .await
                .expect("the read's job ends well");
        }
        assert_eq!(last, "Failed: Superseded");
        let failed = state_of(&t.pool, old.id).await;
        assert_eq!(
            (
                failed.state.as_str(),
                failed.error_class.as_deref(),
                failed.attempts
            ),
            ("failed", Some(outbox::SUPERSEDED), 1)
        );
        assert_eq!(state_of(&t.pool, newer.id).await.state, "pending");
        assert_eq!(d.sent.load(Ordering::SeqCst), 1, "sent once");
        assert!(bodies(&t.pool, "act_resent").await.is_empty());
        assert_eq!(d.changes(), vec!["sent", "failed"]);
        t.drop_db().await;
    }

    /// Two enqueues of one file and one flow under two keys at once make one act: the other finds
    /// it by its open key. It is sent once.
    #[tokio::test]
    async fn two_enqueues_racing_for_one_open_key_make_one_act() {
        let Some(t) = TestDb::new("outbox_race").await else {
            return;
        };
        let journal = Journal::new();
        let d = Scripted::new(
            READ_NOW,
            vec![Delivered::Answered(json!({}))],
            vec![Confirmation::Done(json!({}))],
        );
        let outbox = Outbox::new(t.pool.clone(), journal.clone()).deliver("send", d.clone());
        for round in 0..5 {
            let path = format!("/race-{round}.mkv");
            let (a, b) = tokio::join!(
                enqueue(
                    &t.pool,
                    &journal,
                    &outbox,
                    new_act(&format!("api:o:a{round}"), &path)
                ),
                enqueue(
                    &t.pool,
                    &journal,
                    &outbox,
                    new_act(&format!("api:o:b{round}"), &path)
                ),
            );
            let (a, b) = (a.unwrap(), b.unwrap());
            let new = [&a, &b]
                .iter()
                .filter(|e| matches!(e, Enqueued::New(_)))
                .count();
            assert_eq!(new, 1, "{a:?} {b:?}");
            assert!(
                [&a, &b]
                    .iter()
                    .any(|e| matches!(e, Enqueued::Open(open) if open.id == a.act().id)),
                "{a:?} {b:?}"
            );
            assert_eq!(a.act().id, b.act().id);
        }
        let acts: i64 = sqlx::query_scalar("SELECT count(*) FROM acts")
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(acts, 5);
        let sends: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs WHERE kind = 'act.send'")
            .fetch_one(&t.pool)
            .await
            .unwrap();
        assert_eq!(sends, 5, "one send queued for each act");
        t.drop_db().await;
    }

    /// An act whose action's access was taken back fails before anything is sent. A pending act
    /// is cancelled before a worker takes it. An action confirmed by its answer is confirmed at
    /// once.
    #[tokio::test]
    async fn an_act_is_refused_cancelled_or_confirmed_by_its_answer() {
        let Some(t) = TestDb::new("outbox_hands").await else {
            return;
        };
        let journal = Journal::new();
        let d = Scripted::new(
            Plan {
                confirm: ConfirmBy::Answer,
                resend: Resend::WhenAbsent,
            },
            vec![Delivered::Answered(json!({"done": true}))],
            Vec::new(),
        );
        let outbox = Outbox::new(t.pool.clone(), journal.clone()).deliver("send", d.clone());
        d.allows.store(false, Ordering::SeqCst);
        let refused = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k1", "/a.mkv"))
            .await
            .unwrap()
            .into_act();
        run_acts(&t.pool, &outbox).await;
        let refused = state_of(&t.pool, refused.id).await;
        assert_eq!(
            (
                refused.state.as_str(),
                refused.error_class.as_deref(),
                refused.attempts
            ),
            ("failed", Some(outbox::ACCESS), 0)
        );
        assert_eq!(d.sent.load(Ordering::SeqCst), 0);
        d.allows.store(true, Ordering::SeqCst);
        // Cancelled while pending: its job goes with it. Nothing is sent.
        let pending = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k2", "/b.mkv"))
            .await
            .unwrap()
            .into_act();
        let cancelled = outbox
            .cancel(pending.id, "user:owner@example.org")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cancelled.state, "cancelled");
        assert!(
            outbox
                .cancel(pending.id, "user:owner@example.org")
                .await
                .unwrap()
                .is_none()
        );
        run_acts(&t.pool, &outbox).await;
        assert_eq!(d.sent.load(Ordering::SeqCst), 0);
        let job: String = sqlx::query_scalar(
            "SELECT state FROM jobs WHERE kind = 'act.send' AND (payload->>'act')::bigint = $1",
        )
        .bind(pending.id)
        .fetch_one(&t.pool)
        .await
        .unwrap();
        assert_eq!(job, "cancelled");
        // Confirmed by its answer.
        let answered = enqueue(&t.pool, &journal, &outbox, new_act("api:o:k3", "/c.mkv"))
            .await
            .unwrap()
            .into_act();
        run_acts(&t.pool, &outbox).await;
        assert_eq!(state_of(&t.pool, answered.id).await.state, "confirmed");
        assert_eq!(d.changes(), vec!["failed", "cancelled", "confirmed"]);
        // An act at rest is waited for no longer.
        let rested = outbox::rested(
            &t.pool,
            answered.id,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(rested.state, "confirmed");
        t.drop_db().await;
    }
}
