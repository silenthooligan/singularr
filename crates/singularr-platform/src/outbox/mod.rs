//! The act outbox (platform design, sections 4.5 to 4.7). An act is what Singularr asks a
//! service to do: hand a file to a flow, grab a release. It is a row of `acts` before it is sent.
//!
//! - The caller enqueues it in its own transaction ([`Outbox::enqueue`]). The row, the job of its
//!   send on the `acts` queue, the wake-up and the `act_requested` event commit together. Its key
//!   names the request: a second request with the key is the same act. The key with another
//!   request is refused ([`EnqueueError::Mismatch`]). An open key holds while Singularr does not
//!   know whether the service has the act: pending, sending or unknown. A second act with it is
//!   that act. A sent act holds it no more: the service refuses a second copy itself.
//! - A worker of the `acts` queue sends it by compare and set: `pending` becomes `sending` before
//!   the call. An act is sent once. Its delivery says then whether it may go out
//!   ([`Deliver::held`]). An act goes out only while the integration's access for its action is
//!   `act`. An act held back fails before anything is sent.
//! - The answer settles it: `sent` for a 2xx, `confirmed` at once where the answer is the
//!   confirmation, `failed` for a clear no, `unknown` without a clear answer. An act its send's
//!   job finds `sending` has no answer recorded: it is `unknown`, never `pending`.
//! - A read confirms an act that was sent or is unknown ([`Deliver::confirm`]), in a job of its
//!   own: six reads at most, the first after its wait, each next wait twice the last, none past
//!   the window. A read that finds it confirms it. A read that proves it absent at the last read
//!   fails it. An action sent again when absent is sent again instead, three sends at most. A
//!   newer act that holds the open key by then stands for it: it fails. Reads that never find it
//!   leave it as it is: the owner settles an unknown act by hand.
//! - Every change of state is an event: `act_requested`, `act_sent`, `act_unknown`,
//!   `act_confirmed`, `act_failed`, `act_cancelled`, `act_resent`. The move to `sending` is none:
//!   the move after it carries the answer. The domain hears of each change in the transaction that
//!   makes it ([`Deliver::changed`]).

pub mod store;

use crate::journal::{Journal, JournalTx, NewEvent, event};
use crate::runtime::{Cx, Done, Failed, Handler, Retry};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use store::Move;

/// The kind of the job that sends an act.
pub const SEND_KIND: &str = "act.send";
/// The kind of the job that reads whether an act took place.
pub const CONFIRM_KIND: &str = "act.confirm";
/// The queue of both.
pub const QUEUE: &str = "acts";
/// The workers of the `acts` queue (section 4.2).
pub const WORKERS: usize = 2;
/// Reads after a send, at most (section 4.4).
pub const READS: u32 = 6;
/// Sends of an act at most, for an action sent again when a read proves it absent.
pub const SENDS: i32 = 3;
/// Runs of the job of a send or of a read at most. A run cut short is one.
pub(crate) const SEND_ATTEMPTS: i32 = 5;
/// The window of a key made without the caller's: a double click within it is one act.
pub const BUCKET_SECS: i64 = 300;

pub const PENDING: &str = "pending";
pub const SENDING: &str = "sending";
pub const SENT: &str = "sent";
pub const UNKNOWN: &str = "unknown";
pub const CONFIRMED: &str = "confirmed";
pub const FAILED: &str = "failed";
pub const CANCELLED: &str = "cancelled";
/// The states of an act that is open: neither confirmed nor failed nor cancelled.
pub const OPEN: [&str; 4] = [PENDING, SENDING, SENT, UNKNOWN];
/// The states in which an act holds its open key: Singularr does not know whether the service has
/// it. A sent act's service answered that it has it.
pub const HOLDING: [&str; 3] = [PENDING, SENDING, UNKNOWN];

