//! What the API asks about the home movies, the owner's annotations, and the scan that keeps
//! the tapes in step with the folder.

use super::model::{
    Chapter, InboxQuery, TagCount, Tape, TapeAnnotation, TapePage, TapePatch, TapeQuery,
    TapeReading, TapeTotals, YearCount,
};
use super::scan::{self, Reading, ScannedTape};
use super::store::{self, TapeRow};
use crate::core::error::{AppError, AppResult};
use crate::core::journal::{Journal, event};
use crate::core::model::{FolderScan, Page, Paging, ScanReport};
use crate::core::observe;
use crate::core::units;
use crate::lifecycle::sync::jitter;
use serde_json::json;
use sqlx::PgPool;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

pub const FOLDER: &str = "home_movies";

const MAX_TITLE: usize = 200;
const MAX_TAGS: usize = 100;
const MAX_TAG: usize = 100;
const MAX_CHAPTERS: usize = 200;
const MAX_NOTES: usize = 10_000;

/// The home movies folder and the lock that keeps two scans from running at once.
pub struct HomeMovies {
    pub root: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl HomeMovies {
    pub fn new(root: &str) -> Self {
        HomeMovies {
            root: PathBuf::from(root),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn root_text(&self) -> String {
        self.root.display().to_string()
    }
}

/// A stored tape as the owner sees it: the annotation where there is one, else the reading.
pub fn tape(row: TapeRow) -> Tape {
    let annotation: TapeAnnotation =
        serde_json::from_value(row.annotation.clone()).unwrap_or_default();
    let reading = row.reading();
    let read = TapeReading {
        title: reading.title,
        year_from: reading.year_from,
        year_to: reading.year_to,
        uncertain: reading.uncertain,
        chapters: reading
            .chapters
            .into_iter()
            .map(|label| Chapter {
                label,
                start_seconds: None,
            })
            .collect(),
    };
    // Years the owner set replace the reading's both, and are taken as certain unless the
    // owner says otherwise.
    let dated = annotation.year_from.is_some() || annotation.year_to.is_some();
    let (year_from, year_to) = if dated {
        (
            annotation.year_from.or(annotation.year_to),
            annotation.year_to.or(annotation.year_from),
        )
    } else {
        (read.year_from, read.year_to)
    };
    let uncertain = annotation
        .uncertain
        .unwrap_or(if dated { false } else { read.uncertain });
    let file_name = row.path.rsplit('/').next().unwrap_or(&row.path).to_string();
    Tape {
        id: row.id,
        file_name,
        path: row.path,
        size_bytes: row.size_bytes,
        modified_at: row.modified_at,
        title: annotation
            .title
            .clone()
            .unwrap_or_else(|| read.title.clone()),
        year_from,
        year_to,
        uncertain,
        chapters: annotation
            .chapters
            .clone()
            .unwrap_or_else(|| read.chapters.clone()),
        people: annotation.people.clone().unwrap_or_default(),
        places: annotation.places.clone().unwrap_or_default(),
        private: annotation.private.unwrap_or(false),
        notes: annotation.notes.clone(),
        annotated: row.annotated_at.is_some(),
        annotated_at: row.annotated_at,
        read,
        annotation,
        still_url: None,
        missing: row.missing_since.is_some(),
        missing_since: row.missing_since,
        first_seen_at: row.first_seen_at,
        updated_at: row.updated_at,
    }
}

fn has(list: &[String], wanted: &str) -> bool {
    list.iter().any(|v| v.eq_ignore_ascii_case(wanted.trim()))
}

fn matches(t: &Tape, q: &TapeQuery, words: &[String]) -> bool {
    if t.missing != q.missing.unwrap_or(false) {
        return false;
    }
    if let Some(year) = q.year {
        let (Some(from), Some(to)) = (t.year_from, t.year_to) else {
            return false;
        };
        if year < from || year > to {
            return false;
        }
    }
    if let Some(p) = q.person.as_deref().filter(|p| !p.trim().is_empty())
        && !has(&t.people, p)
    {
        return false;
    }
    if let Some(p) = q.place.as_deref().filter(|p| !p.trim().is_empty())
        && !has(&t.places, p)
    {
        return false;
    }
    if q.uncertain.is_some_and(|u| u != t.uncertain)
        || q.private.is_some_and(|p| p != t.private)
        || q.inbox.is_some_and(|i| i == t.annotated)
    {
        return false;
    }
    if !words.is_empty() {
        let mut text = vec![t.title.to_lowercase(), t.path.to_lowercase()];
        text.extend(t.chapters.iter().map(|c| c.label.to_lowercase()));
        text.extend(t.people.iter().map(|p| p.to_lowercase()));
        text.extend(t.places.iter().map(|p| p.to_lowercase()));
        if let Some(notes) = &t.notes {
            text.push(notes.to_lowercase());
        }
        let all = text.join("\n");
        if !words.iter().all(|w| all.contains(w.as_str())) {
            return false;
        }
    }
    true
}

fn tag_counts<'a>(lists: impl Iterator<Item = &'a Vec<String>>) -> Vec<TagCount> {
    // Counted case-insensitively, shown as first written.
    let mut counts: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for list in lists {
        let mut seen = HashSet::new();
        for name in list {
            let key = name.to_lowercase();
            if seen.insert(key.clone()) {
                counts.entry(key).or_insert_with(|| (name.clone(), 0)).1 += 1;
            }
        }
    }
    counts
        .into_values()
        .map(|(name, tapes)| TagCount { name, tapes })
        .collect()
}

/// Order: by the first year (undated last), then by file.
fn order(tapes: &mut [Tape]) {
    tapes.sort_by_key(|t| (t.year_from.is_none(), t.year_from, t.path.clone()));
}

/// Tapes, filtered and paged, with the years, people and places of all tapes for the filter
/// lists, and the totals.
pub async fn list(pool: &PgPool, q: &TapeQuery) -> AppResult<TapePage> {
    let mut tapes: Vec<Tape> = store::all(pool).await?.into_iter().map(tape).collect();
    order(&mut tapes);
    let present: Vec<&Tape> = tapes.iter().filter(|t| !t.missing).collect();
    let mut years: BTreeMap<i32, i64> = BTreeMap::new();
    for t in &present {
        if let Some(y) = t.year_from {
            *years.entry(y).or_default() += 1;
        }
    }
    let totals = TapeTotals {
        tapes: present.len() as i64,
        size_bytes: present.iter().map(|t| t.size_bytes).sum(),
        first_year: present.iter().filter_map(|t| t.year_from).min(),
        last_year: present.iter().filter_map(|t| t.year_to).max(),
        inbox: present.iter().filter(|t| !t.annotated).count() as i64,
        uncertain: present.iter().filter(|t| t.uncertain).count() as i64,
        private: present.iter().filter(|t| t.private).count() as i64,
        missing: tapes.iter().filter(|t| t.missing).count() as i64,
    };
    let people = tag_counts(present.iter().map(|t| &t.people));
    let places = tag_counts(present.iter().map(|t| &t.places));
    let words: Vec<String> =
        q.q.as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
    let matching: Vec<Tape> = tapes
        .into_iter()
        .filter(|t| matches(t, q, &words))
        .collect();
    let paging = Paging::new(q.page, q.per_page);
    let total = matching.len() as i64;
    let items = matching
        .into_iter()
        .skip(paging.offset() as usize)
        .take(paging.limit() as usize)
        .collect();
    Ok(TapePage {
        items,
        page: paging.page,
        per_page: paging.per_page,
        total,
        years: years
            .into_iter()
            .map(|(year, tapes)| YearCount { year, tapes })
            .collect(),
        people,
        places,
        totals,
    })
}

/// Tapes that are there and were never annotated, newest first.
pub async fn inbox(pool: &PgPool, q: &InboxQuery) -> AppResult<Page<Tape>> {
    let mut tapes: Vec<Tape> = store::all(pool)
        .await?
        .into_iter()
        .map(tape)
        .filter(|t| !t.missing && !t.annotated)
        .collect();
    tapes.sort_by(|a, b| {
        b.first_seen_at
            .cmp(&a.first_seen_at)
            .then(a.path.cmp(&b.path))
    });
    let paging = Paging::new(q.page, q.per_page);
    let total = tapes.len() as i64;
    let items = tapes
        .into_iter()
        .skip(paging.offset() as usize)
        .take(paging.limit() as usize)
        .collect();
    Ok(Page::new(items, paging, total))
}

pub async fn get(pool: &PgPool, id: i64) -> AppResult<Tape> {
    store::get(pool, id)
        .await?
        .map(tape)
        .ok_or_else(|| AppError::not_found(format!("There is no recording {id}.")))
}

pub async fn inbox_count(pool: &PgPool) -> AppResult<i64> {
    Ok(store::inbox_count(pool).await?)
}

fn text_field(name: &str, value: &str, max: usize) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::bad_request(format!(
            "`{name}` is empty; null restores the file name's value"
        )));
    }
    if value.chars().count() > max {
        return Err(AppError::bad_request(format!(
            "`{name}` is longer than {max} characters"
        )));
    }
    Ok(value.to_string())
}

