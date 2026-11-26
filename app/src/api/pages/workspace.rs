//! The workspace record: names, the caller, the media server's sections with their counts, the
//! integrations, the counts of the sidebar and the bell.

use super::activity::{self, Queued};
use super::managers;
use super::model::{
    Acting, Attention, Formats, Portal, Profile, ServerName, Workspace, WorkspaceCounts,
    WorkspaceLibrary, WorkspaceUser,
};
use crate::api::state::AppState;
use crate::catalog::model::Library;
use crate::catalog::service as catalog;
use crate::catalog::store::{self as catalog_store, LibraryTotals};
use crate::core::auth::Caller;
use crate::core::config::{Config, IntegrationType};
use crate::core::error::AppResult;
use crate::engines::model::IntegrationView;
use crate::engines::service as engines;
use crate::engines::store as engine_store;
use crate::lifecycle::server::plain_video;
use crate::music::store as music_store;
use crate::requests::service as requests;
use chrono::{DateTime, Utc};
use singularr_core::Policy;
use std::time::Duration;

/// How long the workspace waits for the managers' wanted lists before it answers without
/// their total.
const WANTED_WAIT: Duration = Duration::from_millis(1500);
/// Items of the bell.
const BELL: usize = 10;

/// The order of the sidebar: movies, shows, music, the rest; then by name.
fn kind_order(kind: &str) -> u8 {
    match kind {
        "movie" => 0,
        "show" => 1,
        "music" => 2,
        _ => 3,
    }
}

