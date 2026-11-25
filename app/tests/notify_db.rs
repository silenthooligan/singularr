//! Notifications against a PostgreSQL: channels and their journal events, the dispatcher, the
//! sender with its three attempts, test messages, the processing events, and the routes.
//!
//! Like `db.rs`, these run only when `SINGULARR_TEST_DATABASE_URL` names a server the tests may
//! create databases on. Nothing leaves: HTTP goes to a recorded transport through the real way
//! out, mail to a recorded mailer.
#![cfg(feature = "ssr")]

use chrono::{Duration as Span, Utc};
use serde_json::{Value, json};
use singularr::core::config::Config;
use singularr::core::journal::{Journal, event};
use singularr::core::model::Event;
use singularr::core::outbound::recorded::Recorded;
use singularr::core::outbound::{Allowlist, Body, Method, Outbound};
use singularr::core::smtp::recorded::Recorded as Mails;
use singularr::core::{db, store as core_store};
use singularr::engines::service::EngineSet;
use singularr::notify::model::{EVENTS, Notice};
use singularr::notify::service::{
    ChangeError, ChannelInput, ChannelPatch, Describe, Notifier, TestInput,
};
use singularr::notify::store;
use singularr::processing::service as processing;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const HUSH: &str = "http://10.0.0.60:8787";
const SLACK: &str = "https://hooks.slack.com/services/T000/B000/slack-secret-token";
const HOOK: &str = "https://hooks.example.com/singularr/hook-path-secret";
const LATE: &str = "https://late.example.com/singularr/late-path-secret";
const DISCORD: &str = "https://discord.com/api/webhooks/1/discord-secret-token";
const DISCORD_NEW: &str = "https://discord.com/api/webhooks/2/discord-new-token";
const OWNER: &str = "user:owner@example.com";

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

/// The way out with one integration listed (the processing bridge), over recorded answers.
fn way_out(recorded: Arc<Recorded>) -> Outbound {
    let mut allow = Allowlist::default();
    allow
        .allow_integration("hush", HUSH, singularr::engines::hush::calls())
        .unwrap();
    Outbound::with_transport(allow, recorded)
}

/// A describer that needs no catalog: each event as a notice in plain words.
struct Plain;

