//! The answers of the page routes, in the shapes of the web client's sample files
//! (`web/data/*.json`). Field names are the client's; a fact Singularr does not know is left
//! out or null, never made up.

use crate::engines::model::IntegrationView;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use utoipa::ToSchema;

// Shared pieces ---------------------------------------------------------------------------------

/// A title as a card or a table row shows it. `id` is `movie-<tmdb>` or `show-<tmdb>`, null
/// when the title has no TMDB id; `state` is absent when nothing is known of it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TitleCard {
    pub id: Option<String>,
    /// `movie`, `show`, or `track` for music that plays.
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    /// A TMDB image path (`/abc.jpg`) or the full address the manager gives.
    pub poster: Option<String>,
    /// 0 to 10.
    pub rating: Option<f64>,
    /// A word of the status vocabulary: `available`, `partial`, `downloading`, `importing`,
    /// `attention`, `requested`, `missing`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// An episode of a show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EpisodeLine {
    pub season: i32,
    pub number: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

// Workspace ---------------------------------------------------------------------------------------

/// `GET /api/v1/workspace`: what every page of the workspace starts from.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    /// The workspace's name (configuration).
    pub name: String,
    /// `observe` while Singularr may act on no integration: it reads them and changes nothing
    /// on them. `act` once the access of one of them is `act`.
    pub mode: String,
    /// The integrations Singularr may act on. Empty in `observe`.
    pub acting: Vec<Acting>,
    pub user: WorkspaceUser,
    /// The media server's sections, movies first, then shows, music and the rest.
    pub libraries: Vec<WorkspaceLibrary>,
    /// Every integration with its health and what Singularr may do with it.
    pub integrations: Vec<IntegrationView>,
    pub counts: WorkspaceCounts,
    /// The media server, once it has been read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<ServerName>,
    /// The request portal, when its public address is configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub portal: Option<Portal>,
    /// Where Sign out leads (`SINGULARR_SIGN_OUT_URL`); absent when unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sign_out: Option<String>,
    /// What needs attention, newest first, at most 10 (the top bar's bell).
    pub attention: Vec<Attention>,
    /// The policy's profiles.
    pub profiles: Vec<Profile>,
    /// How the interface writes dates and times (the settings of Dates and times).
    pub formats: Formats,
    pub generated: DateTime<Utc>,
}

/// An integration Singularr may act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Acting {
    /// The integration id.
    pub id: String,
    pub name: String,
}

/// How dates and times are written, from the settings in force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Formats {
    /// The IANA name of the zone times are shown in (`general.timeZone`); empty for the
    /// browser's own.
    pub time_zone: String,
    /// `general.dateFormat`: `D MMM YYYY`, `MMM D, YYYY`, `YYYY-MM-DD`, `DD/MM/YYYY` or
    /// `MM/DD/YYYY`.
    pub date: String,
    /// `general.timeFormat`: `24h` or `12h`.
    pub time: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WorkspaceUser {
    /// The signed-in address, or the API key's name.
    pub name: String,
    /// `owner`, or `key` for a program with an API key.
    pub role: String,
}

/// One section of the media server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLibrary {
    pub id: String,
    /// `movie`, `show`, `music` or `homevideo` (a video section no manager looks after).
    pub kind: String,
    pub name: String,
    /// The section's key on the server.
    pub plex_key: Option<String>,
    /// The integration that manages the section.
    pub manager: Option<String>,
    pub locations: Vec<String>,
    /// Movies and shows: the titles its manager lists. Music: albums.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub titles: Option<i64>,
    /// Movies: titles with a file. Shows: episodes with a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub have: Option<i64>,
    /// Bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Shows: episodes counted toward completion.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episodes: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub albums: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artists: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracks: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceCounts {
    /// Items that need attention: stuck downloads, downloads no manager owns, integrations
    /// that stopped answering, processing runs that are stuck, failed or were refused.
    pub attention: i64,
    /// Downloads in motion: downloading, importing, or waiting for a manager to own them.
    pub queue: i64,
    /// Requests waiting for a decision.
    pub requests_pending: i64,
    /// Entries of the managers' wanted lists (missing and below the cutoff). Absent when the
    /// managers did not answer in time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wanted: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ServerName {
    /// The server's own name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Portal {
    pub name: String,
    pub url: String,
}

