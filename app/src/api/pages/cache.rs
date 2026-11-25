//! Answers kept a short time in memory, so a page load does not ask a manager each time.
//!
//! What the managers alone know (their calendar, history and wanted lists) is asked on demand
//! with GET and kept for minutes; what the media server plays and what the processing services
//! work on are kept for seconds; the library tables, built from the database, are kept for half
//! a minute. A caller that asks while an answer is being made waits for it, so one page load
//! asks a manager once.

use crate::catalog::model::TitleKind;
use crate::engines::{CalendarEntry, HistoryEntry, ServerSession, WantedPage};
use chrono::NaiveDate;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// How long each answer is kept.
pub const LIBRARY_KEEP: Duration = Duration::from_secs(30);
pub const HISTORY_KEEP: Duration = Duration::from_secs(120);
pub const WANTED_KEEP: Duration = Duration::from_secs(300);
pub const CALENDAR_KEEP: Duration = Duration::from_secs(300);
pub const SESSIONS_KEEP: Duration = Duration::from_secs(10);
/// An answer that misses a manager that did not answer is kept a short time only.
pub const PARTIAL_KEEP: Duration = Duration::from_secs(20);

/// One remembered answer.
pub struct Slot<T> {
    cell: Mutex<Option<(Instant, Duration, Arc<T>)>>,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot {
            cell: Mutex::new(None),
        }
    }
}

impl<T> Slot<T> {
    /// The answer while it is fresh, else a new one from `fill`, which also says how long to
    /// keep it.
    pub async fn get<F, Fut, E>(&self, fill: F) -> Result<Arc<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<(T, Duration), E>>,
    {
        let mut cell = self.cell.lock().await;
        if let Some((at, keep, value)) = cell.as_ref()
            && at.elapsed() < *keep
        {
            return Ok(value.clone());
        }
        let (value, keep) = fill().await?;
        let value = Arc::new(value);
        *cell = Some((Instant::now(), keep, value.clone()));
        Ok(value)
    }

    /// The same for an answer that is always made.
    pub async fn make<F, Fut>(&self, fill: F) -> Arc<T>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = (T, Duration)>,
    {
        let mut cell = self.cell.lock().await;
        if let Some((at, keep, value)) = cell.as_ref()
            && at.elapsed() < *keep
        {
            return value.clone();
        }
        let (value, keep) = fill().await;
        let value = Arc::new(value);
        *cell = Some((Instant::now(), keep, value.clone()));
        value
    }
}

/// A body built once, with the tag that names it.
pub struct Built {
    pub body: Vec<u8>,
    pub etag: String,
}

/// One manager's list.
#[derive(Debug, Clone)]
pub struct ManagerList<T> {
    /// The integration id.
    pub engine: String,
    pub kind: TitleKind,
    pub entries: Vec<T>,
}

/// One manager's two wanted lists.
#[derive(Debug, Clone)]
pub struct ManagerWanted {
    pub engine: String,
    pub kind: TitleKind,
    pub missing: WantedPage,
    pub cutoff: WantedPage,
}

pub type Calendars = Vec<ManagerList<CalendarEntry>>;

/// The calendars kept, by their first and last day.
type CalendarSlots = HashMap<(NaiveDate, NaiveDate), Arc<Slot<Calendars>>>;

/// Everything the page routes keep.
#[derive(Default)]
pub struct PageCache {
    pub movies: Slot<Built>,
    pub shows: Slot<Built>,
    pub history: Slot<Vec<ManagerList<HistoryEntry>>>,
    pub wanted: Slot<Vec<ManagerWanted>>,
    calendars: std::sync::Mutex<CalendarSlots>,
    /// `None`: no media server is configured.
    pub sessions: Slot<Option<Vec<ServerSession>>>,
    /// How each processing service answers now, and what their workers are on: filled and
    /// kept by `api::processing`.
    pub processing: Slot<crate::processing::service::Live>,
}

impl PageCache {
    /// The slot of one span of days. A handful of spans are kept; more start over.
    pub fn calendar(&self, from: NaiveDate, to: NaiveDate) -> Arc<Slot<Calendars>> {
        let mut map = self
            .calendars
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if map.len() >= 16 && !map.contains_key(&(from, to)) {
            map.clear();
        }
        map.entry((from, to)).or_default().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn an_answer_is_made_once_while_it_is_fresh() {
        let slot: Slot<usize> = Slot::default();
        let made = AtomicUsize::new(0);
        for _ in 0..3 {
            let v = slot
                .get(|| async {
                    Ok::<_, ()>((made.fetch_add(1, Ordering::SeqCst), Duration::from_secs(60)))
                })
                .await
                .unwrap();
            assert_eq!(*v, 0);
        }
        assert_eq!(made.load(Ordering::SeqCst), 1);
        // An answer kept for no time is made again; a failure keeps nothing.
        let short: Slot<usize> = Slot::default();
        short
            .get(|| async { Ok::<_, ()>((1, Duration::ZERO)) })
            .await
            .unwrap();
        assert!(
            short
                .get(|| async { Err::<(usize, Duration), _>(()) })
                .await
                .is_err()
        );
        let again = short
            .get(|| async { Ok::<_, ()>((2, Duration::from_secs(60))) })
            .await
            .unwrap();
        assert_eq!(*again, 2);
        let cache = PageCache::default();
        let d = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert!(Arc::ptr_eq(&cache.calendar(d, d), &cache.calendar(d, d)));
    }
}