fn year_field(name: &str, year: i32) -> AppResult<i32> {
    if (1900..=2100).contains(&year) {
        Ok(year)
    } else {
        Err(AppError::bad_request(format!(
            "`{name}` is a year from 1900 to 2100, not {year}"
        )))
    }
}

fn tags(name: &str, values: &[String]) -> AppResult<Vec<String>> {
    if values.len() > MAX_TAGS {
        return Err(AppError::bad_request(format!(
            "`{name}` holds at most {MAX_TAGS} names"
        )));
    }
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let v = text_field(name, v, MAX_TAG)?;
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&v)) {
            out.push(v);
        }
    }
    Ok(out)
}

fn chapters(values: &[Chapter]) -> AppResult<Vec<Chapter>> {
    if values.len() > MAX_CHAPTERS {
        return Err(AppError::bad_request(format!(
            "`chapters` holds at most {MAX_CHAPTERS} chapters"
        )));
    }
    values
        .iter()
        .map(|c| {
            if c.start_seconds.is_some_and(|s| !(0..=172_800).contains(&s)) {
                return Err(AppError::bad_request(
                    "a chapter's `start_seconds` is from 0 to 172800 (48 hours)",
                ));
            }
            Ok(Chapter {
                label: text_field("chapters.label", &c.label, MAX_TITLE)?,
                start_seconds: c.start_seconds,
            })
        })
        .collect()
}