/// One thing that needs attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Attention {
    /// `q-<hash>` for a download, the integration's id for an integration, the run's id for a
    /// processing run.
    pub id: String,
    /// `queue`, `integration` or `processing`.
    pub kind: String,
    /// A processing run: its title's name (with the episode, `S01 E03`), or its source's file
    /// name when no title holds it.
    pub title: String,
    pub detail: String,
    /// `bad` or `warn`. A processing run that is stuck is `warn`; one that failed or was
    /// refused in the last seven days is `bad`.
    pub tone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<DateTime<Utc>>,
}

/// A profile of the policy (a lane in the code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    pub id: String,
    /// The manager's profile the lane stands for, else the lane's label.
    pub name: String,
    pub label: String,
    /// `movie` or `show`, from the managers the lane stands for.
    pub kind: Option<String>,
    /// The score at which a held file is good enough. Absent: no cutoff.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<i32>,
    pub min_score: i32,
    pub upgrade_step: i32,
}

// Library tables -----------------------------------------------------------------------------

/// `GET /api/v1/library/movies` and `/library/shows`: one row per title, the values in the
/// order of `columns`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LibraryTable {
    pub columns: Vec<String>,
    #[schema(value_type = Vec<Vec<Object>>)]
    pub rows: Vec<Vec<serde_json::Value>>,
}

// Activity ----------------------------------------------------------------------------------------

/// The answer of `GET /api/v1/activity/queue`, `/history` and `/wanted`: the part asked for,
/// each row with its kind; the other parts are absent.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Activity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue: Option<Vec<QueueRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history: Option<Vec<HistoryRow>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wanted: Option<Vec<WantedRow>>,
}

/// One download the server tracks.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct QueueRow {
    /// `q-<download id>`: a torrent's hash, a usenet client's job id, lower case.
    pub id: String,
    /// `movie` or `show`.
    pub kind: String,
    pub title: TitleCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeLine>,
    /// A whole season.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pack: Option<Pack>,
    /// The edition the release name states.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edition: Option<String>,
    /// The release name.
    pub release: String,
    /// `1080p WEB-DL`.
    pub quality: Option<String>,
    /// Lines: 1080, 2160.
    pub resolution: Option<i32>,
    pub source: Option<String>,
    pub codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr: Option<String>,
    pub group: Option<String>,
    /// The indexer the release came from, as the manager's queue named it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexer: Option<String>,
    /// The download client's name.
    pub client: Option<String>,
    /// The manager that owns the download.
    pub manager: Option<String>,
    /// `downloading`, `importing`, `seeding` (a torrent that is done), `completed` and `failed`
    /// (a usenet download its client is done with), or `unassigned` (no manager owns it).
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attention: Option<QueueAttention>,
    /// A finished download its manager refused to import: who refused, why, and the two
    /// scores and custom formats when the manager's message gives them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refusal: Option<crate::acquire::model::Refusal>,
    /// 0 to 100.
    pub progress: f64,
    pub size: i64,
    /// Bytes downloaded.
    pub done: i64,
    /// While it downloads, as the client last said: bytes per second, seconds left, and the
    /// seeds and peers a torrent is connected to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seeds: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peers: Option<i64>,
    pub added: Option<DateTime<Utc>>,
    /// The manager's profile of the title.
    pub profile: Option<String>,
    /// The release's score in the title's profile, with the rules that gave it.
    pub score: Option<i32>,
    pub trace: Vec<TraceStep>,
    /// What the journal saw happen to the download, oldest first.
    pub timeline: Vec<TimelineStep>,
    /// `r-<id>`: an open request for the title.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Pack {
    pub season: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct QueueAttention {
    /// What needs attention, in the interface's words: `Radarr refused the import · 300
    /// against 600`.
    pub reason: String,
    pub since: Option<DateTime<Utc>>,
    /// The manager's own sentence, where `reason` is Singularr's reading of it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TraceStep {
    pub label: String,
    pub points: i32,
    pub why: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TimelineStep {
    pub at: DateTime<Utc>,
    /// A word of the status vocabulary.
    pub stage: String,
    pub detail: String,
}

/// One record of a manager's history.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct HistoryRow {
    /// `h-<integration>-<the manager's record id>`.
    pub id: String,
    pub at: DateTime<Utc>,
    pub kind: String,
    pub title: TitleCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeLine>,
    /// `grabbed`, `imported`, `upgraded`, `failed` or `removed`.
    pub event: String,
    /// The manager's quality name: `WEBDL-1080p`.
    pub quality: Option<String>,
    /// For an upgrade: the quality of the file it replaced.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub source: Option<String>,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexer: Option<String>,
}

