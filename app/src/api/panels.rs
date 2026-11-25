//! The panels of the Settings page, in the shapes of the client's samples (`web/data/
//! integrations.json`, `integrations/<id>.json`, `libraries.json`, `people.json`, `jobs.json`,
//! `system.json`): types, the integration types and groups as data, and the pure functions that
//! build the answers. The routes are in `api::config`.

use crate::catalog::model::Library;
use crate::catalog::store::LibraryHoldings;
use crate::core::config::{Config, Integration, IntegrationType};
use crate::core::jobs::{Check, Job};
use crate::core::model::Scope;
use crate::engines::access::actions_of;
use crate::engines::model::{ActionView, IntegrationView};
use crate::engines::store::EngineStateRow;
use crate::lifecycle::server::plain_video;
use crate::settings::model::FieldView;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use singularr_core::Policy;
use std::collections::BTreeMap;

// Integrations -----------------------------------------------------------------------------------

/// One integration, as the Settings page lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationPanel {
    pub id: String,
    /// `plex`, `radarr`, `sonarr`, `qbittorrent`, `prowlarr`, `tdarr`, `hush`, `seerr`, and the
    /// metadata providers `tmdb`, `tvdb`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The group of its type: `server`, `managers`, `downloads`, `indexers`, `processing`,
    /// `requests`, `metadata`.
    pub group: String,
    pub name: String,
    pub role: String,
    /// `ok`, `warn` or `down`.
    pub state: String,
    pub detail: String,
    pub version: Option<String>,
    /// Its address, without credentials.
    pub address: Option<String>,
    /// `environment` (the environment form) or `file` (the instance file).
    pub source: String,
    /// With `environment`: the variable that sets it.
    pub env: Option<String>,
    /// With `file`: the instance file.
    pub file: Option<String>,
    /// Always false in this version: integrations are configuration.
    pub editable: bool,
    /// Its address and (where its type needs one) its secret are set, so it is asked.
    pub configured: bool,
    /// The last contact: a pass of its jobs or a test.
    pub last_check: Option<DateTime<Utc>>,
    /// The last pass that succeeded.
    pub last_sync: Option<DateTime<Utc>>,
    pub latency_ms: Option<i32>,
    /// The last round trips, oldest first.
    pub latency: Vec<i32>,
    /// What its passes counted: `{"titles": 4470, "queue": 3}`.
    #[schema(value_type = Object)]
    pub counts: Value,
    /// What Singularr reads from it, in words.
    pub reads: Vec<String>,
    /// What Singularr can change in it, in words: the labels of `actions`. It changes nothing
    /// while `access` is `read`.
    pub writes: Vec<String>,
    /// Why its last call failed.
    pub error: Option<String>,
    /// The ids of the jobs that read it (`GET /api/v1/jobs`).
    pub jobs: Vec<String>,
    /// What Singularr may do with it: `read`, or `act` (read it and do what `actions` lists).
    /// The owner changes it with `PATCH /api/v1/integrations/{id}`.
    pub access: String,
    /// Where `access` comes from, in the words of a setting's source: `default`,
    /// `environment`, `file` or `saved`. The environment and the file fix it; the variable or
    /// the file is named by the field `integrations.<id>.access` of the integration's own
    /// record.
    pub access_source: String,
    /// What Singularr can do to an integration of this type beyond reading it. Empty: it can
    /// only be read.
    pub actions: Vec<ActionView>,
}

/// A group of integration types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IntegrationGroup {
    pub id: String,
    pub label: String,
    pub description: String,
}

/// A field an integration of a type has: what an instance file entry or the environment form
/// gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TypeField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub placeholder: Option<String>,
    pub help: String,
    /// `connection` or `schedule`.
    pub part: String,
}

/// A type of integration this version knows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IntegrationKind {
    #[serde(rename = "type")]
    pub kind: String,
    pub label: String,
    pub group: String,
    pub description: String,
    /// How many of the type this version reads; absent for no limit.
    pub max: Option<u32>,
    /// This version reads it. A type it only lists says false.
    pub available: bool,
    pub reads: Vec<String>,
    /// What Singularr can change in an integration of the type once its access is `act`, in
    /// words.
    pub writes: Vec<String>,
    pub fields: Vec<TypeField>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IntegrationPanels {
    /// Always false in this version: integrations are added in the environment or the instance
    /// file.
    pub editable: bool,
    /// The instance file, when one is read.
    pub file: Option<String>,
    pub groups: Vec<IntegrationGroup>,
    pub types: Vec<IntegrationKind>,
    pub integrations: Vec<IntegrationPanel>,
}

