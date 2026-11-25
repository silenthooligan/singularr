//! What the pages ask the managers and the media server for, on demand, through the read-only
//! ports (`engines`) and so through `core::outbound`. Each answer is kept in the page cache; a
//! manager that does not answer is left out of the answer, which is then kept a short time.

use super::cache::{
    CALENDAR_KEEP, Calendars, HISTORY_KEEP, ManagerList, ManagerWanted, PARTIAL_KEEP,
    SESSIONS_KEEP, WANTED_KEEP,
};
use crate::api::state::AppState;
use crate::engines::{HistoryEntry, ServerSession};
use chrono::NaiveDate;
use futures::future::join_all;
use std::sync::Arc;
use std::time::Duration;

/// Records of each manager's history the pages read.
pub const HISTORY_LIMIT: u32 = 500;
/// Entries of each wanted list the pages read; the totals count them all.
pub const WANTED_LIMIT: u32 = 1000;

fn keep(full: Duration, all_answered: bool) -> Duration {
    if all_answered { full } else { PARTIAL_KEEP }
}

/// Every manager's newest history.
pub async fn history(state: &AppState) -> Arc<Vec<ManagerList<HistoryEntry>>> {
    state
        .pages
        .history
        .make(|| async {
            let asked = join_all(state.engines.arrs.iter().map(|arr| async move {
                let e = arr.engine();
                match arr.history(HISTORY_LIMIT).await {
                    Ok(entries) => Some(ManagerList {
                        engine: e.id.clone(),
                        kind: e.kind,
                        entries,
                    }),
                    Err(err) => {
                        tracing::warn!(engine = %e.id, error = %err, "the history could not be read");
                        None
                    }
                }
            }))
            .await;
            let all = asked.iter().all(Option::is_some);
            (
                asked.into_iter().flatten().collect(),
                keep(HISTORY_KEEP, all),
            )
        })
        .await
}

/// Every manager's two wanted lists.
pub async fn wanted(state: &AppState) -> Arc<Vec<ManagerWanted>> {
    state
        .pages
        .wanted
        .make(|| async {
            let asked = join_all(state.engines.arrs.iter().map(|arr| async move {
                let e = arr.engine();
                let (missing, cutoff) = tokio::join!(
                    arr.wanted(false, WANTED_LIMIT),
                    arr.wanted(true, WANTED_LIMIT)
                );
                match (missing, cutoff) {
                    (Ok(missing), Ok(cutoff)) => Some(ManagerWanted {
                        engine: e.id.clone(),
                        kind: e.kind,
                        missing,
                        cutoff,
                    }),
                    (Err(err), _) | (_, Err(err)) => {
                        tracing::warn!(engine = %e.id, error = %err, "the wanted lists could not be read");
                        None
                    }
                }
            }))
            .await;
            let all = asked.iter().all(Option::is_some);
            (
                asked.into_iter().flatten().collect(),
                keep(WANTED_KEEP, all),
            )
        })
        .await
}

/// The total of the wanted lists, when the managers answer within `wait`; `None` without a
/// manager or when they are slower. The lists are read to the end in the background either
/// way, so the next page has them.
pub async fn wanted_total(state: &AppState, wait: Duration) -> Option<i64> {
    if state.engines.arrs.is_empty() {
        return None;
    }
    let s = state.clone();
    let task = tokio::spawn(async move { wanted(&s).await });
    let lists = tokio::time::timeout(wait, task).await.ok()?.ok()?;
    Some(lists.iter().map(|w| w.missing.total + w.cutoff.total).sum())
}

/// Every manager's calendar over these days.
pub async fn calendars(state: &AppState, from: NaiveDate, to: NaiveDate) -> Arc<Calendars> {
    let slot = state.pages.calendar(from, to);
    slot.make(|| async {
        let asked = join_all(state.engines.arrs.iter().map(|arr| async move {
            let e = arr.engine();
            match arr.calendar(from, to).await {
                Ok(entries) => Some(ManagerList {
                    engine: e.id.clone(),
                    kind: e.kind,
                    entries,
                }),
                Err(err) => {
                    tracing::warn!(engine = %e.id, error = %err, "the calendar could not be read");
                    None
                }
            }
        }))
        .await;
        let all = asked.iter().all(Option::is_some);
        (
            asked.into_iter().flatten().collect(),
            keep(CALENDAR_KEEP, all),
        )
    })
    .await
}

/// What plays on the media server now; `None` when none is configured. A server that does
/// not answer plays nothing as far as the page knows, and is asked again soon.
pub async fn sessions(state: &AppState) -> Arc<Option<Vec<ServerSession>>> {
    state
        .pages
        .sessions
        .make(|| async {
            let Some(server) = state.engines.media_server.as_ref() else {
                return (None, SESSIONS_KEEP);
            };
            match server.sessions().await {
                Ok(list) => (Some(list), SESSIONS_KEEP),
                Err(err) => {
                    tracing::warn!(server = server.id(), error = %err, "the sessions could not be read");
                    (Some(Vec::new()), PARTIAL_KEEP)
                }
            }
        })
        .await
}
