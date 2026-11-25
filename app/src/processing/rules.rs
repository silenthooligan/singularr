//! Processing rules: Singularr's own rules that send a file to a flow of a processing service.
//!
//! A rule has a name, is on or off, runs on every import or on request for the files that
//! match then, says what a file must be, and names a service and one of its flows. What a
//! file must be is said in the policy's condition language (`singularr_core::policy::Condition`:
//! codec, resolution, source, dynamic range, audio, size, release group, title tags, ...),
//! with the libraries it lies in, the policy's traits it carries (a named list of release
//! groups, say) and its bitrate.
//!
//! For every import the rules pass records what each import rule would have sent, whether or
//! not anything is sent: an off rule leaves a `shadow` entry, so it can be watched before it is
//! trusted. A rule that is on sends only while its service's access is `act` and the
//! integrations are read; a file is sent once, by any rule, and never when its title carries
//! the tag the service's adapter names as protecting, or when processing made it. Every entry
//! is kept in the rule's log; a send is also a `processing_sent` event in the rule's name.

use super::model::{IMPORTED, file_name};
use super::service::{SendOutcome, Sending, send};
use super::store;
use crate::catalog::store::{self as catalog_store, RuleFileRow, TitleRef};
use crate::core::journal::Journal;
use crate::core::outbound::Access;
use crate::core::store as core_store;
use crate::engines::Processor;
use crate::engines::service::EngineSet;
use crate::policy::held_context;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use singularr_core::policy::{Condition, EvalContext, Policy};
use singularr_core::release::ParsedRelease;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// The events an import is: a file the catalog did not hold, or one that took another's place.
pub const IMPORT_KINDS: [&str; 2] = ["copy_added", "copy_replaced"];
/// Events the import pass reads at once.
pub const EVENTS_AT_ONCE: i64 = 500;
/// Files one request sends at most; the answer says how many matched.
pub const REQUEST_MAX: usize = 50;
/// The words of a rule's two moments.
pub const ON_IMPORT: &str = "import";
pub const ON_REQUEST: &str = "request";

/// What a file must be for a rule. Every part that is set must hold.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", default, deny_unknown_fields)]
pub struct RuleConditions {
    /// What the file is, in the policy's condition language, as the policy writes it:
    /// `video_codec`, `resolution`, `source`, `hdr`, `sdr`, `audio_codec`, `atmos`,
    /// `channels`, `group`, `collective`, `title_tag`, `min_size_gb`, `max_size_gb`, `any`,
    /// `not`, and the rest of that language.
    #[schema(value_type = Object)]
    pub file: Condition,
    /// The libraries it lies in, by id. Empty: any.
    pub libraries: Vec<String>,
    /// The policy's traits it carries, by id (a named list of release groups, say): one of
    /// them. Empty: no such condition.
    pub traits: Vec<String>,
    /// Its bitrate in Mbit/s, from its size and its running time. A file whose running time
    /// is not known matches neither.
    pub min_bitrate: Option<f64>,
    pub max_bitrate: Option<f64>,
}

impl RuleConditions {
    /// Nothing is asked of the file: such a rule would match every file.
    pub fn is_empty(&self) -> bool {
        self.libraries.is_empty()
            && self.traits.is_empty()
            && self.min_bitrate.is_none()
            && self.max_bitrate.is_none()
            && self.file == Condition::default()
    }
}

/// A rule as it is saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleBody {
    pub name: String,
    pub enabled: bool,
    /// `import` (every imported file) or `request` (the files that match when asked).
    pub when: String,
    pub conditions: RuleConditions,
    /// The processing service, by integration id, and the id of one of its flows.
    pub service: String,
    pub flow: String,
}

/// A rule as stored.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RuleRow {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    pub run_on: String,
    #[sqlx(json)]
    pub conditions: RuleConditions,
    pub service: String,
    pub flow: String,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

impl RuleRow {
    pub fn body(&self) -> RuleBody {
        RuleBody {
            name: self.name.clone(),
            enabled: self.enabled,
            when: self.run_on.clone(),
            conditions: self.conditions.clone(),
            service: self.service.clone(),
            flow: self.flow.clone(),
        }
    }
}