/// One check of an integration: a pass of one of its jobs, a test, or a change between up and
/// down from the journal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CheckEntry {
    pub at: DateTime<Utc>,
    /// `ok` or `down`.
    pub state: String,
    pub detail: String,
    /// What was asked: a job's pass (`titles`), `test`, or `journal`.
    pub via: String,
    pub ms: Option<i64>,
}

/// `GET /api/v1/integrations/{id}`: the integration, its fields and its recent checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IntegrationDetail {
    #[serde(flatten)]
    pub integration: IntegrationPanel,
    /// Its configuration as settings fields (`integrations.<id>.address`, the secret only as
    /// set or not, how often it is read), each with its source and its `part`.
    pub settings: Vec<FieldView>,
    /// Recent checks, newest first.
    pub log: Vec<CheckEntry>,
}

/// `PATCH /api/v1/integrations/{id}`: what Singularr may do with the integration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IntegrationPatch {
    /// `read`: Singularr reads it and changes nothing in it. `act`: it may also do what the
    /// integration's `actions` list.
    pub access: String,
}

/// `POST /api/v1/integrations/{id}/test`: one read-only call now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IntegrationTest {
    pub ok: bool,
    /// How long it took.
    pub ms: i64,
    pub version: Option<String>,
    pub detail: String,
}

/// A run asked for: the jobs woken.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Started {
    pub started: bool,
    pub jobs: Vec<String>,
}

/// The groups of integration types, in the page's order.
pub fn groups() -> Vec<IntegrationGroup> {
    [
        ("server", "Media server", "Libraries, sessions and music"),
        ("managers", "Managers", "Movie and show managers"),
        ("downloads", "Download clients", "Release downloads"),
        ("indexers", "Indexers", "Release search"),
        ("processing", "Processing", "Flows for imported files"),
        (
            "requests",
            "Request sources",
            "Requests made outside the portal",
        ),
        (
            "metadata",
            "Metadata providers",
            "Titles, people, artwork, ratings and episode order",
        ),
    ]
    .into_iter()
    .map(|(id, label, description)| IntegrationGroup {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
    })
    .collect()
}

/// The group of an integration type.
pub fn group_of(kind: IntegrationType) -> &'static str {
    match kind {
        IntegrationType::Plex => "server",
        IntegrationType::Radarr | IntegrationType::Sonarr => "managers",
        IntegrationType::Qbittorrent => "downloads",
        IntegrationType::Prowlarr => "indexers",
        IntegrationType::Tdarr | IntegrationType::Hush => "processing",
        IntegrationType::Seerr => "requests",
        IntegrationType::Tautulli => "server",
    }
}

/// What Singularr reads from an integration of this type: what its adapter asks for.
pub fn reads(kind: IntegrationType) -> Vec<String> {
    let words: &[&str] = match kind {
        IntegrationType::Radarr => &[
            "Movies, files and custom formats",
            "Quality profiles and tags",
            "Queue",
            "Grab history",
            "Release name parsing",
        ],
        IntegrationType::Sonarr => &[
            "Series, seasons and episode files",
            "Quality profiles and tags",
            "Queue",
            "Grab history",
            "Release name parsing",
        ],
        IntegrationType::Qbittorrent => &["Torrent state, progress and folder"],
        IntegrationType::Seerr => &[
            "Requests and status",
            "Requester display names",
            "Title lookups",
            "Discover lists",
        ],
        IntegrationType::Plex => &[
            "Libraries and folders",
            "Movies and shows with ratings, plays and editions",
            "Artists, albums and tracks",
            "Now playing",
            "Artwork",
        ],
        IntegrationType::Tdarr => &[
            "Version",
            "Nodes, worker limits and queue lengths",
            "Active workers",
            "Libraries and flows",
            "Statistics",
            "File status of open runs",
        ],
        IntegrationType::Hush => &[
            "Runs: flow, trigger, state, source, output and result",
            "Run events",
            "Ledger and watcher status",
            "Flow options for a file",
        ],
        IntegrationType::Prowlarr => &[],
        IntegrationType::Tautulli => &[
            "Play history with durations, devices and delivery",
            "Users",
            "Version",
        ],
    };
    words.iter().map(|w| w.to_string()).collect()
}

