//! `GET /api/v1/journal`: the journal as the Journal page draws it (`web/data/journal.json`).
//! Every event comes with who or what made it (`actor` as `{kind, id, name}`), what it is about
//! (`subject` as `{type, id, label}`, a title by the id the pages link it with), and one line in
//! plain words written from its body. `routes::router` merges the route with one line.
//!
//! The kinds are the page's words, `<subject>.<verb>` (`request.created`, `file.upgraded`); the
//! stored kind is `code`. `GET /api/v1/events` and the event stream keep the stored kinds.

use super::routes::ApiQuery;
use super::state::AppState;
use crate::catalog::store::{self as catalog_store, TitleRef};
use crate::core::error::AppError;
use crate::core::model::Event;
use crate::core::schema::ProblemDoc;
use crate::core::store as core_store;
use crate::core::units;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use utoipa::OpenApi;

/// Events a page holds at most.
const MAX_LIMIT: u32 = 500;

/// Filters of `GET /api/v1/journal`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct JournalQuery {
    /// Events on the page, 1 to 500. Default 100.
    pub limit: Option<u32>,
    /// Only events older than this one (`e-123`, or `123`): the `next` of the page before.
    pub before: Option<String>,
    /// Kinds, separated by commas: the page's (`request.created`) or the stored ones
    /// (`request_added`).
    pub kind: Option<String>,
    /// Who made it, separated by commas: as the answer's actor writes it (`integration:radarr`,
    /// `user:owner@example.org`, `key:dashboard`, `system:policy`) or as stored
    /// (`sync:radarr`).
    pub actor: Option<String>,
    /// Words in the summary's facts: titles, names, files, kinds.
    pub q: Option<String>,
}

/// Who or what made an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JournalActor {
    /// `user` (a person: the owner, or a requester), `key` (a program), `integration` (what
    /// an integration reported), `system` (Singularr itself: the policy, a folder scan, the
    /// configuration, the portal).
    pub kind: String,
    pub id: String,
    pub name: String,
}

/// What an event is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JournalSubject {
    /// `title`, `download`, `integration`, `policy`, `key`, `request`, `album`, `tape`,
    /// `setting`.
    #[serde(rename = "type")]
    pub kind: String,
    /// A title's id is the pages' own: `movie-603`, `show-1396`.
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JournalEvent {
    /// `e-<seq>`.
    pub id: String,
    /// The journal's own number: a larger one was committed later.
    pub seq: i64,
    pub at: DateTime<Utc>,
    /// The page's word for it: `request.created`, `file.upgraded`, `integration.down`.
    pub kind: String,
    /// The stored kind: `request_added`, `copy_replaced`, `engine_down`.
    pub code: String,
    pub actor: JournalActor,
    pub subject: JournalSubject,
    /// What happened, in one line.
    pub summary: String,
    /// The event that led to this one: `e-<seq>`.
    pub cause: Option<String>,
    #[schema(value_type = Object)]
    pub body: Value,
}

/// One page of the journal, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JournalPage {
    pub events: Vec<JournalEvent>,
    /// The `before` of the next, older page; absent on the last page.
    pub next: Option<String>,
    /// The newest event's `seq`, for the stream (`/api/v1/events/stream?after=`).
    pub latest: i64,
}

/// What the summaries name: integrations, titles, the portal.
#[derive(Debug, Clone, Default)]
pub struct Names {
    /// Integration id to its name.
    pub integrations: BTreeMap<String, String>,
    pub titles: BTreeMap<i64, TitleRef>,
    /// The portal's name.
    pub portal: String,
}

impl Names {
    fn integration(&self, id: &str) -> String {
        self.integrations
            .get(id)
            .cloned()
            .unwrap_or_else(|| id.to_string())
    }
}

// Reading a body ---------------------------------------------------------------------------------

fn text<'a>(b: &'a Value, key: &str) -> Option<&'a str> {
    b.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn int(b: &Value, key: &str) -> Option<i64> {
    b.get(key).and_then(Value::as_i64)
}

fn flag(b: &Value, key: &str) -> bool {
    b.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn list(b: &Value, key: &str) -> Vec<String> {
    b.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s.clone()),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ints(b: &Value, key: &str) -> Vec<i32> {
    b.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_i64().and_then(|n| i32::try_from(n).ok()))
                .collect()
        })
        .unwrap_or_default()
}