/// The class of a failed act whose action's access was taken back before its send.
pub const ACCESS: &str = "access";
/// The class of a failed act of an action this process delivers no act of.
pub const UNAVAILABLE: &str = "unavailable";
/// The class of a failed act a read proved absent.
pub const ABSENT: &str = "absent";
/// The class of an unknown act the owner settled as not sent.
pub const BY_HAND: &str = "by_hand";
/// The class of an unknown act whose send has no answer recorded: its run's lease ran out, its
/// process stopped or the write of its answer failed.
pub const UNRECORDED: &str = "unrecorded";
/// The class of a failed act a read proved absent whose open key a newer act holds: the newer
/// act stands for it.
pub const SUPERSEDED: &str = "superseded";

/// The events of an act's changes.
pub const ACT_REQUESTED: &str = "act_requested";
pub const ACT_SENT: &str = "act_sent";
pub const ACT_UNKNOWN: &str = "act_unknown";
pub const ACT_CONFIRMED: &str = "act_confirmed";
pub const ACT_FAILED: &str = "act_failed";
pub const ACT_CANCELLED: &str = "act_cancelled";
pub const ACT_RESENT: &str = "act_resent";

/// One act as stored.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Act {
    pub id: i64,
    pub integration: String,
    pub action: String,
    pub idempotency_key: String,
    pub open_key: Option<String>,
    pub request_hash: String,
    /// What is sent, secrets left out.
    pub request: Value,
    pub subject_type: Option<String>,
    pub subject_id: Option<String>,
    pub state: String,
    /// Its sends so far.
    pub attempts: i32,
    /// What the service answered. A read's finding is under `confirmed`.
    pub result: Option<Value>,
    pub error: Option<String>,
    pub error_class: Option<String>,
    pub actor: String,
    pub caused_by: Option<i64>,
    pub created_at: DateTime<Utc>,
    /// When its last send went out.
    pub sent_at: Option<DateTime<Utc>>,
    /// When it was confirmed, failed or cancelled.
    pub settled_at: Option<DateTime<Utc>>,
}

impl Act {
    /// Pending, sending, sent or unknown.
    pub fn is_open(&self) -> bool {
        OPEN.contains(&self.state.as_str())
    }

    /// No worker has it in hand: neither pending nor sending.
    pub fn rests(&self) -> bool {
        self.state != PENDING && self.state != SENDING
    }

    /// A text of its request.
    pub fn asked(&self, key: &str) -> Option<&str> {
        self.request.get(key).and_then(Value::as_str)
    }

    /// A text of its result.
    pub fn answered(&self, key: &str) -> Option<&str> {
        self.result.as_ref()?.get(key).and_then(Value::as_str)
    }
}

/// An act to enqueue.
#[derive(Debug, Clone, PartialEq)]
pub struct NewAct {
    pub integration: String,
    /// An action the integration's type declares: `send`.
    pub action: String,
    /// What names the request: `api:<actor>:<key>`, `rule:<rule>:<file>:<claim>`.
    pub key: String,
    /// What holds while the act is pending, sending or unknown ([`HOLDING`]):
    /// `send:<integration>:<path>:<flow>`.
    pub open_key: Option<String>,
    /// What is sent, secrets left out.
    pub request: Value,
    /// What it is about: (`title`, its id).
    pub subject: Option<(String, String)>,
    /// Who asks: `user:<address>`, `key:<name>`, `rule:<id>`.
    pub actor: String,
    /// The event that led to it.
    pub caused_by: Option<i64>,
}

/// What an enqueue found or did.
#[derive(Debug, Clone, PartialEq)]
pub enum Enqueued {
    /// A new act, its send queued.
    New(Act),
    /// The key named an act of the same request already: that act.
    Same(Act),
    /// Another act holds the open key ([`HOLDING`]): that act.
    Open(Act),
}

impl Enqueued {
    pub fn act(&self) -> &Act {
        match self {
            Enqueued::New(a) | Enqueued::Same(a) | Enqueued::Open(a) => a,
        }
    }

    pub fn into_act(self) -> Act {
        match self {
            Enqueued::New(a) | Enqueued::Same(a) | Enqueued::Open(a) => a,
        }
    }
}

