//! Channels as the API changes them, the dispatcher that follows the journal, and the sender.
//!
//! - **Changes.** A channel is added, changed and removed with one journal event each
//!   (`notification_channel_added`, `notification_channel_changed`,
//!   `notification_channel_removed`) that names what changed and never holds a secret. Its HTTP
//!   address is listed in the way out as a target of its own, under `channel:<id>`.
//! - **Dispatch.** After each commit (the journal's live channel), and every minute, the
//!   dispatcher reads the journal past the channels' cursors, has the events that are
//!   notification events described ([`Describe`]), writes one delivery for each channel that
//!   wants each, and moves the cursors, in one transaction. The cursors are stored, so nothing
//!   is sent twice across a restart; a channel's cursor starts at the event of its own
//!   creation, so nothing from before it is sent; a disabled channel's cursor moves without a
//!   delivery, and one enabled again starts at that change.
//! - **Sending.** A delivery is claimed (its attempt counted) before anything is sent. Three
//!   attempts, 30 s and then 2 min apart; after the third it is failed, and the channel shows
//!   it. An attempt a stop cut short counts, and the delivery waits again.
//! - **Tests.** A test message goes out at once, one attempt, and the answer comes back in the
//!   destination's own words.

use super::message::{self, Message};
use super::model::{self, ChannelType, Checked, Errors, Notice};
use super::store::{self, ChannelRow, ChannelWrite, DeliveryRow, Ended, NewDelivery};
use crate::core::journal::{Journal, event};
use crate::core::model::Event;
use crate::core::outbound::{Body, OutRequest, Outbound, Target};
use crate::core::smtp::{MailError, Mailer, Security, Server};
use crate::core::store as core_store;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};
use tokio::task::JoinHandle;

/// Attempts a message gets before its delivery is failed.
pub const ATTEMPTS: i32 = 3;
/// Seconds after a failed attempt before the next: after the first, after the second.
pub const RETRY_AFTER: [i64; 2] = [30, 120];
/// Journal events one dispatch reads at a time.
const PAGE: i64 = 200;
/// Deliveries one round sends at most.
const ROUND: i64 = 50;
/// The longest the dispatcher waits without a commit.
const IDLE: Duration = Duration::from_secs(60);
/// How long a burst of commits is let settle before a round.
const SETTLE: Duration = Duration::from_secs(2);

/// The key a channel's address is listed under in the way out.
fn target_key(id: i64) -> String {
    format!("channel:{id}")
}

fn on() -> bool {
    true
}

/// A channel to add (`POST /api/v1/notifications/channels`). It holds secrets, so it has no
/// `Debug`.
#[derive(Clone, Deserialize, utoipa::ToSchema)]
pub struct ChannelInput {
    /// `slack`, `discord`, `email`, `webhook`, `ntfy`, `telegram`, `pushover` or `gotify`.
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default = "on")]
    pub enabled: bool,
    /// The type's fields by key, its secrets among them.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub settings: Map<String, Value>,
    /// The events it is told about.
    #[serde(default)]
    pub events: Vec<String>,
    /// `movie`, `show`; none: both.
    #[serde(default)]
    pub kinds: Vec<String>,
}

/// A change to a channel (`PATCH /api/v1/notifications/channels/{id}`). What it leaves out
/// stays. In `settings`, a field left out stays, a secret given as empty text stays, and
/// `null` empties a field.
#[derive(Clone, Default, Deserialize, utoipa::ToSchema)]
pub struct ChannelPatch {
    pub name: Option<String>,
    pub enabled: Option<bool>,
    #[schema(value_type = Option<Object>)]
    pub settings: Option<Map<String, Value>>,
    pub events: Option<Vec<String>>,
    pub kinds: Option<Vec<String>>,
}

/// A channel to test before it is saved (`POST /api/v1/notifications/test`).
#[derive(Clone, Deserialize, utoipa::ToSchema)]
pub struct TestInput {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    #[schema(value_type = Object)]
    pub settings: Map<String, Value>,
    /// A saved channel of the same type whose secrets fill the ones left empty: a change is
    /// tested before it is saved.
    #[serde(default)]
    pub channel: Option<i64>,
}

/// How a test message went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TestOutcome {
    /// The destination took the message.
    pub ok: bool,
    /// Its status (HTTP), when it answered.
    pub status: Option<i32>,
    /// What it said, in its own words, or why nothing reached it.
    pub detail: String,
    /// How long it took, in milliseconds.
    pub ms: i64,
    /// The delivery that records it; none for a channel that is not saved.
    pub delivery: Option<i64>,
}