/// `a, b and c`.
fn and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The last part of a path: the file's name.
fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// A file in words: `Bluray-1080p by QxR, 4.20 GB, score 410`.
fn file_words(c: &Value) -> String {
    let quality = text(c, "quality").or_else(|| text(c, "arr_quality"));
    let mut parts = Vec::new();
    match (quality, text(c, "group")) {
        (Some(q), Some(g)) => parts.push(format!("{q} by {g}")),
        (Some(q), None) => parts.push(q.to_string()),
        (None, Some(g)) => parts.push(format!("by {g}")),
        (None, None) => {
            if let Some(name) = text(c, "release_name") {
                parts.push(name.to_string());
            }
        }
    }
    if let Some(edition) = text(c, "edition") {
        parts.push(edition.to_string());
    }
    if let Some(bytes) = int(c, "size_bytes").filter(|b| *b > 0) {
        parts.push(units::size(bytes));
    }
    if let Some(score) = int(c, "score") {
        parts.push(format!("score {score}"));
    }
    if parts.is_empty() {
        "a file".to_string()
    } else {
        parts.join(", ")
    }
}

/// `S05E10`, `S01E01-E02`, `season 3`.
fn episode_part(b: &Value) -> Option<String> {
    let season = int(b, "season")?;
    let eps = ints(b, "episodes");
    Some(match eps.as_slice() {
        [] => format!("season {season}"),
        [one] => format!("S{season:02}E{one:02}"),
        [first, .., last] => format!("S{season:02}E{first:02}-E{last:02}"),
    })
}