/// Why an act was not enqueued.
#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    /// The key names an act of another request.
    #[error("the key names another request")]
    Mismatch(Box<Act>),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// How an act is confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmBy {
    /// Its answer: a 2xx confirms it at once.
    Answer,
    /// A read: the first `first` after the send, then after `every`, twice that, four times and
    /// on, [`READS`] at most, none later than `window` after the send.
    Read {
        first: Duration,
        every: Duration,
        window: Duration,
    },
}

/// Whether an act a read proved absent is sent again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resend {
    /// It fails.
    Never,
    /// It is sent again, [`SENDS`] sends at most.
    WhenAbsent,
}

/// How one action's acts are confirmed and whether they are sent again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    pub confirm: ConfirmBy,
    pub resend: Resend,
}

/// What a send's answer said.
#[derive(Debug, Clone, PartialEq)]
pub enum Delivered {
    /// A 2xx. The value is what the service returned: ids, a command.
    Answered(Value),
    /// A clear no: nothing took place. `class` sorts it. `result` keeps what the service said.
    No {
        error: String,
        class: String,
        result: Option<Value>,
    },
    /// No clear answer: it may have taken place.
    Unknown { error: String },
}

/// What a read found.
#[derive(Debug, Clone, PartialEq)]
pub enum Confirmation {
    /// It took place. The value is what the read found: a run, a download id.
    Done(Value),
    /// Not to be seen yet.
    NotYet,
    /// It did not take place.
    Absent,
}

/// Why an act may not go out now. It fails before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    pub error: String,
    /// What sorts it: [`ACCESS`] or a class of the domain's.
    pub class: String,
}

impl Held {
    pub fn new(error: &str, class: &str) -> Held {
        Held {
            error: error.to_string(),
            class: class.to_string(),
        }
    }
}

/// The delivery of one action's acts, from the domain that knows the action.
#[async_trait]
pub trait Deliver: Send + Sync + 'static {
    /// How its acts are confirmed and whether they are sent again.
    fn plan(&self, act: &Act) -> Plan;

    /// Why the act may not go out now: the integration's access for its action is not `act`. A
    /// domain holds its acts back for reasons of its own too: a processing send while the sync is
    /// off. `None` when it may go out.
    fn held(&self, act: &Act) -> Option<Held>;

    /// Send it once. The sender never retries an act.
    async fn send(&self, act: &Act) -> Delivered;

    /// Read whether it took place. An error counts as not seen yet.
    async fn confirm(&self, act: &Act) -> Result<Confirmation, String>;

    /// Words for its events, merged into each event's body: what the act is about.
    fn words(&self, _act: &Act) -> Map<String, Value> {
        Map::new()
    }

    /// A change of its state, in the transaction that makes it.
    async fn changed(&self, _tx: &mut JournalTx, _act: &Act) -> Result<(), sqlx::Error> {
        Ok(())
    }
}

/// The acts of one process: the deliveries of the actions it sends acts of.
#[derive(Clone)]
pub struct Outbox {
    pool: PgPool,
    journal: Journal,
    by_action: BTreeMap<String, Arc<dyn Deliver>>,
}

/// A key of an act asked for with the caller's own key (an `Idempotency-Key` header).
pub fn api_key(actor: &str, given: &str) -> String {
    format!("api:{actor}:{given}")
}

/// A key of an act asked for without the caller's key: the request's hash and its 5 min bucket.
pub fn bucket_key(actor: &str, hash: &str, at: DateTime<Utc>) -> String {
    format!(
        "api:{actor}:{hash}:{}",
        at.timestamp().div_euclid(BUCKET_SECS)
    )
}

/// A value with every object's keys in order.
fn canonical(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let sorted: BTreeMap<&String, Value> =
                m.iter().map(|(k, v)| (k, canonical(v))).collect();
            Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// The SHA-256 of a request's canonical form. The document hashed holds its integration and its
/// action too.
pub fn request_hash(integration: &str, action: &str, request: &Value) -> String {
    let doc = json!({"integration": integration, "action": action, "request": canonical(request)});
    hex::encode(Sha256::digest(doc.to_string().as_bytes()))
}

/// When the read after read `read` comes, from the send: `first + every x (2^read - 1)`.
fn next_read(first: Duration, every: Duration, read: u32) -> Duration {
    first.saturating_add(every.saturating_mul((1u32 << read.min(20)) - 1))
}

/// An act's line on the Jobs page.
fn line(act: &Act) -> String {
    match (act.state.as_str(), act.error.as_deref()) {
        (FAILED, Some(why)) => format!("Failed: {why}"),
        (UNKNOWN, _) => "Unconfirmed".to_string(),
        (state, _) => {
            let mut c = state.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect())
                .unwrap_or_default()
        }
    }
}