/// The annotation after a patch, and the names of the fields the patch touched.
pub fn apply_patch(
    current: &TapeAnnotation,
    patch: &TapePatch,
) -> AppResult<(TapeAnnotation, Vec<&'static str>)> {
    let mut a = current.clone();
    let mut fields = Vec::new();
    if let Some(v) = &patch.title {
        a.title = v
            .as_deref()
            .map(|t| text_field("title", t, MAX_TITLE))
            .transpose()?;
        fields.push("title");
    }
    if let Some(v) = patch.year_from {
        a.year_from = v.map(|y| year_field("year_from", y)).transpose()?;
        fields.push("year_from");
    }
    if let Some(v) = patch.year_to {
        a.year_to = v.map(|y| year_field("year_to", y)).transpose()?;
        fields.push("year_to");
    }
    if let (Some(from), Some(to)) = (a.year_from, a.year_to)
        && to < from
    {
        return Err(AppError::bad_request(format!(
            "`year_to` ({to}) is before `year_from` ({from})"
        )));
    }
    if let Some(v) = patch.uncertain {
        a.uncertain = v;
        fields.push("uncertain");
    }
    if let Some(v) = &patch.chapters {
        a.chapters = v.as_deref().map(chapters).transpose()?;
        fields.push("chapters");
    }
    if let Some(v) = &patch.people {
        a.people = v.as_deref().map(|p| tags("people", p)).transpose()?;
        fields.push("people");
    }
    if let Some(v) = &patch.places {
        a.places = v.as_deref().map(|p| tags("places", p)).transpose()?;
        fields.push("places");
    }
    if let Some(v) = patch.private {
        a.private = v;
        fields.push("private");
    }
    if let Some(v) = &patch.notes {
        a.notes = match v.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(n) if n.chars().count() > MAX_NOTES => {
                return Err(AppError::bad_request(format!(
                    "`notes` is longer than {MAX_NOTES} characters"
                )));
            }
            Some(n) => Some(n.to_string()),
        };
        fields.push("notes");
    }
    Ok((a, fields))
}

/// Annotate a tape. Any patch, an empty one too, takes the tape out of the inbox. The journal
/// records which fields changed, not their values.
pub async fn patch(
    pool: &PgPool,
    journal: &Journal,
    actor: &str,
    id: i64,
    patch: &TapePatch,
) -> AppResult<Tape> {
    let mut tx = journal.begin(pool).await?;
    let row = store::get_in(tx.conn(), id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no recording {id}.")))?;
    let current: TapeAnnotation =
        serde_json::from_value(row.annotation.clone()).unwrap_or_default();
    let (annotation, fields) = apply_patch(&current, patch)?;
    let value = serde_json::to_value(&annotation).map_err(|e| AppError::Internal(e.to_string()))?;
    store::annotate(tx.conn(), id, &value).await?;
    tx.append_one(event(
        "tape_annotated",
        "tape",
        id,
        actor,
        json!({ "path": row.path, "fields": fields, "first": row.annotated_at.is_none() }),
    ))
    .await?;
    tx.commit().await?;
    get(pool, id).await
}

fn implausible(known: usize, found: usize) -> bool {
    (found == 0 && known > 0) || (known >= 20 && found * 2 < known)
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(
        "the folder holds {found} tapes where {known} are known; the scan was not applied (is the folder mounted?)"
    )]
    Implausible { known: usize, found: usize },
}