/// One entry of a manager's wanted lists.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WantedRow {
    /// `w-<integration>-<missing|cutoff>-<the manager's id>`.
    pub id: String,
    pub kind: String,
    pub title: TitleCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeLine>,
    /// `missing`, or `upgrade` (held below the profile's cutoff).
    pub why: String,
    /// When the movie came out for a home library, or the episode aired.
    pub released: Option<NaiveDate>,
    pub profile: Option<String>,
    /// The manager's quality name of the file held.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub have: Option<String>,
    /// What the profile wants: its cutoff quality, as the manager names qualities.
    pub want: Option<String>,
    pub last_search: Option<DateTime<Utc>>,
    pub monitored: bool,
}

// Requests ----------------------------------------------------------------------------------------

/// `GET /api/v1/requests`: every request, newest first, and the people who ask.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RequestsPage {
    pub requests: Vec<RequestItem>,
    pub people: Vec<Person>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RequestItem {
    /// `r-<id>`.
    pub id: String,
    pub title: TitleCard,
    pub what: What,
    /// `HD` or `4K`.
    pub target: String,
    pub by: By,
    pub at: DateTime<Utc>,
    /// `requested` (waiting for a decision), `approved`, `downloading`, `importing`,
    /// `attention`, `partial`, `available`, `missing` (it left the library), `declined`,
    /// `failed`.
    pub status: String,
    /// 0 to 100, while it downloads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<f64>,
    /// One sentence a rule adds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub rules: Vec<RuleLine>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<SeasonCount>,
    /// `q-<hash>`: the downloads for the title.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<String>,
}

/// What was asked for: `{type: "movie"}` or `{type: "seasons", seasons: [1, 2]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct What {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seasons: Option<Vec<i32>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct By {
    /// `u-<the request site's account id>`.
    pub id: Option<String>,
    /// A display name, never an address.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RuleLine {
    pub label: String,
    /// `pass`, `fail` or `skip`.
    pub result: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SeasonCount {
    pub seasons: i64,
}

/// Someone who can request, or did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Person {
    /// `u-<the request site's account id>`.
    pub id: String,
    pub name: String,
    /// `owner` or `member`.
    pub role: String,
    pub requests: i64,
}

// Calendar ----------------------------------------------------------------------------------------

/// `GET /api/v1/calendar`: episodes airing and movies releasing, from the managers'
/// calendars.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Calendar {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub today: NaiveDate,
    pub items: Vec<CalendarItem>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CalendarItem {
    pub date: NaiveDate,
    /// When an episode airs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<DateTime<Utc>>,
    pub kind: String,
    pub title: TitleCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeLine>,
    /// Movies: `theaters`, `digital` or `physical`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// `available`, `downloading`, `importing`, `attention`, `unaired`, `unreleased`,
    /// `missing`.
    pub state: String,
}

// Home --------------------------------------------------------------------------------------------

/// `GET /api/v1/home`: the state of the server on one page.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Home {
    pub stats: Vec<Stat>,
    /// Requests waiting, downloads that need attention, integrations that stopped answering,
    /// processing runs that are stuck, failed or were refused, newest first.
    pub attention: Vec<HomeAttention>,
    pub added: Added,
    /// The next seven days of the calendar.
    pub upcoming: Vec<CalendarItem>,
    /// Downloads in motion, by kind.
    pub queue: QueueCounts,
    /// `r-<id>` of the requests waiting for a decision, newest first.
    pub requests: Vec<String>,
    /// What plays on the media server now.
    pub now_playing: Vec<Playing>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Stat {
    pub id: String,
    pub label: String,
    pub value: i64,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct HomeAttention {
    pub id: String,
    /// `request`, `queue`, `integration` or `processing`.
    pub kind: String,
    pub title: String,
    pub reason: String,
    pub since: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Added {
    pub movies: Vec<AddedMovie>,
    pub episodes: Vec<AddedEpisode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AddedMovie {
    #[serde(flatten)]
    pub card: TitleCard,
    pub added: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AddedEpisode {
    pub added: DateTime<Utc>,
    pub episode: EpisodeLine,
    pub title: TitleCard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct QueueCounts {
    pub movies: i64,
    pub shows: i64,
}

/// Something that plays on the media server.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Playing {
    /// `s-<n>`.
    pub id: String,
    pub title: TitleCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeLine>,
    /// Who watches, by their name on the media server.
    pub user: Option<PlayingUser>,
    pub player: Option<String>,
    /// 0 to 100.
    pub progress: Option<f64>,
    /// `Direct play`, `Direct stream` or `Transcode`.
    pub stream: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlayingUser {
    pub name: String,
}