/// The field key of a type's secret: `apiKey`, `token`, `password`.
pub fn secret_key(kind: IntegrationType) -> &'static str {
    match kind {
        IntegrationType::Plex => "token",
        IntegrationType::Qbittorrent => "password",
        _ => "apiKey",
    }
}

fn type_field(
    key: &str,
    label: &str,
    kind: &str,
    placeholder: Option<&str>,
    help: &str,
    part: &str,
) -> TypeField {
    TypeField {
        key: key.to_string(),
        label: label.to_string(),
        kind: kind.to_string(),
        placeholder: placeholder.map(str::to_string),
        help: help.to_string(),
        part: part.to_string(),
    }
}

/// Every type this version knows, with the metadata providers, as data.
pub fn kinds() -> Vec<IntegrationKind> {
    let mut out: Vec<IntegrationKind> = IntegrationType::ALL
        .into_iter()
        .map(|k| {
            let secret = match k {
                IntegrationType::Plex => "Token",
                IntegrationType::Qbittorrent => "Password",
                _ => "API key",
            };
            // The host and the port each program listens on unless it is told otherwise. The
            // bridge has no port of its own to name.
            let host = match k {
                IntegrationType::Plex => "plex:32400",
                IntegrationType::Radarr => "radarr:7878",
                IntegrationType::Sonarr => "sonarr:8989",
                IntegrationType::Qbittorrent => "qbittorrent:8080",
                IntegrationType::Prowlarr => "prowlarr:9696",
                IntegrationType::Tdarr => "tdarr:8265",
                IntegrationType::Hush => "hush",
                IntegrationType::Seerr => "seerr:5055",
                IntegrationType::Tautulli => "tautulli:8181",
            };
            let placeholder = format!("http://{host}");
            let mut fields = vec![type_field(
                "address",
                "Address",
                "url",
                Some(placeholder.as_str()),
                "As reached from the server",
                "connection",
            )];
            if k == IntegrationType::Qbittorrent {
                fields.push(type_field(
                    "username",
                    "Username",
                    "text",
                    None,
                    "Empty for a client that needs no sign-in",
                    "connection",
                ));
            }
            fields.push(type_field(
                secret_key(k),
                secret,
                "secret",
                None,
                if k == IntegrationType::Hush {
                    "Needed only when the service requires a key"
                } else {
                    "Read from an environment variable"
                },
                "connection",
            ));
            let read = reads(k);
            if !read.is_empty() {
                fields.push(type_field(
                    "syncEvery",
                    "Sync interval",
                    "duration",
                    None,
                    "Set in the environment, per integration type",
                    "schedule",
                ));
            }
            IntegrationKind {
                kind: k.as_str().to_string(),
                label: match k {
                    IntegrationType::Plex => "Plex Media Server".to_string(),
                    other => other.label().to_string(),
                },
                group: group_of(k).to_string(),
                description: match k {
                    IntegrationType::Plex => {
                        "Libraries, ratings, plays, editions, music and sessions"
                    }
                    IntegrationType::Radarr => "Movies, files, editions, queue and history",
                    IntegrationType::Sonarr => "Shows, episodes, files, queue and history",
                    IntegrationType::Qbittorrent => "Torrents, progress and unassigned downloads",
                    IntegrationType::Prowlarr => "Indexers",
                    IntegrationType::Tdarr => {
                        "Worker nodes, queues, active workers, libraries and flows"
                    }
                    IntegrationType::Hush => "Processing runs, their results and imports",
                    IntegrationType::Seerr => "Requests made in Seerr",
                    IntegrationType::Tautulli => {
                        "Play history with durations, devices and delivery"
                    }
                }
                .to_string(),
                max: match k {
                    IntegrationType::Plex
                    | IntegrationType::Qbittorrent
                    | IntegrationType::Seerr
                    | IntegrationType::Tautulli => Some(1),
                    _ => None,
                },
                available: !read.is_empty(),
                reads: read,
                writes: actions_of(k).iter().map(|a| a.label.to_string()).collect(),
                fields,
            }
        })
        .collect();
    for (id, label, description, reads, field, secret) in [
        (
            "tmdb",
            "TMDB",
            "Titles, people, collections, artwork, trailers, ratings and where to watch",
            &[
                "Titles, seasons and episodes",
                "People and credits",
                "Collections",
                "Artwork, trailers and where to watch",
            ][..],
            "token",
            "Read access token",
        ),
        (
            "tvdb",
            "TVDB",
            "Episode order and numbering",
            &["Series and seasons"][..],
            "key",
            "API key",
        ),
    ] {
        out.push(IntegrationKind {
            kind: id.to_string(),
            label: label.to_string(),
            group: "metadata".to_string(),
            description: description.to_string(),
            max: Some(1),
            available: true,
            reads: reads.iter().map(|r| r.to_string()).collect(),
            writes: Vec::new(),
            fields: vec![type_field(
                field,
                secret,
                "secret",
                None,
                "Read from an environment variable",
                "connection",
            )],
        });
    }
    out
}

