//! Apply what an engine reports to the catalog: compare with what is stored, write the
//! difference, and append one journal event per change, all in one transaction.
//!
//! One task per engine calls into here, so an engine's rows have a single writer. That lets
//! the comparison and the assessments (the slow part) run before the transaction opens.

use super::ingest::{self, Assess, Assessment, EngineRef, FileFacts, TitleContext, TitleFacts};
use super::store::{self, CopyKey, CopyRow, CopyWrite, LinkRow};
use crate::core::journal::{Journal, JournalTx, NewEvent, event};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub titles_added: usize,
    pub titles_updated: usize,
    pub titles_removed: usize,
    pub copies_added: usize,
    pub copies_replaced: usize,
    pub copies_removed: usize,
    pub copies_changed: usize,
    pub events: usize,
}

impl ApplyReport {
    pub fn changed(&self) -> bool {
        *self != ApplyReport::default()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// The engine listed far fewer titles than are known. Nothing was applied, because a
    /// short answer from a struggling engine must not read as a mass removal.
    #[error("{engine} listed {listed} titles where {known} are known; the answer was not applied")]
    Implausible {
        engine: String,
        known: usize,
        listed: usize,
    },
    #[error("the assessment task failed: {0}")]
    Task(String),
}

/// A listing this much shorter than what is stored is refused.
fn implausible(known: usize, listed: usize) -> bool {
    known >= 20 && listed * 2 < known
}

/// What to do with one file, decided by comparing ids, hashes and episode slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileChange {
    /// A file the catalog does not hold yet. The index is into the incoming files.
    Add(usize),
    /// A stored copy the engine no longer lists.
    Remove(i64),
    /// A stored copy whose place a new file took (same movie, or same episodes).
    Replace { old: i64, new: usize },
    /// The same file id with other facts (renamed, transcoded in place, another lane).
    Update { id: i64, new: usize },
}

/// Compare one title's stored copies with the files its engine lists now.
/// `incoming` holds each file's id, hash, season and episodes.
pub fn diff_files(
    existing: &[CopyKey],
    incoming: &[(i64, &str, Option<i32>, &[i32])],
) -> Vec<FileChange> {
    let stored: HashMap<i64, &CopyKey> = existing.iter().map(|k| (k.engine_file_id, k)).collect();
    let listed: HashSet<i64> = incoming.iter().map(|f| f.0).collect();
    let mut changes = Vec::new();
    let mut added = Vec::new();
    for (index, (file_id, hash, _, _)) in incoming.iter().enumerate() {
        match stored.get(file_id) {
            Some(key) if key.facts_hash != *hash => changes.push(FileChange::Update {
                id: key.id,
                new: index,
            }),
            Some(_) => {}
            None => added.push(index),
        }
    }
    let mut removed: Vec<&CopyKey> = existing
        .iter()
        .filter(|k| !listed.contains(&k.engine_file_id))
        .collect();
    for index in added {
        let (_, _, season, episodes) = incoming[index];
        // A movie has one slot. An episode file replaces the copy of the same episodes;
        // files whose episodes are unknown are never paired.
        let slot = removed.iter().position(|k| {
            k.season_number == season
                && k.episode_numbers.as_slice() == episodes
                && (season.is_none() || !episodes.is_empty())
        });
        match slot {
            Some(pos) => changes.push(FileChange::Replace {
                old: removed.remove(pos).id,
                new: index,
            }),
            None => changes.push(FileChange::Add(index)),
        }
    }
    changes.extend(removed.into_iter().map(|k| FileChange::Remove(k.id)));
    changes
}

/// A change with everything needed to write it.
struct Planned {
    change: FileChange,
    /// The incoming file with its hash and assessment, for every change but a removal.
    file: Option<(FileFacts, String, Assessment)>,
}

/// One title of one engine, for the file writes and their events.
#[derive(Clone)]
struct TitleCtx {
    title_id: i64,
    title: String,
    engine_title_id: i64,
    lane: Option<String>,
}

