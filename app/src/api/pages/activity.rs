//! Activity: the queue from the downloads the server tracks (joined with what the managers'
//! queues say, stored by the sync), the managers' history and their wanted lists.

use super::cards::Cards;
use super::labels::{self, HeldFacts};
use super::managers;
use super::model::{
    Activity, EpisodeLine, HistoryRow, Pack, QueueAttention, QueueRow, TimelineStep, TitleCard,
    TraceStep, WantedRow,
};
use crate::acquire::refusal;
use crate::api::state::AppState;
use crate::catalog::model::TitleKind;
use crate::catalog::store as catalog_store;
use crate::core::config::ArrKind;
use crate::core::error::AppResult;
use crate::core::model::Event;
use crate::core::store as core_store;
use crate::lifecycle::model::Download;
use crate::lifecycle::store as downloads_store;
use crate::requests::store as requests_store;
use chrono::{DateTime, Utc};
use singularr_core::EvalContext;
use singularr_core::policy::evaluate;
use singularr_core::release::{self, MediaKind, ParsedRelease};
use std::collections::HashMap;

/// History rows the page gets, newest first.
pub const HISTORY_ROWS: usize = 500;

/// One download as the queue shows it.
#[derive(Debug, Clone)]
pub struct Queued {
    pub row: QueueRow,
}

impl Queued {
    /// Not done: everything but a download that seeds, as the Activity page counts "in
    /// motion" (one no manager owns waits for someone to deal with it).
    pub fn in_motion(&self) -> bool {
        self.row.stage != "seeding"
    }
}

/// `Stalled: no seeds` from `stalled: no seeds`.
fn sentence(text: &str) -> String {
    labels::stream(text.trim())
}

/// The kind of a download: its title's, else its manager's, else what its name reads as.
fn kind_of(
    state: &AppState,
    card: Option<&TitleCard>,
    owner: Option<&str>,
    parsed: &ParsedRelease,
) -> String {
    if let Some(c) = card {
        return c.kind.clone();
    }
    if let Some(a) = owner.and_then(|o| state.config.arr(o)) {
        return match a.kind {
            ArrKind::Radarr => TitleKind::Movie,
            ArrKind::Sonarr => TitleKind::Show,
        }
        .as_str()
        .to_string();
    }
    match parsed.kind {
        MediaKind::Episode | MediaKind::SeasonPack => "show".to_string(),
        _ if parsed.season.is_some() => "show".to_string(),
        _ => "movie".to_string(),
    }
}

fn integration_name(state: &AppState, id: Option<&str>) -> Option<String> {
    let id = id?;
    Some(
        state
            .config
            .integration(id)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| id.to_string()),
    )
}

/// The timeline of one download from its journal events.
fn timeline(events: &[&Event], client: &str, owned: bool) -> Vec<TimelineStep> {
    events
        .iter()
        .filter_map(|e| {
            let reason = e
                .body
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(sentence);
            let (stage, detail) = match e.kind.as_str() {
                "download_added" => ("downloading", format!("Added to {client}")),
                "download_completed" if owned => ("importing", "Download complete".to_string()),
                "download_completed" => ("seeding", "Download complete".to_string()),
                "download_stuck" => ("attention", reason.unwrap_or_default()),
                "download_orphaned" => ("unassigned", reason.unwrap_or_default()),
                // The owner had the manager import it as it is.
                "download_import_forced" => ("importing", "Import anyway".to_string()),
                _ => return None,
            };
            Some(TimelineStep {
                at: e.at,
                stage: stage.to_string(),
                detail,
            })
        })
        .collect()
}

/// The last time one of these events happened.
fn last_of(events: &[&Event], kind: &str) -> Option<DateTime<Utc>> {
    events.iter().rev().find(|e| e.kind == kind).map(|e| e.at)
}