/// Write what a scan found. A new file is a new tape (`tape_added`), unless exactly one tape
/// that is gone has the same size and no other new file does: then the tape was moved or
/// renamed (`tape_moved`) and keeps its annotation. A file that is gone marks its tape missing
/// (`tape_missing`); a missing tape whose file is back is there again (`tape_added`).
pub async fn apply(
    pool: &PgPool,
    journal: &Journal,
    found: &[ScannedTape],
    actor: &str,
) -> Result<ScanReport, ApplyError> {
    let mut tx = journal.begin(pool).await?;
    let stored = store::all_in(tx.conn()).await?;
    let present = stored.iter().filter(|t| t.missing_since.is_none()).count();
    if implausible(present, found.len()) {
        return Err(ApplyError::Implausible {
            known: present,
            found: found.len(),
        });
    }
    let by_path: HashMap<&str, &TapeRow> = stored.iter().map(|t| (t.path.as_str(), t)).collect();
    let found_paths: HashSet<&str> = found.iter().map(|t| t.path.as_str()).collect();
    let mut gone: Vec<&TapeRow> = stored
        .iter()
        .filter(|t| !found_paths.contains(t.path.as_str()))
        .collect();
    let new_sizes: Vec<i64> = found
        .iter()
        .filter(|t| !by_path.contains_key(t.path.as_str()))
        .map(|t| t.size_bytes)
        .collect();
    let mut report = ScanReport {
        folder: FOLDER.to_string(),
        found: found.len() as i64,
        ..ScanReport::default()
    };
    let mut events = Vec::new();
    for t in found {
        let reading: Reading = scan::read_name(&t.path);
        match by_path.get(t.path.as_str()) {
            Some(row) if row.missing_since.is_some() => {
                store::refresh(tx.conn(), row.id, t, &reading).await?;
                report.added += 1;
                events.push(event(
                    "tape_added",
                    "tape",
                    row.id,
                    actor,
                    json!({ "path": t.path, "size_bytes": t.size_bytes, "returned": true }),
                ));
            }
            Some(row) => {
                if row.size_bytes != t.size_bytes
                    || row.modified_at != t.modified_at
                    || row.reading() != reading
                {
                    store::refresh(tx.conn(), row.id, t, &reading).await?;
                    report.changed += 1;
                }
            }
            None => {
                let same_size_new = new_sizes.iter().filter(|s| **s == t.size_bytes).count();
                let candidates: Vec<usize> = gone
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| g.size_bytes == t.size_bytes)
                    .map(|(i, _)| i)
                    .collect();
                if t.size_bytes > 0 && same_size_new == 1 && candidates.len() == 1 {
                    let old = gone.remove(candidates[0]);
                    store::refresh(tx.conn(), old.id, t, &reading).await?;
                    report.changed += 1;
                    events.push(event(
                        "tape_moved",
                        "tape",
                        old.id,
                        actor,
                        json!({ "from": old.path, "to": t.path, "size_bytes": t.size_bytes }),
                    ));
                } else {
                    let id = store::insert(tx.conn(), t, &reading).await?;
                    report.added += 1;
                    events.push(event(
                        "tape_added",
                        "tape",
                        id,
                        actor,
                        json!({ "path": t.path, "size_bytes": t.size_bytes, "returned": false }),
                    ));
                }
            }
        }
    }
    for g in gone.iter().filter(|g| g.missing_since.is_none()) {
        store::mark_missing(tx.conn(), g.id).await?;
        report.removed += 1;
        events.push(event(
            "tape_missing",
            "tape",
            g.id,
            actor,
            json!({ "path": g.path, "size_bytes": g.size_bytes }),
        ));
    }
    report.events = events.len() as i64;
    tx.append(events).await?;
    tx.commit().await?;
    Ok(report)
}