/// Why a change was not made.
#[derive(Debug)]
pub enum ChangeError {
    /// What was given makes no channel: why, by key (`name`, `settings.url`).
    Invalid(Errors),
    NotFound,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ChangeError {
    fn from(e: sqlx::Error) -> Self {
        ChangeError::Db(e)
    }
}

/// Turns journal events into notices: the event as the journal route gives it, its title
/// card, its facts, the link to its title. The layers above have the words and the cards;
/// the dispatcher is handed one.
#[async_trait]
pub trait Describe: Send + Sync {
    /// One notice for each event, in order. Each comes with the notification event it is.
    async fn describe(&self, events: &[(Event, &'static str)]) -> Result<Vec<Notice>, String>;
}

/// One attempt to send a message.
#[derive(Debug, Clone, PartialEq)]
struct Attempt {
    ok: bool,
    status: Option<i32>,
    detail: String,
    ms: i64,
}

impl Attempt {
    fn failed(why: &str) -> Attempt {
        Attempt {
            ok: false,
            status: None,
            detail: why.to_string(),
            ms: 0,
        }
    }
}

/// A channel's settings and secrets, as checked when they were saved.
pub fn checked_of(row: &ChannelRow) -> Checked {
    Checked {
        settings: row.settings.clone(),
        secrets: row.secrets.clone(),
    }
}

/// The mail server of an email channel.
fn mail_server(checked: &Checked) -> Option<Server> {
    Some(Server {
        host: checked.text("host")?.to_string(),
        port: u16::try_from(checked.int("port")?).ok()?,
        security: Security::parse(checked.text("security").unwrap_or("starttls"))?,
        user: checked.text("user").map(str::to_string),
        password: checked.text("password").map(str::to_string),
    })
}

/// The field that holds the address of an HTTP channel, for an error about it.
fn address_field(kind: ChannelType) -> &'static str {
    match kind {
        ChannelType::Ntfy | ChannelType::Gotify => "settings.server",
        ChannelType::Telegram | ChannelType::Pushover => "settings.token",
        _ => "settings.url",
    }
}

/// What a journal event about a channel says: never a secret, only which secret changed.
fn channel_body(row: &ChannelRow, changed: &[String]) -> Value {
    json!({
        "channel_id": row.id,
        "name": row.name,
        "type": row.kind,
        "enabled": row.enabled,
        "events": row.events,
        "kinds": row.kinds,
        "changed": changed,
    })
}

/// What a change changed, by name: `name`, `enabled`, `events`, `kinds`, `settings.<field>`,
/// `secret.<field>` (a secret's name, never its value).
fn changes(old: &ChannelRow, new: &ChannelRow) -> Vec<String> {
    let mut out = Vec::new();
    if old.name != new.name {
        out.push("name".to_string());
    }
    if old.enabled != new.enabled {
        out.push("enabled".to_string());
    }
    if old.events != new.events {
        out.push("events".to_string());
    }
    if old.kinds != new.kinds {
        out.push("kinds".to_string());
    }
    changed_keys("settings", &old.settings, &new.settings, &mut out);
    changed_keys("secret", &old.secrets, &new.secrets, &mut out);
    out
}

/// The keys whose values differ between two maps, as `<prefix>.<key>`.
fn changed_keys(
    prefix: &str,
    a: &Map<String, Value>,
    b: &Map<String, Value>,
    out: &mut Vec<String>,
) {
    let mut names: Vec<&String> = a.keys().chain(b.keys()).collect();
    names.sort();
    names.dedup();
    for key in names {
        if a.get(key) != b.get(key) {
            out.push(format!("{prefix}.{key}"));
        }
    }
}

/// The value of a check, or its error under `key`.
fn keep<T>(errors: &mut Errors, key: &str, checked: Result<T, String>) -> Option<T> {
    match checked {
        Ok(value) => Some(value),
        Err(why) => {
            errors.insert(key.to_string(), why);
            None
        }
    }
}

/// A channel that passed its checks.
struct Valid {
    kind: ChannelType,
    name: String,
    checked: Checked,
    events: Vec<String>,
    kinds: Vec<String>,
}

/// What a channel is checked with.
struct Asked<'a> {
    kind: &'a str,
    name: &'a str,
    settings: Option<&'a Map<String, Value>>,
    stored: Option<&'a Checked>,
    events: &'a [String],
    kinds: &'a [String],
}

/// Channels, their dispatch and their messages. Every clone shares the way out, and with it
/// the channels' addresses.
#[derive(Clone)]
pub struct Notifier {
    pool: PgPool,
    journal: Journal,
    out: Outbound,
    mail: Arc<dyn Mailer>,
}

impl Notifier {
    pub fn new(pool: PgPool, journal: Journal, out: Outbound, mail: Arc<dyn Mailer>) -> Notifier {
        Notifier {
            pool,
            journal,
            out,
            mail,
        }
    }