/// Every download the server tracks, newest first. With `full`, each row also carries its
/// score and trace, its timeline and the request for its title; without, only what the
/// counts and the attention list need.
pub async fn queue(state: &AppState, full: bool) -> AppResult<Vec<Queued>> {
    let pool = &state.pool;
    let mut downloads: Vec<Download> = downloads_store::all(pool).await?;
    downloads.sort_by(|a, b| {
        b.added_at
            .cmp(&a.added_at)
            .then_with(|| a.hash.cmp(&b.hash))
    });
    let title_ids: Vec<i64> = downloads.iter().filter_map(|d| d.title_id).collect();
    let cards = Cards::of_titles(pool, &title_ids).await?;
    let hashes: Vec<String> = downloads
        .iter()
        .filter(|d| full || d.stuck || d.orphan)
        .map(|d| d.hash.clone())
        .collect();
    let events = core_store::subject_events(pool, "download", &hashes, 20_000).await?;
    let mut by_hash: HashMap<&str, Vec<&Event>> = HashMap::new();
    for e in &events {
        by_hash.entry(e.subject_id.as_str()).or_default().push(e);
    }
    let client = integration_name(state, Some(&state.config.qbit.id))
        .unwrap_or_else(|| "the download client".to_string());
    // The open request of each title, newest first.
    let open: Vec<(String, i64, i64)> = if full {
        let tmdb: Vec<i64> = title_ids
            .iter()
            .filter_map(|id| cards.row(*id).and_then(|r| r.tmdb_id))
            .collect();
        requests_store::open_requests(pool, &tmdb).await?
    } else {
        Vec::new()
    };
    let parsed: Vec<ParsedRelease> = downloads.iter().map(|d| release::parse(&d.name)).collect();
    // Episode names the catalog knows, for single episodes of known shows.
    let episode_keys: Vec<(i64, i32, i32)> = downloads
        .iter()
        .zip(&parsed)
        .filter_map(|(d, p)| {
            let title = d.title_id?;
            match (p.season, p.episodes.first()) {
                (Some(s), Some(e)) => Some((title, i32::from(s), i32::from(*e))),
                _ => None,
            }
        })
        .collect();
    let names: HashMap<(i64, i32, i32), String> = if full {
        catalog_store::episode_names_of(pool, &episode_keys)
            .await?
            .into_iter()
            .filter_map(|n| Some(((n.title_id, n.season_number, n.episode_number), n.name?)))
            .collect()
    } else {
        HashMap::new()
    };
    let active = state.policy.current();
    let policy = &active.policy;
    let mut out = Vec::with_capacity(downloads.len());
    for (d, p) in downloads.iter().zip(&parsed) {
        let card = d.title_id.and_then(|id| cards.card(id));
        let kind = kind_of(state, card.as_ref(), d.owner.as_deref(), p);
        let title = card.unwrap_or_else(|| TitleCard {
            id: None,
            kind: kind.clone(),
            title: p.title.clone().unwrap_or_else(|| d.name.clone()),
            year: p.year.map(i32::from),
            poster: None,
            rating: None,
            state: None,
        });
        let evs: &[&Event] = match by_hash.get(d.hash.as_str()) {
            Some(list) => list.as_slice(),
            None => &[],
        };
        // A finished download its manager holds back from the library: who refused, why, and
        // the scores when the manager says them; its own sentence stays beside it.
        let refusal = d
            .owner
            .as_deref()
            .filter(|_| d.stuck && refusal::is_refused(d.queue_state.as_deref()))
            .map(|owner| {
                let messages: Vec<String> =
                    serde_json::from_value(d.queue_messages.clone()).unwrap_or_default();
                let by = integration_name(state, Some(owner)).unwrap_or_default();
                refusal::read(&by, &messages)
            });
        let attention = if let Some(r) = &refusal {
            Some(QueueAttention {
                reason: refusal::words(r),
                since: last_of(evs, "download_stuck"),
                message: d.stuck_reason.clone(),
            })
        } else if d.stuck {
            Some(QueueAttention {
                reason: sentence(
                    d.stuck_reason
                        .as_deref()
                        .unwrap_or("Manager reports a problem"),
                ),
                since: last_of(evs, "download_stuck"),
                message: None,
            })
        } else if d.orphan {
            Some(QueueAttention {
                reason: sentence(d.orphan_reason.as_deref().unwrap_or("No manager")),
                since: last_of(evs, "download_orphaned"),
                message: None,
            })
        } else {
            None
        };
        let facts = HeldFacts::from_release(p);
        let season = p.season.map(i32::from);
        let (episode, pack) = match (season, p.episodes.first()) {
            (Some(s), Some(n)) => (
                Some(EpisodeLine {
                    season: s,
                    number: i32::from(*n),
                    title: d
                        .title_id
                        .and_then(|t| names.get(&(t, s, i32::from(*n))).cloned()),
                }),
                None,
            ),
            (Some(s), None) if kind == "show" => (None, Some(Pack { season: s })),
            _ => (None, None),
        };
        let (score, trace) = match d
            .title_id
            .and_then(|t| cards.lane(t, d.owner.as_deref()))
            .and_then(|l| policy.lane(&l))
            .filter(|_| full)
        {
            Some(lane) => {
                let ctx = EvalContext {
                    size_bytes: u64::try_from(d.size_bytes).ok(),
                    ..EvalContext::default()
                };
                let v = evaluate(policy, lane, p, &ctx);
                (
                    Some(v.score),
                    v.trace
                        .into_iter()
                        .map(|t| TraceStep {
                            label: t.label,
                            points: t.points,
                            why: t.why,
                        })
                        .collect(),
                )
            }
            None => (None, Vec::new()),
        };
        let request = d
            .title_id
            .and_then(|t| cards.row(t))
            .and_then(|r| {
                let tmdb = r.tmdb_id?;
                open.iter().find(|(k, n, _)| *k == r.kind && *n == tmdb)
            })
            .map(|(_, _, id)| format!("r-{id}"));
        let progress = (d.progress * 1000.0).round() / 10.0;
        out.push(Queued {
            row: QueueRow {
                id: format!("q-{}", d.hash.to_ascii_lowercase()),
                kind,
                title,
                episode,
                pack,
                edition: p.editions.first().cloned(),
                release: d.name.clone(),
                quality: labels::quality_text(facts.resolution, facts.source),
                resolution: facts.resolution.map(labels::lines),
                source: facts.source.map(|s| labels::source(s).to_string()),
                codec: facts.video_codec.map(|c| labels::codec(c).to_string()),
                hdr: labels::hdr(&facts.hdr),
                group: facts.group.clone(),
                indexer: d.indexer.clone(),
                client: Some(client.clone()),
                manager: integration_name(state, d.owner.as_deref()),
                stage: labels::queue_stage(d.orphan, d.progress, d.queue_state.is_some())
                    .to_string(),
                attention,
                refusal,
                progress,
                size: d.size_bytes,
                done: (d.size_bytes as f64 * d.progress.clamp(0.0, 1.0)).round() as i64,
                speed: d.speed_bytes.map(i64::from),
                eta: d.eta_seconds.map(i64::from),
                seeds: d.seeds.map(i64::from),
                peers: d.peers.map(i64::from),
                added: d.added_at,
                profile: d
                    .title_id
                    .and_then(|t| cards.profile(t, d.owner.as_deref())),
                score,
                trace,
                timeline: if full {
                    timeline(evs, &client, d.owner.is_some())
                } else {
                    Vec::new()
                },
                request,
            },
        });
    }
    Ok(out)
}