fn db(e: sqlx::Error) -> String {
    e.to_string()
}

/// Wait for an act to rest: neither pending nor sending. A sent one is waited for `settle` more,
/// for the read that confirms it. `wait` at most in all. The act as it stands then. `None` when
/// there is no such act.
pub async fn rested(
    pool: &PgPool,
    id: i64,
    settle: Duration,
    wait: Duration,
) -> Result<Option<Act>, sqlx::Error> {
    let began = Instant::now();
    let mut delay = Duration::from_millis(25);
    let mut sent: Option<Instant> = None;
    loop {
        let Some(act) = store::get(pool, id).await? else {
            return Ok(None);
        };
        let rested = match act.state.as_str() {
            PENDING | SENDING => false,
            SENT => sent.get_or_insert_with(Instant::now).elapsed() >= settle,
            _ => true,
        };
        if rested || began.elapsed() >= wait {
            return Ok(Some(act));
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_millis(250));
    }
}

/// One act.
pub async fn act(pool: &PgPool, id: i64) -> Result<Option<Act>, sqlx::Error> {
    store::get(pool, id).await
}

impl Outbox {
    /// The outbox of a process. It has no delivery yet.
    pub fn new(pool: PgPool, journal: Journal) -> Outbox {
        Outbox {
            pool,
            journal,
            by_action: BTreeMap::new(),
        }
    }

    /// The delivery of one action's acts.
    pub fn deliver(mut self, action: &str, delivery: Arc<dyn Deliver>) -> Outbox {
        self.by_action.insert(action.to_string(), delivery);
        self
    }

    /// The process delivers acts of this action.
    pub fn delivers(&self, action: &str) -> bool {
        self.by_action.contains_key(action)
    }

    fn of(&self, action: &str) -> Option<Arc<dyn Deliver>> {
        self.by_action.get(action).cloned()
    }

    /// The words of an act: what it is about, from its action's delivery. None for an action this
    /// process delivers no act of.
    pub fn words(&self, act: &Act) -> Map<String, Value> {
        self.of(&act.action)
            .map(|d| d.words(act))
            .unwrap_or_default()
    }

    /// An event of an act's change, about the act's subject when it has one.
    fn event(&self, kind: &str, act: &Act, actor: &str) -> NewEvent {
        let mut body = self.words(act);
        body.insert("act".to_string(), json!(act.id));
        body.insert("integration".to_string(), json!(act.integration));
        body.insert("action".to_string(), json!(act.action));
        body.insert("state".to_string(), json!(act.state));
        if let Some(error) = &act.error {
            body.insert("error".to_string(), json!(error));
        }
        if let Some(class) = &act.error_class {
            body.insert("error_class".to_string(), json!(class));
        }
        let (kind_of, id) = match (&act.subject_type, &act.subject_id) {
            (Some(t), Some(id)) => (t.clone(), id.clone()),
            _ => ("act".to_string(), act.id.to_string()),
        };
        event(kind, &kind_of, id, actor, Value::Object(body))
    }