    /// At start: every channel's address into the way out, and the attempts a stop cut short
    /// back in the queue. Returns how many channels there are.
    pub async fn load(&self) -> Result<usize, sqlx::Error> {
        let channels = store::channels(&self.pool).await?;
        for c in &channels {
            if let Some(kind) = ChannelType::parse(&c.kind) {
                self.list(c.id, kind, &checked_of(c));
            }
        }
        store::reset_interrupted(&self.pool, ATTEMPTS, Utc::now()).await?;
        Ok(channels.len())
    }

    /// List a channel's address in the way out, or take it off when it has none.
    fn list(&self, id: i64, kind: ChannelType, checked: &Checked) {
        let key = target_key(id);
        let target = model::target(kind, checked)
            .and_then(|url| Target::new(&format!("notify:{}", kind.as_str()), &url).ok());
        match target {
            Some(t) => self.out.targets().put(&key, t),
            None => self.out.targets().remove(&key),
        }
    }

    /// Check a channel. An address that is an integration's is refused: a channel never gets
    /// more than its one address, and an integration's host is judged by its own rule.
    fn check(&self, asked: Asked<'_>) -> Result<Valid, Errors> {
        let mut errors = Errors::new();
        let name = keep(&mut errors, "name", model::check_name(asked.name));
        let events = keep(&mut errors, "events", model::check_events(asked.events));
        let kinds = keep(&mut errors, "kinds", model::check_kinds(asked.kinds));
        let Some(kind) = ChannelType::parse(asked.kind) else {
            errors.insert("type".to_string(), "Not a type of channel".to_string());
            return Err(errors);
        };
        let checked = match model::check_settings(kind, asked.settings, asked.stored) {
            Ok(checked) => Some(checked),
            Err(e) => {
                errors.extend(e);
                None
            }
        };
        if let Some(url) = checked.as_ref().and_then(|c| model::target(kind, c))
            && self.out.listed(&url).is_some()
        {
            errors.insert(
                address_field(kind).to_string(),
                "An integration's address".to_string(),
            );
        }
        match (name, events, kinds, checked) {
            (Some(name), Some(events), Some(kinds), Some(checked)) if errors.is_empty() => {
                Ok(Valid {
                    kind,
                    name,
                    checked,
                    events,
                    kinds,
                })
            }
            _ => Err(errors),
        }
    }

    pub async fn channels(&self) -> Result<Vec<ChannelRow>, sqlx::Error> {
        store::channels(&self.pool).await
    }

    pub async fn channel(&self, id: i64) -> Result<Option<ChannelRow>, sqlx::Error> {
        store::channel(&self.pool, id).await
    }

    /// Each channel's newest delivery that ended, by channel.
    pub async fn last_ended(&self) -> Result<HashMap<i64, DeliveryRow>, sqlx::Error> {
        Ok(store::last_ended(&self.pool)
            .await?
            .into_iter()
            .map(|d| (d.channel_id, d))
            .collect())
    }

