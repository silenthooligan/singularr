//! Downloads hygiene: who owns each torrent, which are stuck, which are orphans.
//!
//! A download's owner is the arr whose queue or grab history has its hash. Stage 1 only
//! reports; nothing is removed or changed in the download client.

use super::model::Download;
use super::store;
use crate::catalog::store as catalog_store;
use crate::core::journal::{Journal, NewEvent, event};
use crate::engines::{QueueItem, Torrent};
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};

/// What the arrs currently claim. Built by the queue and history loops.
#[derive(Debug, Clone, Default)]
pub struct Claims {
    /// The last queue read of each arr, by engine id.
    pub queues: BTreeMap<String, Vec<QueueItem>>,
    /// Hashes each arr has grabbed, upper case, by engine id.
    pub history: BTreeMap<String, HashSet<String>>,
    /// Every configured arr has a queue read and a loaded grab history. Until then a torrent
    /// nobody claims cannot be called an orphan.
    pub complete: bool,
}

/// What the claims say about one torrent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Judged {
    pub owner: Option<String>,
    pub owner_source: Option<&'static str>,
    pub engine_title_id: Option<i64>,
    pub queue_status: Option<String>,
    pub queue_state: Option<String>,
    pub queue_health: Option<String>,
    pub queue_messages: Vec<String>,
    pub stuck: bool,
    pub stuck_reason: Option<String>,
    /// `None`: it cannot be settled yet.
    pub orphan: Option<bool>,
    pub orphan_reason: Option<String>,
    /// The indexer the queue names.
    pub indexer: Option<String>,
}

fn is_stuck(item: &QueueItem) -> bool {
    let state = item.tracked_state.as_deref().unwrap_or_default();
    let health = item.tracked_status.as_deref().unwrap_or_default();
    ["importblocked", "failedpending", "failed"].contains(&state.to_ascii_lowercase().as_str())
        || ["warning", "error"].contains(&health.to_ascii_lowercase().as_str())
}