/// A setting's value in words.
fn value_words(v: &Value) -> String {
    match v {
        Value::Null => "none".to_string(),
        Value::String(s) if s.is_empty() => "none".to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(true) => "on".to_string(),
        Value::Bool(false) => "off".to_string(),
        Value::Array(items) if items.is_empty() => "none".to_string(),
        Value::Array(items) => items.iter().map(value_words).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

/// `title_added` as words: `Title added`.
fn humanize(kind: &str) -> String {
    let words = kind.replace(['_', '.'], " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => "Event".to_string(),
    }
}

// Kinds --------------------------------------------------------------------------------------------

/// Every stored kind and the page's word for it. Two stored kinds have more than one: a
/// replaced file is `file.upgraded` when its score rose, and a request's change of status is
/// `request.<its new status>`.
pub const KINDS: &[(&str, &str)] = &[
    ("title_added", "title.added"),
    ("title_removed", "title.removed"),
    ("copy_added", "file.added"),
    ("copy_replaced", "file.replaced"),
    ("copy_replaced", "file.upgraded"),
    ("copy_removed", "file.removed"),
    ("copy_changed", "file.changed"),
    ("score_changed", "file.rescored"),
    ("untracked_file_found", "file.found"),
    ("untracked_file_gone", "file.gone"),
    ("download_added", "download.added"),
    ("download_completed", "download.completed"),
    ("download_stuck", "download.attention"),
    ("download_orphaned", "download.orphaned"),
    ("download_removed", "download.removed"),
    ("download_import_forced", "download.forced"),
    ("download_queue_removed", "download.dropped"),
    ("search_sent", "release.searched"),
    ("release_grabbed", "release.grabbed"),
    ("engine_down", "integration.down"),
    ("engine_up", "integration.up"),
    ("integration_access_changed", "integration.access"),
    ("integrations_pruned", "integration.pruned"),
    ("processing_sent", "processing.sent"),
    ("processing_finished", "processing.finished"),
    ("processing_failed", "processing.failed"),
    ("processing_stuck", "processing.stuck"),
    ("notification_channel_added", "notification.added"),
    ("notification_channel_changed", "notification.changed"),
    ("notification_channel_removed", "notification.removed"),
    ("processing_worker_changed", "processing.worker"),
    ("processing_rule_saved", "rule.saved"),
    ("processing_rule_removed", "rule.removed"),
    ("policy_loaded", "policy.changed"),
    ("policy_model_saved", "policy.saved"),
    ("key_created", "key.created"),
    ("key_revoked", "key.revoked"),
    ("request_added", "request.created"),
    ("request_status_changed", "request.approved"),
    ("request_status_changed", "request.processing"),
    ("request_status_changed", "request.available"),
    ("request_status_changed", "request.declined"),
    ("request_status_changed", "request.failed"),
    ("request_status_changed", "request.removed"),
    ("request_status_changed", "request.pending"),
    ("request_status_changed", "request.updated"),
    ("request_removed", "request.withdrawn"),
    ("request_intent_shadowed", "request.asked"),
    ("album_added", "album.added"),
    ("album_removed", "album.removed"),
    ("tape_added", "tape.added"),
    ("tape_missing", "tape.missing"),
    ("tape_moved", "tape.moved"),
    ("tape_annotated", "tape.annotated"),
    ("setting_changed", "setting.changed"),
];

/// The page's word for an event.
fn page_kind(e: &Event) -> String {
    match e.kind.as_str() {
        "copy_replaced" if int(&e.body, "score_delta").is_some_and(|d| d > 0) => {
            "file.upgraded".to_string()
        }
        "request_status_changed" => {
            let to = text(&e.body, "to").unwrap_or("");
            match to {
                "approved" | "processing" | "available" | "declined" | "failed" | "removed"
                | "pending" => format!("request.{to}"),
                _ => "request.updated".to_string(),
            }
        }
        stored => KINDS
            .iter()
            .find(|(k, _)| *k == stored)
            .map(|(_, word)| word.to_string())
            .unwrap_or_else(|| stored.replacen('_', ".", 1)),
    }
}

// Actors and subjects ----------------------------------------------------------------------------

/// Who made an event. A request on Seerr was made by its requester; what an integration
/// reported, by that integration.
fn actor(e: &Event, names: &Names) -> JournalActor {
    let person = |name: &str| JournalActor {
        kind: "user".to_string(),
        id: format!("requester:{name}"),
        name: name.to_string(),
    };
    if matches!(e.kind.as_str(), "request_added" | "request_intent_shadowed")
        && let Some(by) = text(&e.body, "requested_by")
    {
        return person(by);
    }
    let system = |id: &str, name: &str| JournalActor {
        kind: "system".to_string(),
        id: id.to_string(),
        name: name.to_string(),
    };
    let stored = e.actor.as_str();
    match stored.split_once(':') {
        Some(("user", who)) => JournalActor {
            kind: "user".to_string(),
            id: who.to_string(),
            name: who.to_string(),
        },
        Some(("key", name)) => JournalActor {
            kind: "key".to_string(),
            id: name.to_string(),
            name: name.to_string(),
        },
        Some(("sync", id)) => JournalActor {
            kind: "integration".to_string(),
            id: id.to_string(),
            name: names.integration(id),
        },
        Some(("scan", "music")) => system("scan:music", "Music folder scan"),
        Some(("scan", "home_movies")) => system("scan:home_movies", "Home movies folder scan"),
        Some(("scan", "movies")) => system("scan:movies", "Movie root folder scan"),
        Some(("requests", _)) => system("portal", &names.portal),
        _ => match stored {
            "policy" => system("policy", "Policy"),
            "config" => system("config", "Configuration"),
            other => system(other, &humanize(other)),
        },
    }
}

/// The stored actors an actor of the answer stands for, for the `actor` filter.
fn stored_actors(asked: &str) -> Vec<String> {
    match asked.split_once(':') {
        Some(("integration", id)) => vec![format!("sync:{id}")],
        Some(("system", "portal")) => vec!["requests:shadow".to_string()],
        Some(("system", rest)) => vec![rest.to_string()],
        _ => vec![asked.to_string()],
    }
}

/// The id the pages give a title: `movie-603`. A title the catalog no longer holds keeps the
/// TMDB id its event named, else its catalog id.
fn title_id(id: Option<i64>, b: &Value, names: &Names) -> (String, Option<String>) {
    let known = id.and_then(|id| names.titles.get(&id));
    let kind = known
        .map(|t| t.kind.clone())
        .or_else(|| text(b, "kind").map(str::to_string));
    let tmdb = known.and_then(|t| t.tmdb_id).or_else(|| int(b, "tmdb_id"));
    let page_id = match (kind.as_deref(), tmdb) {
        (Some(kind @ ("movie" | "show")), Some(tmdb)) => format!("{kind}-{tmdb}"),
        _ => format!("title-{}", id.unwrap_or_default()),
    };
    let label = text(b, "title")
        .map(str::to_string)
        .or_else(|| known.map(|t| t.title.clone()));
    (page_id, label)
}

/// The catalog id of an event about a title itself.
fn own_title(e: &Event) -> Option<i64> {
    if e.subject_type == "title" {
        e.subject_id.parse().ok()
    } else {
        None
    }
}

fn subject(e: &Event, names: &Names) -> JournalSubject {
    let b = &e.body;
    let make = |kind: &str, id: String, label: String| JournalSubject {
        kind: kind.to_string(),
        id,
        label,
    };
    match e.subject_type.as_str() {
        "title" | "copy" => {
            let id = int(b, "title_id").or_else(|| own_title(e));
            let (page_id, label) = title_id(id, b, names);
            let mut label = label.unwrap_or_else(|| "A title".to_string());
            if let Some(ep) = episode_part(b) {
                label = format!("{label} {ep}");
            }
            make("title", page_id, label)
        }
        "download" => make(
            "download",
            e.subject_id.clone(),
            text(b, "name").unwrap_or("A download").to_string(),
        ),
        "engine" => make(
            "integration",
            e.subject_id.clone(),
            names.integration(&e.subject_id),
        ),
        "policy" => make(
            "policy",
            "policy".to_string(),
            match text(b, "version") {
                Some(v) => format!("Policy {v}"),
                None => "Policy".to_string(),
            },
        ),
        "key" => make(
            "key",
            e.subject_id.clone(),
            text(b, "name").unwrap_or("An API key").to_string(),
        ),
        "request" => make(
            "request",
            e.subject_id.clone(),
            text(b, "title").unwrap_or("A request").to_string(),
        ),
        "request_intent" => make(
            "request",
            format!("intent-{}", e.subject_id),
            text(b, "title").unwrap_or("A request").to_string(),
        ),
        "album" => make(
            "album",
            e.subject_id.clone(),
            match (text(b, "artist"), text(b, "album")) {
                (Some(artist), Some(album)) => format!("{artist}, {album}"),
                (None, Some(album)) => album.to_string(),
                _ => "An album".to_string(),
            },
        ),
        "tape" => make(
            "tape",
            e.subject_id.clone(),
            text(b, "path")
                .or_else(|| text(b, "to"))
                .map(|p| file_name(p).to_string())
                .unwrap_or_else(|| "A recording".to_string()),
        ),
        "setting" => make(
            "setting",
            e.subject_id.clone(),
            text(b, "label").unwrap_or(&e.subject_id).to_string(),
        ),
        "rule" => make(
            "rule",
            e.subject_id.clone(),
            text(b, "name").unwrap_or("A rule").to_string(),
        ),
        "config" => make(
            "integration",
            "integrations".to_string(),
            "Integrations".to_string(),
        ),
        "notification_channel" => make(
            "channel",
            e.subject_id.clone(),
            text(b, "name").unwrap_or("A channel").to_string(),
        ),
        // A run no title holds, named by its file.
        "processing_run" => make(
            "run",
            e.subject_id.clone(),
            text(b, "file").unwrap_or("A run").to_string(),
        ),
        other => make(other, e.subject_id.clone(), humanize(&e.subject_id)),
    }
}

// Summaries --------------------------------------------------------------------------------------

fn status_words(status: &str) -> &'static str {
    match status {
        "pending" => "pending",
        "approved" => "approved",
        "processing" => "downloading",
        "available" => "available",
        "removed" => "removed",
        "declined" => "declined",
        "failed" => "failed",
        _ => "unknown",
    }
}

/// What a request asked for beyond its title, which is the event's subject:
/// `seasons 1 and 2, 4K`. `None` for a whole title in HD.
fn asked_for(b: &Value) -> Option<String> {
    let mut parts = Vec::new();
    let seasons = ints(b, "seasons");
    if !seasons.is_empty() {
        parts.push(crate::requests::rules::seasons_text(&seasons));
    }
    if flag(b, "is_4k") {
        parts.push("4K".to_string());
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Text with its first letter in upper case: `Approved`.
fn capital(text: &str) -> String {
    let mut c = text.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// `Label: detail`, or the label alone when there is no detail.
fn labeled(label: &str, detail: Option<String>) -> String {
    match detail.filter(|d| !d.is_empty()) {
        Some(d) => format!("{label}: {d}"),
        None => label.to_string(),
    }
}

/// One line for an event, from its body: a short past-tense phrase in the words of
/// `docs/copy.md` (`Imported: WEBDL-1080p by GRP`, `Access changed to write`). The subject is
/// the event's own field and is not said again.
pub fn summary(e: &Event, names: &Names) -> String {
    let b = &e.body;
    let integration = |key: &str| {
        text(b, key)
            .map(|id| names.integration(id))
            .unwrap_or_else(|| "an integration".to_string())
    };
    match e.kind.as_str() {
        "title_added" => {
            if flag(b, "new_to_catalog") {
                format!("Added by {}", integration("engine"))
            } else {
                format!("Also added by {}", integration("engine"))
            }
        }
        "title_removed" => {
            if flag(b, "gone_from_catalog") {
                format!("Removed by {}", integration("engine"))
            } else {
                format!("Removed from {} only", integration("engine"))
            }
        }
        "copy_added" => {
            let mut line = format!("Imported: {}", file_words(b));
            let guards = list(b, "protections");
            if !guards.is_empty() {
                line.push_str(&format!(", protected by {}", and(&guards)));
            }
            line
        }
        "copy_replaced" => {
            let new = b.get("new").cloned().unwrap_or(Value::Null);
            let old = b.get("old").cloned().unwrap_or(Value::Null);
            let verb = if int(b, "score_delta").is_some_and(|d| d > 0) {
                "Upgraded to"
            } else {
                "Replaced with"
            };
            if old.is_null() {
                format!("{verb} {}", file_words(&new))
            } else {
                format!("{verb} {}, was {}", file_words(&new), file_words(&old))
            }
        }
        "copy_removed" => format!("File removed: {}", file_words(b)),
        "copy_changed" => {
            let what: Vec<String> = list(b, "changed")
                .iter()
                .map(|c| {
                    match c.as_str() {
                        "relative_path" => "name",
                        "size_bytes" => "size",
                        "release_name" => "release name",
                        "lane" => "quality profile",
                        "score" => "score",
                        _ => "details",
                    }
                    .to_string()
                })
                .collect();
            labeled("File changed", (!what.is_empty()).then(|| and(&what)))
        }
        "score_changed" => match (int(b, "old_score"), int(b, "new_score")) {
            (Some(old), Some(new)) => format!("Rescored from {old} to {new}"),
            (None, Some(new)) => format!("Scored {new}"),
            (Some(old), None) => format!("Unscored, was {old}"),
            (None, None) => "Rescored".to_string(),
        },
        "untracked_file_found" => {
            let file = text(b, "file_name").unwrap_or("a file");
            match text(b, "edition_label") {
                Some(ed) => format!("Untracked file found: {file} ({ed})"),
                None => format!("Untracked file found: {file}"),
            }
        }
        "untracked_file_gone" => format!(
            "Untracked file gone: {}",
            text(b, "file_name").unwrap_or("a file")
        ),
        "download_added" => match text(b, "owner") {
            Some(owner) => format!("Added for {}", names.integration(owner)),
            None => "Added".to_string(),
        },
        "download_completed" => "Completed".to_string(),
        "download_stuck" => labeled("Stuck", text(b, "reason").map(str::to_string)),
        "download_orphaned" => labeled("Unassigned", text(b, "reason").map(str::to_string)),
        "download_removed" => "Removed from download client".to_string(),
        // What the owner asked of a manager.
        "search_sent" => {
            let who = integration("manager");
            match int(b, "count") {
                Some(n) => format!(
                    "Search sent to {who}: {}",
                    units::counted(n, "title", "titles")
                ),
                None => format!("Search sent to {who}"),
            }
        }
        "release_grabbed" => {
            let who = integration("manager");
            labeled(
                &format!("Grabbed by {who}"),
                text(b, "release").map(str::to_string),
            )
        }
        "download_import_forced" => format!("Import anyway sent to {}", integration("manager")),
        "download_queue_removed" => {
            let who = integration("manager");
            if flag(b, "blocklist") {
                format!("Removed from {who}, release blocklisted")
            } else {
                format!("Removed from {who}")
            }
        }
        "engine_down" => {
            let why = text(b, "error").unwrap_or("no answer");
            if flag(b, "first_contact") {
                format!("Offline at first contact: {why}")
            } else {
                format!("Went offline: {why}")
            }
        }
        "engine_up" => match (flag(b, "first_contact"), text(b, "version")) {
            (true, Some(v)) => format!("Connected, version {v}"),
            (true, None) => "Connected".to_string(),
            (false, _) => "Back online".to_string(),
        },
        // The access to an integration, as the owner set it.
        "integration_access_changed" => {
            let to = if text(b, "to") == Some("act") {
                "write"
            } else {
                "read-only"
            };
            match text(b, "label") {
                Some(label) => format!("{label}: access changed to {to}"),
                None => format!("Access changed to {to}"),
            }
        }
        // A processing rule the owner added or saved again, or removed.
        "processing_rule_saved" => {
            let name = text(b, "name").unwrap_or("A rule");
            if flag(b, "created") {
                format!("Rule {name} added")
            } else {
                match int(b, "version") {
                    Some(v) => format!("Rule {name} saved, version {v}"),
                    None => format!("Rule {name} saved"),
                }
            }
        }
        "processing_rule_removed" => {
            format!("Rule {} removed", text(b, "name").unwrap_or("A rule"))
        }
        // A change the owner made to a worker service.
        "processing_worker_changed" => {
            let who = text(b, "name")
                .map(str::to_string)
                .unwrap_or_else(|| names.integration(&e.subject_id));
            let node = text(b, "node_name").or_else(|| text(b, "node"));
            let node = node.unwrap_or("a node");
            let library = text(b, "library_name")
                .or_else(|| text(b, "library"))
                .unwrap_or("a library");
            match text(b, "act") {
                Some("limits") => {
                    let kind = match text(b, "kind") {
                        Some("transcodeGpu") => "transcode GPU",
                        Some("transcodeCpu") => "transcode CPU",
                        Some("healthGpu") => "health check GPU",
                        Some("healthCpu") => "health check CPU",
                        _ => "worker",
                    };
                    match (int(b, "from"), int(b, "to")) {
                        (Some(from), Some(to)) => {
                            format!("Worker limit on {node}: {kind} {from} to {to}")
                        }
                        _ => format!("Worker limit on {node} changed"),
                    }
                }
                Some("pause") if flag(b, "paused") => format!("{node} paused"),
                Some("pause") => format!("{node} resumed"),
                Some("cancel") => match text(b, "file") {
                    Some(file) => format!("Worker on {node} cancelled: {file}"),
                    None => format!("Worker on {node} cancelled"),
                },
                Some("requeue") => match text(b, "file") {
                    Some(file) => format!("Requeued in {who}: {file}"),
                    None => format!("File requeued in {who}"),
                },
                Some("scan") => format!("Scan of {library} started"),
                Some("watch") if flag(b, "on") => format!("Folder watching on for {library}"),
                Some("watch") => format!("Folder watching off for {library}"),
                Some("flow") => {
                    let flow = text(b, "flow_name")
                        .or_else(|| text(b, "flow"))
                        .unwrap_or("a flow");
                    format!("Flow of {library} set to {flow}")
                }
                _ => format!("{who} changed"),
            }
        }
        // A file the owner handed to a flow of a processing service.
        "processing_sent" => {
            let flow = text(b, "flow_label")
                .or_else(|| text(b, "flow"))
                .unwrap_or("a flow");
            let line = format!("Sent to {} for {flow}", integration("service"));
            match text(b, "file") {
                Some(file) => format!("{line}: {file}"),
                None => line,
            }
        }
        // What the processing pass saw of a run: it closed, or it is stuck.
        "processing_finished" | "processing_failed" | "processing_stuck" => {
            let flow = text(b, "flow_label")
                .or_else(|| text(b, "flow"))
                .unwrap_or("A flow");
            let file = text(b, "file").unwrap_or("file");
            match e.kind.as_str() {
                "processing_finished" => match int(b, "saved").filter(|s| *s > 0) {
                    Some(saved) => format!("Finished {flow}: {file}, saved {}", units::size(saved)),
                    None => format!("Finished {flow}: {file}"),
                },
                "processing_failed" => {
                    let why = text(b, "detail").unwrap_or(file);
                    if text(b, "state") == Some("refused") {
                        format!("Refused by {}: {why}", integration("service"))
                    } else {
                        format!("Failed in {flow}: {why}")
                    }
                }
                _ => match int(b, "open_for") {
                    Some(open) => format!(
                        "Stuck in {flow} for {}: {file}",
                        units::span(chrono::Duration::seconds(open))
                    ),
                    None => format!("Stuck in {flow}: {file}"),
                },
            }
        }
        // The owner's notification channels; the subject names the channel.
        "notification_channel_added" => {
            match text(b, "type").and_then(crate::notify::model::ChannelType::parse) {
                Some(kind) => format!("Created: {}", kind.label()),
                None => "Created".to_string(),
            }
        }
        "notification_channel_changed" => {
            let what: Vec<String> = list(b, "changed")
                .iter()
                .map(|c| match c.as_str() {
                    "enabled" if flag(b, "enabled") => "enabled".to_string(),
                    "enabled" => "disabled".to_string(),
                    other => other
                        .split_once('.')
                        .map(|(_, field)| field.to_string())
                        .unwrap_or_else(|| other.to_string()),
                })
                .collect();
            if what.is_empty() {
                "Changed".to_string()
            } else {
                format!("Changed: {}", and(&what))
            }
        }
        "notification_channel_removed" => "Deleted".to_string(),
        "integrations_pruned" => {
            let gone = list(&b["catalog"], "engines");
            let copies = int(&b["catalog"], "copies").unwrap_or(0);
            let titles = int(&b["catalog"], "titles").unwrap_or(0);
            let set_aside = int(b, "downloads_set_aside").unwrap_or(0);
            if gone.is_empty() && set_aside > 0 {
                format!(
                    "Set aside: {}",
                    units::counted(set_aside, "download", "downloads")
                )
            } else if gone.is_empty() {
                "Data of unlisted integrations removed".to_string()
            } else {
                format!(
                    "Data of {} removed: {}, {}",
                    and(&gone),
                    units::counted(copies, "file", "files"),
                    units::counted(titles, "title", "titles")
                )
            }
        }
        "policy_loaded" => {
            let name = text(b, "name").unwrap_or("policy");
            format!(
                "Loaded {name}: {}, {}, {}",
                units::counted(int(b, "traits").unwrap_or(0), "trait", "traits"),
                units::counted(
                    int(b, "lanes").unwrap_or(0),
                    "quality profile",
                    "quality profiles"
                ),
                units::counted(
                    int(b, "protections").unwrap_or(0),
                    "protection",
                    "protections"
                )
            )
        }
        "policy_model_saved" => {
            let subject = text(b, "subject").unwrap_or("");
            let (what, id) = subject.split_once(':').unwrap_or((subject, ""));
            let version = int(b, "version").unwrap_or(0);
            match text(b, "action").unwrap_or("") {
                "seeded" => format!("Version {version} started from the policy file"),
                "imported" => format!("Version {version}: policy file imported"),
                "restored" => format!("Version {version}: version {id} restored"),
                action => {
                    let noun = match what {
                        "profile" => "Quality profile",
                        "list" => "List",
                        "naming" => "Naming",
                        _ => "Profiles and lists",
                    };
                    let verb = if action.ends_with("_deleted") {
                        "deleted"
                    } else {
                        "saved"
                    };
                    format!("Version {version}: {noun} {id} {verb}")
                }
            }
        }
        "key_created" => {
            let scopes = list(b, "scopes");
            if scopes.is_empty() {
                "Created".to_string()
            } else {
                format!("Created with scopes {}", and(&scopes))
            }
        }
        "key_revoked" => "Revoked".to_string(),
        "request_added" => labeled(
            &format!(
                "Requested by {}",
                text(b, "requested_by").unwrap_or("someone")
            ),
            asked_for(b),
        ),
        "request_status_changed" => {
            let to = text(b, "to").unwrap_or("");
            let mut line = capital(status_words(to));
            if let Some(from) = text(b, "from").filter(|f| *f != to) {
                line.push_str(&format!(", was {}", status_words(from)));
            }
            line
        }
        "request_removed" => "Removed from Seerr".to_string(),
        "request_intent_shadowed" => {
            let gate = match b.get("gate").and_then(Value::as_str) {
                Some("approve") => ", within the season limit",
                Some("trim") => ", trimmed by the season limit",
                Some("decline") => ", declined by the season limit",
                Some("exempt") => ", exempt from the season limit",
                _ => "",
            };
            let what = asked_for(b).map(|w| format!(", {w}")).unwrap_or_default();
            format!(
                "Recorded, not sent: {} on {}{what}{gate}",
                text(b, "requested_by").unwrap_or("someone"),
                names.portal
            )
        }
        "album_added" => {
            let mut parts = Vec::new();
            if let Some(year) = int(b, "year") {
                parts.push(year.to_string());
            }
            if let Some(n) = int(b, "tracks") {
                parts.push(units::counted(n, "track", "tracks"));
            }
            labeled("Added", (!parts.is_empty()).then(|| parts.join(", ")))
        }
        "album_removed" => "Removed from folder".to_string(),
        "tape_added" => {
            if flag(b, "returned") {
                "Back in folder".to_string()
            } else {
                "Added".to_string()
            }
        }
        "tape_missing" => "Missing from folder".to_string(),
        "tape_moved" => format!(
            "Moved or renamed from {}",
            text(b, "from").map(file_name).unwrap_or("another file")
        ),
        "tape_annotated" => {
            let fields = list(b, "fields");
            let what = (!fields.is_empty()).then(|| and(&fields).replace('_', " "));
            if flag(b, "first") {
                labeled("Annotated", what)
            } else {
                labeled("Annotation changed", what)
            }
        }
        "setting_changed" => {
            let own = text(b, "scope") == Some("user");
            let to = value_words(&b.get("to").cloned().unwrap_or(Value::Null));
            let from = value_words(&b.get("from").cloned().unwrap_or(Value::Null));
            match (own, flag(b, "reset")) {
                (false, true) => format!("Reset to default: {to}"),
                (true, true) => format!("Personal value reset: {to}"),
                (false, false) => format!("Changed from {from} to {to}"),
                (true, false) => format!("Personal value changed from {from} to {to}"),
            }
        }
        other => humanize(other),
    }
}

/// An event as the Journal page reads it.
pub fn journal_event(e: &Event, names: &Names) -> JournalEvent {
    JournalEvent {
        id: format!("e-{}", e.id),
        seq: e.id,
        at: e.at,
        kind: page_kind(e),
        code: e.kind.clone(),
        actor: actor(e, names),
        subject: subject(e, names),
        summary: summary(e, names),
        cause: e.cause.map(|c| format!("e-{c}")),
        body: e.body.clone(),
    }
}

/// `e-123` or `123`.
fn seq(text: &str) -> Option<i64> {
    let t = text.trim();
    t.strip_prefix("e-")
        .unwrap_or(t)
        .parse::<i64>()
        .ok()
        .filter(|n| *n > 0)
}

/// The journal, newest first, as the Journal page draws it: each event with its actor, its
/// subject and a line in plain words. Paged with `before` (the answer's `next`); filtered by
/// `kind`, `actor` and words (`q`).
#[utoipa::path(get, path = "/api/v1/journal", tag = "journal", params(JournalQuery),
    responses((status = 200, body = JournalPage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn journal(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<JournalQuery>,
) -> Result<Json<JournalPage>, AppError> {
    let limit = q.limit.unwrap_or(100).clamp(1, MAX_LIMIT);
    let before = match q.before.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(b) => Some(seq(b).ok_or_else(|| {
            AppError::bad_request(format!("`before` is an event id such as e-123, not `{b}`."))
        })?),
        None => None,
    };
    let asked: Vec<String> = q
        .kind
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    let words: BTreeSet<&str> = asked
        .iter()
        .filter(|k| k.contains('.'))
        .map(String::as_str)
        .collect();
    let mut kinds: BTreeSet<String> = asked.iter().filter(|k| !k.contains('.')).cloned().collect();
    for word in &words {
        let stored: Vec<&str> = KINDS
            .iter()
            .filter(|(_, w)| w == word)
            .map(|(k, _)| *k)
            .collect();
        if stored.is_empty() {
            return Err(AppError::bad_request(format!(
                "`{word}` is not a kind of the journal."
            )));
        }
        kinds.extend(stored.into_iter().map(str::to_string));
    }
    let actors: Vec<String> = q
        .actor
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .flat_map(stored_actors)
        .collect();
    let kinds: Vec<String> = kinds.into_iter().collect();
    let rows = core_store::find_events(
        &state.pool,
        &kinds,
        &actors,
        q.q.as_deref(),
        before,
        i64::from(limit) + 1,
    )
    .await?;
    let more = rows.len() > limit as usize;
    let rows: Vec<Event> = rows.into_iter().take(limit as usize).collect();
    let next = if more {
        rows.last().map(|e| format!("e-{}", e.id))
    } else {
        None
    };

    let ids: Vec<i64> = rows
        .iter()
        .filter_map(|e| int(&e.body, "title_id").or_else(|| own_title(e)))
        .collect::<BTreeSet<i64>>()
        .into_iter()
        .collect();
    let names = Names {
        integrations: state
            .config
            .integrations
            .iter()
            .map(|i| (i.id.clone(), i.name.clone()))
            .collect(),
        titles: catalog_store::title_refs(&state.pool, &ids)
            .await?
            .into_iter()
            .map(|t| (t.id, t))
            .collect(),
        portal: state
            .settings
            .text("portal.name")
            .unwrap_or_else(|| "the portal".to_string()),
    };
    let plain: BTreeSet<&str> = asked
        .iter()
        .filter(|k| !k.contains('.'))
        .map(String::as_str)
        .collect();
    let events = rows
        .iter()
        .map(|e| journal_event(e, &names))
        // A page word that stands for part of a stored kind (`file.upgraded`) keeps only that
        // part.
        .filter(|e| {
            words.is_empty() || words.contains(e.kind.as_str()) || plain.contains(e.code.as_str())
        })
        .collect();
    Ok(Json(JournalPage {
        events,
        next,
        latest: core_store::latest_event_id(&state.pool).await?,
    }))
}

/// The journal route, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new().route("/journal", get(journal))
}

#[derive(OpenApi)]
#[openapi(paths(journal))]
struct JournalDoc;

/// The journal route's part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    JournalDoc::openapi()
}

#[cfg(test)]
mod tests;