/// Scan the folder now and apply what it holds. One scan at a time.
pub async fn scan(
    pool: &PgPool,
    journal: &Journal,
    lib: &HomeMovies,
    actor: &str,
) -> AppResult<ScanReport> {
    let _one_at_a_time = lib.lock.lock().await;
    let started = Instant::now();
    let root = lib.root.clone();
    let walked = tokio::task::spawn_blocking(move || scan::scan(&root))
        .await
        .map_err(|e| AppError::Internal(e.to_string()))?;
    let outcome = match walked {
        Ok((tapes, skipped)) => apply(pool, journal, &tapes, actor)
            .await
            .map(|r| (r, tapes.len(), skipped))
            .map_err(|e| match e {
                ApplyError::Db(e) => AppError::Db(e),
                other => AppError::Upstream(other.to_string()),
            }),
        Err(e) => Err(AppError::Upstream(e)),
    };
    let took = started.elapsed();
    let took_ms = i64::try_from(took.as_millis()).unwrap_or(i64::MAX);
    observe::sync_pass(FOLDER, "scan", took, outcome.is_ok());
    match outcome {
        Ok((mut report, tapes, skipped)) => {
            report.took_ms = took_ms;
            let counts = json!({ "tapes": tapes, "skipped": skipped });
            store::record_scan(pool, &lib.root_text(), true, took_ms, None, &counts).await?;
            Ok(report)
        }
        Err(e) => {
            let message = e.to_string();
            if let Err(db) = store::record_scan(
                pool,
                &lib.root_text(),
                false,
                took_ms,
                Some(&message),
                &json!({}),
            )
            .await
            {
                tracing::warn!(error = %db, "the home movies scan state could not be recorded");
            }
            Err(e)
        }
    }
}

pub async fn state(pool: &PgPool, lib: &HomeMovies) -> AppResult<FolderScan> {
    Ok(match store::scan_state(pool).await? {
        Some(r) => FolderScan {
            folder: FOLDER.to_string(),
            root: r.root,
            ok: r.ok,
            last_scan_at: Some(r.last_scan_at),
            last_ok_at: r.last_ok_at,
            took_ms: r.took_ms,
            error: r.error,
            counts: r.counts,
        },
        None => FolderScan {
            folder: FOLDER.to_string(),
            root: lib.root_text(),
            ok: false,
            last_scan_at: None,
            last_ok_at: None,
            took_ms: None,
            error: None,
            counts: json!({}),
        },
    })
}

/// Scan every `interval`, after a random delay of up to `jitter_max`.
pub fn spawn_scans(
    pool: PgPool,
    journal: Journal,
    lib: Arc<HomeMovies>,
    interval: Duration,
    jitter_max: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let job = crate::core::jobs::registry()
            .register(crate::core::jobs::JobSpec::folder("home-movies", interval));
        job.wait(jitter(jitter_max)).await;
        loop {
            let started = job.begin();
            match scan(&pool, &journal, &lib, "scan:home_movies").await {
                Ok(r) => {
                    if r.added + r.removed + r.changed > 0 {
                        tracing::info!(
                            added = r.added,
                            missing = r.removed,
                            changed = r.changed,
                            tapes = r.found,
                            "home movies updated"
                        );
                    }
                    job.end(
                        started,
                        Ok(format!(
                            "{}, {} added, {} missing, {} changed",
                            units::counted(r.found, "recording", "recordings"),
                            units::count(r.added),
                            units::count(r.removed),
                            units::count(r.changed)
                        )),
                    );
                }
                Err(e) => {
                    tracing::warn!(root = %lib.root_text(), error = %e, "home movies scan failed");
                    job.end(started, Err(e.to_string()));
                }
            }
            job.wait(interval).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_sets_clears_and_checks() {
        let current = TapeAnnotation {
            title: Some("Old".into()),
            notes: Some("keep".into()),
            ..TapeAnnotation::default()
        };
        let patch: TapePatch = serde_json::from_value(json!({
            "title": null,
            "year_from": 1986,
            "year_to": 1987,
            "people": ["Jordan", "jordan", " Opa "],
            "chapters": [{"label": " Easter 1987 ", "start_seconds": 90}]
        }))
        .unwrap();
        let (a, fields) = apply_patch(&current, &patch).unwrap();
        assert_eq!(a.title, None, "null goes back to the reading");
        assert_eq!(a.notes.as_deref(), Some("keep"), "absent keeps the value");
        assert_eq!(
            a.people,
            Some(vec!["Jordan".to_string(), "Opa".to_string()])
        );
        assert_eq!(a.chapters.as_ref().unwrap()[0].label, "Easter 1987");
        assert_eq!(
            fields,
            vec!["title", "year_from", "year_to", "chapters", "people"]
        );

        for bad in [
            json!({"year_from": 1990, "year_to": 1980}),
            json!({"year_from": 1850}),
            json!({"title": "  "}),
            json!({"chapters": [{"label": "x", "start_seconds": -1}]}),
        ] {
            let p: TapePatch = serde_json::from_value(bad.clone()).unwrap();
            assert!(apply_patch(&current, &p).is_err(), "{bad} must be refused");
        }
        let (a, fields) = apply_patch(&current, &TapePatch::default()).unwrap();
        assert_eq!((a, fields.len()), (current, 0));
    }
}