/// An address without a user name or password in it.
pub fn without_credentials(url: &str) -> String {
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let host_end = rest.find('/').unwrap_or(rest.len());
            match rest[..host_end].rsplit_once('@') {
                Some((_, host)) => format!("{scheme}://{host}{}", &rest[host_end..]),
                None => url.to_string(),
            }
        }
        None => url.to_string(),
    }
}

/// The numbers among what an integration's passes counted.
fn numbers(counts: &Value) -> Value {
    match counts.as_object() {
        Some(map) => Value::Object(
            map.iter()
                .filter(|(_, v)| v.is_number())
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ),
        None => Value::Object(Default::default()),
    }
}

/// One integration's panel, from its configuration, its view, its recorded state, its recent
/// checks and its jobs. Pure.
pub fn integration_panel(
    i: &Integration,
    view: IntegrationView,
    row: Option<&EngineStateRow>,
    checks: &[Check],
    jobs: Vec<String>,
    instance_path: Option<&str>,
) -> IntegrationPanel {
    let from_file = i.source != "environment";
    let last_check = [
        row.and_then(|r| r.last_sync_at),
        checks.first().map(|c| c.at),
    ]
    .into_iter()
    .flatten()
    .max();
    IntegrationPanel {
        id: view.id,
        kind: view.kind,
        group: group_of(i.kind).to_string(),
        name: view.name,
        role: view.role,
        state: view.state.clone(),
        detail: view.detail,
        version: view.version,
        address: i.url.as_deref().map(without_credentials),
        source: if from_file { "file" } else { "environment" }.to_string(),
        env: (!from_file).then(|| i.url_var.clone()),
        file: if from_file {
            instance_path.map(str::to_string)
        } else {
            None
        },
        editable: false,
        configured: i.configured(),
        last_check,
        last_sync: row.and_then(|r| r.last_ok_at),
        latency_ms: row.and_then(|r| r.latency_ms),
        latency: row.map(|r| r.latency_history.clone()).unwrap_or_default(),
        counts: row
            .map(|r| numbers(&r.counts))
            .unwrap_or_else(|| Value::Object(Default::default())),
        reads: reads(i.kind),
        writes: view.actions.iter().map(|a| a.label.clone()).collect(),
        error: row
            .filter(|r| !r.ok)
            .and_then(|r| r.last_error.clone())
            .filter(|_| view.state == "down"),
        jobs,
        access: view.access,
        access_source: view.access_source,
        actions: view.actions,
    }
}

/// A metadata provider as an integration: configured by its variable, asked when a page needs
/// a record, so it has no jobs and no recorded state.
pub fn provider_panel(
    id: &str,
    name: &str,
    configured: bool,
    needs: Option<&str>,
    host: &str,
) -> IntegrationPanel {
    let kind = kinds().into_iter().find(|k| k.kind == id);
    IntegrationPanel {
        id: id.to_string(),
        kind: id.to_string(),
        group: "metadata".to_string(),
        name: name.to_string(),
        role: kind
            .as_ref()
            .map(|k| k.description.clone())
            .unwrap_or_default(),
        state: if configured { "ok" } else { "warn" }.to_string(),
        detail: if configured {
            "Configured".to_string()
        } else {
            match needs {
                Some(var) => format!("Not configured: {var} is not set"),
                None => "Not configured".to_string(),
            }
        },
        version: None,
        address: Some(format!("https://{host}")),
        source: if configured { "environment" } else { "default" }.to_string(),
        env: needs.map(str::to_string),
        file: None,
        editable: false,
        configured,
        last_check: None,
        last_sync: None,
        latency_ms: None,
        latency: Vec::new(),
        counts: Value::Object(Default::default()),
        reads: kind.map(|k| k.reads).unwrap_or_default(),
        writes: Vec::new(),
        error: None,
        jobs: Vec::new(),
        // A provider is asked for records and nothing else.
        access: "read".to_string(),
        access_source: "default".to_string(),
        actions: Vec::new(),
    }
}

