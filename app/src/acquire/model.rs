//! The answers of search, grab and import. Field names are the web client's.

use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

/// `GET /api/v1/titles/{id}/releases`: what one search found, each release with Singularr's
/// verdict beside the manager's, and the files held now.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseList {
    pub title: SearchTitle,
    /// The manager that searched.
    pub manager: SearchManager,
    /// What was searched for: the movie (both null), a season, or one episode.
    pub scope: SearchScope,
    /// When the search answered.
    pub searched: DateTime<Utc>,
    /// How long it took, in milliseconds.
    pub took: i64,
    /// The quality profile the releases are judged in. Null when the title has none in force.
    pub profile: Option<ProfileRef>,
    /// The files held now for what was searched, each with its score in both.
    pub held: Vec<HeldView>,
    pub counts: ReleaseCounts,
    /// Accepted by Singularr first, then by Singularr's score.
    pub releases: Vec<ReleaseView>,
}

/// The title a search, a grab or an import is for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SearchTitle {
    /// `movie-603`, `show-1396`; null for a title without a TMDB id.
    pub id: Option<String>,
    /// `movie` or `show`.
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SearchManager {
    /// The integration id.
    pub id: String,
    pub name: String,
    /// What Singularr may do with it: `read` or `act`. A grab needs `act`.
    pub access: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SearchScope {
    pub season: Option<i32>,
    pub episode: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRef {
    pub id: String,
    pub label: String,
    pub min_score: i32,
    /// The score at which a held file is good enough. Null: no cutoff.
    pub cutoff: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseCounts {
    pub total: usize,
    /// Releases Singularr would take.
    pub accepted: usize,
    /// Releases the manager would take.
    pub manager_accepted: usize,
}

/// One line of a score: the rule, its points, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TraceLine {
    pub label: String,
    pub points: i32,
    pub why: String,
}

/// One reason, as a label: `code` is stable, `label` is the interface's word.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Chip {
    /// `no_file`, `better_quality`, `better_score`, `held_outside`, `prerelease`,
    /// `unknown_quality`, `not_in_profile`, `below_minimum`, `protected`, `lower_quality`,
    /// `cutoff_met`, `not_better`; `trait` for a rule that took points away.
    pub code: String,
    pub label: String,
}

/// A file held now, scored by Singularr and by the manager.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HeldView {
    /// The file's id in the catalog; what a release's `against` names.
    pub copy_id: i64,
    /// The file name.
    pub file: String,
    /// The release name the file was scored by.
    pub release: String,
    pub season: Option<i32>,
    pub episodes: Vec<i32>,
    pub size: i64,
    pub edition: String,
    pub singularr: HeldScore,
    pub manager: ManagerScore,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct HeldScore {
    pub score: i32,
    pub quality: Option<String>,
    pub trace: Vec<TraceLine>,
}

/// What a manager says of a file or a release: its quality name, its custom formats and their
/// score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ManagerScore {
    pub quality: Option<String>,
    pub score: Option<i32>,
    pub formats: Vec<String>,
}

/// One release a search found.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseView {
    /// With `indexerId`, what a grab names.
    pub guid: String,
    pub indexer_id: i64,
    pub indexer: Option<String>,
    /// The release name.
    pub name: String,
    /// `torrent` or `usenet`.
    pub protocol: Option<String>,
    /// The manager's quality name: `Bluray-1080p`.
    pub quality: Option<String>,
    pub size: Option<i64>,
    /// Days since it was posted.
    pub age: Option<i64>,
    pub published: Option<DateTime<Utc>>,
    pub seeders: Option<i64>,
    pub leechers: Option<i64>,
    pub languages: Vec<String>,
    pub season: Option<i32>,
    pub episodes: Vec<i32>,
    pub full_season: bool,
    pub singularr: SingularrVerdict,
    pub manager: ManagerVerdict,
}

/// Singularr's verdict on a release: parsed, scored in the profile, and decided against the file
/// held now, with its reasons.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct SingularrVerdict {
    /// It would be taken: there is nothing held, or it beats what is held.
    pub accepted: bool,
    /// `grab` (nothing held), `upgrade` or `hold`.
    pub action: String,
    pub score: i32,
    /// Singularr's reading of the name.
    pub quality: Option<String>,
    pub group: Option<String>,
    pub edition: Option<String>,
    /// Ladder position in the profile, 0 is best; null when the profile does not take it.
    pub rank: Option<usize>,
    /// Why, as labels: the outcome first, then rules that took points away.
    pub chips: Vec<Chip>,
    /// Why, as the decision's sentences.
    pub reasons: Vec<String>,
    pub trace: Vec<TraceLine>,
    /// The held file it was compared with (`held[].copyId`).
    pub against: Option<i64>,
}

/// The manager's word on a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ManagerVerdict {
    /// The manager would take it.
    pub accepted: bool,
    pub score: Option<i32>,
    pub formats: Vec<String>,
    /// Why not, in the manager's words.
    pub rejections: Vec<String>,
    /// Only for now (a delay, a client that is down).
    pub temporary: bool,
}

/// What Singularr read from a manager's refusal to import a download.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Refusal {
    /// The manager's name.
    pub by: String,
    /// Why, as a label: `Not a custom format upgrade`, `Not an upgrade`, `Sample`, ...
    pub reason: String,
    /// The download's custom formats and score, when the message gives them.
    pub new: Option<RefusalSide>,
    /// The held file's, when the message gives them.
    pub held: Option<RefusalSide>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RefusalSide {
    pub score: Option<i32>,
    pub formats: Vec<String>,
}

/// What an act on a manager came to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ActAnswer {
    /// `search`, `grab`, `import` or `remove`.
    pub act: String,
    /// The manager's integration id.
    pub manager: String,
    /// The title it was for.
    pub title: Option<SearchTitle>,
    /// What it names: the release grabbed, the download imported or removed, the search
    /// command (`MoviesSearch`, `EpisodeSearch`, `SeasonSearch`, `SeriesSearch`).
    pub detail: String,
    /// The id of the command the manager queued, when it queued one.
    pub command: Option<i64>,
}

/// `POST /api/v1/search`: what each manager was asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct SearchManyAnswer {
    pub sent: Vec<ActAnswer>,
    /// How many titles and episodes were searched for.
    pub count: usize,
}
