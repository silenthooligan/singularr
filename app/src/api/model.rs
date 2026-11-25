//! Answers that draw on several modules at once.

use crate::catalog::model::{CatalogCounts, DiffCounts};
use crate::core::model::FolderScan;
use crate::engines::model::EngineStatus;
use crate::lifecycle::model::{Download, DownloadCounts};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct LaneSummary {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct PolicySummary {
    pub hash: String,
    pub name: String,
    pub version: String,
    /// `file:<path>` or `builtin`.
    pub source: String,
    pub loaded_at: DateTime<Utc>,
    pub traits: usize,
    pub lanes: Vec<LaneSummary>,
    pub protections: usize,
}

/// What needs attention, in one answer: the home page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Overview {
    pub catalog: CatalogCounts,
    pub downloads: DownloadCounts,
    /// Copies where Singularr's reading differs from the arr's.
    pub shadow: DiffCounts,
    pub engines: Vec<EngineStatus>,
    /// The newest stuck downloads, at most ten.
    pub stuck: Vec<Download>,
    /// The newest orphans, at most ten.
    pub orphans: Vec<Download>,
    pub policy: PolicySummary,
    pub latest_event_id: i64,
    pub sync_enabled: bool,
}

/// One engine's last sync, for the sidebar and the top bar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct EngineSync {
    pub engine: String,
    /// The integration type: `radarr`, `sonarr`, a download client's (`qbittorrent`,
    /// `transmission`, `deluge`, `sabnzbd`, `nzbget`), `seerr`.
    pub kind: String,
    pub library: Option<String>,
    pub configured: bool,
    pub ok: bool,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
}

/// The counts the sidebar shows, in one small answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Summary {
    /// Pending requests on both Seerr sites.
    pub requests_pending: i64,
    pub downloads_stuck: i64,
    pub downloads_orphans: i64,
    /// Downloads in progress.
    pub downloads_active: i64,
    /// Tapes never annotated.
    pub home_movies_inbox: i64,
    /// Every engine of the configuration with its last sync.
    pub engines: Vec<EngineSync>,
    /// The last scan of the music and the home movies folders.
    pub folders: Vec<FolderScan>,
    pub latest_event_id: i64,
    pub sync_enabled: bool,
}