    /// Enqueue an act in the caller's transaction: the row, the job of its send, the wake-up and
    /// the `act_requested` event. A key taken by an act of the same request is that act. One
    /// taken by an act of another request is refused. An open key another act holds is that act.
    pub async fn enqueue(&self, tx: &mut JournalTx, new: NewAct) -> Result<Enqueued, EnqueueError> {
        let hash = request_hash(&new.integration, &new.action, &new.request);
        // The act that holds the open key may settle between the insert and the read: once more.
        for _ in 0..3 {
            if let Some(act) = store::insert(tx.conn(), &new, &hash).await? {
                store::queue_send(tx.conn(), &act, 1).await?;
                tx.append_one(self.event(ACT_REQUESTED, &act, &act.actor))
                    .await?;
                return Ok(Enqueued::New(act));
            }
            if let Some(act) = store::by_key(tx.conn(), &new.key).await? {
                return if act.request_hash == hash {
                    Ok(Enqueued::Same(act))
                } else {
                    Err(EnqueueError::Mismatch(Box::new(act)))
                };
            }
            if let Some(open) = &new.open_key
                && let Some(act) = store::open_by_key(tx.conn(), open).await?
            {
                return Ok(Enqueued::Open(act));
            }
        }
        Err(EnqueueError::Db(sqlx::Error::Protocol(
            "the act's keys changed hands three times in a row".to_string(),
        )))
    }

    /// The work of a send's job: send a pending act once. An act its job finds `sending` was in
    /// hand of a run that lost its lease: it is unknown now. Returns the job's line.
    pub async fn send(&self, id: i64) -> Result<String, String> {
        let Some(act) = store::get(&self.pool, id).await.map_err(db)? else {
            return Ok("No such act".to_string());
        };
        match act.state.as_str() {
            PENDING => self.deliver_now(act).await,
            SENDING => self.lost(&act).await,
            state => Ok(format!("Already {state}")),
        }
    }

    async fn deliver_now(&self, act: Act) -> Result<String, String> {
        let Some(d) = self.of(&act.action) else {
            return self
                .refuse(&act, "No delivery for this action", UNAVAILABLE)
                .await;
        };
        if let Some(held) = d.held(&act) {
            return self.refuse(&act, &held.error, &held.class).await;
        }
        // The compare and set: one worker sends it.
        let Some(taken) = store::take(&self.pool, act.id).await.map_err(db)? else {
            return Ok("Taken elsewhere".to_string());
        };
        let delivered = d.send(&taken).await;
        let plan = d.plan(&taken);
        let (to, result, error, class) = match delivered {
            Delivered::Answered(v) => {
                let to = if plan.confirm == ConfirmBy::Answer {
                    CONFIRMED
                } else {
                    SENT
                };
                (to, Some(v), None, None)
            }
            Delivered::No {
                error,
                class,
                result,
            } => (FAILED, result, Some(error), Some(class)),
            Delivered::Unknown { error } => (UNKNOWN, None, Some(error), Some(UNKNOWN.to_string())),
        };
        let mut tx = self.journal.begin(&self.pool).await.map_err(db)?;
        let moved = store::moved(
            tx.conn(),
            taken.id,
            &[SENDING],
            to,
            Move {
                result: result.as_ref(),
                error: error.as_deref(),
                class: class.as_deref(),
            },
        )
        .await
        .map_err(db)?;
        let Some(after) = moved else {
            tracing::warn!(target: "singularr::outbox", act = taken.id, "the answer came after the act left sending: a read settles it");
            return Ok("Answer came late".to_string());
        };
        let kinds: &[&str] = match to {
            CONFIRMED => &[ACT_SENT, ACT_CONFIRMED],
            SENT => &[ACT_SENT],
            FAILED => &[ACT_FAILED],
            _ => &[ACT_UNKNOWN],
        };
        for kind in kinds {
            tx.append_one(self.event(kind, &after, &after.actor))
                .await
                .map_err(db)?;
        }
        if to == SENT || to == UNKNOWN {
            first_read(tx.conn(), &after, &plan).await.map_err(db)?;
        }
        d.changed(&mut tx, &after).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(line(&after))
    }