// Libraries --------------------------------------------------------------------------------------

/// One library, as the Settings page lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPanel {
    pub id: String,
    pub name: String,
    /// `movie`, `show`, `music`, `homevideo` or `photo`.
    pub kind: String,
    /// The Plex server's section key; absent for a manager's own library.
    pub plex_key: Option<String>,
    /// The integration that manages it (a Radarr or Sonarr), when one does.
    pub manager: Option<String>,
    /// Its folders as the Plex server names them.
    pub locations: Vec<String>,
    /// Its folders as its manager names them; the server's folders when no manager does.
    pub roots: Vec<String>,
    /// Titles its manager lists (movies, shows), albums for music.
    pub titles: Option<i64>,
    /// Movies with a file, episodes with a file, albums.
    pub have: Option<i64>,
    /// Episodes of its shows, as the manager counts them.
    pub episodes: Option<i64>,
    pub albums: Option<i64>,
    pub tracks: Option<i64>,
    /// Bytes of its files.
    pub size: Option<i64>,
    /// The first profile the policy gives its manager, by the profile's id (`movies-hd`).
    pub profile: Option<String>,
    /// Every profile the policy gives its manager.
    pub profiles: Vec<String>,
    /// When what it holds was last read: the server's or the manager's last pass.
    pub scanned: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LibraryPanels {
    pub libraries: Vec<LibraryPanel>,
    /// The number of libraries.
    pub total: i64,
}

/// The profiles (lanes) the policy gives a manager, by id.
pub fn profiles_of(policy: &Policy, manager: &str) -> Vec<String> {
    let prefix = format!("{}:", manager.to_ascii_lowercase());
    policy
        .lanes
        .iter()
        .filter(|l| {
            l.arr_profiles
                .iter()
                .any(|p| p.to_ascii_lowercase().starts_with(&prefix))
        })
        .map(|l| l.id.clone())
        .collect()
}

/// What the libraries panel is made of besides the library rows.
pub struct LibraryFacts<'a> {
    pub holdings: &'a [LibraryHoldings],
    pub policy: &'a Policy,
    /// The server's albums and tracks, when it has music.
    pub albums: Option<(i64, i64, i64)>,
    /// The last pass that succeeded, by integration id.
    pub last_ok: &'a BTreeMap<String, DateTime<Utc>>,
}

