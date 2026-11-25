//! Requests and the discover lists as the API and the web app see them.

use crate::engines::model::DiscoverList;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What the season gate (seerr-bridge) makes of a request. Singularr only reports it: the
/// bridge acts on the Plex site's pending TV requests.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct SeasonGate {
    /// `approve`, `trim`, `decline`, `exempt` (the owner) or `not_gated` (a movie, a 4K
    /// request, or a site the gate does not act on).
    pub result: String,
    /// One sentence that says why.
    pub reason: String,
    /// Seasons of one show a friend may hold at a time.
    pub limit: i32,
    /// The seasons asked for.
    pub asked: Vec<i32>,
    /// Seasons of this show the requester already holds through earlier requests on the
    /// same site.
    pub held: Vec<i32>,
    /// The seasons that pass.
    pub approved: Vec<i32>,
    /// The seasons the gate declines (the requester is told by email).
    pub declined: Vec<i32>,
}

/// The friend-request cleanup (Maintainerr) as it applies to a request. Singularr does not read
/// plays yet, so it shows the rule and the dates the rule gives from the request date.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Cleanup {
    /// The cleanup applies to this request: a friend's TV request on a site it acts on, with
    /// something in the library.
    pub applies: bool,
    /// One sentence that says why, or why not.
    pub reason: String,
    /// A season watched to the end leaves this many days later.
    pub watched_days: i32,
    /// A season with no play for this many days enters the cleanup.
    pub unplayed_days: i32,
    /// Days between entering the cleanup for no play and the removal.
    pub unplayed_removal_days: i32,
    /// False: Singularr does not know plays yet, so the watched date and the last play are
    /// unknown, and the dates below count from the request.
    pub plays_known: bool,
    /// The date the days count from: the request date.
    pub counts_from: Option<DateTime<Utc>>,
    /// When a season that is never played enters the cleanup.
    pub unplayed_from: Option<DateTime<Utc>>,
    /// When a season that is never played leaves the library.
    pub leaves_if_unplayed: Option<DateTime<Utc>>,
}

/// One request on a Seerr site, with what Singularr derives from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Request {
    pub id: i64,
    /// The media server the request is for: `plex`, the one this version serves.
    pub site: String,
    /// Seerr's id of the request on that site.
    pub seerr_id: i64,
    /// `movie` or `show`.
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub is_4k: bool,
    /// The one word the Requests page shows: `pending`, `approved`, `processing`,
    /// `available`, `removed` (it was in the library and has left), `declined`, `failed` or
    /// `unknown`.
    pub status: String,
    /// Seerr's request status: `pending`, `approved`, `declined`, `failed`, `completed` or
    /// `unknown`.
    pub request_status: String,
    /// Seerr's media status for the request's tier: `unknown`, `pending`, `processing`,
    /// `partially_available`, `available`, `blocklisted` or `deleted`.
    pub media_status: String,
    /// Who asked: a display name, never an email address.
    pub requested_by: String,
    /// The requester's account is one of the owner's (`SINGULARR_REQUEST_OWNERS`, by site and
    /// Seerr user id).
    pub owner: bool,
    /// When the request was made.
    pub created_at: DateTime<Utc>,
    /// When Seerr last changed it.
    pub updated_at: Option<DateTime<Utc>>,
    /// The seasons asked for. Empty for a movie.
    pub seasons: Vec<i32>,
    /// The title as Seerr has it. Absent until the title is looked up.
    pub title: Option<String>,
    pub year: Option<i32>,
    /// TMDB images: the poster at w342, the backdrop at w1280.
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    /// The catalog title, when Singularr holds it (matched by TMDB id, then TVDB id).
    pub title_id: Option<i64>,
    pub gate: SeasonGate,
    pub cleanup: Cleanup,
    /// When Singularr first saw the request, and when it last changed the row.
    pub first_seen_at: DateTime<Utc>,
    pub synced_at: DateTime<Utc>,
}

/// Filters of `GET /api/v1/requests`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct RequestQuery {
    /// One status or several separated by commas: `pending`, `approved`, `processing`,
    /// `available`, `removed`, `declined`, `failed`, `unknown`.
    pub status: Option<String>,
    /// `plex`, the media server this version serves.
    pub site: Option<String>,
    /// The requester's display name, any case.
    pub person: Option<String>,
    /// `movie` or `show` (`tv` reads as `show`).
    pub kind: Option<String>,
    /// Words of the title.
    pub q: Option<String>,
    /// Requests for this catalog title.
    pub title: Option<i64>,
    /// `created` (the default: newest first), `updated` or `title`. Prefix `-` to reverse.
    pub sort: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// Requests per status, under every filter but `status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct RequestCounts {
    pub all: i64,
    pub pending: i64,
    pub approved: i64,
    pub processing: i64,
    pub available: i64,
    pub removed: i64,
    pub declined: i64,
    pub failed: i64,
    pub unknown: i64,
}

/// One requester, with the number of requests under every filter but `person`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Requester {
    pub name: String,
    pub owner: bool,
    pub requests: i64,
}

/// The answer of `GET /api/v1/requests`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RequestPage {
    pub items: Vec<Request>,
    pub page: u32,
    pub per_page: u32,
    /// Requests that match the filters, over all pages.
    pub total: i64,
    pub counts: RequestCounts,
    /// Everyone who asked, most requests first.
    pub people: Vec<Requester>,
}