    /// A pending act that is not sent: failed with why, before anything went out.
    async fn refuse(&self, act: &Act, why: &str, class: &str) -> Result<String, String> {
        let mut tx = self.journal.begin(&self.pool).await.map_err(db)?;
        let moved = store::moved(
            tx.conn(),
            act.id,
            &[PENDING],
            FAILED,
            Move {
                result: None,
                error: Some(why),
                class: Some(class),
            },
        )
        .await
        .map_err(db)?;
        let Some(after) = moved else {
            return Ok("Taken elsewhere".to_string());
        };
        tx.append_one(self.event(ACT_FAILED, &after, &after.actor))
            .await
            .map_err(db)?;
        if let Some(d) = self.of(&after.action) {
            d.changed(&mut tx, &after).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(line(&after))
    }

    /// An act its send's job finds `sending`: a run took it and recorded no answer. Its lease ran
    /// out, its process stopped or the write of its answer failed. The call may have gone out: the
    /// act is unknown. Reads follow.
    async fn lost(&self, act: &Act) -> Result<String, String> {
        let mut tx = self.journal.begin(&self.pool).await.map_err(db)?;
        let moved = store::moved(
            tx.conn(),
            act.id,
            &[SENDING],
            UNKNOWN,
            Move {
                result: None,
                error: Some("No answer recorded"),
                class: Some(UNRECORDED),
            },
        )
        .await
        .map_err(db)?;
        let Some(after) = moved else {
            return Ok("Settled meanwhile".to_string());
        };
        tx.append_one(self.event(ACT_UNKNOWN, &after, &after.actor))
            .await
            .map_err(db)?;
        if let Some(d) = self.of(&after.action) {
            first_read(tx.conn(), &after, &d.plan(&after))
                .await
                .map_err(db)?;
            d.changed(&mut tx, &after).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok("Unconfirmed: no answer recorded".to_string())
    }

    /// The work of a read's job: the `read`th read after the act's `send`th send.
    pub async fn confirm(&self, id: i64, send: i32, read: u32) -> Result<String, String> {
        let Some(act) = store::get(&self.pool, id).await.map_err(db)? else {
            return Ok("No such act".to_string());
        };
        if act.state != SENT && act.state != UNKNOWN {
            return Ok(format!("Already {}", act.state));
        }
        if act.attempts != send {
            return Ok("A later send has reads of its own".to_string());
        }
        let Some(d) = self.of(&act.action) else {
            return Ok("No delivery for this action".to_string());
        };
        let plan = d.plan(&act);
        let ConfirmBy::Read {
            first,
            every,
            window,
        } = plan.confirm
        else {
            return Ok("Confirmed by its answer".to_string());
        };
        let found = match d.confirm(&act).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(target: "singularr::outbox", act = id, error = %e, "a read of an act failed: it counts as not seen yet");
                Confirmation::NotYet
            }
        };
        if let Confirmation::Done(value) = found {
            return match self.found(id, value).await.map_err(db)? {
                Some(after) => Ok(line(&after)),
                None => Ok("Settled meanwhile".to_string()),
            };
        }
        let since = act.sent_at.unwrap_or(act.created_at);
        let elapsed = (Utc::now() - since).to_std().unwrap_or_default();
        let next = next_read(first, every, read);
        if read < READS && next <= window {
            let mut tx = self.journal.begin(&self.pool).await.map_err(db)?;
            store::queue_read(
                tx.conn(),
                &act,
                send,
                read + 1,
                next.saturating_sub(elapsed),
            )
            .await
            .map_err(db)?;
            tx.commit().await.map_err(db)?;
            return Ok(format!("Not seen yet: read {read} of {READS}"));
        }
        match (found, plan.resend) {
            (Confirmation::Absent, Resend::WhenAbsent) if act.attempts < SENDS => {
                self.again(&act, &d).await
            }
            (Confirmation::Absent, _) => self.fail_open(&act, &d, "Not done", ABSENT).await,
            _ => Ok(format!(
                "Not seen after {read} reads: it stays {}",
                act.state
            )),
        }
    }

