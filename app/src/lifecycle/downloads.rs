//! Downloads hygiene: who owns each download, which are stuck, which are orphans.
//!
//! A download's owner is the arr whose queue or grab history has its download id: a torrent's
//! hash, a usenet client's job id. Each download client is read on its own. Its pass touches
//! its own rows, and the rows set aside under a download id it lists for the first time. Stage
//! 1 only reports. Nothing is removed or changed in a download client.

use super::model::Download;
use super::store;
use crate::catalog::store as catalog_store;
use crate::core::journal::{Journal, NewEvent, event};
use crate::engines::clients::HISTORY_DAYS;
use crate::engines::{ClientDownload, ClientListing, QueueItem};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::json;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};

/// What the arrs currently claim. Built by the queue and history loops.
#[derive(Debug, Clone, Default)]
pub struct Claims {
    /// The last queue read of each arr, by engine id.
    pub queues: BTreeMap<String, Vec<QueueItem>>,
    /// The download ids each arr has grabbed, upper case, by engine id.
    pub history: BTreeMap<String, HashSet<String>>,
    /// Every configured arr has a queue read and a loaded grab history. Until then a download
    /// nobody claims cannot be called an orphan.
    pub complete: bool,
}

/// What the claims say about one download.
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

/// Decide owner, stuck and orphan for one download, by its download id. `stored_owner` is the
/// owner already on record: a claim found once is kept when the arrs no longer show it (a
/// finished import leaves the queue, and old grabs fall off the history that is read).
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
    /// Rows past the listing's edge and older than the history's window, gone without an
    /// event.
    pub aged: usize,
    /// Rows set aside under another client that this one took over.
    pub taken_over: usize,
}