/// Decide owner, stuck and orphan for one torrent. `stored_owner` is the owner already on
/// record: a claim found once is kept when the arrs no longer show it (a finished import
/// leaves the queue, and old grabs fall off the history that is read).
pub fn judge(hash: &str, claims: &Claims, stored_owner: Option<(&str, &str)>) -> Judged {
    let in_queue = claims.queues.iter().find_map(|(engine, items)| {
        items
            .iter()
            .find(|i| i.download_id.as_deref() == Some(hash))
            .map(|i| (engine, i))
    });
    if let Some((engine, item)) = in_queue {
        let stuck = is_stuck(item);
        let no_title = item.engine_title_id.is_none();
        return Judged {
            owner: Some(engine.clone()),
            owner_source: Some("queue"),
            engine_title_id: item.engine_title_id,
            queue_status: item.status.clone(),
            queue_state: item.tracked_state.clone(),
            queue_health: item.tracked_status.clone(),
            queue_messages: item.messages.clone(),
            stuck,
            stuck_reason: stuck.then(|| {
                if item.messages.is_empty() {
                    format!(
                        "{engine} reports {} ({})",
                        item.tracked_state.as_deref().unwrap_or("no state"),
                        item.tracked_status.as_deref().unwrap_or("no status")
                    )
                } else {
                    item.messages.join("; ")
                }
            }),
            orphan: Some(no_title),
            orphan_reason: no_title.then(|| format!("In the {engine} queue without a title")),
            indexer: item.indexer.clone(),
        };
    }
    let in_history = claims
        .history
        .iter()
        .find(|(_, hashes)| hashes.contains(hash))
        .map(|(engine, _)| engine.clone());
    if let Some(engine) = in_history {
        return Judged {
            owner: Some(engine),
            owner_source: Some("history"),
            orphan: Some(false),
            ..Judged::default()
        };
    }
    if let Some((owner, source)) = stored_owner {
        return Judged {
            owner: Some(owner.to_string()),
            owner_source: Some(if source == "queue" {
                "queue"
            } else {
                "history"
            }),
            orphan: Some(false),
            ..Judged::default()
        };
    }
    Judged {
        orphan: claims.complete.then_some(true),
        orphan_reason: claims
            .complete
            .then(|| "Not in any manager's queue or grab history".to_string()),
        ..Judged::default()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownloadsReport {
    pub listed: usize,
    pub added: usize,
    pub completed: usize,
    pub stuck: usize,
    pub orphaned: usize,
    pub removed: usize,
    pub updated: usize,
}

fn body(d: &Download) -> serde_json::Value {
    json!({
        "hash": d.hash,
        "name": d.name,
        "category": d.category,
        "state": d.state,
        "progress": d.progress,
        "size_bytes": d.size_bytes,
        "owner": d.owner,
        "owner_source": d.owner_source,
        "title_id": d.title_id,
    })
}

/// The events one torrent raises, given the row stored before (if any) and the row now.
pub fn events_for(before: Option<&Download>, now: &Download) -> Vec<NewEvent> {
    let actor = "sync:qbittorrent";
    let mut events = Vec::new();
    let with = |extra: serde_json::Value| {
        let mut b = body(now);
        if let (Some(base), serde_json::Value::Object(more)) = (b.as_object_mut(), extra) {
            base.extend(more);
        }
        b
    };
    match before {
        None => events.push(event(
            "download_added",
            "download",
            &now.hash,
            actor,
            body(now),
        )),
        Some(b) if b.progress < 1.0 && now.progress >= 1.0 => events.push(event(
            "download_completed",
            "download",
            &now.hash,
            actor,
            with(json!({ "completed_at": now.completed_at })),
        )),
        Some(_) => {}
    }
    let was_stuck = before.is_some_and(|b| b.stuck);
    let reason_changed = before.is_some_and(|b| b.stuck_reason != now.stuck_reason);
    if now.stuck && (!was_stuck || reason_changed) {
        events.push(event(
            "download_stuck",
            "download",
            &now.hash,
            actor,
            with(json!({
                "reason": now.stuck_reason,
                "queue_state": now.queue_state,
                "queue_health": now.queue_health,
                "messages": now.queue_messages,
            })),
        ));
    }
    if now.orphan && !before.is_some_and(|b| b.orphan) {
        events.push(event(
            "download_orphaned",
            "download",
            &now.hash,
            actor,
            with(json!({ "reason": now.orphan_reason })),
        ));
    }
    events
}

/// What is compared to decide whether a stored row needs writing.
fn same(a: &Download, b: &Download) -> bool {
    a.name == b.name
        && a.category == b.category
        && a.state == b.state
        && a.progress == b.progress
        && a.size_bytes == b.size_bytes
        && a.completed_at == b.completed_at
        && a.save_path == b.save_path
        && a.owner == b.owner
        && a.owner_source == b.owner_source
        && a.title_id == b.title_id
        && a.queue_status == b.queue_status
        && a.queue_state == b.queue_state
        && a.queue_health == b.queue_health
        && a.queue_messages == b.queue_messages
        && a.stuck == b.stuck
        && a.stuck_reason == b.stuck_reason
        && a.orphan == b.orphan
        && a.orphan_reason == b.orphan_reason
        && a.speed_bytes == b.speed_bytes
        && a.eta_seconds == b.eta_seconds
        && a.seeds == b.seeds
        && a.peers == b.peers
        && a.indexer == b.indexer
        // Seeding time moves every pass; a row is rewritten for it once an hour.
        && (a.seeding_seconds - b.seeding_seconds).abs() < 3600
}

/// Compare the client's torrents with what is stored, write the difference and append the
/// events, in one transaction.
pub async fn reconcile(
    pool: &PgPool,
    journal: &Journal,
    torrents: Vec<Torrent>,
    claims: &Claims,
) -> Result<DownloadsReport, sqlx::Error> {
    let stored: HashMap<String, Download> = store::all(pool)
        .await?
        .into_iter()
        .map(|d| (d.hash.clone(), d))
        .collect();
    let mut report = DownloadsReport {
        listed: torrents.len(),
        ..DownloadsReport::default()
    };
    let listed: HashSet<&str> = torrents.iter().map(|t| t.hash.as_str()).collect();
    // A list far shorter than what is stored is not trusted for removals.
    let removals_trusted = !(stored.len() >= 20 && torrents.len() * 2 < stored.len());
    let gone: Vec<&Download> = if removals_trusted {
        stored
            .values()
            .filter(|d| !listed.contains(d.hash.as_str()))
            .collect()
    } else {
        tracing::warn!(
            stored = stored.len(),
            listed = torrents.len(),
            "the download client listed far fewer torrents than are stored; removals are skipped this pass"
        );
        Vec::new()
    };

    let mut writes: Vec<(Download, Vec<NewEvent>)> = Vec::new();
    let now = Utc::now();
    for t in &torrents {
        let before = stored.get(&t.hash);
        let stored_owner = before.and_then(|b| {
            Some((
                b.owner.as_deref()?,
                b.owner_source.as_deref().unwrap_or("history"),
            ))
        });
        let j = judge(&t.hash, claims, stored_owner);
        let title_id = match (&j.owner, j.engine_title_id) {
            (Some(owner), Some(engine_title_id)) => {
                catalog_store::title_id_of(pool, owner, engine_title_id).await?
            }
            // No queue item names the title any more: keep what was found earlier.
            _ => before.and_then(|b| b.title_id),
        };
        let row = Download {
            hash: t.hash.clone(),
            name: t.name.clone(),
            category: t.category.clone(),
            state: t.state.clone(),
            progress: t.progress,
            size_bytes: t.size_bytes,
            added_at: t.added_at,
            completed_at: t.completed_at,
            seeding_seconds: t.seeding_seconds,
            save_path: t.save_path.clone(),
            owner: j.owner,
            owner_source: j.owner_source.map(str::to_string),
            title_id,
            title: None,
            queue_status: j.queue_status,
            queue_state: j.queue_state,
            queue_health: j.queue_health,
            queue_messages: json!(j.queue_messages),
            stuck: j.stuck,
            stuck_reason: j.stuck_reason,
            // While it cannot be settled, the stored answer stands.
            orphan: j.orphan.unwrap_or_else(|| before.is_some_and(|b| b.orphan)),
            orphan_reason: match j.orphan {
                Some(_) => j.orphan_reason,
                None => before.and_then(|b| b.orphan_reason.clone()),
            },
            first_seen_at: before.map(|b| b.first_seen_at).unwrap_or(now),
            updated_at: now,
            // How it moves, only while it is not complete: a seeding torrent's row stays still.
            speed_bytes: (t.progress < 1.0).then_some(t.speed).flatten(),
            eta_seconds: (t.progress < 1.0).then_some(t.eta).flatten(),
            seeds: (t.progress < 1.0).then_some(t.seeds).flatten(),
            peers: (t.progress < 1.0).then_some(t.peers).flatten(),
            // The release's indexer does not change once a queue has named it.
            indexer: j.indexer.or_else(|| before.and_then(|b| b.indexer.clone())),
        };
        let events = events_for(before, &row);
        if before.is_none_or(|b| !same(b, &row)) || !events.is_empty() {
            writes.push((row, events));
        }
    }
    if writes.is_empty() && gone.is_empty() {
        return Ok(report);
    }

    let mut tx = journal.begin(pool).await?;
    let mut events: Vec<NewEvent> = Vec::new();
    for (row, row_events) in writes {
        store::upsert(tx.conn(), &row).await?;
        for e in &row_events {
            match e.kind.as_str() {
                "download_added" => report.added += 1,
                "download_completed" => report.completed += 1,
                "download_stuck" => report.stuck += 1,
                "download_orphaned" => report.orphaned += 1,
                _ => {}
            }
        }
        if row_events.is_empty() {
            report.updated += 1;
        }
        events.extend(row_events);
    }
    for d in gone {
        store::delete(tx.conn(), &d.hash).await?;
        report.removed += 1;
        events.push(event(
            "download_removed",
            "download",
            &d.hash,
            "sync:qbittorrent",
            body(d),
        ));
    }
    tx.append(events).await?;
    tx.commit().await?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA1";
    const B: &str = "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB2";
    const C: &str = "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC3";
    const D: &str = "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD4";

    fn item(
        hash: &str,
        title: Option<i64>,
        state: &str,
        health: &str,
        messages: &[&str],
    ) -> QueueItem {
        QueueItem {
            queue_id: 1,
            engine_title_id: title,
            title: "Release".to_string(),
            download_id: Some(hash.to_string()),
            status: Some("completed".to_string()),
            tracked_state: Some(state.to_string()),
            tracked_status: Some(health.to_string()),
            messages: messages.iter().map(|m| m.to_string()).collect(),
            indexer: None,
        }
    }

    fn claims(complete: bool) -> Claims {
        Claims {
            queues: BTreeMap::from([
                (
                    "radarr".to_string(),
                    vec![
                        item(
                            A,
                            Some(2877),
                            "importBlocked",
                            "warning",
                            &["Not an upgrade for existing movie file(s)"],
                        ),
                        item(C, None, "importPending", "ok", &[]),
                    ],
                ),
                ("sonarr".to_string(), vec![]),
            ]),
            history: BTreeMap::from([
                ("radarr".to_string(), HashSet::from([A.to_string()])),
                ("sonarr".to_string(), HashSet::from([B.to_string()])),
            ]),
            complete,
        }
    }

    #[test]
    fn the_queue_names_the_owner_and_what_is_stuck() {
        let j = judge(A, &claims(true), None);
        assert_eq!(
            (j.owner.as_deref(), j.owner_source),
            (Some("radarr"), Some("queue"))
        );
        assert_eq!(j.engine_title_id, Some(2877));
        assert!(j.stuck);
        assert_eq!(
            j.stuck_reason.as_deref(),
            Some("Not an upgrade for existing movie file(s)")
        );
        assert_eq!(j.orphan, Some(false));
    }

    #[test]
    fn grab_history_names_the_owner_when_the_queue_does_not() {
        let j = judge(B, &claims(true), None);
        assert_eq!(
            (j.owner.as_deref(), j.owner_source),
            (Some("sonarr"), Some("history"))
        );
        assert!(!j.stuck);
        assert_eq!(j.orphan, Some(false));
    }

    #[test]
    fn a_queue_item_with_no_title_is_an_orphan() {
        let j = judge(C, &claims(true), None);
        assert_eq!(j.owner.as_deref(), Some("radarr"));
        assert_eq!(j.orphan, Some(true));
        assert!(j.orphan_reason.unwrap().contains("without a title"));
    }

    #[test]
    fn a_torrent_nobody_claims_is_an_orphan_only_once_every_arr_was_heard() {
        let settled = judge(D, &claims(true), None);
        assert_eq!((settled.owner, settled.orphan), (None, Some(true)));
        let unsettled = judge(D, &claims(false), None);
        assert_eq!((unsettled.owner, unsettled.orphan), (None, None));
        // An owner on record is kept when the arrs no longer show the claim.
        let kept = judge(D, &claims(true), Some(("radarr-4k", "history")));
        assert_eq!(
            (kept.owner.as_deref(), kept.orphan),
            (Some("radarr-4k"), Some(false))
        );
    }

    #[test]
    fn hashes_compare_in_upper_case() {
        // The adapters upper-case both sides; a lower-case hash is another string here.
        let j = judge(&A.to_lowercase(), &claims(true), None);
        assert_eq!(j.owner, None);
    }

    fn download(hash: &str, progress: f64) -> Download {
        Download {
            hash: hash.to_string(),
            name: "Release".to_string(),
            category: Some("radarr".to_string()),
            state: "downloading".to_string(),
            progress,
            size_bytes: 10,
            added_at: None,
            completed_at: None,
            seeding_seconds: 0,
            save_path: None,
            owner: Some("radarr".to_string()),
            owner_source: Some("queue".to_string()),
            title_id: None,
            title: None,
            queue_status: None,
            queue_state: None,
            queue_health: None,
            queue_messages: json!([]),
            stuck: false,
            stuck_reason: None,
            orphan: false,
            orphan_reason: None,
            first_seen_at: Utc::now(),
            updated_at: Utc::now(),
            speed_bytes: None,
            eta_seconds: None,
            seeds: None,
            peers: None,
            indexer: None,
        }
    }

    fn kinds(events: &[NewEvent]) -> Vec<&str> {
        events.iter().map(|e| e.kind.as_str()).collect()
    }

    #[test]
    fn events_follow_the_transitions() {
        let half = download(A, 0.5);
        assert_eq!(kinds(&events_for(None, &half)), vec!["download_added"]);
        assert!(events_for(Some(&half), &half).is_empty());

        let done = download(A, 1.0);
        assert_eq!(
            kinds(&events_for(Some(&half), &done)),
            vec!["download_completed"]
        );
        assert!(
            events_for(Some(&done), &done).is_empty(),
            "completed is said once"
        );

        let mut stuck = done.clone();
        stuck.stuck = true;
        stuck.stuck_reason = Some("Not an upgrade".to_string());
        assert_eq!(
            kinds(&events_for(Some(&done), &stuck)),
            vec!["download_stuck"]
        );
        assert!(events_for(Some(&stuck), &stuck).is_empty());
        let mut other_reason = stuck.clone();
        other_reason.stuck_reason = Some("No files found".to_string());
        assert_eq!(
            kinds(&events_for(Some(&stuck), &other_reason)),
            vec!["download_stuck"]
        );

        let mut orphan = done.clone();
        orphan.orphan = true;
        orphan.owner = None;
        assert_eq!(
            kinds(&events_for(Some(&done), &orphan)),
            vec!["download_orphaned"]
        );
        assert_eq!(
            kinds(&events_for(None, &orphan)),
            vec!["download_added", "download_orphaned"]
        );
    }

    #[test]
    fn a_row_is_rewritten_for_real_changes_only() {
        let a = download(A, 0.5);
        let mut b = a.clone();
        b.seeding_seconds = 120;
        b.updated_at = Utc::now();
        assert!(same(&a, &b));
        b.progress = 0.6;
        assert!(!same(&a, &b));
    }
}