/// One entry of a rule's log, as stored.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LogRow {
    pub id: i64,
    pub rule_id: i64,
    pub rule_version: i32,
    pub at: DateTime<Utc>,
    pub event_id: Option<i64>,
    pub title_id: Option<i64>,
    pub copy_id: Option<i64>,
    pub path: String,
    pub service: String,
    pub flow: String,
    pub outcome: String,
    pub detail: Option<String>,
    pub evidence: Option<String>,
    pub run_id: Option<String>,
}

/// An entry to write.
#[derive(Debug, Clone, PartialEq)]
pub struct NewLog {
    pub rule_id: i64,
    pub rule_version: i32,
    pub event_id: Option<i64>,
    pub title_id: Option<i64>,
    pub copy_id: Option<i64>,
    pub path: String,
    pub service: String,
    pub flow: String,
    pub outcome: &'static str,
    pub detail: Option<String>,
    pub evidence: Option<String>,
    pub run_id: Option<String>,
}

/// A file of the library, read for the rules.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleFile {
    pub copy: i64,
    pub title: TitleRef,
    pub library: String,
    /// The whole path. `None` when its manager names no folder.
    pub path: Option<String>,
    pub size: i64,
    pub held: ParsedRelease,
    pub ctx: EvalContext,
    /// The tags its title carries in the manager whose file it is.
    pub tags: Vec<String>,
    /// Its running time, when the manager's probe says.
    pub seconds: Option<i64>,
}

/// A running time as the managers write it: `1:43:34`, `43:34`.
fn seconds(text: &str) -> Option<i64> {
    let parts: Vec<i64> = text
        .trim()
        .split(':')
        .map(|p| p.trim().parse::<i64>().ok())
        .collect::<Option<Vec<_>>>()?;
    let total = parts.iter().fold(0, |sum, p| sum * 60 + p);
    (total > 0 && parts.len() >= 2).then_some(total)
}

impl RuleFile {
    pub fn of(row: RuleFileRow) -> RuleFile {
        let held: ParsedRelease = serde_json::from_value(row.held.clone()).unwrap_or_default();
        let ctx = held_context(
            row.size_bytes,
            &row.tags,
            row.original_language.as_deref(),
            &row.languages,
            row.media_info.as_ref(),
        );
        let seconds = row
            .media_info
            .as_ref()
            .and_then(|m| m.get("runTime"))
            .and_then(|t| t.as_str())
            .and_then(seconds);
        RuleFile {
            copy: row.id,
            path: row.full_path(),
            title: TitleRef {
                id: row.title_id,
                kind: row.kind,
                tmdb_id: row.tmdb_id,
                title: row.title,
                year: row.year,
            },
            library: row.library_id,
            size: row.size_bytes,
            held,
            ctx,
            tags: row.tags,
            seconds,
        }
    }

    /// Its bitrate in Mbit/s, when its running time is known.
    pub fn bitrate(&self) -> Option<f64> {
        let seconds = self.seconds.filter(|s| *s > 0)?;
        Some(self.size as f64 * 8.0 / seconds as f64 / 1_000_000.0)
    }
}

/// Whether a file meets a rule's conditions, and on what evidence. `None`: it does not.
pub fn matches(c: &RuleConditions, f: &RuleFile, policy: &Policy) -> Option<String> {
    let mut why: Vec<String> = Vec::new();
    if !c.libraries.is_empty() {
        if !c.libraries.iter().any(|l| l == &f.library) {
            return None;
        }
        why.push(format!("library {}", f.library));
    }
    if !c.traits.is_empty() {
        let found = policy
            .traits
            .iter()
            .filter(|t| c.traits.contains(&t.id))
            .find(|t| t.when.matches(&f.held, &f.ctx).is_some())?;
        why.push(format!("trait {}", found.label));
    }
    if c.min_bitrate.is_some() || c.max_bitrate.is_some() {
        let mbps = f.bitrate()?;
        if c.min_bitrate.is_some_and(|m| mbps < m) || c.max_bitrate.is_some_and(|m| mbps > m) {
            return None;
        }
        why.push(format!("{mbps:.1} Mbit/s"));
    }
    let said = c.file.matches(&f.held, &f.ctx)?;
    if said != "always" || why.is_empty() {
        why.push(said);
    }
    Some(why.join(", "))
}

/// What a rule does with a file it matched, before anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The rule is off: what it would have sent is logged, nothing is sent.
    Shadow,
    /// It is on and cannot send now: the reason, in a few words.
    Held(String),
    /// It is on and will not send this file: the reason.
    Skipped(String),
    Send,
}