fn plan_files(
    existing: &[CopyKey],
    files: Vec<FileFacts>,
    title: &TitleContext<'_>,
    assess: &dyn Assess,
) -> Vec<Planned> {
    let hashes: Vec<String> = files
        .iter()
        .map(|f| ingest::file_facts_hash(f, title))
        .collect();
    let keys: Vec<(i64, &str, Option<i32>, &[i32])> = files
        .iter()
        .zip(&hashes)
        .map(|(f, h)| {
            (
                f.engine_file_id,
                h.as_str(),
                f.season_number,
                f.episode_numbers.as_slice(),
            )
        })
        .collect();
    let changes = diff_files(existing, &keys);
    let mut slots: Vec<Option<FileFacts>> = files.into_iter().map(Some).collect();
    changes
        .into_iter()
        .map(|change| {
            let index = match change {
                FileChange::Add(i) => Some(i),
                FileChange::Replace { new, .. } => Some(new),
                FileChange::Update { new, .. } => Some(new),
                FileChange::Remove(_) => None,
            };
            let file = index.and_then(|i| {
                let file = slots[i].take()?;
                let assessment = assess.assess(&file, title);
                Some((file, hashes[i].clone(), assessment))
            });
            Planned { change, file }
        })
        .collect()
}

fn old_copy_json(row: &CopyRow) -> Value {
    json!({
        "copy_id": row.id,
        "relative_path": row.relative_path,
        "release_name": row.release_name,
        "size_bytes": row.size_bytes,
        "lane": row.lane,
        "quality": row.mq_quality,
        "group": row.mq_group,
        "edition": row.mq_edition,
        "score": row.mq_score,
        "arr_quality": row.arr_quality,
        "arr_score": row.arr_score,
        "verdict": row.verdict,
    })
}

fn new_copy_json(
    copy_id: i64,
    lane: Option<&str>,
    file: &FileFacts,
    a: &Assessment,
    full: bool,
) -> Value {
    let mut v = json!({
        "copy_id": copy_id,
        "relative_path": file.relative_path,
        "release_name": a.release_name,
        "size_bytes": file.size_bytes,
        "lane": lane,
        "quality": a.mq_quality,
        "group": a.mq_group,
        "edition": a.mq_edition,
        "score": a.mq_score,
        "arr_quality": file.arr_quality,
        "arr_score": file.arr_score,
    });
    if full {
        v["verdict"] = serde_json::to_value(a.verdict_view()).unwrap_or(Value::Null);
    }
    v
}

fn subject_json(engine: &EngineRef, t: &TitleCtx, season: Option<i32>, episodes: &[i32]) -> Value {
    json!({
        "title_id": t.title_id,
        "title": t.title,
        "library": engine.library,
        "engine": engine.id,
        "season": season,
        "episodes": episodes,
    })
}

fn merge(mut base: Value, extra: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (base.as_object_mut(), extra) {
        a.extend(b);
    }
    base
}

