//! Downloads as the API and the web app see them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One download in a download client (a torrent, or a usenet download), with the arr that owns
/// it and what that arr says. A download is one client's: the key is the client and the id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct Download {
    /// The id the managers know the download by, upper case: a torrent's hash, a usenet
    /// client's job id (SABnzbd's `nzo_id`, NZBGet's `drone` parameter or `NZBID`).
    pub hash: String,
    /// The download client that lists it, by integration id.
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub client: String,
    /// `torrent` or `usenet`.
    #[serde(default = "torrent")]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub protocol: String,
    /// The client's own id of it, as the client writes it.
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub item_id: Option<String>,
    pub name: String,
    pub category: Option<String>,
    /// The client's own word for where it stands: `downloading`, `stalledDL`, `seed`,
    /// `Paused`, `PP_QUEUED`, ...
    pub state: String,
    /// The same in the product's words: `queued`, `downloading`, `stalled`, `paused`,
    /// `checking`, `unpacking`, `seeding`, `completed` or `failed`.
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub status: Option<String>,
    /// 0 to 1.
    pub progress: f64,
    pub size_bytes: i64,
    pub added_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub seeding_seconds: i64,
    pub save_path: Option<String>,
    /// The arr whose queue or grab history has this hash. Absent: no arr claims it.
    pub owner: Option<String>,
    /// `queue` or `history`: where the claim was found.
    pub owner_source: Option<String>,
    pub title_id: Option<i64>,
    pub title: Option<String>,
    /// The arr's queue status, tracked state and health while the download is in a queue.
    pub queue_status: Option<String>,
    pub queue_state: Option<String>,
    pub queue_health: Option<String>,
    #[cfg_attr(feature = "ssr", schema(value_type = Vec<String>))]
    pub queue_messages: serde_json::Value,
    /// The arr reports an import that is blocked, or a warning or an error.
    pub stuck: bool,
    pub stuck_reason: Option<String>,
    /// No engine owns it, or it sits in a queue with no title behind it.
    pub orphan: bool,
    pub orphan_reason: Option<String>,
    pub first_seen_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// While it is not complete, as the client last reported it: bytes per second, seconds
    /// left, and the seeds and peers it is connected to.
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub speed_bytes: Option<i32>,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub eta_seconds: Option<i32>,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub seeds: Option<i32>,
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub peers: Option<i32>,
    /// The indexer the release came from, as a manager's queue named it.
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub indexer: Option<String>,
}

fn torrent() -> String {
    "torrent".to_string()
}

impl Download {
    /// A usenet download. It ends once its client is done with it.
    pub fn usenet(&self) -> bool {
        self.protocol == "usenet"
    }
}

/// Filters of `GET /api/v1/downloads`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct DownloadQuery {
    /// An engine id, or `none` for downloads no arr claims.
    pub owner: Option<String>,
    /// The download client, by integration id.
    pub client: Option<String>,
    /// `torrent` or `usenet`.
    pub protocol: Option<String>,
    /// The client's state, exactly as it reports it.
    pub state: Option<String>,
    pub category: Option<String>,
    pub stuck: Option<bool>,
    pub orphan: Option<bool>,
    /// `true`: still downloading. `false`: complete.
    pub active: Option<bool>,
    /// Words of the download's name.
    pub q: Option<String>,
    /// `added` (default, newest first), `name`, `progress`, `size` or `state`. Prefix `-`
    /// to reverse.
    pub sort: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct DownloadCounts {
    pub total: i64,
    pub active: i64,
    pub complete: i64,
    pub stuck: i64,
    pub orphans: i64,
    /// Downloads no arr claims, whether or not that could be settled yet.
    pub unowned: i64,
}