/// The libraries: the Plex server's sections, each with what its manager holds; the managers'
/// own libraries when there are no sections, and those no section stands for. Pure.
pub fn libraries_panel(rows: &[Library], facts: &LibraryFacts<'_>) -> Vec<LibraryPanel> {
    let held = |id: &str| facts.holdings.iter().find(|h| h.library_id == id);
    let sections: Vec<&Library> = rows.iter().filter(|l| l.section_key.is_some()).collect();
    let music_sections = sections
        .iter()
        .filter(|l| l.kind.as_deref() == Some("music"))
        .count();
    let last = |id: Option<&str>| id.and_then(|i| facts.last_ok.get(i)).copied();
    let managed = |l: &Library, section: Option<&Library>| {
        let kind = section
            .and_then(|s| s.kind.clone())
            .or_else(|| l.kind.clone())
            .unwrap_or_default();
        let h = held(&l.id);
        let (have, episodes) = match kind.as_str() {
            "show" => (
                Some(h.map(|h| h.episode_files).unwrap_or(0)),
                Some(h.map(|h| h.episodes).unwrap_or(0)),
            ),
            _ => (Some(h.map(|h| h.with_file).unwrap_or(0)), None),
        };
        let manager = l.integration.clone().unwrap_or_else(|| l.id.clone());
        let profiles = profiles_of(facts.policy, &manager);
        LibraryPanel {
            id: section
                .map(|s| s.id.clone())
                .unwrap_or_else(|| l.id.clone()),
            name: section
                .map(|s| s.name.clone())
                .unwrap_or_else(|| l.name.clone()),
            kind,
            plex_key: section.and_then(|s| s.section_key.clone()),
            locations: section.map(|s| s.locations.clone()).unwrap_or_default(),
            roots: l.root_folders.clone(),
            titles: Some(l.titles),
            have,
            episodes,
            albums: None,
            tracks: None,
            size: Some(l.size_bytes),
            profile: profiles.first().cloned(),
            profiles,
            scanned: last(Some(manager.as_str())),
            manager: Some(manager),
        }
    };
    let mut out = Vec::new();
    for s in &sections {
        let manager = s
            .managed_by
            .as_deref()
            .and_then(|m| rows.iter().find(|l| l.section_key.is_none() && l.id == m));
        match manager {
            Some(m) => {
                let mut lib = managed(m, Some(s));
                lib.scanned = lib.scanned.max(last(s.integration.as_deref()));
                out.push(lib);
            }
            None => {
                // A video section no manager looks after is a plain video library here as
                // it is in the workspace record, whatever agent the server gave it.
                let kind = if plain_video(s.kind.as_deref(), true, s.managed_by.is_some()) {
                    "homevideo".to_string()
                } else {
                    s.kind.clone().unwrap_or_default()
                };
                let music = (kind == "music" && music_sections == 1)
                    .then_some(facts.albums)
                    .flatten();
                out.push(LibraryPanel {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    plex_key: s.section_key.clone(),
                    manager: s.managed_by.clone(),
                    locations: s.locations.clone(),
                    roots: s.locations.clone(),
                    titles: music.map(|(albums, _, _)| albums),
                    have: music.map(|(albums, _, _)| albums),
                    episodes: None,
                    albums: music.map(|(albums, _, _)| albums),
                    tracks: music.map(|(_, tracks, _)| tracks),
                    size: music.map(|(_, _, bytes)| bytes),
                    profile: None,
                    profiles: Vec::new(),
                    scanned: last(s.integration.as_deref()),
                    kind,
                });
            }
        }
    }
    // A manager's own library that no section stands for keeps its titles in view.
    for l in rows.iter().filter(|l| l.section_key.is_none()) {
        let stood_for = sections
            .iter()
            .any(|s| s.managed_by.as_deref() == Some(l.id.as_str()));
        if !stood_for {
            out.push(managed(l, None));
        }
    }
    out
}

// People -----------------------------------------------------------------------------------------