/// The managers' history, newest first, with upgrades folded into one row.
pub async fn history(state: &AppState) -> AppResult<Vec<HistoryRow>> {
    let lists = managers::history(state).await;
    let wanted: Vec<(String, Vec<i64>)> = lists
        .iter()
        .map(|l| {
            (
                l.engine.clone(),
                l.entries.iter().filter_map(|e| e.engine_title_id).collect(),
            )
        })
        .collect();
    let cards = Cards::of_engine_titles(&state.pool, &wanted).await?;
    let mut rows = Vec::new();
    for list in lists.iter() {
        let events: Vec<labels::Event<'_>> = list
            .entries
            .iter()
            .map(|e| labels::Event {
                at: e.at,
                title: e.engine_title_id,
                episode: e.episode.as_ref().map(|x| (x.season, x.number)),
                event: &e.event,
                reason: e.reason.as_deref(),
                quality: e.quality.as_deref(),
            })
            .collect();
        let kind = list.kind.as_str();
        for (e, folded) in list.entries.iter().zip(labels::fold_upgrades(&events)) {
            let Some((event, from)) = folded else {
                continue;
            };
            let release = e.source_title.clone().unwrap_or_default();
            // A record whose title the catalog does not hold is named by its release.
            let title =
                cards.card_for(&list.engine, e.engine_title_id.unwrap_or(0), kind, &release);
            let detail = match event {
                "grabbed" => match &e.indexer {
                    Some(i) => format!("From {i}: {release}"),
                    None => release.clone(),
                },
                "imported" | "upgraded" => match &e.imported_path {
                    Some(p) => format!("{} into {}", labels::file_name(p), parent(p)),
                    None => release.clone(),
                },
                _ => e
                    .reason
                    .as_deref()
                    .map(sentence)
                    .unwrap_or_else(|| release.clone()),
            };
            rows.push(HistoryRow {
                id: format!("h-{}-{}", list.engine, e.id),
                at: e.at,
                kind: kind.to_string(),
                title,
                episode: e.episode.as_ref().map(|x| EpisodeLine {
                    season: x.season,
                    number: x.number,
                    title: x.title.clone(),
                }),
                event: event.to_string(),
                source: e
                    .quality
                    .as_deref()
                    .and_then(labels::source_of_quality)
                    .map(str::to_string),
                quality: e.quality.clone(),
                from: from.map(str::to_string),
                detail,
                size: e.size,
                group: e.group.clone(),
                indexer: e.indexer.clone(),
            });
        }
    }
    rows.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.id.cmp(&b.id)));
    rows.truncate(HISTORY_ROWS);
    Ok(rows)
}