    /// An act a read proved absent at its last read, of an action sent again when absent: pending
    /// again, its next send queued. A sent act holds its open key no more: a newer act that took it
    /// stands for this one. This one fails then ([`SUPERSEDED`]).
    async fn again(&self, act: &Act, d: &Arc<dyn Deliver>) -> Result<String, String> {
        let mut tx = self.journal.begin(&self.pool).await.map_err(db)?;
        let moved = match store::moved(
            tx.conn(),
            act.id,
            &[SENT, UNKNOWN],
            PENDING,
            Move::default(),
        )
        .await
        {
            Ok(moved) => moved,
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                drop(tx);
                return self.fail_open(act, d, "Superseded", SUPERSEDED).await;
            }
            Err(e) => return Err(db(e)),
        };
        let Some(after) = moved else {
            return Ok("Settled meanwhile".to_string());
        };
        store::queue_send(tx.conn(), &after, after.attempts + 1)
            .await
            .map_err(db)?;
        tx.append_one(self.event(ACT_RESENT, &after, &after.actor))
            .await
            .map_err(db)?;
        d.changed(&mut tx, &after).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok("Absent: sent again".to_string())
    }

    /// An act a read proved absent at its last read: failed. `why` and `class` say why.
    async fn fail_open(
        &self,
        act: &Act,
        d: &Arc<dyn Deliver>,
        why: &str,
        class: &str,
    ) -> Result<String, String> {
        let mut tx = self.journal.begin(&self.pool).await.map_err(db)?;
        let moved = store::moved(
            tx.conn(),
            act.id,
            &[SENT, UNKNOWN],
            FAILED,
            Move {
                result: None,
                error: Some(why),
                class: Some(class),
            },
        )
        .await
        .map_err(db)?;
        let Some(after) = moved else {
            return Ok("Settled meanwhile".to_string());
        };
        tx.append_one(self.event(ACT_FAILED, &after, &after.actor))
            .await
            .map_err(db)?;
        d.changed(&mut tx, &after).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(line(&after))
    }

    /// An act that was sent or is unknown, found done by a read: confirmed. What the read found
    /// goes under its result's `confirmed`. A read made elsewhere confirms it the same way: the
    /// processing pass reads the services' runs. `None` when the act was neither sent nor unknown.
    pub async fn found(&self, id: i64, value: Value) -> Result<Option<Act>, sqlx::Error> {
        let mut tx = self.journal.begin(&self.pool).await?;
        let Some(act) = store::get(tx.conn(), id).await? else {
            return Ok(None);
        };
        let mut result = match act.result.clone() {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        result.insert("confirmed".to_string(), value);
        let result = Value::Object(result);
        let moved = store::moved(
            tx.conn(),
            id,
            &[SENT, UNKNOWN],
            CONFIRMED,
            Move {
                result: Some(&result),
                error: None,
                class: None,
            },
        )
        .await?;
        let Some(after) = moved else {
            return Ok(None);
        };
        tx.append_one(self.event(ACT_CONFIRMED, &after, &after.actor))
            .await?;
        if let Some(d) = self.of(&after.action) {
            d.changed(&mut tx, &after).await?;
        }
        tx.commit().await?;
        Ok(Some(after))
    }

    /// Cancel a pending act before a worker takes it: the owner's. `None` when it is not pending.
    pub async fn cancel(&self, id: i64, actor: &str) -> Result<Option<Act>, sqlx::Error> {
        let mut tx = self.journal.begin(&self.pool).await?;
        let moved = store::moved(
            tx.conn(),
            id,
            &[PENDING],
            CANCELLED,
            Move {
                result: None,
                error: Some("Cancelled"),
                class: Some(CANCELLED),
            },
        )
        .await?;
        let Some(after) = moved else {
            return Ok(None);
        };
        store::cancel_sends(tx.conn(), id).await?;
        tx.append_one(self.event(ACT_CANCELLED, &after, actor))
            .await?;
        if let Some(d) = self.of(&after.action) {
            d.changed(&mut tx, &after).await?;
        }
        tx.commit().await?;
        Ok(Some(after))
    }

    /// Settle an unknown act by hand as not sent: the owner's. It fails. `None` when it is not
    /// unknown.
    pub async fn not_sent(&self, id: i64, actor: &str) -> Result<Option<Act>, sqlx::Error> {
        let mut tx = self.journal.begin(&self.pool).await?;
        let moved = store::moved(
            tx.conn(),
            id,
            &[UNKNOWN],
            FAILED,
            Move {
                result: None,
                error: Some("Not sent"),
                class: Some(BY_HAND),
            },
        )
        .await?;
        let Some(after) = moved else {
            return Ok(None);
        };
        tx.append_one(self.event(ACT_FAILED, &after, actor)).await?;
        if let Some(d) = self.of(&after.action) {
            d.changed(&mut tx, &after).await?;
        }
        tx.commit().await?;
        Ok(Some(after))
    }
}

