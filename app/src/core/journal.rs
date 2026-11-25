//! The journal: every change is appended to `events` in the transaction that makes it, and
//! published on a broadcast channel once that transaction has committed.
//!
//! Writers go through [`Journal::begin`]. It hands out one [`JournalTx`] at a time, so events
//! are committed and published in id order and a stream that resumes "after id N" misses
//! nothing.

use super::model::Event;
pub use super::store::NewEvent;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard, broadcast};

/// Live events kept for slow stream readers. A reader that falls further behind is told so
/// and reads the gap from the table.
const CHANNEL_CAPACITY: usize = 2048;

#[derive(Clone)]
pub struct Journal {
    tx: broadcast::Sender<Arc<Event>>,
    gate: Arc<Mutex<()>>,
}

impl Default for Journal {
    fn default() -> Self {
        Journal::new()
    }
}

impl Journal {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Journal {
            tx,
            gate: Arc::new(Mutex::new(())),
        }
    }

    /// Live events, in commit order.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.tx.subscribe()
    }

    /// Start a transaction that may append events. Only one is open at a time.
    pub async fn begin(&self, pool: &PgPool) -> Result<JournalTx, sqlx::Error> {
        let gate = self.gate.clone().lock_owned().await;
        let tx = pool.begin().await?;
        Ok(JournalTx {
            tx,
            pending: Vec::new(),
            journal: self.clone(),
            _gate: gate,
        })
    }

    fn publish(&self, events: &[Event]) {
        for e in events {
            metrics::counter!("singularr_events_appended_total", "kind" => e.kind.clone())
                .increment(1);
            // No receiver is not an error: nobody is watching the stream.
            let _ = self.tx.send(Arc::new(e.clone()));
        }
    }
}

/// A database transaction with the journal attached. Dropping it without [`JournalTx::commit`]
/// rolls everything back and publishes nothing.
pub struct JournalTx {
    tx: Transaction<'static, Postgres>,
    pending: Vec<Event>,
    journal: Journal,
    _gate: OwnedMutexGuard<()>,
}

impl JournalTx {
    /// The connection, for the read-model writes that belong to the same change.
    pub fn conn(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// Append events in order. Returns them with their ids, so a later event can name an
    /// earlier one as its cause.
    pub async fn append(&mut self, events: Vec<NewEvent>) -> Result<Vec<Event>, sqlx::Error> {
        let stored = super::store::append_events(&mut self.tx, &events).await?;
        self.pending.extend(stored.iter().cloned());
        Ok(stored)
    }

    pub async fn append_one(&mut self, event: NewEvent) -> Result<Event, sqlx::Error> {
        let mut stored = self.append(vec![event]).await?;
        stored.pop().ok_or(sqlx::Error::RowNotFound)
    }

    /// Events appended so far in this transaction.
    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// Commit, then publish what was appended.
    pub async fn commit(self) -> Result<Vec<Event>, sqlx::Error> {
        let JournalTx {
            tx,
            pending,
            journal,
            _gate,
        } = self;
        tx.commit().await?;
        journal.publish(&pending);
        Ok(pending)
    }
}

/// Shorthand for building an event.
pub fn event(
    kind: &str,
    subject_type: &str,
    subject_id: impl ToString,
    actor: &str,
    body: serde_json::Value,
) -> NewEvent {
    NewEvent {
        kind: kind.to_string(),
        subject_type: subject_type.to_string(),
        subject_id: subject_id.to_string(),
        actor: actor.to_string(),
        cause: None,
        body,
    }
}

impl NewEvent {
    pub fn caused_by(mut self, cause: Option<i64>) -> Self {
        self.cause = cause;
        self
    }
}