#[async_trait::async_trait]
impl Describe for Plain {
    async fn describe(&self, events: &[(Event, &'static str)]) -> Result<Vec<Notice>, String> {
        Ok(events
            .iter()
            .map(|(e, id)| Notice {
                event: id.to_string(),
                seq: Some(e.id),
                at: e.at,
                kind: e
                    .body
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                summary: format!("{} of {}", e.kind, e.subject_id),
                subject: e.subject_id.clone(),
                title: None,
                facts: Vec::new(),
                link: None,
                journal: json!({"seq": e.id, "code": e.kind}),
                test: false,
            })
            .collect())
    }
}

fn input(value: Value) -> ChannelInput {
    serde_json::from_value(value).unwrap()
}

fn patch(value: Value) -> ChannelPatch {
    serde_json::from_value(value).unwrap()
}

fn invalid(result: Result<impl Sized, ChangeError>) -> BTreeMap<String, String> {
    match result {
        Err(ChangeError::Invalid(errors)) => errors,
        Err(other) => panic!("not refused as invalid: {other:?}"),
        Ok(_) => panic!("taken"),
    }
}

async fn append(pool: &PgPool, journal: &Journal, kind: &str, subject: (&str, &str), body: Value) {
    let mut tx = journal.begin(pool).await.unwrap();
    tx.append_one(event(kind, subject.0, subject.1, "sync:test", body))
        .await
        .unwrap();
    tx.commit().await.unwrap();
}

/// Every delivery of one channel, oldest first.
async fn of(n: &Notifier, channel: i64) -> Vec<store::DeliveryRow> {
    let mut rows = n.deliveries(Some(channel), None, 500).await.unwrap();
    rows.reverse();
    rows
}

/// The events of deliveries, in their order.
fn events(rows: &[store::DeliveryRow]) -> Vec<String> {
    rows.iter().map(|d| d.event.clone()).collect()
}

#[tokio::test]
async fn the_dispatcher_follows_the_journal_once_per_channel_and_event() {
    let Some(t) = TestDb::new("notify_dispatch").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    let out = way_out(recorded.clone());
    let mails = Mails::new();
    let n = Notifier::new(t.pool.clone(), journal.clone(), out.clone(), mails.clone());

    // An event from before any channel: no channel is ever told about it.
    append(
        &t.pool,
        &journal,
        "copy_replaced",
        ("copy", "1"),
        json!({"title_id": 7, "kind": "show", "score_delta": 80}),
    )
    .await;

    // What makes no channel is refused, by key.
    let errors = invalid(
        n.create(&input(json!({"type": "slack", "name": "Ops"})), OWNER)
            .await,
    );
    assert_eq!(errors["settings.url"], "Required");
    let errors = invalid(
        n.create(
            &input(json!({"type": "webhook", "name": " ", "settings": {"url": format!("{HUSH}/hook")}, "events": ["file.renamed"]})),
            OWNER,
        )
        .await,
    );
    assert_eq!(errors["settings.url"], "An integration's address");
    assert_eq!(errors["name"], "Required");
    assert_eq!(errors["events"], "Not an event: file.renamed");
    let errors = invalid(
        n.create(&input(json!({"type": "teams", "name": "x"})), OWNER)
            .await,
    );
    assert_eq!(errors["type"], "Not a type of channel");

    let all: Vec<&str> = EVENTS.iter().map(|e| e.id).collect();
    let slack = n
        .create(
            &input(
                json!({"type": "slack", "name": "Ops", "settings": {"url": SLACK},
                          "events": ["file.upgraded", "request.created"], "kinds": ["show"]}),
            ),
            OWNER,
        )
        .await
        .unwrap();
    assert!(slack.settings.is_empty(), "a webhook address is a secret");
    assert_eq!(slack.secrets["url"], SLACK);
    let hook = n
        .create(
            &input(json!({"type": "webhook", "name": "Dashboard",
                          "settings": {"url": HOOK, "secret": "hook-secret-value"}, "events": all})),
            OWNER,
        )
        .await
        .unwrap();
    let off = n
        .create(
            &input(json!({"type": "discord", "name": "Chat", "enabled": false,
                          "settings": {"url": DISCORD}, "events": ["file.upgraded"]})),
            OWNER,
        )
        .await
        .unwrap();
    let mail = n
        .create(
            &input(
                json!({"type": "email", "name": "Mail", "events": ["integration.down"],
                          "settings": {"host": "smtp.example.org", "from": "singularr@example.org",
                                       "to": ["owner@example.org"], "user": "singularr",
                                       "password": "mail-password-value"}}),
            ),
            OWNER,
        )
        .await
        .unwrap();
    assert_eq!(mail.settings["port"], 587);
    assert!(!mail.settings.contains_key("password"));
    // Each HTTP channel's address is in the way out; email has none.
    let listed: Vec<String> = out.targets().list().into_iter().map(|(k, _)| k).collect();
    assert_eq!(
        listed,
        [
            format!("channel:{}", slack.id),
            format!("channel:{}", hook.id),
            format!("channel:{}", off.id)
        ]
    );

    // Four events: a file upgraded (a show), a request (a movie), an integration offline, and
    // a setting no channel is told about.
    append(
        &t.pool,
        &journal,
        "copy_replaced",
        ("copy", "2"),
        json!({"title_id": 7, "kind": "show", "score_delta": 80}),
    )
    .await;
    append(
        &t.pool,
        &journal,
        "request_added",
        ("request", "9"),
        json!({"kind": "movie", "requested_by": "Sam"}),
    )
    .await;
    append(
        &t.pool,
        &journal,
        "engine_down",
        ("engine", "radarr"),
        json!({"error": "connection refused"}),
    )
    .await;
    append(
        &t.pool,
        &journal,
        "setting_changed",
        ("setting", "general.name"),
        json!({}),
    )
    .await;
    // Slack: the file (a show) and not the request (a movie). The webhook: all three. The
    // disabled channel: nothing. Mail: the integration.
    assert_eq!(n.dispatch(&Plain).await.unwrap(), 5);
    assert_eq!(n.dispatch(&Plain).await.unwrap(), 0, "nothing twice");
    assert_eq!(events(&of(&n, slack.id).await), ["file.upgraded"]);
    assert_eq!(
        events(&of(&n, hook.id).await),
        ["file.upgraded", "request.created", "integration.down"]
    );
    assert!(of(&n, off.id).await.is_empty());
    assert_eq!(events(&of(&n, mail.id).await), ["integration.down"]);

    // Slack takes its message, the webhook answers 500 every time, the mail server takes it.
    recorded.answer(Method::Post, SLACK, 200, "ok");
    recorded.answer(Method::Post, HOOK, 500, r#"{"error": "busy"}"#);
    let now = Utc::now();
    assert_eq!(n.send_due(now).await.unwrap(), 5);
    let sent = of(&n, slack.id).await;
    assert_eq!(
        (
            sent[0].state.as_str(),
            sent[0].attempts,
            sent[0].status,
            sent[0].detail.as_deref()
        ),
        ("sent", 1, Some(200), Some("ok"))
    );
    let posted = recorded.calls_sent();
    let to_slack = posted.iter().find(|c| c.url == SLACK).unwrap();
    let Body::Json(text) = &to_slack.body else {
        panic!("Slack is sent JSON");
    };
    let body: Value = serde_json::from_str(text).unwrap();
    assert_eq!(body["text"], "File upgraded: 2");
    let to_hook = posted.iter().find(|c| c.url == HOOK).unwrap();
    for header in ["Singularr-Event", "Singularr-Delivery", "Authorization"] {
        assert!(
            to_hook.headers.iter().any(|(name, _)| name == header),
            "{header}"
        );
    }
    assert!(to_hook.headers.contains(&(
        "Authorization".to_string(),
        "Bearer hook-secret-value".to_string()
    )));
    let mailed = mails.sent();
    assert_eq!(mailed.len(), 1);
    assert_eq!(mailed[0].1.subject, "Integration offline: radarr");
    assert_eq!(
        (mailed[0].0.host.as_str(), mailed[0].0.port),
        ("smtp.example.org", 587)
    );
    assert_eq!(mailed[0].0.password.as_deref(), Some("mail-password-value"));

    // Three attempts, 30 s and then 2 min apart; then failed.
    let waiting = of(&n, hook.id).await;
    assert!(
        waiting
            .iter()
            .all(|d| d.state == "pending" && d.attempts == 1)
    );
    assert!(
        waiting
            .iter()
            .all(|d| d.status == Some(500) && d.detail.as_deref() == Some("busy"))
    );
    assert_eq!(n.send_due(now + Span::seconds(10)).await.unwrap(), 0);
    assert_eq!(n.send_due(now + Span::seconds(31)).await.unwrap(), 3);
    assert!(of(&n, hook.id).await.iter().all(|d| d.attempts == 2));
    assert_eq!(n.send_due(now + Span::seconds(100)).await.unwrap(), 0);
    assert_eq!(n.send_due(now + Span::seconds(160)).await.unwrap(), 3);
    let failed = of(&n, hook.id).await;
    assert!(
        failed
            .iter()
            .all(|d| d.state == "failed" && d.attempts == 3 && d.finished_at.is_some())
    );
    assert_eq!(n.send_due(now + Span::hours(1)).await.unwrap(), 0);
    let hook_calls = recorded.urls_seen().iter().filter(|u| *u == HOOK).count();
    assert_eq!(hook_calls, 9, "three deliveries, three attempts each");
    // The channel shows its last delivery failed.
    let last = n.last_ended().await.unwrap();
    assert_eq!(last[&hook.id].state, "failed");
    assert_eq!(last[&slack.id].state, "sent");

    // A restart reads the cursors: nothing is dispatched again.
    let again = Notifier::new(t.pool.clone(), journal.clone(), out.clone(), mails.clone());
    assert_eq!(again.load().await.unwrap(), 4);
    assert_eq!(again.dispatch(&Plain).await.unwrap(), 0);

    // A channel added now hears of nothing from before it.
    let late = n
        .create(
            &input(json!({"type": "webhook", "name": "Late", "settings": {"url": LATE}, "events": all})),
            OWNER,
        )
        .await
        .unwrap();
    assert_eq!(n.dispatch(&Plain).await.unwrap(), 0);
    append(
        &t.pool,
        &journal,
        "copy_added",
        ("copy", "3"),
        json!({"title_id": 8, "kind": "movie"}),
    )
    .await;
    assert_eq!(
        n.dispatch(&Plain).await.unwrap(),
        2,
        "the webhook and the late one"
    );
    assert_eq!(events(&of(&n, late.id).await), ["file.added"]);

    // Disabled, Slack hears nothing; enabled again, nothing from while it was off.
    let changed = n
        .change(slack.id, &patch(json!({"enabled": false})), OWNER)
        .await
        .unwrap();
    assert!(!changed.enabled);
    append(
        &t.pool,
        &journal,
        "copy_replaced",
        ("copy", "4"),
        json!({"title_id": 7, "kind": "show", "score_delta": 10}),
    )
    .await;
    assert_eq!(n.dispatch(&Plain).await.unwrap(), 2);
    n.change(slack.id, &patch(json!({"enabled": true})), OWNER)
        .await
        .unwrap();
    assert_eq!(n.dispatch(&Plain).await.unwrap(), 0);
    append(
        &t.pool,
        &journal,
        "copy_replaced",
        ("copy", "5"),
        json!({"title_id": 7, "kind": "show", "score_delta": 10}),
    )
    .await;
    assert_eq!(n.dispatch(&Plain).await.unwrap(), 3);
    assert_eq!(
        events(&of(&n, slack.id).await),
        ["file.upgraded", "file.upgraded"]
    );

    // A secret left empty stays; a new one is named in the journal, never its value.
    n.change(
        hook.id,
        &patch(json!({"settings": {"secret": "", "url": ""}})),
        OWNER,
    )
    .await
    .unwrap();
    let kept = n.channel(hook.id).await.unwrap().unwrap();
    assert_eq!(kept.secrets["secret"], "hook-secret-value");
    assert_eq!(kept.secrets["url"], HOOK);
    n.change(
        hook.id,
        &patch(json!({"settings": {"secret": "new-secret-value"}, "name": "Board"})),
        OWNER,
    )
    .await
    .unwrap();
    let kinds: Vec<String> = [
        "notification_channel_added",
        "notification_channel_changed",
        "notification_channel_removed",
    ]
    .iter()
    .map(|k| k.to_string())
    .collect();
    n.remove(off.id, OWNER).await.unwrap();
    assert!(
        !out.targets()
            .list()
            .iter()
            .any(|(k, _)| *k == format!("channel:{}", off.id))
    );
    let journaled = core_store::find_events(&t.pool, &kinds, &[], None, None, 100)
        .await
        .unwrap();
    // The change that left everything as it was is no event.
    assert_eq!(
        journaled.len(),
        5 + 3 + 1,
        "five added, three changed, one removed"
    );
    let renamed = journaled
        .iter()
        .find(|e| e.body["name"] == "Board")
        .unwrap();
    assert_eq!(renamed.body["changed"], json!(["name", "secret.secret"]));
    assert_eq!(renamed.actor, OWNER);
    for e in &journaled {
        let text = e.body.to_string();
        for secret in [
            "slack-secret-token",
            "hook-path-secret",
            "hook-secret-value",
            "new-secret-value",
            "discord-secret-token",
            "mail-password-value",
        ] {
            assert!(!text.contains(secret), "{} holds {secret}", e.kind);
        }
    }
    t.drop_db().await;
}

#[tokio::test]
async fn a_test_message_is_sent_at_once_and_answered_in_the_destination_s_words() {
    let Some(t) = TestDb::new("notify_test").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    let out = way_out(recorded.clone());
    let mails = Mails::new();
    let n = Notifier::new(t.pool.clone(), journal.clone(), out.clone(), mails.clone());
    let slack = n
        .create(
            &input(json!({"type": "slack", "name": "Ops", "settings": {"url": SLACK}})),
            OWNER,
        )
        .await
        .unwrap();
    let mail = n
        .create(
            &input(json!({"type": "email", "name": "Mail",
                          "settings": {"host": "smtp.example.org", "from": "singularr@example.org",
                                       "to": ["owner@example.org"], "user": "singularr",
                                       "password": "mail-password-value"}})),
            OWNER,
        )
        .await
        .unwrap();
    recorded.answer(Method::Post, SLACK, 404, "no_service");
    let tested = n.test_saved(slack.id).await.unwrap();
    assert!(!tested.ok);
    assert_eq!(
        (tested.status, tested.detail.as_str()),
        (Some(404), "no_service")
    );
    let recorded_test = of(&n, slack.id).await;
    assert_eq!(recorded_test.len(), 1);
    assert!(recorded_test[0].test && recorded_test[0].state == "failed");
    assert_eq!(Some(recorded_test[0].id), tested.delivery);
    recorded.answer(Method::Post, SLACK, 200, "ok");
    assert!(n.test_saved(slack.id).await.unwrap().ok);
    let posted = recorded.calls_sent();
    let Body::Json(text) = &posted.last().unwrap().body else {
        panic!("Slack is sent JSON");
    };
    assert!(text.contains("Test message: Ops"), "{text}");

    // A channel that is not saved: its address is listed for the one message only.
    recorded.answer(
        Method::Post,
        DISCORD_NEW,
        401,
        r#"{"message": "Invalid Webhook Token", "code": 50027}"#,
    );
    let tried = n
        .test_unsaved(
            &serde_json::from_value::<TestInput>(
                json!({"type": "discord", "name": "New", "settings": {"url": DISCORD_NEW}}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        (
            tried.ok,
            tried.status,
            tried.detail.as_str(),
            tried.delivery
        ),
        (false, Some(401), "Invalid Webhook Token", None)
    );
    assert!(
        out.check(Method::Post, DISCORD_NEW, &Body::Json("{}".into()))
            .is_err(),
        "taken off again"
    );
    assert_eq!(
        recorded
            .urls_seen()
            .iter()
            .filter(|u| *u == DISCORD_NEW)
            .count(),
        1
    );
    // A change is tested before it is saved, with the stored secret where the form left it
    // empty; the mail server's refusal comes back in its words.
    mails.refuse("535 5.7.8 Authentication failed");
    let refused = n
        .test_unsaved(
            &serde_json::from_value::<TestInput>(json!({"type": "email", "channel": mail.id,
                "settings": {"password": "", "port": 465, "security": "tls"}}))
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        (refused.ok, refused.detail.as_str()),
        (false, "535 5.7.8 Authentication failed")
    );
    let tried = mails.sent();
    assert_eq!(tried.len(), 1);
    assert_eq!(tried[0].0.port, 465);
    assert_eq!(tried[0].0.password.as_deref(), Some("mail-password-value"));
    // What is not a channel is refused before anything is sent.
    let errors = invalid(
        n.test_unsaved(
            &serde_json::from_value::<TestInput>(
                json!({"type": "gotify", "settings": {"server": "not a url"}}),
            )
            .unwrap(),
        )
        .await,
    );
    assert_eq!(errors["settings.server"], "Not an http or https address");
    assert_eq!(errors["settings.token"], "Required");
    t.drop_db().await;
}

#[tokio::test]
async fn processing_runs_that_close_or_stick_are_journaled_once() {
    let Some(t) = TestDb::new("notify_processing").await else {
        return;
    };
    let journal = Journal::new();
    let recorded = Recorded::new();
    let out = way_out(recorded.clone());
    let env = BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("HUSH_URL", HUSH),
    ]);
    let config = Config::from(&env).unwrap();
    let engines = Arc::new(EngineSet::from_config(&config, &out));
    let bridge = engines.processor("hush").unwrap().clone();
    let deps = processing::ProcessingDeps {
        pool: t.pool.clone(),
        journal: journal.clone(),
        engines: engines.clone(),
        interval: Duration::from_secs(60),
        jitter: Duration::ZERO,
        settings: Arc::new(singularr::settings::Settings::new(&config, &env, None)),
        policy: singularr::policy::PolicyCell::handle(singularr::policy::ActivePolicy::builtin()),
    };
    let ledger = fixture("hush_history.json");
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/health"),
        200,
        &fixture("hush_health.json"),
    );
    recorded.answer(Method::Get, &format!("{HUSH}/history.json"), 200, &ledger);
    let first = processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    assert_eq!(first.report.open, 2);
    assert!(first.closed.is_empty(), "nothing was open before");

    // The two staged runs started days ago and no worker is on them: stuck, journaled once.
    assert_eq!(processing::journal_stuck(&deps, &bridge).await.unwrap(), 2);
    assert_eq!(processing::journal_stuck(&deps, &bridge).await.unwrap(), 0);
    let stuck = core_store::find_events(
        &t.pool,
        &["processing_stuck".to_string()],
        &[],
        None,
        None,
        10,
    )
    .await
    .unwrap();
    assert_eq!(stuck.len(), 2);
    for e in &stuck {
        assert_eq!(e.actor, "sync:hush");
        assert!(e.body["open_for"].as_i64().unwrap() > 86_400);
        assert_eq!(
            singularr::notify::model::event_of(&e.kind, &e.body),
            Some("processing.stuck")
        );
    }

    // Then one is imported and the other fails: each is journaled as it closes.
    let mut changed: Value = serde_json::from_str(&ledger).unwrap();
    for job in changed["jobs"].as_array_mut().unwrap() {
        match job["id"].as_str() {
            Some("20261115T001015Z-cb34e7ec") => job["state"] = json!("promoted"),
            Some("20261111T214740Z-e90af04d") => job["state"] = json!("failed"),
            _ => {}
        }
    }
    recorded.answer(
        Method::Get,
        &format!("{HUSH}/history.json"),
        200,
        &changed.to_string(),
    );
    let second = processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    let mut closed: Vec<(String, String)> = second
        .closed
        .iter()
        .map(|r| (r.id.clone(), r.state.clone()))
        .collect();
    closed.sort();
    assert_eq!(
        closed,
        [
            (
                "hush:20261111T214740Z-e90af04d".to_string(),
                "failed".to_string()
            ),
            (
                "hush:20261115T001015Z-cb34e7ec".to_string(),
                "imported".to_string()
            ),
        ]
    );
    assert_eq!(
        processing::journal_closed(&t.pool, &journal, &bridge, &second.closed)
            .await
            .unwrap(),
        2
    );
    let kinds = [
        "processing_finished".to_string(),
        "processing_failed".to_string(),
    ];
    let news = core_store::find_events(&t.pool, &kinds, &[], None, None, 10)
        .await
        .unwrap();
    let mut seen: Vec<(String, String)> = news
        .iter()
        .map(|e| {
            (
                e.kind.clone(),
                e.body["run"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    seen.sort();
    assert_eq!(
        seen,
        [
            (
                "processing_failed".to_string(),
                "hush:20261111T214740Z-e90af04d".to_string()
            ),
            (
                "processing_finished".to_string(),
                "hush:20261115T001015Z-cb34e7ec".to_string()
            ),
        ]
    );
    for e in &news {
        assert!(e.body["flow_label"].as_str().is_some_and(|l| !l.is_empty()));
        assert!(e.body["file"].as_str().is_some_and(|f| f.ends_with(".mkv")));
    }
    // A pass that finds nothing new closes nothing.
    let third = processing::pass(&t.pool, &bridge, processing::Read::Whole)
        .await
        .unwrap();
    assert!(third.closed.is_empty());
    t.drop_db().await;
}

// The routes ------------------------------------------------------------------------------------

use axum::http::{Request, StatusCode};
use singularr::api::routes;
use singularr::api::state::{AppState, Inner};
use std::sync::Mutex;
use tower::ServiceExt;

/// The application state over a test database: no integration but the bridge, a way out over
/// recorded answers, a recorded mailer.
fn app_state(pool: &PgPool, journal: Journal, out: &Outbound, mails: Arc<Mails>) -> AppState {
    let env = BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("SINGULARR_OWNER_EMAILS", "owner@example.com"),
    ]);
    let config = Config::from(&env).unwrap();
    AppState::new(Inner {
        engines: Arc::new(EngineSet::from_config(&config, out)),
        metadata: Arc::new(singularr::metadata::service::ProviderSet::from_config(
            &config, out,
        )),
        owners: singularr::core::identity::Allowlist::parse(&config.owner_emails),
        requests: Arc::new(singularr::requests::service::Requests::new(
            &config.request_rules,
        )),
        music: Arc::new(singularr::music::service::MusicLibrary::new(
            &config.music.root,
        )),
        home_movies: Arc::new(singularr::homemovies::service::HomeMovies::new(
            &config.home_movies.root,
        )),
        settings: Arc::new(singularr::settings::Settings::new(&config, &env, None)),
        pages: Default::default(),
        search: Arc::new(singularr::search::Search::new(
            pool.clone(),
            &config.search,
            out,
        )),
        notify: Notifier::new(pool.clone(), journal.clone(), out.clone(), mails),
        config,
        pool: pool.clone(),
        journal,
        policy: singularr::policy::PolicyCell::handle(singularr::policy::ActivePolicy::builtin()),
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
            axum::body::Body::from(b.to_string())
        }
        None => axum::body::Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Log lines, kept to be read.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Lines;

    fn make_writer(&'a self) -> Lines {
        self.clone()
    }
}

const SECRETS: [&str; 4] = [
    "slack-secret-token",
    "hook-path-secret",
    "hook-secret-value",
    "discord-new-token",
];

/// Every log line of this test process, from any thread: the global subscriber, set once.
fn log_lines() -> Lines {
    static LOG: std::sync::OnceLock<Lines> = std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        let lines = Lines::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(lines.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        if let Err(e) = tracing::subscriber::set_global_default(subscriber) {
            eprintln!("the log capture could not be installed: {e}");
        }
        lines
    })
    .clone()
}

fn holds_no_secret(what: &str, text: &str) {
    for secret in SECRETS {
        assert!(!text.contains(secret), "{what} holds {secret}: {text}");
    }
}

#[tokio::test]
async fn the_routes_are_the_owner_s_and_never_show_a_secret() {
    let Some(t) = TestDb::new("notify_routes").await else {
        return;
    };
    let lines = log_lines();
    tracing::warn!(target: "singularr::test", "probe of the log capture");
    let journal = Journal::new();
    let recorded = Recorded::new();
    let out = way_out(recorded.clone());
    let st = app_state(&t.pool, journal.clone(), &out, Mails::new());
    let app = routes::router(st.clone());
    let owner_caller = singularr::core::auth::Caller::Owner {
        email: "owner@example.com".into(),
    };
    let reader = singularr::core::auth::create_key(
        &t.pool,
        &journal,
        &owner_caller,
        &singularr::core::model::NewApiKey {
            name: "reader".into(),
            scopes: vec![singularr::core::model::Scope::Read],
        },
    )
    .await
    .unwrap();
    let bearer = format!("Bearer {}", reader.secret);
    let read: [(&str, &str); 1] = [("authorization", bearer.as_str())];
    let owner: [(&str, &str); 1] = [("x-auth-request-email", "owner@example.com")];

    // A reader sees the types, the events and the channels.
    let (status, view) = call(&app, "GET", "/api/v1/notifications", &read, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["types"].as_array().unwrap().len(), 8);
    assert_eq!(view["events"].as_array().unwrap().len(), EVENTS.len());
    assert_eq!(view["groups"].as_array().unwrap().len(), 5);
    assert_eq!(view["channels"], json!([]));

    // Every type refuses a channel without its required fields, by key.
    for ty in view["types"].as_array().unwrap() {
        let kind = ty["type"].as_str().unwrap();
        let (status, refused) = call(
            &app,
            "POST",
            "/api/v1/notifications/channels",
            &owner,
            Some(json!({"type": kind, "name": "x", "settings": {}})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{kind}");
        let mut wanted: Vec<String> = ty["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["required"] == true && f.get("default").is_none())
            .map(|f| format!("settings.{}", f["key"].as_str().unwrap()))
            .collect();
        wanted.sort();
        let mut got: Vec<String> = refused["errors"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        got.sort();
        assert_eq!(got, wanted, "{kind}");
    }

    // Only the owner adds a channel.
    let slack = json!({"type": "slack", "name": "Ops", "settings": {"url": SLACK},
                       "events": ["file.upgraded"]});
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/notifications/channels",
        &read,
        Some(slack.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/notifications/channels",
        &owner,
        Some(slack),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    holds_no_secret("the answer", &created.to_string());
    assert_eq!(created["secrets"], json!({"url": true}));
    assert_eq!(created["settings"], json!({}));
    assert_eq!(
        (created["destination"].as_str(), created["state"].as_str()),
        (Some("hooks.slack.com"), Some("new"))
    );
    let id = created["id"].as_i64().unwrap();
    let (status, hook) = call(
        &app,
        "POST",
        "/api/v1/notifications/channels",
        &owner,
        Some(json!({"type": "webhook", "name": "Board",
                    "settings": {"url": HOOK, "secret": "hook-secret-value"}})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(hook["secrets"], json!({"secret": true, "url": true}));
    let hook_id = hook["id"].as_i64().unwrap();

    // Changes: the owner's; a bad address is refused by key.
    let path = format!("/api/v1/notifications/channels/{id}");
    let (status, _) = call(&app, "PATCH", &path, &read, Some(json!({"enabled": false}))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, refused) = call(
        &app,
        "PATCH",
        &path,
        &owner,
        Some(json!({"settings": {"url": "nonsense"}})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        refused["errors"]["settings.url"],
        "Not an http or https address"
    );
    let (status, renamed) = call(
        &app,
        "PATCH",
        &path,
        &owner,
        Some(json!({"name": "Ops room", "settings": {"url": ""}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(renamed["name"], "Ops room");
    assert_eq!(
        renamed["secrets"],
        json!({"url": true}),
        "the empty field kept it"
    );

    // A test now, in the destination's words; the owner's alone.
    recorded.answer(Method::Post, SLACK, 200, "ok");
    let test = format!("{path}/test");
    let (status, _) = call(&app, "POST", &test, &read, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, tested) = call(&app, "POST", &test, &owner, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&tested["ok"], &tested["status"], &tested["detail"]),
        (&json!(true), &json!(200), &json!("ok"))
    );
    let (status, tested) = call(&app, "POST", &test, &owner, None).await;
    assert_eq!((status, &tested["ok"]), (StatusCode::OK, &json!(true)));
    recorded.answer(
        Method::Post,
        DISCORD_NEW,
        401,
        r#"{"message": "Invalid Webhook Token", "code": 50027}"#,
    );
    let (status, tried) = call(
        &app,
        "POST",
        "/api/v1/notifications/test",
        &owner,
        Some(json!({"type": "discord", "settings": {"url": DISCORD_NEW}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (&tried["ok"], &tried["status"], &tried["detail"]),
        (&json!(false), &json!(401), &json!("Invalid Webhook Token"))
    );
    holds_no_secret("a test's answer", &tried.to_string());
    let (status, _) = call(
        &app,
        "POST",
        "/api/v1/notifications/test",
        &read,
        Some(json!({"type": "discord", "settings": {"url": DISCORD_NEW}})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The delivery log, a page at a time.
    let (status, page) = call(
        &app,
        "GET",
        &format!("/api/v1/notifications/deliveries?channel={id}&limit=1"),
        &read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["deliveries"].as_array().unwrap().len(), 1);
    let first = &page["deliveries"][0];
    assert_eq!(
        (&first["event"], &first["state"], &first["test"]),
        (&json!("test"), &json!("sent"), &json!(true))
    );
    assert_eq!(first["headline"], "Test message: Ops room");
    assert_eq!(first["channelName"], "Ops room");
    let next = page["next"].as_i64().expect("an older page");
    let (_, older) = call(
        &app,
        "GET",
        &format!("/api/v1/notifications/deliveries?channel={id}&limit=1&before={next}"),
        &read,
        None,
    )
    .await;
    assert_eq!(older["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(older["next"], Value::Null);
    let (_, view) = call(&app, "GET", "/api/v1/notifications", &read, None).await;
    holds_no_secret("the overview", &view.to_string());
    let ops = view["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap();
    assert_eq!(ops["state"], "ok");
    assert_eq!(ops["lastDelivery"]["state"], "sent");
    assert_eq!(view["deliveries"].as_array().unwrap().len(), 2);

    // Removed: the owner's; then it is gone.
    let (status, _) = call(&app, "DELETE", &path, &read, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, gone) = call(&app, "DELETE", &path, &owner, None).await;
    assert_eq!(status, StatusCode::OK);
    holds_no_secret("the removal's answer", &gone.to_string());
    let (status, _) = call(&app, "DELETE", &path, &owner, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, view) = call(&app, "GET", "/api/v1/notifications", &read, None).await;
    assert_eq!(view["channels"].as_array().unwrap().len(), 1);
    assert_eq!(view["channels"][0]["id"], hook_id);

    // The journal names each change, never a secret.
    let (status, journal_page) = call(
        &app,
        "GET",
        "/api/v1/journal?kind=notification.added,notification.changed,notification.removed",
        &read,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let kinds: Vec<&str> = journal_page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        [
            "notification.removed",
            "notification.changed",
            "notification.added",
            "notification.added",
        ]
    );
    holds_no_secret("the journal", &journal_page.to_string());

    // The document lists the routes and the webhook's document.
    let (_, doc) = call(&app, "GET", "/api/v1/openapi.json", &read, None).await;
    for (path, method) in [
        ("/api/v1/notifications", "get"),
        ("/api/v1/notifications/channels", "post"),
        ("/api/v1/notifications/channels/{id}", "patch"),
        ("/api/v1/notifications/channels/{id}", "delete"),
        ("/api/v1/notifications/channels/{id}/test", "post"),
        ("/api/v1/notifications/test", "post"),
        ("/api/v1/notifications/deliveries", "get"),
    ] {
        assert!(doc["paths"][path][method].is_object(), "{method} {path}");
    }
    assert!(doc["components"]["schemas"]["WebhookDocument"].is_object());

    // Every call went out and was logged, and no log line holds a secret.
    let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains("probe of the log capture"),
        "the log capture works: {} bytes",
        logged.len()
    );
    let outbound: Vec<&str> = logged
        .lines()
        .filter(|l| l.contains("singularr::outbound"))
        .collect();
    assert!(
        outbound.iter().any(|l| l.contains("notify:slack")),
        "the calls are logged: {}",
        outbound.join("\n")
    );
    assert!(outbound.iter().any(|l| l.contains("(channel address)")));
    holds_no_secret("the log", &logged);
    t.drop_db().await;
}