    /// Deliveries, newest first: of one channel or of all, older than `before`.
    pub async fn deliveries(
        &self,
        channel: Option<i64>,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<DeliveryRow>, sqlx::Error> {
        store::deliveries(&self.pool, channel, before, limit).await
    }

    /// Add a channel. It is told about what happens from now on.
    pub async fn create(&self, input: &ChannelInput, by: &str) -> Result<ChannelRow, ChangeError> {
        let v = self
            .check(Asked {
                kind: &input.kind,
                name: &input.name,
                settings: Some(&input.settings),
                stored: None,
                events: &input.events,
                kinds: &input.kinds,
            })
            .map_err(ChangeError::Invalid)?;
        let mut tx = self.journal.begin(&self.pool).await?;
        let row = store::insert_channel(
            tx.conn(),
            &ChannelWrite {
                kind: v.kind.as_str(),
                name: &v.name,
                enabled: input.enabled,
                settings: &v.checked.settings,
                secrets: &v.checked.secrets,
                events: &v.events,
                kinds: &v.kinds,
                by,
            },
        )
        .await?;
        let added = tx
            .append_one(event(
                "notification_channel_added",
                "notification_channel",
                row.id,
                by,
                channel_body(&row, &[]),
            ))
            .await?;
        store::start_at(tx.conn(), row.id, added.id).await?;
        tx.commit().await?;
        self.list(row.id, v.kind, &v.checked);
        Ok(ChannelRow {
            last_event: added.id,
            ..row
        })
    }

    /// Change a channel. Disabled, what waits for it is dropped; enabled again, it is told
    /// about what happens from then on.
    pub async fn change(
        &self,
        id: i64,
        patch: &ChannelPatch,
        by: &str,
    ) -> Result<ChannelRow, ChangeError> {
        let old = store::channel(&self.pool, id)
            .await?
            .ok_or(ChangeError::NotFound)?;
        let stored = checked_of(&old);
        let v = self
            .check(Asked {
                kind: &old.kind,
                name: patch.name.as_deref().unwrap_or(&old.name),
                settings: patch.settings.as_ref(),
                stored: Some(&stored),
                events: patch.events.as_deref().unwrap_or(&old.events),
                kinds: patch.kinds.as_deref().unwrap_or(&old.kinds),
            })
            .map_err(ChangeError::Invalid)?;
        let enabled = patch.enabled.unwrap_or(old.enabled);
        let mut tx = self.journal.begin(&self.pool).await?;
        let row = store::update_channel(
            tx.conn(),
            id,
            &ChannelWrite {
                kind: v.kind.as_str(),
                name: &v.name,
                enabled,
                settings: &v.checked.settings,
                secrets: &v.checked.secrets,
                events: &v.events,
                kinds: &v.kinds,
                by,
            },
        )
        .await?
        .ok_or(ChangeError::NotFound)?;
        let changed = changes(&old, &row);
        let mut last_event = row.last_event;
        if !changed.is_empty() {
            let e = tx
                .append_one(event(
                    "notification_channel_changed",
                    "notification_channel",
                    id,
                    by,
                    channel_body(&row, &changed),
                ))
                .await?;
            if enabled && !old.enabled {
                store::start_at(tx.conn(), id, e.id).await?;
                last_event = e.id;
            }
        }
        if !enabled {
            store::drop_waiting(tx.conn(), id).await?;
        }
        tx.commit().await?;
        self.list(id, v.kind, &v.checked);
        Ok(ChannelRow { last_event, ..row })
    }

    /// Remove a channel and what was sent to it.
    pub async fn remove(&self, id: i64, by: &str) -> Result<ChannelRow, ChangeError> {
        let mut tx = self.journal.begin(&self.pool).await?;
        let row = store::delete_channel(tx.conn(), id)
            .await?
            .ok_or(ChangeError::NotFound)?;
        tx.append_one(event(
            "notification_channel_removed",
            "notification_channel",
            id,
            by,
            channel_body(&row, &[]),
        ))
        .await?;
        tx.commit().await?;
        self.out.targets().remove(&target_key(id));
        Ok(row)
    }

    /// Send one message to one channel now.
    async fn deliver(
        &self,
        kind: ChannelType,
        checked: &Checked,
        notice: &Notice,
        delivery: Option<i64>,
    ) -> Attempt {
        let started = Instant::now();
        let mut attempt = match message::compose(kind, checked, notice, delivery) {
            Err(why) => Attempt::failed(&why),
            Ok(Message::Post { url, headers, body }) => {
                let mut req = OutRequest::post(url, Body::Json(body.to_string()));
                for (name, value) in headers {
                    req = req.header(&name, value);
                }
                match self.out.send(req).await {
                    Ok(res) => Attempt {
                        ok: res.ok(),
                        status: Some(i32::from(res.status)),
                        detail: message::answer_words(&res.body),
                        ms: 0,
                    },
                    Err(e) => Attempt::failed(&e.to_string()),
                }
            }
            Ok(Message::Mail(mail)) => match mail_server(checked) {
                None => Attempt::failed("The channel has no mail server"),
                Some(server) => match self.mail.send(&server, &mail).await {
                    Ok(words) => Attempt {
                        ok: true,
                        status: None,
                        detail: words,
                        ms: 0,
                    },
                    Err(MailError::Refused(words)) => Attempt::failed(&words),
                    Err(e) => Attempt::failed(&e.to_string()),
                },
            },
        };
        attempt.detail = message::scrub(&attempt.detail, &checked.secret_values());
        attempt.ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        attempt
    }

    /// Send a test message to a saved channel now, and record it with its deliveries.
    pub async fn test_saved(&self, id: i64) -> Result<TestOutcome, ChangeError> {
        let row = store::channel(&self.pool, id)
            .await?
            .ok_or(ChangeError::NotFound)?;
        let Some(kind) = ChannelType::parse(&row.kind) else {
            return Err(ChangeError::Invalid(Errors::from([(
                "type".to_string(),
                "Not a type of channel".to_string(),
            )])));
        };
        let checked = checked_of(&row);
        let now = Utc::now();
        let notice = model::test_notice(&row.name, kind, now);
        let json = serde_json::to_value(&notice).unwrap_or(Value::Null);
        let delivery = store::insert_attempt(&self.pool, id, model::TEST, &json, now).await?;
        self.list(id, kind, &checked);
        let attempt = self.deliver(kind, &checked, &notice, Some(delivery)).await;
        store::finish(
            &self.pool,
            delivery,
            &Ended {
                state: if attempt.ok {
                    store::SENT
                } else {
                    store::FAILED
                },
                next_at: None,
                at: Utc::now(),
                status: attempt.status,
                detail: Some(attempt.detail.clone()).filter(|d| !d.is_empty()),
            },
        )
        .await?;
        Ok(TestOutcome {
            ok: attempt.ok,
            status: attempt.status,
            detail: attempt.detail,
            ms: attempt.ms,
            delivery: Some(delivery),
        })
    }

    /// Send a test message to a channel that is not saved: its address is listed for this
    /// one message and taken off again.
    pub async fn test_unsaved(&self, input: &TestInput) -> Result<TestOutcome, ChangeError> {
        let Some(kind) = ChannelType::parse(&input.kind) else {
            return Err(ChangeError::Invalid(Errors::from([(
                "type".to_string(),
                "Not a type of channel".to_string(),
            )])));
        };
        let saved = match input.channel {
            Some(id) => Some(
                store::channel(&self.pool, id)
                    .await?
                    .ok_or(ChangeError::NotFound)?,
            ),
            None => None,
        };
        let stored = saved
            .as_ref()
            .filter(|row| row.kind == kind.as_str())
            .map(checked_of);
        let name = input
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(kind.label())
            .to_string();
        let no_events: [String; 0] = [];
        let v = self
            .check(Asked {
                kind: kind.as_str(),
                name: &name,
                settings: Some(&input.settings),
                stored: stored.as_ref(),
                events: &no_events,
                kinds: &no_events,
            })
            .map_err(ChangeError::Invalid)?;
        let notice = model::test_notice(&v.name, kind, Utc::now());
        let key = format!("test:{}", uuid::Uuid::new_v4().simple());
        if let Some(t) = model::target(kind, &v.checked)
            .and_then(|url| Target::new(&format!("notify:{}", kind.as_str()), &url).ok())
        {
            self.out.targets().put(&key, t);
        }
        let attempt = self.deliver(kind, &v.checked, &notice, None).await;
        self.out.targets().remove(&key);
        Ok(TestOutcome {
            ok: attempt.ok,
            status: attempt.status,
            detail: attempt.detail,
            ms: attempt.ms,
            delivery: None,
        })
    }

    /// One round of the dispatcher: the journal past the channels' cursors, a page at a time,
    /// into deliveries. Returns how many deliveries it wrote.
    pub async fn dispatch(&self, describe: &dyn Describe) -> Result<u64, String> {
        let mut written = 0;
        loop {
            let channels = store::channels(&self.pool)
                .await
                .map_err(|e| e.to_string())?;
            let Some(from) = channels.iter().map(|c| c.last_event).min() else {
                return Ok(written);
            };
            let page = core_store::events_after(&self.pool, from, PAGE)
                .await
                .map_err(|e| e.to_string())?;
            let Some(last) = page.last().map(|e| e.id) else {
                return Ok(written);
            };
            let wanted: Vec<(Event, &'static str)> = page
                .into_iter()
                .filter_map(|e| model::event_of(&e.kind, &e.body).map(|id| (e, id)))
                .collect();
            let notices = if wanted.is_empty() {
                Vec::new()
            } else {
                describe.describe(&wanted).await?
            };
            let mut rows: Vec<NewDelivery> = Vec::new();
            for c in channels.iter().filter(|c| c.enabled) {
                for ((e, _), notice) in wanted.iter().zip(&notices) {
                    if e.id > c.last_event && model::wants(&c.events, &c.kinds, notice) {
                        rows.push(NewDelivery {
                            channel_id: c.id,
                            event_id: Some(e.id),
                            event: notice.event.clone(),
                            notice: serde_json::to_value(notice).unwrap_or(Value::Null),
                        });
                    }
                }
            }
            let ids: Vec<i64> = channels.iter().map(|c| c.id).collect();
            let stored: Result<u64, sqlx::Error> = async {
                let mut tx = self.pool.begin().await?;
                let added = store::insert_deliveries(&mut tx, &rows, Utc::now()).await?;
                store::advance(&mut tx, &ids, last).await?;
                tx.commit().await?;
                Ok(added)
            }
            .await;
            written += stored.map_err(|e| e.to_string())?;
        }
    }

    /// Send what is due at `now`, a round's worth at most. A failed attempt waits
    /// [`RETRY_AFTER`] for the next; the third fails the delivery. Returns the attempts made.
    pub async fn send_due(&self, now: DateTime<Utc>) -> Result<usize, String> {
        let due = store::due(&self.pool, now, ROUND)
            .await
            .map_err(|e| e.to_string())?;
        if due.is_empty() {
            return Ok(0);
        }
        let channels: HashMap<i64, ChannelRow> = store::channels(&self.pool)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|c| (c.id, c))
            .collect();
        let mut made = 0;
        for d in due {
            let Some(number) = store::claim(&self.pool, d.id)
                .await
                .map_err(|e| e.to_string())?
            else {
                continue;
            };
            made += 1;
            let channel = channels.get(&d.channel_id);
            let notice: Option<Notice> = serde_json::from_value(d.notice.clone()).ok();
            let kind = channel.and_then(|c| ChannelType::parse(&c.kind));
            let attempt = match (channel, kind, notice) {
                (Some(c), _, _) if !c.enabled => Attempt::failed("Disabled"),
                (Some(c), Some(kind), Some(notice)) => {
                    self.deliver(kind, &checked_of(c), &notice, Some(d.id))
                        .await
                }
                (None, _, _) => Attempt::failed("The channel was removed"),
                (_, None, _) => Attempt::failed("Not a type of channel"),
                (_, _, None) => Attempt::failed("The message could not be read"),
            };
            let at = Utc::now();
            let ended = if attempt.ok {
                Ended {
                    state: store::SENT,
                    next_at: None,
                    at,
                    status: attempt.status,
                    detail: Some(attempt.detail).filter(|d| !d.is_empty()),
                }
            } else if number < ATTEMPTS && channel.is_some_and(|c| c.enabled) {
                let index = usize::try_from(number - 1)
                    .unwrap_or(0)
                    .min(RETRY_AFTER.len() - 1);
                Ended {
                    state: store::PENDING,
                    next_at: Some(now + chrono::Duration::seconds(RETRY_AFTER[index])),
                    at,
                    status: attempt.status,
                    detail: Some(attempt.detail),
                }
            } else {
                Ended {
                    state: store::FAILED,
                    next_at: None,
                    at,
                    status: attempt.status,
                    detail: Some(attempt.detail),
                }
            };
            store::finish(&self.pool, d.id, &ended)
                .await
                .map_err(|e| e.to_string())?;
        }
        Ok(made)
    }

    /// The dispatcher and the sender, after each commit and every minute, until the journal
    /// closes.
    pub fn spawn(self, describe: Arc<dyn Describe>) -> JoinHandle<()> {
        tokio::spawn(async move { self.run(describe).await })
    }

    async fn run(self, describe: Arc<dyn Describe>) {
        let mut events = self.journal.subscribe();
        loop {
            match self.dispatch(describe.as_ref()).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(deliveries = n, "notifications dispatched"),
                Err(e) => tracing::warn!(error = %e, "notifications could not be dispatched"),
            }
            match self.send_due(Utc::now()).await {
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "notifications could not be sent"),
            }
            // The next commit, the next delivery that is due, or a minute.
            let wait = match store::next_due(&self.pool).await {
                Ok(Some(at)) => (at - Utc::now()).to_std().unwrap_or(Duration::ZERO),
                _ => IDLE,
            }
            .clamp(Duration::from_secs(1), IDLE);
            tokio::select! {
                got = events.recv() => match got {
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        tokio::time::sleep(SETTLE).await;
                        while matches!(events.try_recv(), Ok(_) | Err(TryRecvError::Lagged(_))) {}
                    }
                    Err(RecvError::Closed) => return,
                },
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }
}
