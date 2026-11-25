//! Engine state and the arr's own reading of a name, as the API shows them.

use crate::catalog::model::{ArrReading, SingularrReading};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use singularr_core::ParsedRelease;

/// The media server a request is for: the value of a request's `site`. This version serves
/// Plex alone, so every Seerr integration takes requests for it.
pub const SITE: &str = "plex";

/// The outside lists Seerr serves on Discover, from TMDB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum DiscoverList {
    Trending,
    PopularMovies,
    PopularShows,
    UpcomingMovies,
}

impl DiscoverList {
    pub const ALL: [DiscoverList; 4] = [
        DiscoverList::Trending,
        DiscoverList::PopularMovies,
        DiscoverList::PopularShows,
        DiscoverList::UpcomingMovies,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            DiscoverList::Trending => "trending",
            DiscoverList::PopularMovies => "popular_movies",
            DiscoverList::PopularShows => "popular_shows",
            DiscoverList::UpcomingMovies => "upcoming_movies",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DiscoverList::Trending => "Trending",
            DiscoverList::PopularMovies => "Popular movies",
            DiscoverList::PopularShows => "Popular shows",
            DiscoverList::UpcomingMovies => "Upcoming",
        }
    }

    pub fn parse(s: &str) -> Option<DiscoverList> {
        let s = s.trim().to_ascii_lowercase().replace('-', "_");
        DiscoverList::ALL.into_iter().find(|l| l.as_str() == s)
    }
}

/// One engine and how its last sync went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct EngineStatus {
    /// The integration id.
    pub engine: String,
    /// `radarr`, `sonarr`, `qbittorrent`, `seerr`, `plex`, `tdarr` or `hush`.
    pub kind: String,
    pub library: Option<String>,
    /// False when its URL or key is not set; it is then not asked at all.
    pub configured: bool,
    /// The last call to it succeeded.
    pub ok: bool,
    pub version: Option<String>,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_ok_at: Option<DateTime<Utc>>,
    /// Why the last call failed. Left out of the public health route.
    pub last_error: Option<String>,
    /// The round trip of the last call that succeeded, in milliseconds.
    pub latency_ms: Option<i32>,
    /// The last 20 round-trip times, oldest first.
    pub latency_ms_history: Vec<i32>,
    /// What the last passes counted: titles, files, queue items, torrents.
    #[cfg_attr(feature = "ssr", schema(value_type = Object))]
    pub counts: serde_json::Value,
    /// Failed calls in a row.
    pub failures: i32,
}

/// One thing Singularr can do to an integration beyond reading it: `{id, label}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ActionView {
    /// `send`.
    pub id: String,
    /// `Send a file to a flow`.
    pub label: String,
}

/// One configured integration and its health, as the workspace record names it:
/// `{id, type, name, role, state, version, detail, access, accessSource, actions}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct IntegrationView {
    pub id: String,
    /// `plex`, `radarr`, `sonarr`, `qbittorrent`, `prowlarr`, `tdarr`, `hush` or `seerr`.
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    /// What it does here: `media server`, `movies`, `shows`, `downloads`, `indexers`,
    /// `processing`, `requests`.
    pub role: String,
    /// `ok` (its last call succeeded), `warn` (not configured, not contacted yet, or a type
    /// this version lists without reading) or `down` (its last call failed).
    pub state: String,
    pub version: Option<String>,
    /// The state in words: why it is `warn` or `down`, or what the last pass read.
    pub detail: String,
    /// What Singularr may do with it: `read`, or `act` (read it and do what `actions` lists).
    pub access: String,
    /// Where `access` comes from, in the words of a setting's source: `default`,
    /// `environment`, `file` or `saved`. The environment and the file fix it.
    pub access_source: String,
    /// What Singularr can do to an integration of this type beyond reading it. Empty: it can
    /// only be read, and `access` is `read` whatever is asked.
    pub actions: Vec<ActionView>,
}

/// The answer of `GET /api/v1/integrations`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct IntegrationList {
    pub integrations: Vec<IntegrationView>,
}

/// The answer of `GET /api/v1/health`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Health {
    /// `ok` when the database answers and every configured engine answered its last call,
    /// otherwise `degraded`.
    pub status: String,
    pub version: String,
    pub database: bool,
    /// False when `SINGULARR_SYNC_ENABLED` is off: the catalog is served but not refreshed.
    pub sync_enabled: bool,
    pub policy_hash: String,
    pub policy_source: String,
    pub engines: Vec<EngineStatus>,
}

/// What an arr's own parser makes of a release name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ArrParse {
    pub title: Option<String>,
    pub year: Option<i32>,
    pub season: Option<i32>,
    pub episodes: Vec<i32>,
    pub quality: Option<String>,
    pub group: Option<String>,
    pub edition: Option<String>,
    pub languages: Vec<String>,
    pub custom_formats: Vec<String>,
    /// The custom format score. An arr scores a bare name against its formats; the score a
    /// file gets in a profile can differ.
    pub score: Option<i32>,
    /// The arr matched the name to a title it manages.
    pub matched_title: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ShadowParseRequest {
    pub name: String,
    /// The arr integration to ask, by id: `radarr`, `sonarr`.
    pub arr: String,
    /// The lane Singularr scores the name in. Left out: the first lane mapped to that arr.
    #[serde(default)]
    pub lane: Option<String>,
}

/// One name, read by Singularr and by an arr.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ShadowParseResponse {
    pub name: String,
    pub arr: String,
    pub lane: Option<String>,
    #[cfg_attr(feature = "ssr", schema(value_type = crate::core::schema::ParsedReleaseDoc))]
    pub singularr: ParsedRelease,
    pub singularr_reading: SingularrReading,
    pub arr_parse: ArrParse,
    pub arr_reading: ArrReading,
    /// Any of `quality`, `group`, `edition`, `score`.
    pub differs: Vec<String>,
}