fn body(d: &Download) -> serde_json::Value {
    json!({
        "hash": d.hash,
        "client": d.client,
        "protocol": d.protocol,
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

/// Whether a download is complete: a torrent once all of it is there, a usenet download once
/// its client reports it completed (one that failed is past its download and is not).
pub fn complete(d: &Download) -> bool {
    if d.usenet() {
        d.status.as_deref() == Some("completed")
    } else {
        d.progress >= 1.0
    }
}

/// The events one download raises, given the row stored before (if any) and the row now. They
/// are its client's: `sync:<client id>`.
pub fn events_for(before: Option<&Download>, now: &Download) -> Vec<NewEvent> {
    let actor = format!("sync:{}", now.client);
    let actor = actor.as_str();
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
        Some(b) if !complete(b) && complete(now) => events.push(event(
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
        && a.protocol == b.protocol
        && a.item_id == b.item_id
        && a.status == b.status
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

/// When a stored download finished, if that is at or before the listing's edge. A finished
/// download there is past the history the client was read over: its absence from the listing
/// is no removal.
fn past_edge(d: &Download, edge: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    let edge = edge?;
    let finished = matches!(d.status.as_deref(), Some("completed" | "failed"));
    d.completed_at.filter(|at| finished && *at <= edge)
}

/// Compare one client's listing with what is stored of that client, write the difference and
/// append the events, in one transaction. Two downloads of one listing with the same download
/// id are one: the first counts.
///
/// A stored download the listing leaves out is removed, with `download_removed`. One past the
/// listing's edge (a usenet job older than the history read) is no removal. Its row stays as it
/// is while it is inside the window of [`HISTORY_DAYS`] days. It goes without an event once it
/// is older. A job that comes back inside the window finds its row.
///
/// A download id the client lists for the first time can be one whose row was set aside: the
/// client it was stored under is one the configuration no longer lists (an id renamed). The row
/// is taken over with what it held (the owner, the title, the indexer, when it was first seen),
/// and no `download_added` is raised.
pub async fn reconcile(
    pool: &PgPool,
    journal: &Journal,
    client: &str,
    listing: impl Into<ClientListing>,
    claims: &Claims,
) -> Result<DownloadsReport, sqlx::Error> {
    let ClientListing {
        downloads,
        finished_since,
    } = listing.into();
    let stored: HashMap<String, Download> = store::of_client(pool, client)
        .await?
        .into_iter()
        .map(|d| (d.hash.clone(), d))
        .collect();
    let mut seen: HashSet<String> = HashSet::new();
    let downloads: Vec<ClientDownload> = downloads
        .into_iter()
        .filter(|d| seen.insert(d.download_id.clone()))
        .collect();
    let mut report = DownloadsReport {
        listed: downloads.len(),
        ..DownloadsReport::default()
    };
    let listed: HashSet<&str> = downloads.iter().map(|t| t.download_id.as_str()).collect();
    let now = Utc::now();
    let window_start = now - TimeDelta::days(HISTORY_DAYS);
    // What is stored and not listed: past the edge and older than the window (it goes without
    // an event), past the edge inside the window (it stays), or gone.
    let mut aged: Vec<&Download> = Vec::new();
    let mut held = 0;
    let mut unlisted: Vec<&Download> = Vec::new();
    for d in stored
        .values()
        .filter(|d| !listed.contains(d.hash.as_str()))
    {
        match past_edge(d, finished_since) {
            Some(at) if at < window_start => aged.push(d),
            Some(_) => held += 1,
            None => unlisted.push(d),
        }
    }
    // A list far shorter than what is stored is not trusted for removals.
    let in_scope = stored.len() - aged.len() - held;
    let removals_trusted = !(in_scope >= 20 && downloads.len() * 2 < in_scope);
    let gone: Vec<&Download> = if removals_trusted {
        unlisted
    } else {
        tracing::warn!(
            client,
            stored = in_scope,
            listed = downloads.len(),
            "the download client listed far fewer downloads than are stored; removals are skipped this pass"
        );
        Vec::new()
    };
    // The rows set aside under the ids this client lists for the first time.
    let first: Vec<String> = downloads
        .iter()
        .filter(|t| !stored.contains_key(&t.download_id))
        .map(|t| t.download_id.clone())
        .collect();
    let set_aside: HashMap<String, Download> = store::set_aside_of(pool, &first)
        .await?
        .into_iter()
        .map(|d| (d.hash.clone(), d))
        .collect();

    let mut writes: Vec<(Download, Vec<NewEvent>, Option<String>)> = Vec::new();
    for t in &downloads {
        let adopted = (!stored.contains_key(&t.download_id))
            .then(|| set_aside.get(&t.download_id))
            .flatten();
        let before = stored.get(&t.download_id).or(adopted);
        let stored_owner = before.and_then(|b| {
            Some((
                b.owner.as_deref()?,
                b.owner_source.as_deref().unwrap_or("history"),
            ))
        });
        let j = judge(&t.download_id, claims, stored_owner);
        let title_id = match (&j.owner, j.engine_title_id) {
            (Some(owner), Some(engine_title_id)) => {
                catalog_store::title_id_of(pool, owner, engine_title_id).await?
            }
            // No queue item names the title any more: keep what was found earlier.
            _ => before.and_then(|b| b.title_id),
        };
        let row = Download {
            hash: t.download_id.clone(),
            client: client.to_string(),
            protocol: t.protocol.as_str().to_string(),
            item_id: Some(t.id.clone()),
            name: t.name.clone(),
            category: t.category.clone(),
            state: t.state.clone(),
            status: Some(t.status.as_str().to_string()),
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
            // How it moves, only while it is not complete: a seeding torrent's row stays still,
            // and a usenet download past its download moves no more.
            speed_bytes: (t.progress < 1.0).then_some(t.speed).flatten(),
            eta_seconds: (t.progress < 1.0).then_some(t.eta).flatten(),
            seeds: (t.progress < 1.0).then_some(t.seeds).flatten(),
            peers: (t.progress < 1.0).then_some(t.peers).flatten(),
            // The release's indexer does not change once a queue has named it.
            indexer: j.indexer.or_else(|| before.and_then(|b| b.indexer.clone())),
        };
        let events = events_for(before, &row);
        if adopted.is_some() || before.is_none_or(|b| !same(b, &row)) || !events.is_empty() {
            writes.push((row, events, adopted.map(|a| a.client.clone())));
        }
    }
    if writes.is_empty() && gone.is_empty() && aged.is_empty() {
        return Ok(report);
    }

    let mut tx = journal.begin(pool).await?;
    let mut events: Vec<NewEvent> = Vec::new();
    for (row, row_events, adopted_from) in writes {
        if let Some(from) = adopted_from {
            store::delete(tx.conn(), &from, &row.hash).await?;
            report.taken_over += 1;
        }
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
        store::delete(tx.conn(), client, &d.hash).await?;
        report.removed += 1;
        events.push(event(
            "download_removed",
            "download",
            &d.hash,
            &format!("sync:{client}"),
            body(d),
        ));
    }
    for d in aged {
        store::delete(tx.conn(), client, &d.hash).await?;
        report.aged += 1;
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
            client: "qbittorrent".to_string(),
            protocol: "torrent".to_string(),
            item_id: Some(hash.to_ascii_lowercase()),
            name: "Release".to_string(),
            category: Some("radarr".to_string()),
            state: "downloading".to_string(),
            status: Some("downloading".to_string()),
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

    /// A usenet download is matched to its manager's queue by its job id, as a torrent is by its
    /// hash. Its events are its client's.
    #[test]
    fn a_usenet_download_is_judged_by_its_job_id_and_journaled_as_its_client() {
        const NZO: &str = "ECEE74C5-6619-4FB7-BA9A-2EE4CF76E5F2";
        let mut c = claims(true);
        c.queues
            .get_mut("sonarr")
            .unwrap()
            .push(item(NZO, Some(12), "downloading", "ok", &[]));
        let j = judge(NZO, &c, None);
        assert_eq!(
            (j.owner.as_deref(), j.owner_source, j.engine_title_id),
            (Some("sonarr"), Some("queue"), Some(12))
        );
        assert_eq!(j.orphan, Some(false));
        let mut d = download(NZO, 0.4);
        d.client = "usenet".to_string();
        d.protocol = "usenet".to_string();
        assert!(d.usenet());
        let events = events_for(None, &d);
        assert_eq!(events[0].actor, "sync:usenet");
        assert_eq!(
            (&events[0].body["client"], &events[0].body["protocol"]),
            (&json!("usenet"), &json!("usenet"))
        );
        // The qBittorrent of the environment form journals as it always did.
        assert_eq!(
            events_for(None, &download(A, 0.4))[0].actor,
            "sync:qbittorrent"
        );
        // A usenet job completes when its client reports it completed. The end of its download
        // and the start of its unpacking are no completion. A failed job never completes.
        let at = |status: &str, progress: f64| {
            let mut job = d.clone();
            job.status = Some(status.to_string());
            job.progress = progress;
            job
        };
        let unpacking = at("unpacking", 1.0);
        assert!(events_for(Some(&at("downloading", 0.9)), &unpacking).is_empty());
        assert_eq!(
            kinds(&events_for(Some(&unpacking), &at("completed", 1.0))),
            vec!["download_completed"]
        );
        assert!(events_for(Some(&at("downloading", 0.9)), &at("failed", 1.0)).is_empty());
        assert!(!complete(&at("failed", 1.0)) && complete(&download(A, 1.0)));
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
        // A row stored before the product's status was kept is written once, to add it.
        let mut older = a.clone();
        older.status = None;
        assert!(!same(&older, &a));
    }
}