/// What is known of a file and its service when a rule decides.
#[derive(Clone, Copy)]
pub struct Facts<'a> {
    /// The service, when it is configured.
    pub processor: Option<&'a Arc<dyn Processor>>,
    pub access: Access,
    /// The integrations are read.
    pub sync: bool,
    /// A rule sent this file before.
    pub sent_before: bool,
    /// Processing made this file: it is the result of an imported run.
    pub processed: bool,
}

/// The decision of an on rule, in order: the service must be there and the file must have a
/// path; a protected title, a file processing made and a file sent before are skipped; then
/// the access and the sync must allow the send.
pub fn decide(rule: &RuleRow, file: &RuleFile, facts: Facts<'_>) -> Decision {
    if !rule.enabled {
        return Decision::Shadow;
    }
    let Some(processor) = facts.processor else {
        return Decision::Held("Service not configured".to_string());
    };
    if file.path.is_none() {
        return Decision::Held("No file path".to_string());
    }
    if let Some(tag) = processor.protecting_tag()
        && file.tags.iter().any(|t| t.trim().eq_ignore_ascii_case(tag))
    {
        return Decision::Skipped(format!("Protected by tag {tag}"));
    }
    if facts.processed {
        return Decision::Skipped("Processing result".to_string());
    }
    if facts.sent_before {
        return Decision::Skipped("Sent before".to_string());
    }
    if facts.access != Access::Act {
        return Decision::Held("Read-only".to_string());
    }
    if !facts.sync {
        return Decision::Held("Integration sync is off".to_string());
    }
    Decision::Send
}

/// What the files of these titles that processing made are named: the results of their
/// imported runs, by title.
async fn processed_names(
    pool: &PgPool,
    files: &[&RuleFile],
) -> Result<HashSet<(i64, String)>, sqlx::Error> {
    let mut ids: Vec<i64> = files.iter().map(|f| f.title.id).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(store::results_of_titles(pool, &ids, IMPORTED)
        .await?
        .into_iter()
        .map(|(title, path)| (title, file_name(&path).to_string()))
        .collect())
}

/// The rules and the files they matched, as entries to write: each decided, and the files an
/// on rule may send sent now. `sent` holds the paths sent so far, by this call and before it.
#[allow(clippy::too_many_arguments)]
async fn apply(
    pool: &PgPool,
    journal: &Journal,
    engines: &EngineSet,
    sync: bool,
    matched: Vec<(&RuleRow, &RuleFile, String, Option<i64>)>,
    sent: &mut HashSet<String>,
) -> Result<Vec<NewLog>, String> {
    let files: Vec<&RuleFile> = matched.iter().map(|(_, f, ..)| *f).collect();
    let processed = processed_names(pool, &files)
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(matched.len());
    for (rule, file, evidence, event) in matched {
        let processor = engines.processor(&rule.service);
        let path = file.path.clone().unwrap_or_default();
        let facts = Facts {
            processor,
            access: engines.access.of(&rule.service).access,
            sync,
            sent_before: sent.contains(&path),
            processed: file
                .path
                .as_deref()
                .is_some_and(|p| processed.contains(&(file.title.id, file_name(p).to_string()))),
        };
        let mut entry = NewLog {
            rule_id: rule.id,
            rule_version: rule.version,
            event_id: event,
            title_id: Some(file.title.id),
            copy_id: Some(file.copy),
            path: path.clone(),
            service: rule.service.clone(),
            flow: rule.flow.clone(),
            outcome: "shadow",
            detail: None,
            evidence: Some(evidence),
            run_id: None,
        };
        match decide(rule, file, facts) {
            Decision::Shadow => {}
            Decision::Held(why) => {
                entry.outcome = "held";
                entry.detail = Some(why);
            }
            Decision::Skipped(why) => {
                entry.outcome = "skipped";
                entry.detail = Some(why);
            }
            Decision::Send => {
                let Some(processor) = processor else {
                    continue;
                };
                let actor = format!("rule:{}", rule.id);
                let what = Sending {
                    actor: &actor,
                    title: &file.title,
                    path: &path,
                    flow: &rule.flow,
                };
                match send(pool, journal, processor, what).await {
                    Ok(SendOutcome::Taken(handed)) => {
                        let handed = *handed;
                        entry.outcome = "sent";
                        entry.run_id = handed.run.map(|r| r.id).or_else(|| {
                            handed
                                .job
                                .map(|job| super::model::run_id(processor.id(), &job))
                        });
                        sent.insert(path.clone());
                    }
                    Ok(SendOutcome::NotOffered) => {
                        entry.outcome = "held";
                        entry.detail = Some("Sending not supported".to_string());
                    }
                    Ok(SendOutcome::Unanswered) => {
                        // It may still stage the file: never sent a second time.
                        entry.outcome = "failed";
                        entry.detail = Some("No answer in time".to_string());
                        sent.insert(path.clone());
                    }
                    Ok(SendOutcome::Refused { reason, detail, .. }) => {
                        entry.outcome = "refused";
                        entry.detail = detail.or(reason);
                    }
                    Err(e) => {
                        entry.outcome = "failed";
                        entry.detail = Some(e.to_string());
                    }
                }
            }
        }
        out.push(entry);
    }
    Ok(out)
}