/// Someone or something that signs in: the owner, a person the server is shared with (their
/// Seerr account), or a program with an API key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PersonView {
    pub id: String,
    pub name: String,
    /// `person` or `program`.
    pub kind: String,
    /// A role id: `owner`, `admin`, `member`, `guest`. Absent for a program without `admin`.
    pub role: Option<String>,
    /// `proxy` (the owner, through the sign-in proxy), `plex` (Singularr Requests), `key`.
    pub sign_in: Option<String>,
    /// The newest sign-in or use.
    pub last_seen: Option<DateTime<Utc>>,
    /// The newest request on Seerr.
    pub last_request: Option<DateTime<Utc>>,
    /// Requests on Seerr.
    pub requests: Option<i64>,
    /// Limits over a period: none in this version (the season gate is not a limit over time).
    #[schema(value_type = Option<Object>)]
    pub limits: Option<Value>,
    /// The season gate, when it applies: seasons of one show held at a time.
    pub season_gate: Option<i64>,
    /// Not known in this version (Seerr's permissions are not read).
    pub auto_approve: Option<bool>,
    /// What they may ask for from the portal.
    pub target: Option<String>,
    /// Since when they are known: not known in this version.
    pub since: Option<DateTime<Utc>>,
    /// A program's scopes.
    pub scopes: Option<Vec<Scope>>,
    /// Where they come from: `configuration`, `seerr`, `keys`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RoleView {
    pub id: String,
    pub label: String,
    pub description: String,
    /// Permission ids; `*` is every permission.
    pub permissions: Vec<String>,
    /// Roles are fixed in this version.
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PermissionView {
    pub id: String,
    pub label: String,
    pub group: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PeoplePanel {
    pub people: Vec<PersonView>,
    pub roles: Vec<RoleView>,
    pub permissions: Vec<PermissionView>,
}

/// Every permission a role can hold, as data a later version gives roles by hand.
pub const PERMISSIONS: &[(&str, &str, &str)] = &[
    ("portal.open", "Portal access", "Portal"),
    ("requests.create", "Request titles", "Requests"),
    ("requests.uhd", "Request 4K", "Requests"),
    ("requests.all", "View all requests", "Requests"),
    ("requests.approve", "Manage requests", "Requests"),
    ("workspace.open", "Workspace access", "Workspace"),
    (
        "library.edit",
        "Edit monitoring and quality profiles",
        "Workspace",
    ),
    ("activity.manage", "Manage downloads", "Workspace"),
    ("journal.read", "View journal", "Workspace"),
    ("rules.edit", "Edit rules", "Administration"),
    (
        "settings.edit",
        "Edit settings and integrations",
        "Administration",
    ),
    ("people.manage", "Manage people and roles", "Administration"),
    ("keys.manage", "Manage API keys", "Administration"),
];

pub fn permissions() -> Vec<PermissionView> {
    PERMISSIONS
        .iter()
        .map(|(id, label, group)| PermissionView {
            id: id.to_string(),
            label: label.to_string(),
            group: group.to_string(),
        })
        .collect()
}

/// The roles of this version. In this version the owner is whoever the configuration names,
/// the people the server is shared with are members, and no one is named admin or guest yet.
pub fn roles() -> Vec<RoleView> {
    let role = |id: &str, label: &str, description: &str, permissions: &[&str]| RoleView {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        permissions: permissions.iter().map(|p| p.to_string()).collect(),
        editable: false,
    };
    vec![
        role(
            "owner",
            "Owner",
            "Full access, including people, roles and API keys",
            &["*"],
        ),
        role(
            "admin",
            "Admin",
            "Requests, rules, settings and integrations",
            &[
                "workspace.open",
                "requests.create",
                "requests.uhd",
                "requests.all",
                "requests.approve",
                "library.edit",
                "activity.manage",
                "rules.edit",
                "settings.edit",
                "journal.read",
                "portal.open",
            ],
        ),
        role(
            "member",
            "Member",
            "Portal access and requests",
            &["portal.open", "requests.create"],
        ),
        role(
            "guest",
            "Guest",
            "Portal access without requests",
            &["portal.open"],
        ),
    ]
}

// Jobs -------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    /// `radarr:titles`, `folder:music`.
    pub id: String,
    pub label: String,
    pub description: String,
    /// The integration it reads, when it reads one.
    pub integration: Option<String>,
    /// Seconds between runs.
    pub every: u64,
    pub last_run: Option<DateTime<Utc>>,
    /// `ok` or `failed`; absent before its first run.
    pub last_result: Option<String>,
    pub last_detail: Option<String>,
    /// How long the last run took, in milliseconds.
    pub took: Option<i64>,
    pub next_run: Option<DateTime<Utc>>,
    pub running: bool,
    pub can_run: bool,
    /// Runs since the process started.
    pub runs: u64,
}

/// Something Singularr keeps to answer faster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CacheView {
    pub id: String,
    pub label: String,
    /// PostgreSQL's estimate of its rows.
    pub entries: i64,
    /// Bytes on disk.
    pub size: i64,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobsPanel {
    pub jobs: Vec<JobView>,
    pub cache: Vec<CacheView>,
    /// The integrations are read (`SINGULARR_SYNC_ENABLED`); off, their jobs do not run.
    pub sync_enabled: bool,
    /// What a job knows is since this start.
    pub since: DateTime<Utc>,
}

/// A job's words: its label, and what it reads in a few words.
fn job_words(pass: &str, of: &str) -> (String, String) {
    let (label, reads) = match pass {
        "titles" => (
            format!("{of} titles and files"),
            "Titles, and the files of changed titles",
        ),
        "queue" => (
            format!("{of} download queue"),
            "Queue items and their status",
        ),
        "history" => (
            format!("{of} grab history"),
            "Grabs, to match downloads to their manager",
        ),
        "downloads" => (
            format!("{of} downloads"),
            "Downloads, their manager and stuck state",
        ),
        "server" => (
            format!("{of} libraries, ratings and plays"),
            "Libraries, items, ratings, plays, editions and music",
        ),
        "requests" => (
            format!("{of} requests"),
            "Requests, accounts and discover lists",
        ),
        "processing" => (
            format!("{of} processing runs"),
            "Status, runs, files and events",
        ),
        "music" => (
            "Music folder scan".to_string(),
            "Artists, albums, covers and formats",
        ),
        "home-movies" => ("Home movies folder scan".to_string(), "Video files"),
        "movie-folders" => (
            "Movie root folder scan".to_string(),
            "Files no manager tracks",
        ),
        other => (other.replace(['-', '_'], " "), ""),
    };
    (label, reads.to_string())
}