/// The media server's sections as the workspace lists them. A video section that no manager
/// looks after is `homevideo`; photo sections are left out (no page shows them).
pub fn sections(
    libraries: &[Library],
    totals: &[LibraryTotals],
    music: Option<music_store::MusicTotals>,
) -> Vec<WorkspaceLibrary> {
    let music_sections = libraries
        .iter()
        .filter(|l| l.section_key.is_some() && l.kind.as_deref() == Some("music"))
        .count();
    let mut out: Vec<WorkspaceLibrary> = libraries
        .iter()
        .filter(|l| l.section_key.is_some())
        .filter_map(|l| {
            let raw = l.kind.as_deref()?;
            let kind = if plain_video(Some(raw), true, l.managed_by.is_some()) {
                "homevideo"
            } else {
                match raw {
                    "movie" => "movie",
                    "show" => "show",
                    "music" => "music",
                    _ => return None,
                }
            };
            let t = l
                .managed_by
                .as_deref()
                .and_then(|m| totals.iter().find(|t| t.library_id == m));
            let mut lib = WorkspaceLibrary {
                id: l.id.clone(),
                kind: kind.to_string(),
                name: l.name.clone(),
                plex_key: l.section_key.clone(),
                manager: l.managed_by.clone(),
                locations: l.locations.clone(),
                titles: None,
                have: None,
                size: None,
                episodes: None,
                albums: None,
                artists: None,
                tracks: None,
            };
            match (kind, t) {
                ("movie", Some(t)) => {
                    lib.titles = Some(t.titles);
                    lib.have = Some(t.with_file);
                    lib.size = Some(t.size_bytes);
                }
                ("show", Some(t)) => {
                    lib.titles = Some(t.titles);
                    lib.have = Some(t.episode_files);
                    lib.episodes = Some(t.episodes);
                    lib.size = Some(t.size_bytes);
                }
                // The server's music is stored for the server as a whole: its counts are a
                // section's only when there is one music section.
                ("music", _) if music_sections == 1 => {
                    if let Some(m) = &music {
                        lib.titles = Some(m.albums);
                        lib.albums = Some(m.albums);
                        lib.artists = Some(m.artists);
                        lib.tracks = Some(m.tracks);
                        lib.size = Some(m.size_bytes);
                    }
                }
                _ => {}
            }
            Some(lib)
        })
        .collect();
    out.sort_by(|a, b| {
        kind_order(&a.kind)
            .cmp(&kind_order(&b.kind))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// The policy's lanes as the client's profiles. A lane's kind is the kind of the managers
/// whose profiles it stands for.
pub fn profiles(policy: &Policy, config: &Config) -> Vec<Profile> {
    policy
        .lanes
        .iter()
        .map(|l| {
            let first = l.arr_profiles.first().and_then(|p| p.split_once(':'));
            let kind = first.and_then(|(arr, _)| {
                config.integration(arr).and_then(|i| match i.kind {
                    IntegrationType::Radarr => Some("movie".to_string()),
                    IntegrationType::Sonarr => Some("show".to_string()),
                    _ => None,
                })
            });
            Profile {
                id: l.id.clone(),
                name: first
                    .map(|(_, p)| p.to_string())
                    .unwrap_or_else(|| l.label.clone()),
                label: l.label.clone(),
                kind,
                cutoff: (l.cutoff_score != i32::MAX).then_some(l.cutoff_score),
                min_score: l.min_score,
                upgrade_step: l.upgrade_step,
            }
        })
        .collect()
}

/// The processing runs that need attention, as items of the list: runs that are stuck (`warn`)
/// and runs that failed or were refused in the last days (`bad`).
pub async fn run_attention(state: &AppState) -> AppResult<Vec<Attention>> {
    Ok(crate::api::processing::alerts(state)
        .await?
        .into_iter()
        .map(|r| Attention {
            id: r.id,
            kind: "processing".to_string(),
            title: r.title,
            detail: r.detail,
            tone: r.tone.to_string(),
            since: r.since,
        })
        .collect())
}

/// What needs attention: downloads that are stuck or that no manager owns, integrations whose
/// last call failed, and the processing runs of [`run_attention`], newest first.
pub fn attention(
    queue: &[Queued],
    integrations: &[IntegrationView],
    down_since: &[(String, Option<DateTime<Utc>>)],
    runs: Vec<Attention>,
) -> Vec<Attention> {
    let mut out: Vec<Attention> = queue
        .iter()
        .filter_map(|q| {
            let a = q.row.attention.as_ref()?;
            Some(Attention {
                id: q.row.id.clone(),
                kind: "queue".to_string(),
                title: q.row.title.title.clone(),
                detail: a.reason.clone(),
                tone: if q.row.stage == "unassigned" {
                    "warn"
                } else {
                    "bad"
                }
                .to_string(),
                since: a.since,
            })
        })
        .collect();
    out.extend(integrations.iter().filter(|i| i.state == "down").map(|i| {
        Attention {
            id: i.id.clone(),
            kind: "integration".to_string(),
            title: i.name.clone(),
            detail: i.detail.clone(),
            tone: "bad".to_string(),
            since: down_since
                .iter()
                .find(|(id, _)| *id == i.id)
                .and_then(|(_, at)| *at),
        }
    }));
    out.extend(runs);
    out.sort_by(|a, b| b.since.cmp(&a.since).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Since when each integration has been down: its last sync after its last success.
pub async fn down_since(state: &AppState) -> AppResult<Vec<(String, Option<DateTime<Utc>>)>> {
    Ok(engine_store::list(&state.pool)
        .await?
        .into_iter()
        .filter(|r| !r.ok)
        .map(|r| (r.engine, r.last_ok_at.or(r.last_sync_at)))
        .collect())
}

/// The integrations Singularr may act on: those with at least one action at `act`.
fn acting(state: &AppState) -> Vec<Acting> {
    state
        .engines
        .access
        .acting()
        .into_iter()
        .map(|(id, name)| Acting { id, name })
        .collect()
}

/// The first media server's own name and version, from its last pass.
async fn server(state: &AppState) -> AppResult<Option<ServerName>> {
    let Some(first) = state.config.media_servers.first() else {
        return Ok(None);
    };
    let rows = engine_store::list(&state.pool).await?;
    let Some(row) = rows.into_iter().find(|r| r.engine == first.id) else {
        return Ok(None);
    };
    Ok(Some(ServerName {
        name: row
            .counts
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        version: row.version,
    }))
}

pub async fn workspace(state: &AppState, caller: &Caller) -> AppResult<Workspace> {
    let pool = &state.pool;
    let who = caller.whoami();
    let libraries = catalog::libraries(pool).await?;
    let totals = catalog_store::library_totals(pool).await?;
    let music = music_store::server_totals(pool).await?;
    let integrations = engines::integrations(pool, &state.config, &state.engines.access).await?;
    let queue = activity::queue(state, false).await?;
    let bell = attention(
        &queue,
        &integrations,
        &down_since(state).await?,
        run_attention(state).await?,
    );
    // Names and addresses are settings: the value in force may come from the environment, the
    // instance file, or what was saved in the interface.
    let w = &state.config.workspace;
    let settings = &state.settings;
    Ok(Workspace {
        name: settings
            .text("general.name")
            .unwrap_or_else(|| w.name.clone()),
        mode: state.engines.access.mode().to_string(),
        acting: acting(state),
        user: WorkspaceUser {
            name: who.name,
            role: who.kind,
        },
        libraries: sections(&libraries, &totals, music),
        counts: WorkspaceCounts {
            attention: bell.len() as i64,
            queue: queue.iter().filter(|q| q.in_motion()).count() as i64,
            requests_pending: requests::pending(pool).await?,
            wanted: managers::wanted_total(state, WANTED_WAIT).await,
        },
        integrations,
        server: server(state).await?,
        portal: settings.text("portal.url").map(|url| Portal {
            name: settings
                .text("portal.name")
                .unwrap_or_else(|| w.portal.clone()),
            url,
        }),
        sign_out: settings.text("general.signOutUrl"),
        attention: bell.into_iter().take(BELL).collect(),
        profiles: profiles(&state.policy.current().policy, &state.config),
        formats: Formats {
            time_zone: settings.text("general.timeZone").unwrap_or_default(),
            date: settings.text("general.dateFormat").unwrap_or_default(),
            time: settings.text("general.timeFormat").unwrap_or_default(),
        },
        generated: Utc::now(),
    })
}