/// What one import pass read and did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ImportPassed {
    /// Import events read.
    pub imports: usize,
    /// Entries written: a rule and a file it matched.
    pub logged: usize,
    pub sent: usize,
}

/// One pass over the imports since the last: every import rule, on or off, against every file
/// imported, each match decided and logged. A first pass only marks where the journal stands:
/// the rules start with the imports that come after them.
pub async fn import_pass(
    pool: &PgPool,
    journal: &Journal,
    engines: &EngineSet,
    policy: &Policy,
    sync: bool,
) -> Result<ImportPassed, String> {
    let db = |e: sqlx::Error| e.to_string();
    let Some(after) = store::rules_cursor(pool).await.map_err(db)? else {
        let latest = core_store::latest_event_id(pool).await.map_err(db)?;
        store::set_rules_cursor(pool, latest).await.map_err(db)?;
        return Ok(ImportPassed::default());
    };
    let events = core_store::events_after(pool, after, EVENTS_AT_ONCE)
        .await
        .map_err(db)?;
    let Some(last) = events.last().map(|e| e.id) else {
        return Ok(ImportPassed::default());
    };
    let imports: Vec<(i64, i64)> = events
        .iter()
        .filter(|e| IMPORT_KINDS.contains(&e.kind.as_str()) && e.subject_type == "copy")
        .filter_map(|e| Some((e.id, e.subject_id.parse::<i64>().ok()?)))
        .collect();
    let rules: Vec<RuleRow> = store::rules(pool)
        .await
        .map_err(db)?
        .into_iter()
        .filter(|r| r.run_on == ON_IMPORT)
        .collect();
    let mut passed = ImportPassed {
        imports: imports.len(),
        ..ImportPassed::default()
    };
    let mut entries = Vec::new();
    if !imports.is_empty() && !rules.is_empty() {
        let ids: Vec<i64> = imports.iter().map(|(_, copy)| *copy).collect();
        let files: HashMap<i64, RuleFile> = catalog_store::rule_files(pool, Some(ids.as_slice()))
            .await
            .map_err(db)?
            .into_iter()
            .map(|row| (row.id, RuleFile::of(row)))
            .collect();
        let mut matched = Vec::new();
        for (event, copy) in &imports {
            let Some(file) = files.get(copy) else {
                continue;
            };
            for rule in &rules {
                if let Some(evidence) = matches(&rule.conditions, file, policy) {
                    matched.push((rule, file, evidence, Some(*event)));
                }
            }
        }
        let paths: Vec<String> = matched
            .iter()
            .filter_map(|(_, f, ..)| f.path.clone())
            .collect();
        let mut sent: HashSet<String> = store::sent_paths(pool, &paths)
            .await
            .map_err(db)?
            .into_iter()
            .collect();
        entries = apply(pool, journal, engines, sync, matched, &mut sent).await?;
    }
    passed.logged = entries.len();
    passed.sent = entries.iter().filter(|e| e.outcome == "sent").count();
    let mut tx = pool.begin().await.map_err(db)?;
    store::insert_log(&mut tx, &entries).await.map_err(db)?;
    store::move_rules_cursor(&mut tx, last).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(passed)
}

/// The files of the library a rule's conditions match now, with the evidence of each, in the
/// catalog's order.
pub async fn matching(
    pool: &PgPool,
    conditions: &RuleConditions,
    policy: &Policy,
) -> Result<Vec<(RuleFile, String)>, sqlx::Error> {
    Ok(catalog_store::rule_files(pool, None)
        .await?
        .into_iter()
        .map(RuleFile::of)
        .filter_map(|f| {
            let why = matches(conditions, &f, policy)?;
            Some((f, why))
        })
        .collect())
}