/// Another request for the same title, on either site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RelatedRequest {
    pub id: i64,
    pub site: String,
    pub status: String,
    pub requested_by: String,
    pub is_4k: bool,
    pub seasons: Vec<i32>,
    pub created_at: DateTime<Utc>,
}

/// The answer of `GET /api/v1/requests/{id}`: the request, and the other requests for the
/// same title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RequestDetail {
    #[serde(flatten)]
    pub request: Request,
    pub related: Vec<RelatedRequest>,
}

/// Filters of `GET /api/v1/discover`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct DiscoverQuery {
    /// One list: `trending`, `popular_movies`, `popular_shows` or `upcoming_movies`. Left
    /// out: all four.
    pub list: Option<String>,
    /// Seerr's page of the list, from 1; 20 titles a page.
    pub page: Option<u32>,
}

/// One title of a discover list, with what Singularr holds of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct DiscoverItem {
    /// `movie` or `show`.
    pub kind: String,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    /// TMDB's rating, 0 to 10, to one decimal.
    pub rating: Option<f64>,
    pub poster_url: Option<String>,
    pub backdrop_url: Option<String>,
    /// From Singularr's own catalog, downloads and requests: `in_library` (a copy is held),
    /// `downloading`, `requested` (an open request, or an arr wants it) or `none`.
    pub library_status: String,
    /// The catalog title, when Singularr knows it.
    pub title_id: Option<i64>,
    /// The newest open request for it, when there is one.
    pub request_id: Option<i64>,
}

/// One discover list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct DiscoverRow {
    pub list: DiscoverList,
    pub label: String,
    pub items: Vec<DiscoverItem>,
    /// Titles in the whole list, as Seerr counts them.
    pub total: i64,
    pub page: u32,
    pub total_pages: u32,
    /// When Seerr was asked. Lists are kept for 30 minutes.
    pub fetched_at: Option<DateTime<Utc>>,
    /// Why the list is empty, when Seerr could not be asked. A list that was fetched before
    /// is served as it was, with `stale` set.
    pub error: Option<String>,
    pub stale: bool,
}

/// The answer of `GET /api/v1/discover`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct DiscoverResponse {
    /// The request site the lists come from (`plex`), when a Seerr integration is configured.
    pub site: Option<String>,
    /// Each list carries its `items`, its `total` and its page.
    pub lists: Vec<DiscoverRow>,
}

// Singularr Requests (the public face) -----------------------------------------------------------

/// A request asked for on Singularr Requests: an intent, and what the full process made of it.
/// In R1 (shadow) nothing is sent: `outcome.would_send` is what would go to Seerr.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct RequestIntent {
    pub id: i64,
    /// Always `plex` in R1.
    pub site: String,
    /// The requester's display name on the Plex site (never an address).
    pub requested_by: String,
    /// The requester's Seerr account, which a forwarded request would go out as.
    pub seerr_user_id: i64,
    /// `movie` or `show`.
    pub kind: String,
    pub tmdb_id: i64,
    /// The seasons asked for; empty for a movie.
    pub seasons: Vec<i32>,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub asked_at: DateTime<Utc>,
    /// `new` (not picked up yet), `shadow` (what would be sent is recorded) or `failed`.
    pub state: String,
    pub processed_at: Option<DateTime<Utc>>,
    /// The season gate's outcome (`gate`), what would be sent (`would_send`, absent when the
    /// gate declines everything) and `sent`, false in R1.
    #[cfg_attr(feature = "ssr", schema(value_type = Option<Object>))]
    pub outcome: Option<serde_json::Value>,
}

/// Filters of `GET /api/v1/requests/intents`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct IntentQuery {
    /// `new`, `shadow` or `failed`.
    pub state: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// The signed-in person of a face request, from the database's session view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct FaceSession {
    pub plex_account_id: i64,
    pub display_name: String,
    pub site: String,
    pub seerr_user_id: i64,
    pub expires_at: DateTime<Utc>,
    pub is_owner: bool,
}

/// The plexseerr account of one Plex account: the sign-in check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct FaceAccount {
    pub plex_account_id: i64,
    pub display_name: String,
    pub seerr_user_id: i64,
    pub is_owner: bool,
}

/// One title of a discover list as the face shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct FaceDiscoverItem {
    pub list: String,
    pub position: i32,
    /// `movie` or `show`.
    pub kind: String,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub rating: Option<f64>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub fetched_at: DateTime<Utc>,
}

/// The request rules as the face states them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct FaceRules {
    pub max_seasons: i32,
    pub gated_sites: Vec<String>,
    /// The owner face, for the link the owner sees.
    pub owner_url: Option<String>,
}

/// One of the signed-in person's own requests: asked here, or on the Plex site.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct FaceMyRequest {
    /// `intent` (asked here) or `seerr` (on the Plex site).
    pub source: String,
    pub id: i64,
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub seasons: Vec<i32>,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub asked_at: DateTime<Utc>,
    /// An intent's state (`new`, `shadow`, `failed`) or the request's one word.
    pub status: String,
    /// An intent's season gate result and its sentence.
    pub gate: Option<String>,
    pub gate_reason: Option<String>,
    pub poster_path: Option<String>,
    pub is_4k: bool,
}