/// The folder a path is in.
fn parent(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
        .rsplit_once(['/', '\\'])
        .map_or("", |(p, _)| p)
}

/// The managers' wanted lists: missing first, then below the cutoff.
pub async fn wanted(state: &AppState) -> AppResult<Vec<WantedRow>> {
    let lists = managers::wanted(state).await;
    let engine_ids: Vec<(String, Vec<i64>)> = lists
        .iter()
        .map(|w| {
            (
                w.engine.clone(),
                w.missing
                    .entries
                    .iter()
                    .chain(&w.cutoff.entries)
                    .map(|e| e.engine_title_id)
                    .collect(),
            )
        })
        .collect();
    let cards = Cards::of_engine_titles(&state.pool, &engine_ids).await?;
    let active = state.policy.current();
    let policy = &active.policy;
    let mut rows = Vec::new();
    for w in lists.iter() {
        let kind = w.kind.as_str();
        for (why, list, page) in [
            ("missing", "missing", &w.missing),
            ("upgrade", "cutoff", &w.cutoff),
        ] {
            for e in &page.entries {
                let title_id = cards.title_of(&w.engine, e.engine_title_id);
                let title = cards.card_for(&w.engine, e.engine_title_id, kind, "Unknown title");
                let lane = title_id
                    .and_then(|t| cards.lane(t, Some(&w.engine)))
                    .and_then(|l| policy.lane(&l));
                rows.push(WantedRow {
                    id: format!("w-{}-{list}-{}", w.engine, e.id),
                    kind: kind.to_string(),
                    title,
                    episode: e.episode.as_ref().map(|x| EpisodeLine {
                        season: x.season,
                        number: x.number,
                        title: x.title.clone(),
                    }),
                    why: why.to_string(),
                    released: e.released.map(|r| r.date_naive()),
                    profile: title_id.and_then(|t| cards.profile(t, Some(&w.engine))),
                    have: e.have.clone(),
                    want: lane.and_then(|l| {
                        l.cutoff_quality
                            .clone()
                            .or_else(|| l.ladder.first().and_then(|s| s.first().cloned()))
                    }),
                    last_search: e.last_search,
                    monitored: e.monitored,
                });
            }
        }
    }
    Ok(rows)
}

/// The parts of the Activity answer a caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    pub queue: bool,
    pub history: bool,
    pub wanted: bool,
}

impl Parts {
    /// The queue alone (`GET /api/v1/activity/queue`).
    pub const QUEUE: Parts = Parts {
        queue: true,
        history: false,
        wanted: false,
    };
    /// The managers' history alone (`GET /api/v1/activity/history`).
    pub const HISTORY: Parts = Parts {
        queue: false,
        history: true,
        wanted: false,
    };
    /// The managers' wanted lists alone (`GET /api/v1/activity/wanted`).
    pub const WANTED: Parts = Parts {
        queue: false,
        history: false,
        wanted: true,
    };
}

/// One part of the answer when it is wanted. A part that is not wanted is never started, so
/// what it would read (the managers, for history and wanted) is not asked.
async fn part<T>(wanted: bool, read: impl Future<Output = AppResult<T>>) -> AppResult<Option<T>> {
    if wanted {
        read.await.map(Some)
    } else {
        Ok(None)
    }
}

/// The Activity answer: the parts asked for, read side by side.
pub async fn activity(state: &AppState, parts: Parts) -> AppResult<Activity> {
    let (queue, history, wanted) = tokio::join!(
        part(parts.queue, queue(state, true)),
        part(parts.history, history(state)),
        part(parts.wanted, wanted(state)),
    );
    Ok(Activity {
        queue: queue?.map(|rows| rows.into_iter().map(|q| q.row).collect()),
        history: history?,
        wanted: wanted?,
    })
}