/// Write one title's planned file changes and queue their events. A movie whose files changed
/// has its editions worked out again (`default_edition` is the list's default key).
#[allow(clippy::too_many_arguments)]
async fn write_files(
    tx: &mut JournalTx,
    engine: &EngineRef,
    t: &TitleCtx,
    plan: Vec<Planned>,
    stamp: &str,
    default_edition: &str,
    events: &mut Vec<NewEvent>,
    report: &mut ApplyReport,
) -> Result<(), sqlx::Error> {
    let actor = ingest::actor(engine);
    let touched = !plan.is_empty();
    for planned in plan {
        let lane = t.lane.as_deref();
        match (planned.change, planned.file) {
            (FileChange::Remove(id), _) => {
                let Some(old) = store::copy_in(tx.conn(), id).await? else {
                    continue;
                };
                store::delete_copy(tx.conn(), id).await?;
                report.copies_removed += 1;
                events.push(event(
                    "copy_removed",
                    "copy",
                    id,
                    &actor,
                    merge(
                        subject_json(engine, t, old.season_number, &old.episode_numbers),
                        old_copy_json(&old),
                    ),
                ));
            }
            (FileChange::Add(_), Some((file, hash, a))) => {
                let id = store::insert_copy(
                    tx.conn(),
                    &CopyWrite {
                        engine,
                        title_id: t.title_id,
                        engine_title_id: t.engine_title_id,
                        lane,
                        file: &file,
                        assessment: &a,
                        facts_hash: &hash,
                        stamp,
                    },
                )
                .await?;
                report.copies_added += 1;
                events.push(event(
                    "copy_added",
                    "copy",
                    id,
                    &actor,
                    merge(
                        subject_json(engine, t, file.season_number, &file.episode_numbers),
                        merge(
                            new_copy_json(id, lane, &file, &a, false),
                            json!({ "protections": a.protections }),
                        ),
                    ),
                ));
            }
            (FileChange::Replace { old, .. }, Some((file, hash, a))) => {
                let before = store::copy_in(tx.conn(), old).await?;
                store::delete_copy(tx.conn(), old).await?;
                let id = store::insert_copy(
                    tx.conn(),
                    &CopyWrite {
                        engine,
                        title_id: t.title_id,
                        engine_title_id: t.engine_title_id,
                        lane,
                        file: &file,
                        assessment: &a,
                        facts_hash: &hash,
                        stamp,
                    },
                )
                .await?;
                report.copies_replaced += 1;
                let old_score = before.as_ref().and_then(|b| b.mq_score);
                let delta = match (old_score, a.mq_score) {
                    (Some(o), Some(n)) => Some(n - o),
                    _ => None,
                };
                events.push(event(
                    "copy_replaced",
                    "copy",
                    id,
                    &actor,
                    merge(
                        subject_json(engine, t, file.season_number, &file.episode_numbers),
                        json!({
                            "old": before.as_ref().map(old_copy_json),
                            "new": new_copy_json(id, lane, &file, &a, true),
                            "score_delta": delta,
                        }),
                    ),
                ));
            }
            (FileChange::Update { id, .. }, Some((file, hash, a))) => {
                let before = store::copy_in(tx.conn(), id).await?;
                store::insert_copy(
                    tx.conn(),
                    &CopyWrite {
                        engine,
                        title_id: t.title_id,
                        engine_title_id: t.engine_title_id,
                        lane,
                        file: &file,
                        assessment: &a,
                        facts_hash: &hash,
                        stamp,
                    },
                )
                .await?;
                let Some(before) = before else { continue };
                // Only what a person would call a change to the file is journaled. Facts the
                // arr recomputes (its own score, media info) are updated silently.
                let mut changed = Vec::new();
                if before.relative_path != file.relative_path {
                    changed.push("relative_path");
                }
                if before.size_bytes != file.size_bytes {
                    changed.push("size_bytes");
                }
                if before.release_name != a.release_name {
                    changed.push("release_name");
                }
                if before.lane.as_deref() != lane {
                    changed.push("lane");
                }
                if before.mq_score != a.mq_score {
                    changed.push("score");
                }
                if changed.is_empty() {
                    continue;
                }
                report.copies_changed += 1;
                events.push(event(
                    "copy_changed",
                    "copy",
                    id,
                    &actor,
                    merge(
                        subject_json(engine, t, file.season_number, &file.episode_numbers),
                        json!({
                            "changed": changed,
                            "old": old_copy_json(&before),
                            "new": new_copy_json(id, lane, &file, &a, true),
                        }),
                    ),
                ));
            }
            // A change other than a removal always carries its file.
            (_, None) => {}
        }
    }
    if touched && engine.kind == super::model::TitleKind::Movie {
        store::refresh_editions(tx.conn(), t.title_id, default_edition).await?;
    }
    Ok(())
}

async fn flush(
    tx: &mut JournalTx,
    events: &mut Vec<NewEvent>,
    report: &mut ApplyReport,
) -> Result<(), sqlx::Error> {
    if events.is_empty() {
        return Ok(());
    }
    report.events += events.len();
    tx.append(std::mem::take(events)).await?;
    Ok(())
}

/// One incoming title, compared and assessed, ready to be written.
struct TitlePlan {
    facts: TitleFacts,
    link: Option<LinkRow>,
    title_changed: bool,
    files: Vec<Planned>,
}

