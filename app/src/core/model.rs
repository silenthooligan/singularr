//! The app's records beside the platform's: the folder scans and the kinds of event the
//! journal holds. The platform's (pages, scopes, journal events, API keys) keep their paths here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[cfg(feature = "ssr")]
pub use singularr_platform::model::{
    ApiKeyInfo, CreatedApiKey, DEFAULT_PER_PAGE, Event, EventQuery, Listing, MAX_PER_PAGE,
    NewApiKey, Page, Paging, Scope, Whoami,
};

/// The kinds of event the journal holds, for filters and for stream listeners.
pub const EVENT_KINDS: [&str; 26] = [
    "title_added",
    "title_removed",
    "copy_added",
    "copy_replaced",
    "copy_removed",
    "copy_changed",
    "score_changed",
    "download_added",
    "download_completed",
    "download_stuck",
    "download_orphaned",
    "download_removed",
    "engine_down",
    "engine_up",
    "policy_loaded",
    "key_created",
    "key_revoked",
    "request_added",
    "request_status_changed",
    "request_removed",
    "album_added",
    "album_removed",
    "tape_added",
    "tape_missing",
    "tape_moved",
    "tape_annotated",
];

/// The last scan of a folder Singularr reads: the music library or the home movies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct FolderScan {
    /// `music` or `home_movies`.
    pub folder: String,
    /// Where the folder is mounted in the container.
    pub root: String,
    /// The last scan succeeded. False before the first scan.
    pub ok: bool,
    pub last_scan_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub took_ms: Option<i64>,
    /// Why the last scan failed.
    pub error: Option<String>,
    /// What the last scan that succeeded found, for example `{"albums": 1345, "tracks": 15210}`.
    #[cfg_attr(feature = "ssr", schema(value_type = Object))]
    pub counts: serde_json::Value,
}

/// What one scan of a folder did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ScanReport {
    /// `music` or `home_movies`.
    pub folder: String,
    /// Albums or tapes in the folder.
    pub found: i64,
    pub added: i64,
    /// Albums deleted from the catalog, or tapes marked missing.
    pub removed: i64,
    /// Items whose files changed, or tapes that were moved or renamed.
    pub changed: i64,
    pub events: i64,
    pub took_ms: i64,
}