/// Queue the first read after a send, for an act confirmed by a read.
async fn first_read(conn: &mut PgConnection, act: &Act, plan: &Plan) -> Result<(), sqlx::Error> {
    if let ConfirmBy::Read { first, .. } = plan.confirm {
        store::queue_read(conn, act, act.attempts, 1, first).await?;
    }
    Ok(())
}

/// How the jobs of the acts are tried again after a run that failed (a database that did not
/// answer): soon. A send is never made twice: its compare and set holds.
fn retry() -> Retry {
    Retry {
        base: Duration::from_secs(5),
        cap: Duration::from_secs(300),
        attempts: SEND_ATTEMPTS,
    }
}

fn payload_number(cx: &Cx, key: &str) -> Option<i64> {
    cx.job.payload.get(key).and_then(Value::as_i64)
}

/// `act.send`: one act's send ([`Outbox::send`]).
pub struct SendJob(pub Arc<Outbox>);

#[async_trait]
impl Handler for SendJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let id = payload_number(&cx, "act").ok_or_else(|| Failed::from("the job names no act"))?;
        self.0.send(id).await.map(Done::from).map_err(Failed::from)
    }

    fn retry(&self) -> Retry {
        retry()
    }
}

/// `act.confirm`: one read of an act ([`Outbox::confirm`]).
pub struct ConfirmJob(pub Arc<Outbox>);

#[async_trait]
impl Handler for ConfirmJob {
    async fn run(&self, cx: Cx) -> Result<Done, Failed> {
        let id = payload_number(&cx, "act").ok_or_else(|| Failed::from("the job names no act"))?;
        let send = payload_number(&cx, "send").unwrap_or(1);
        let read = payload_number(&cx, "read").unwrap_or(1);
        self.0
            .confirm(
                id,
                i32::try_from(send).unwrap_or(1),
                u32::try_from(read).unwrap_or(1),
            )
            .await
            .map(Done::from)
            .map_err(Failed::from)
    }

    fn retry(&self) -> Retry {
        retry()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_hashes_the_same_whatever_the_order_of_its_keys() {
        let a = request_hash("hush", "send", &json!({"path": "/a.mkv", "flow": "audio"}));
        let b = request_hash("hush", "send", &json!({"flow": "audio", "path": "/a.mkv"}));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert_ne!(
            a,
            request_hash("hush", "send", &json!({"path": "/b.mkv", "flow": "audio"}))
        );
        assert_ne!(
            a,
            request_hash("tdarr", "send", &json!({"path": "/a.mkv", "flow": "audio"}))
        );
    }

    #[test]
    fn a_key_without_the_caller_s_holds_for_five_minutes() {
        let at = |secs: i64| DateTime::from_timestamp(1_795_000_200 + secs, 0).unwrap();
        assert_eq!(api_key("user:o@x", "k1"), "api:user:o@x:k1");
        assert_eq!(
            bucket_key("user:o@x", "h", at(0)),
            bucket_key("user:o@x", "h", at(99))
        );
        assert_ne!(
            bucket_key("user:o@x", "h", at(0)),
            bucket_key("user:o@x", "h", at(300))
        );
    }

    #[test]
    fn the_reads_wait_twice_as_long_each_time() {
        let s = Duration::from_secs;
        let at: Vec<u64> = (0..6)
            .map(|r| next_read(s(10), s(30), r).as_secs())
            .collect();
        assert_eq!(at, vec![10, 40, 100, 220, 460, 940]);
        assert_eq!(next_read(Duration::ZERO, s(30), 1).as_secs(), 30);
    }
}