/// What a run on request did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestRun {
    /// Files the rule matches now.
    pub matched: usize,
    /// Of those, the ones taken in this run (at most [`REQUEST_MAX`]): each is an entry.
    pub logged: usize,
    pub sent: usize,
    /// Matching files not sent before that this run did not take.
    pub left: usize,
}

/// Run a rule now for the files it matches: the ones not sent before, at most [`REQUEST_MAX`],
/// each decided and logged like an import. The caller checks that the rule is on.
pub async fn request_run(
    pool: &PgPool,
    journal: &Journal,
    engines: &EngineSet,
    policy: &Policy,
    rule: &RuleRow,
    sync: bool,
) -> Result<RequestRun, String> {
    let db = |e: sqlx::Error| e.to_string();
    let found = matching(pool, &rule.conditions, policy).await.map_err(db)?;
    let paths: Vec<String> = found.iter().filter_map(|(f, _)| f.path.clone()).collect();
    let mut sent: HashSet<String> = store::sent_paths(pool, &paths)
        .await
        .map_err(db)?
        .into_iter()
        .collect();
    let fresh: Vec<&(RuleFile, String)> = found
        .iter()
        .filter(|(f, _)| f.path.as_ref().is_none_or(|p| !sent.contains(p)))
        .collect();
    let taken: Vec<(&RuleRow, &RuleFile, String, Option<i64>)> = fresh
        .iter()
        .take(REQUEST_MAX)
        .map(|(f, why)| (rule, f, why.clone(), None))
        .collect();
    let entries = apply(pool, journal, engines, sync, taken, &mut sent).await?;
    let mut tx = pool.begin().await.map_err(db)?;
    store::insert_log(&mut tx, &entries).await.map_err(db)?;
    tx.commit().await.map_err(db)?;
    Ok(RequestRun {
        matched: found.len(),
        logged: entries.len(),
        sent: entries.iter().filter(|e| e.outcome == "sent").count(),
        left: fresh.len().saturating_sub(REQUEST_MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use singularr_core::release::{AudioCodec, Resolution, VideoCodec};

    fn file(
        name: &str,
        size: i64,
        library: &str,
        tags: &[&str],
        run_time: Option<&str>,
    ) -> RuleFile {
        let media = run_time.map(|t| serde_json::json!({"runTime": t}));
        RuleFile::of(RuleFileRow {
            id: 1,
            title_id: 7,
            title: "Title".to_string(),
            kind: "movie".to_string(),
            tmdb_id: Some(603),
            year: Some(1999),
            library_id: library.to_string(),
            folder: Some("/movies/Title (1999)".to_string()),
            relative_path: name.to_string(),
            size_bytes: size,
            held: serde_json::to_value(singularr_core::release::parse(name)).unwrap(),
            media_info: media,
            languages: Vec::new(),
            original_language: None,
            tags: tags.iter().map(|t| t.to_string()).collect(),
        })
    }

    const H264: &str = "Movie (1999) [Bluray-1080p][DTS-HD MA 5.1][x264]-GRP.mkv";
    const HEVC: &str = "Movie (1999) [WEBDL-2160p][DV HDR10][EAC3 Atmos 5.1][HEVC]-FLUX.mkv";

    #[test]
    fn a_condition_set_is_held_against_known_files() {
        let policy = crate::policy::ActivePolicy::builtin().policy;
        let plain = file(H264, 20_000_000_000, "movies", &[], Some("2:00:00"));
        let uhd = file(HEVC, 9_000_000_000, "movies-4k", &["keep"], None);
        assert_eq!(
            plain.path.as_deref(),
            Some(format!("/movies/Title (1999)/{H264}").as_str())
        );
        // A codec and a resolution, in the policy's own words.
        let to_hevc = RuleConditions {
            file: Condition {
                video_codec: vec![VideoCodec::H264],
                resolution: vec![Resolution::R1080],
                ..Condition::default()
            },
            ..RuleConditions::default()
        };
        assert!(matches(&to_hevc, &plain, &policy).is_some());
        assert_eq!(matches(&to_hevc, &uhd, &policy), None);
        // Audio and a library; a title tag; dynamic range.
        let atmos = RuleConditions {
            file: Condition {
                audio_codec: vec![AudioCodec::Eac3],
                atmos: Some(true),
                ..Condition::default()
            },
            libraries: vec!["movies-4k".to_string()],
            ..RuleConditions::default()
        };
        let why = matches(&atmos, &uhd, &policy).unwrap();
        assert!(why.starts_with("library movies-4k"), "{why}");
        assert_eq!(matches(&atmos, &plain, &policy), None);
        let mut elsewhere = atmos.clone();
        elsewhere.libraries = vec!["movies".to_string()];
        assert_eq!(matches(&elsewhere, &uhd, &policy), None);
        let tagged = RuleConditions {
            file: serde_json::from_value(serde_json::json!({"title_tag": ["keep"], "sdr": false}))
                .unwrap(),
            ..RuleConditions::default()
        };
        assert!(matches(&tagged, &uhd, &policy).is_some());
        assert_eq!(matches(&tagged, &plain, &policy), None);
        // A release group, or a list of them; size.
        let groups = RuleConditions {
            file: serde_json::from_value(
                serde_json::json!({"group": ["flux", "NTb"], "min_size_gb": 5.0}),
            )
            .unwrap(),
            ..RuleConditions::default()
        };
        assert!(matches(&groups, &uhd, &policy).is_some());
        assert_eq!(matches(&groups, &plain, &policy), None);
        // Bitrate: 20 GB over two hours is about 22.2 Mbit/s; without a running time it is
        // not known, and nothing that asks for it matches.
        let heavy = RuleConditions {
            min_bitrate: Some(20.0),
            ..RuleConditions::default()
        };
        assert_eq!(
            matches(&heavy, &plain, &policy).as_deref(),
            Some("22.2 Mbit/s")
        );
        assert_eq!(matches(&heavy, &uhd, &policy), None);
        let light = RuleConditions {
            max_bitrate: Some(10.0),
            ..RuleConditions::default()
        };
        assert_eq!(matches(&light, &plain, &policy), None);
        // A trait of the policy, by its id: a named condition.
        let first = policy.traits.first().unwrap();
        let by_trait = RuleConditions {
            traits: vec![first.id.clone()],
            ..RuleConditions::default()
        };
        let carriers = [&plain, &uhd]
            .into_iter()
            .filter(|f| first.when.matches(&f.held, &f.ctx).is_some())
            .count();
        let matched = [&plain, &uhd]
            .into_iter()
            .filter(|f| matches(&by_trait, f, &policy).is_some())
            .count();
        assert_eq!(matched, carriers);
        // Nothing asked matches everything, and such a rule is refused when it is saved.
        assert!(RuleConditions::default().is_empty() && !to_hevc.is_empty());
        assert_eq!(
            matches(&RuleConditions::default(), &plain, &policy).as_deref(),
            Some("always")
        );
        // Running times as the managers write them.
        assert_eq!(seconds("1:43:34"), Some(6214));
        assert_eq!(seconds("43:34"), Some(2614));
        assert_eq!(seconds("soon"), None);
        assert_eq!(seconds("0:00"), None);
        // The language is strict: a field it does not know is refused.
        assert!(
            serde_json::from_value::<RuleConditions>(
                serde_json::json!({"file": {"codec": ["H264"]}})
            )
            .is_err()
        );
    }

    #[test]
    fn an_on_rule_sends_only_what_it_may() {
        let rule = |enabled: bool| RuleRow {
            id: 1,
            name: "Convert".to_string(),
            enabled,
            run_on: ON_IMPORT.to_string(),
            conditions: RuleConditions::default(),
            service: "bridge".to_string(),
            flow: "hevc".to_string(),
            version: 1,
            created_at: Utc::now(),
            created_by: "user:owner".to_string(),
            updated_at: Utc::now(),
            updated_by: "user:owner".to_string(),
        };
        let plain = file(H264, 1, "movies", &[], None);
        let facts = Facts {
            processor: None,
            access: Access::Read,
            sync: true,
            sent_before: false,
            processed: false,
        };
        // Off: shadow, whatever else holds.
        assert_eq!(decide(&rule(false), &plain, facts), Decision::Shadow);
        // On, without its service.
        assert_eq!(
            decide(&rule(true), &plain, facts),
            Decision::Held("Service not configured".to_string())
        );
    }
}