/// Apply an engine's full list of titles. Titles that are new or changed are written, titles
/// the engine no longer lists are removed with their copies. When the titles carry their
/// files (movies), the files are applied too.
pub async fn apply_titles(
    pool: &PgPool,
    journal: &Journal,
    engine: &EngineRef,
    titles: Vec<TitleFacts>,
    assess: Arc<dyn Assess>,
) -> Result<ApplyReport, SyncError> {
    let links = store::engine_links(pool, &engine.id).await?;
    if implausible(links.len(), titles.len()) {
        return Err(SyncError::Implausible {
            engine: engine.id.clone(),
            known: links.len(),
            listed: titles.len(),
        });
    }
    let keys = store::copy_keys(pool, &engine.id, None).await?;
    let stamp = assess.stamp();
    let default_edition = assess.default_edition();

    // Compare and assess off the async threads: a first sync assesses every file.
    let planner = assess.clone();
    let engine_id = engine.id.clone();
    let (plans, gone, gone_keys) = tokio::task::spawn_blocking(move || {
        let mut by_title: HashMap<i64, Vec<CopyKey>> = HashMap::new();
        for key in keys {
            by_title.entry(key.engine_title_id).or_default().push(key);
        }
        let mut links: HashMap<i64, LinkRow> =
            links.into_iter().map(|l| (l.engine_title_id, l)).collect();
        let mut plans = Vec::new();
        for mut facts in titles {
            // The policy's lanes name the arr profiles they stand for; that places the title.
            facts.lane = planner.lane_for(&engine_id, facts.profile.as_deref());
            let link = links.remove(&facts.engine_title_id);
            let existing = by_title.remove(&facts.engine_title_id).unwrap_or_default();
            let title_changed = link
                .as_ref()
                .is_none_or(|l| l.facts_hash != ingest::title_facts_hash(&facts));
            let files = match facts.files.take() {
                Some(files) => plan_files(
                    &existing,
                    files,
                    &TitleContext {
                        engine: &engine_id,
                        lane: facts.lane.as_deref(),
                        tags: &facts.tags,
                        original_language: facts.original_language.as_deref(),
                    },
                    planner.as_ref(),
                ),
                None => Vec::new(),
            };
            if title_changed || !files.is_empty() {
                plans.push(TitlePlan {
                    facts,
                    link,
                    title_changed,
                    files,
                });
            }
        }
        // What is left in `links` is no longer listed by the engine.
        let gone: Vec<LinkRow> = links.into_values().collect();
        let gone_keys: HashMap<i64, Vec<CopyKey>> = gone
            .iter()
            .filter_map(|l| Some((l.engine_title_id, by_title.remove(&l.engine_title_id)?)))
            .collect();
        (plans, gone, gone_keys)
    })
    .await
    .map_err(|e| SyncError::Task(e.to_string()))?;

    let mut report = ApplyReport::default();
    if plans.is_empty() && gone.is_empty() {
        return Ok(report);
    }

    let actor = ingest::actor(engine);
    let mut events: Vec<NewEvent> = Vec::new();
    let mut tx = journal.begin(pool).await?;
    store::ensure_library(tx.conn(), &engine.library, engine.kind.as_str()).await?;
    let mut roots_changed = false;

    for plan in plans {
        let TitlePlan {
            facts,
            link,
            title_changed,
            files,
        } = plan;
        let title_id = if title_changed {
            let (title_id, new_to_catalog) =
                store::upsert_title(tx.conn(), &engine.id, &facts).await?;
            store::upsert_link(tx.conn(), engine, title_id, &facts).await?;
            if engine.kind == super::model::TitleKind::Show {
                store::refresh_seasons(tx.conn(), title_id).await?;
            }
            roots_changed = true;
            match &link {
                None => {
                    report.titles_added += 1;
                    events.push(event(
                        "title_added",
                        "title",
                        title_id,
                        &actor,
                        json!({
                            "title_id": title_id,
                            "title": facts.title,
                            "kind": facts.kind.as_str(),
                            "year": facts.year,
                            "tmdb_id": facts.tmdb_id,
                            "tvdb_id": facts.tvdb_id,
                            "library": engine.library,
                            "engine": engine.id,
                            "lane": facts.lane,
                            "new_to_catalog": new_to_catalog,
                        }),
                    ));
                }
                Some(_) => report.titles_updated += 1,
            }
            title_id
        } else {
            match &link {
                Some(l) => l.title_id,
                None => continue,
            }
        };
        if !files.is_empty() {
            let ctx = TitleCtx {
                title_id,
                title: facts.title.clone(),
                engine_title_id: facts.engine_title_id,
                lane: facts.lane.clone(),
            };
            write_files(
                &mut tx,
                engine,
                &ctx,
                files,
                &stamp,
                &default_edition,
                &mut events,
                &mut report,
            )
            .await?;
        }
        if events.len() >= 500 {
            flush(&mut tx, &mut events, &mut report).await?;
        }
    }

    for link in gone {
        let ctx = TitleCtx {
            title_id: link.title_id,
            title: link.title.clone(),
            engine_title_id: link.engine_title_id,
            lane: link.lane.clone(),
        };
        let removals = gone_keys
            .get(&link.engine_title_id)
            .map(|keys| {
                keys.iter()
                    .map(|k| Planned {
                        change: FileChange::Remove(k.id),
                        file: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        write_files(
            &mut tx,
            engine,
            &ctx,
            removals,
            &stamp,
            &default_edition,
            &mut events,
            &mut report,
        )
        .await?;
        store::delete_link(tx.conn(), &engine.id, link.engine_title_id).await?;
        let last = store::links_left(tx.conn(), link.title_id).await? == 0;
        if last {
            store::delete_title(tx.conn(), link.title_id).await?;
        } else if engine.kind == super::model::TitleKind::Show {
            // Another engine still holds the show: its seasons are now that engine's alone.
            store::refresh_seasons(tx.conn(), link.title_id).await?;
        }
        roots_changed = true;
        report.titles_removed += 1;
        events.push(event(
            "title_removed",
            "title",
            link.title_id,
            &actor,
            json!({
                "title_id": link.title_id,
                "title": link.title,
                "kind": engine.kind.as_str(),
                "library": engine.library,
                "engine": engine.id,
                "gone_from_catalog": last,
            }),
        ));
        if events.len() >= 500 {
            flush(&mut tx, &mut events, &mut report).await?;
        }
    }

    if roots_changed {
        store::refresh_library_roots(tx.conn(), &engine.library).await?;
    }
    flush(&mut tx, &mut events, &mut report).await?;
    tx.commit().await?;
    Ok(report)
}

/// Apply the files of one title (a show's episode files). `file_count` is what the engine's
/// title listing said, remembered so the next pass can tell whether to read the files again.
pub async fn apply_files(
    pool: &PgPool,
    journal: &Journal,
    engine: &EngineRef,
    engine_title_id: i64,
    file_count: i32,
    files: Vec<FileFacts>,
    assess: Arc<dyn Assess>,
) -> Result<ApplyReport, SyncError> {
    let mut report = ApplyReport::default();
    let links = store::engine_links(pool, &engine.id).await?;
    let Some(link) = links
        .into_iter()
        .find(|l| l.engine_title_id == engine_title_id)
    else {
        // The title went away between the listing and this read; the next listing settles it.
        return Ok(report);
    };
    let existing = store::copy_keys(pool, &engine.id, Some(engine_title_id)).await?;
    let stamp = assess.stamp();
    let default_edition = assess.default_edition();
    let episodes: Vec<(i32, Vec<i32>, Option<String>)> = files
        .iter()
        .filter_map(|f| {
            Some((
                f.season_number?,
                f.episode_numbers.clone(),
                f.episode_name.clone(),
            ))
        })
        .filter(|(_, numbers, _)| !numbers.is_empty())
        .collect();
    let (lane, tags) = (link.lane.clone(), link.tags.clone());
    let (engine_id, language) = (engine.id.clone(), link.original_language.clone());
    let plan = tokio::task::spawn_blocking(move || {
        let title = TitleContext {
            engine: &engine_id,
            lane: lane.as_deref(),
            tags: &tags,
            original_language: language.as_deref(),
        };
        plan_files(&existing, files, &title, assess.as_ref())
    })
    .await
    .map_err(|e| SyncError::Task(e.to_string()))?;

    let ctx = TitleCtx {
        title_id: link.title_id,
        title: link.title.clone(),
        engine_title_id,
        lane: link.lane.clone(),
    };
    let mut events = Vec::new();
    let mut tx = journal.begin(pool).await?;
    store::ensure_library(tx.conn(), &engine.library, engine.kind.as_str()).await?;
    write_files(
        &mut tx,
        engine,
        &ctx,
        plan,
        &stamp,
        &default_edition,
        &mut events,
        &mut report,
    )
    .await?;
    for (season, numbers, name) in &episodes {
        // A file that holds several episodes has no single episode name.
        let name = name.as_deref().filter(|_| numbers.len() == 1);
        store::upsert_episodes(tx.conn(), link.title_id, *season, numbers, name).await?;
    }
    store::mark_files_synced(tx.conn(), &engine.id, engine_title_id, file_count).await?;
    flush(&mut tx, &mut events, &mut report).await?;
    tx.commit().await?;
    Ok(report)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReassessReport {
    pub examined: usize,
    pub updated: usize,
    pub score_changed: usize,
}

fn trait_points(verdict: Option<&Value>) -> HashMap<String, (String, i64)> {
    verdict
        .and_then(|v| v.get("trace"))
        .and_then(Value::as_array)
        .map(|trace| {
            trace
                .iter()
                .filter_map(|t| {
                    Some((
                        t.get("trait_id")?.as_str()?.to_string(),
                        (
                            t.get("label")?.as_str()?.to_string(),
                            t.get("points")?.as_i64()?,
                        ),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The traits whose points differ between two verdicts, as the journal records them.
fn trait_delta(old: Option<&Value>, new: Option<&Value>) -> Vec<Value> {
    let (old, new) = (trait_points(old), trait_points(new));
    let mut ids: Vec<&String> = old.keys().chain(new.keys()).collect();
    ids.sort();
    ids.dedup();
    ids.into_iter()
        .filter(|id| old.get(*id).map(|x| x.1) != new.get(*id).map(|x| x.1))
        .map(|id| {
            json!({
                "trait_id": id,
                "label": new.get(id).or(old.get(id)).map(|x| x.0.clone()),
                "old_points": old.get(id).map(|x| x.1),
                "new_points": new.get(id).map(|x| x.1),
            })
        })
        .collect()
}

/// Assess again every copy that was stored under another stamp (another policy, or another
/// build of the parser). A copy whose score comes out different gets a `score_changed`
/// event; `cause` is the `policy_loaded` event when a new policy is the reason.
pub async fn reassess(
    pool: &PgPool,
    journal: &Journal,
    assess: Arc<dyn Assess>,
    cause: Option<i64>,
) -> Result<ReassessReport, SyncError> {
    let stamp = assess.stamp();
    let default_edition = assess.default_edition();
    let mut report = ReassessReport::default();
    let mut after = 0;
    loop {
        let rows = store::stale_copies(pool, &stamp, after, 1000).await?;
        let Some(last) = rows.last() else { break };
        after = last.0.id;
        report.examined += rows.len();
        let assessor = assess.clone();
        let assessed: Vec<(CopyRow, Assessment)> = tokio::task::spawn_blocking(move || {
            rows.into_iter()
                .map(|(row, tags)| {
                    let title = TitleContext {
                        engine: &row.engine,
                        lane: row.lane.as_deref(),
                        tags: &tags,
                        original_language: row.original_language.as_deref(),
                    };
                    let a = assessor.assess(&row.file_facts(), &title);
                    (row, a)
                })
                .collect()
        })
        .await
        .map_err(|e| SyncError::Task(e.to_string()))?;

        let mut tx = journal.begin(pool).await?;
        let mut same = Vec::new();
        let mut events = Vec::new();
        // Titles whose copies now belong to another edition: their editions are worked out
        // again once the batch is written.
        let mut edition_moved: Vec<i64> = Vec::new();
        for (row, a) in &assessed {
            let parsed = serde_json::to_value(&a.parsed).unwrap_or(Value::Null);
            let held = serde_json::to_value(&a.held).unwrap_or(Value::Null);
            let verdict = a.verdict_view().and_then(|v| serde_json::to_value(v).ok());
            let unchanged = parsed == row.parsed
                && held == row.held
                && a.arr_only_formats == row.arr_only_formats
                && a.singularr_only_traits == row.mq_only_traits
                && verdict == row.verdict
                && a.protections == row.protections
                && a.release_name == row.release_name
                && a.edition.key == row.edition_key
                && a.edition.label == row.edition_label
                && a.edition.marked == row.edition_marked
                && a.edition_rank == row.edition_rank
                && (a.diff_quality, a.diff_group, a.diff_edition, a.diff_score)
                    == (
                        row.diff_quality,
                        row.diff_group,
                        row.diff_edition,
                        row.diff_score,
                    );
            if unchanged {
                same.push(row.id);
                continue;
            }
            store::update_assessment(tx.conn(), row.id, a, &stamp).await?;
            report.updated += 1;
            if (
                a.edition.key.as_str(),
                a.edition.label.as_str(),
                a.edition_rank,
            ) != (
                row.edition_key.as_str(),
                row.edition_label.as_str(),
                row.edition_rank,
            ) && !edition_moved.contains(&row.title_id)
            {
                edition_moved.push(row.title_id);
            }
            if a.mq_score != row.mq_score {
                report.score_changed += 1;
                let delta = match (row.mq_score, a.mq_score) {
                    (Some(o), Some(n)) => Some(n - o),
                    _ => None,
                };
                events.push(
                    event(
                        "score_changed",
                        "copy",
                        row.id,
                        "policy",
                        json!({
                            "title_id": row.title_id,
                            "title": row.title,
                            "library": row.library_id,
                            "engine": row.engine,
                            "lane": row.lane,
                            "release_name": a.release_name,
                            "old_score": row.mq_score,
                            "new_score": a.mq_score,
                            "delta": delta,
                            "assessed_with": stamp,
                            "previously_assessed_with": row.assessed_with,
                            "traits": trait_delta(row.verdict.as_ref(), verdict.as_ref()),
                        }),
                    )
                    .caused_by(cause),
                );
            }
        }
        store::stamp_copies(tx.conn(), &same, &stamp).await?;
        for title_id in edition_moved {
            store::refresh_editions(tx.conn(), title_id, &default_edition).await?;
        }
        tx.append(events).await?;
        tx.commit().await?;
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(id: i64, file: i64, hash: &str, season: Option<i32>, episodes: &[i32]) -> CopyKey {
        CopyKey {
            id,
            title_id: 1,
            engine_title_id: 10,
            engine_file_id: file,
            facts_hash: hash.to_string(),
            season_number: season,
            episode_numbers: episodes.to_vec(),
        }
    }

    #[test]
    fn an_unchanged_file_is_no_change() {
        let existing = [key(1, 100, "h", None, &[])];
        assert!(diff_files(&existing, &[(100, "h", None, &[])]).is_empty());
    }

    #[test]
    fn a_new_movie_file_replaces_the_old_one() {
        let existing = [key(1, 100, "h", None, &[])];
        assert_eq!(
            diff_files(&existing, &[(101, "h2", None, &[])]),
            vec![FileChange::Replace { old: 1, new: 0 }]
        );
        assert_eq!(
            diff_files(&[], &[(101, "h2", None, &[])]),
            vec![FileChange::Add(0)]
        );
        assert_eq!(diff_files(&existing, &[]), vec![FileChange::Remove(1)]);
    }

    #[test]
    fn the_same_file_with_other_facts_is_an_update() {
        let existing = [key(1, 100, "h", None, &[])];
        assert_eq!(
            diff_files(&existing, &[(100, "other", None, &[])]),
            vec![FileChange::Update { id: 1, new: 0 }]
        );
    }

    #[test]
    fn episode_files_pair_by_episode() {
        let existing = [
            key(1, 100, "a", Some(1), &[1]),
            key(2, 101, "b", Some(1), &[2]),
            key(3, 102, "c", Some(1), &[3]),
        ];
        let incoming: Vec<(i64, &str, Option<i32>, &[i32])> = vec![
            (100, "a", Some(1), &[1]),
            (201, "x", Some(1), &[2]),
            (300, "y", Some(2), &[1]),
        ];
        let changes = diff_files(&existing, &incoming);
        assert_eq!(
            changes,
            vec![
                FileChange::Replace { old: 2, new: 1 },
                FileChange::Add(2),
                FileChange::Remove(3),
            ]
        );
    }

    #[test]
    fn files_with_unknown_episodes_are_never_paired() {
        let existing = [key(1, 100, "a", Some(1), &[])];
        let incoming: Vec<(i64, &str, Option<i32>, &[i32])> = vec![(200, "x", Some(1), &[])];
        assert_eq!(
            diff_files(&existing, &incoming),
            vec![FileChange::Add(0), FileChange::Remove(1)]
        );
    }

    #[test]
    fn a_short_listing_is_refused() {
        assert!(implausible(4500, 0));
        assert!(implausible(4500, 2000));
        assert!(!implausible(4500, 4400));
        assert!(!implausible(10, 0), "a small library may really be emptied");
        assert!(!implausible(0, 0));
    }

    #[test]
    fn the_trait_delta_names_what_moved() {
        let old = json!({"trace": [
            {"trait_id": "a", "label": "A", "points": 100},
            {"trait_id": "b", "label": "B", "points": 50}
        ]});
        let new = json!({"trace": [
            {"trait_id": "a", "label": "A", "points": 400},
            {"trait_id": "c", "label": "C", "points": -10}
        ]});
        let delta = trait_delta(Some(&old), Some(&new));
        assert_eq!(delta.len(), 3);
        assert_eq!(delta[0]["trait_id"], "a");
        assert_eq!(
            (
                delta[0]["old_points"].as_i64(),
                delta[0]["new_points"].as_i64()
            ),
            (Some(100), Some(400))
        );
        assert_eq!(delta[1]["new_points"], Value::Null);
        assert_eq!(delta[2]["old_points"], Value::Null);
    }
}