/// One job's view. Pure.
pub fn job_view(job: &Job, config: &Config) -> JobView {
    let s = job.state();
    let of = job
        .spec
        .integration
        .as_deref()
        .map(|id| {
            config
                .integration(id)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| id.to_string())
        })
        .unwrap_or_default();
    let (label, description) = job_words(&job.spec.pass, &of);
    JobView {
        id: job.spec.id.clone(),
        label,
        description,
        integration: job.spec.integration.clone(),
        every: job.spec.every.as_secs(),
        last_run: s.last_run,
        last_result: s
            .last_ok
            .map(|ok| if ok { "ok" } else { "failed" }.to_string()),
        last_detail: s.last_detail,
        took: s.took_ms,
        next_run: s.next_run,
        running: s.running,
        can_run: !s.running,
        runs: s.runs,
    }
}

// System -----------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceChoice {
    pub id: String,
    pub label: String,
    /// What it needs.
    pub needs: String,
}

/// A platform service Singularr uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ServiceView {
    /// `database`, `search`, `cache`, `artwork`.
    pub id: String,
    pub label: String,
    /// The choice in use.
    pub provider: String,
    pub choices: Vec<ServiceChoice>,
    /// `ok`, `warn` or `down`.
    pub state: String,
    pub detail: String,
}

/// One check, as `singularrd doctor` makes it, from what the process knows (no new call to an
/// integration).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CheckView {
    pub id: String,
    pub label: String,
    /// `ok`, `warn` or `down`.
    pub state: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SystemPaths {
    pub web: String,
    pub policy: String,
    pub editions: String,
    pub instance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SystemLinks {
    pub reference: String,
    pub openapi: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SystemView {
    pub version: String,
    pub build: String,
    pub started: DateTime<Utc>,
    /// `observe` while Singularr may act on no integration: it reads them and changes none of
    /// them. `act` once the access of one of them is `act`.
    pub mode: String,
    pub services: Vec<ServiceView>,
    pub checks: Vec<CheckView>,
    pub paths: SystemPaths,
    pub links: SystemLinks,
}

pub fn check(id: &str, label: &str, state: &str, detail: impl Into<String>) -> CheckView {
    CheckView {
        id: id.to_string(),
        label: label.to_string(),
        state: state.to_string(),
        detail: detail.into(),
    }
}

pub fn choice(id: &str, label: &str, needs: &str) -> ServiceChoice {
    ServiceChoice {
        id: id.to_string(),
        label: label.to_string(),
        needs: needs.to_string(),
    }
}

/// The worst of some states: `down` over `warn` over `ok`.
pub fn worst<'a>(states: impl IntoIterator<Item = &'a str>) -> &'static str {
    let mut out = "ok";
    for s in states {
        match s {
            "down" => return "down",
            "warn" => out = "warn",
            _ => {}
        }
    }
    out
}

/// The checks a configuration gives on its own: its warnings and the edge token.
pub fn config_checks(config: &Config) -> Vec<CheckView> {
    let mut out = vec![if config.warnings.is_empty() {
        check("configuration", "Configuration", "ok", "No warnings")
    } else {
        check(
            "configuration",
            "Configuration",
            "warn",
            config.warnings.join("; "),
        )
    }];
    out.push(
        match (config.edge_token_set, config.edge_token_placeholder) {
            (true, _) => check("edge-token", "Edge token", "ok", "Set"),
            (false, true) => check("edge-token", "Edge token", "down", "Still the placeholder"),
            // With no trusted proxy no identity header is honored, so no token is missing.
            (false, false) if config.trusted_proxies.is_none() => check(
                "edge-token",
                "Edge token",
                "ok",
                "Not set; no trusted proxy",
            ),
            (false, false) => check(
                "edge-token",
                "Edge token",
                "warn",
                "Not set; identity headers trusted by proxy address",
            ),
        },
    );
    out
}

/// The ids of each integration's jobs.
pub fn job_ids(jobs: &[std::sync::Arc<Job>]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for j in jobs {
        if let Some(i) = &j.spec.integration {
            out.entry(i.clone()).or_default().push(j.spec.id.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests;
