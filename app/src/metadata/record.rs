//! The metadata records the interface reads, in the shapes of the web client's sample set
//! (`web/data/titles`, `people`, `collections.json`, `browse`): cards, title, credits, season,
//! person and collection records, browse lists and the search answer. Field names are the
//! client's (`airDate`, `knownFor`, `inLibrary`). Images are TMDB paths (`/abc.jpg`), times ISO
//! 8601, sizes bytes, runtimes minutes, resolutions lines (1080, 2160).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Every poster in a row or grid. `state` is absent for a title outside the library that no
/// request names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Card {
    /// `movie-<tmdb>` or `show-<tmdb>`.
    pub id: String,
    /// `movie` or `show`.
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    /// TMDB's vote average, 0 to 10.
    pub rating: Option<f64>,
    /// One word of the status vocabulary: `available`, `partial`, `requested`, `approved`,
    /// `downloading`, `missing`, `unaired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// On a person's cast credits: the character.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// On a person's crew credits: the job.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
}

impl Card {
    /// The kind and TMDB id the card's id names: `movie-603` is (`movie`, 603).
    pub fn key(&self) -> Option<(String, i64)> {
        let (kind, id) = self.id.split_once('-')?;
        Some((kind.to_string(), id.parse().ok()?))
    }
}

/// A rating 0 to 10 and the votes behind it; `votes` is null where the source does not say
/// (IMDb's rating comes from the manager without its votes).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Score {
    pub score: f64,
    pub votes: Option<i64>,
}

/// Rotten Tomatoes, as Plex reports it: percentages, with the image keys that say fresh,
/// rotten, upright or spilled.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RottenTomatoes {
    pub critics: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<i32>,
    /// `rottentomatoes://image.rating.ripe`, `.rotten`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critics_image: Option<String>,
    /// `rottentomatoes://image.rating.upright`, `.spilled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience_image: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Ratings {
    pub tmdb: Option<Score>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<Score>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rt: Option<RottenTomatoes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metacritic: Option<i32>,
}

/// Addresses out, each a full URL.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Links {
    pub tmdb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trakt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub letterboxd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

/// Release dates (a movie: `theatrical`, `digital`, `physical`) or air dates (a show:
/// `first`, `last`, `next`), as `YYYY-MM-DD`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Dates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theatrical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub digital: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

/// A studio or a network. The id is TMDB's company or network id; `browse` is the key of its
/// list (`studio-420`, `network-49`, for `GET /api/v1/discover/{key}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Company {
    pub id: i64,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browse: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CollectionRef {
    /// `collection-<tmdb>`.
    pub id: String,
    pub name: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Video {
    /// `YouTube` or `Vimeo`.
    pub site: String,
    pub key: String,
    pub name: String,
    /// `Trailer`, `Teaser`, `Clip`, `Featurette`, `Behind the Scenes`.
    #[serde(rename = "type")]
    pub kind: String,
}

/// A service the title streams on in the configured region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct WatchProvider {
    pub name: String,
    pub logo: Option<String>,
}

/// One person and one job of a title's crew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CrewCredit {
    /// `person-<tmdb>`.
    pub id: String,
    pub name: String,
    pub job: String,
    /// The department: `Directing`, `Writing`, `Production`, `Sound`, `Camera`, `Editing`.
    pub dept: String,
    pub photo: Option<String>,
    /// A show: the episodes the person had this job on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes: Option<i32>,
}

/// One person of a title's cast, in billing order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CastCredit {
    /// `person-<tmdb>`.
    pub id: String,
    pub name: String,
    /// The character; a show joins the characters a person played, most episodes first.
    pub role: String,
    pub photo: Option<String>,
    /// A show: the episodes the person is in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episodes: Option<i32>,
}

/// Everything of a title's cast and crew (`GET /api/v1/metadata/titles/{kind}/{tmdb}/credits`):
/// the cast in billing order, the crew with the key jobs first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TitleCredits {
    /// `movie-<tmdb>` or `show-<tmdb>`.
    pub id: String,
    pub cast: Vec<CastCredit>,
    pub crew: Vec<CrewCredit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct PersonRef {
    /// `person-<tmdb>`.
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub photo: Option<String>,
}

/// One step of a decision trace, as the interface shows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TraceLine {
    pub label: String,
    pub points: i32,
    pub why: String,
}

/// One file of a movie in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LibraryFile {
    /// The name of the library the file is in: the Plex section, else the manager's.
    pub library: String,
    /// The manager's profile for the file's title.
    pub profile: Option<String>,
    /// Lines: 480, 720, 1080, 2160.
    pub resolution: Option<i32>,
    pub source: Option<String>,
    pub quality: Option<String>,
    pub codec: Option<String>,
    pub bit_depth: Option<i64>,
    pub hdr: Option<String>,
    pub audio: Option<String>,
    pub audio_langs: Vec<String>,
    pub subs: Vec<String>,
    pub size: i64,
    pub runtime: Option<i64>,
    pub fps: Option<f64>,
    pub group: Option<String>,
    pub file_name: String,
    pub scene_name: Option<String>,
    pub added: Option<DateTime<Utc>>,
    pub score: Option<i32>,
    pub formats: Vec<String>,
    pub cutoff_met: bool,
    pub trace: Vec<TraceLine>,
    /// What processing did or does to the file: it is the result of an imported run, or an
    /// open run has it as its source. Null for a file no run names.
    #[serde(default)]
    pub processing: Option<FileProcessing>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct LibraryEdition {
    pub key: String,
    pub label: String,
    pub default: bool,
    pub wanted: bool,
    pub files: Vec<LibraryFile>,
}

/// A show's specials: episodes with a file, and the episodes the manager knows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Specials {
    pub have: i32,
    pub total: i32,
}

/// A season and an episode number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct EpisodeRef {
    pub season: i32,
    pub number: i32,
}

/// One event of a title's history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct HistoryItem {
    pub at: DateTime<Utc>,
    /// `added`, `grabbed`, `downloaded`, `imported`, `upgraded`, `removed`, `failed`.
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeRef>,
    /// The quality as the manager names it: `Bluray-1080p`, `WEBDL-2160p`.
    pub quality: Option<String>,
    /// An upgrade: the quality it replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    pub group: Option<String>,
    /// The source part of the quality: `Bluray`, `WEBDL`.
    pub source: Option<String>,
    pub detail: Option<String>,
}

/// One download of a title that is not done.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct QueueItem {
    /// The torrent hash.
    pub id: String,
    pub kind: String,
    /// The release name.
    pub release: String,
    /// `downloading`, `importing`, `paused` or `queued`.
    pub stage: String,
    /// 0 to 100.
    pub progress: f64,
    pub size: i64,
    /// Bytes done.
    pub done: i64,
    pub added: Option<DateTime<Utc>>,
    /// The manager whose queue has it, by name.
    pub manager: Option<String>,
    /// The download client, by name.
    pub client: Option<String>,
    /// The manager reports a blocked import, a warning or an error.
    pub attention: bool,
    pub reason: Option<String>,
}

/// Who asked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Requester {
    /// `user-<id on the request engine>`.
    pub id: Option<String>,
    pub name: String,
}

/// What a request asks for: `{"type": "movie"}`, `{"type": "show"}` (every season), or
/// `{"type": "seasons", "seasons": [1, 2]}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RequestWhat {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub seasons: Vec<i32>,
}

/// One request of a title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TitleRequest {
    /// `r-<id>`.
    pub id: String,
    pub by: Requester,
    pub what: RequestWhat,
    pub at: DateTime<Utc>,
    /// The request's one word: `pending`, `approved`, `processing`, `available`, `declined`,
    /// `failed`, `removed`.
    pub status: String,
    /// `HD` or `4K`.
    pub target: String,
}

/// Plays on the Plex server: how many, and the last one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Plays {
    pub count: i64,
    pub last: Option<DateTime<Utc>>,
}

/// The title on the Plex server: its item key, and the address that opens it in Plex's web
/// app (built from the server's machine identifier and the key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct PlexLink {
    pub key: String,
    pub url: String,
}

/// What the library has of a title. A movie carries its editions and their files; a show its
/// counts (and the record its seasons).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct LibraryFacts {
    /// `available`, `partial`, `downloading`, `requested` or `missing`.
    pub state: String,
    pub monitored: bool,
    /// The name of the library the title is in: the Plex section, else the manager's.
    pub library: String,
    /// That library's id (`plex-1`, or the manager's id).
    pub library_id: String,
    pub profile: Option<String>,
    pub path: Option<String>,
    pub added: Option<DateTime<Utc>>,
    /// A movie: its editions, each with its files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editions: Option<Vec<LibraryEdition>>,
    /// A movie: the protections that guard one of its files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protections: Option<Vec<String>>,
    /// A show: the size of its files; episodes with a file; episodes the manager knows; the
    /// episodes it counts toward complete (aired, or with a file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub have: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<i32>,
    /// A show: the specials (season 0), counted apart and never in `have` or `total`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specials: Option<Specials>,
    /// Plays on the Plex server; null when the server does not know the title.
    pub plays: Option<Plays>,
    /// The item on the Plex server, when the server pass has matched it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plex: Option<PlexLink>,
}

/// One season of a show on its title record: counts and the episode numbers the library has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeasonSummary {
    pub number: i32,
    pub name: Option<String>,
    pub air_date: Option<String>,
    pub poster: Option<String>,
    /// Episodes with a file.
    pub have: i32,
    /// Episodes TMDB lists.
    pub total: i32,
    /// Episodes aired by today.
    pub aired: i32,
    /// The numbers of the episodes the library has a file of.
    pub in_library: Vec<i32>,
}

/// A rich title record (`GET /api/v1/metadata/titles/{kind}/{tmdb}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TitleRecord {
    /// `movie-<tmdb>` or `show-<tmdb>`.
    pub id: String,
    pub kind: String,
    pub tmdb: i64,
    pub imdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvdb: Option<i64>,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub certification: Option<String>,
    /// Minutes: a movie's runtime, a show's usual episode runtime.
    pub runtime: Option<i32>,
    pub genres: Vec<String>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub ratings: Ratings,
    pub links: Links,
    pub rich: bool,
    /// The title's word in the status vocabulary: what the library has of it, else whether it
    /// is requested or downloading. Absent for a title nobody has or asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub library: Option<LibraryFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    pub tagline: Option<String>,
    pub status: Option<String>,
    pub dates: Dates,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revenue: Option<i64>,
    pub language: Option<String>,
    pub countries: Vec<String>,
    pub studios: Vec<Company>,
    /// The first studio's name (a movie).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub studio: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub networks: Option<Vec<Company>>,
    /// The first network's name (a show).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection: Option<CollectionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    pub videos: Vec<Video>,
    pub watch: Vec<WatchProvider>,
    /// Up to 40 of the crew, the key jobs first: directing, writing, creating, producing,
    /// music, photography, editing. `GET .../credits` has everyone.
    pub crew: Vec<CrewCredit>,
    /// How many crew entries (a person and a job each) the title has in all.
    pub crew_total: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creators: Option<Vec<PersonRef>>,
    /// Up to 40 of the cast in billing order; a show's over all its seasons.
    pub cast: Vec<CastCredit>,
    /// How many people the cast has in all.
    pub cast_total: usize,
    pub recommendations: Vec<Card>,
    pub similar: Vec<Card>,
    /// A show's seasons with what the library holds of each. The episodes are the season
    /// record's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seasons: Option<Vec<SeasonSummary>>,
    /// The title's events, newest first, at most 50. Left out when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryItem>,
    /// Its downloads that are not done.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<QueueItem>,
    /// The requests for it, newest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<TitleRequest>,
    /// The runs of the processing services on its files: the newest 50, with how many are open
    /// and how many there are in all. Null when the installation has no processing
    /// integration; without runs when it has one and nothing was run on this title.
    #[serde(default)]
    pub processing: Option<TitleProcessing>,
    /// Its plays on the media server: how many, by how many people, the hours, the last, and
    /// the newest few (for the owner only). Null when the installation reads no media server
    /// and no play history; zero when it does and nobody played the title.
    #[serde(default)]
    pub plays: Option<TitlePlays>,
}

/// A title's plays (`plays` of the title record).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TitlePlays {
    pub count: i64,
    /// How many people played it.
    pub people: i64,
    pub hours: f64,
    pub last: Option<DateTime<Utc>>,
    /// The newest plays, newest first: for the owner, empty for anyone else.
    pub recent: Vec<TitlePlay>,
}

/// One play of a title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct TitlePlay {
    pub id: i64,
    pub at: DateTime<Utc>,
    /// The person by the media server's account id and name.
    pub person: PlayPerson,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeRef>,
    pub watched_seconds: Option<i32>,
    /// 0 to 100.
    pub progress: Option<f64>,
    pub player: Option<String>,
    pub platform: Option<String>,
    /// `direct play`, `direct stream` or `transcode`.
    pub decision: Option<String>,
}

/// A person of the media server, by its account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct PlayPerson {
    pub id: Option<i64>,
    pub name: Option<String>,
}

/// The file of an episode in the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct EpisodeFile {
    pub quality: Option<String>,
    /// Lines: 720, 1080, 2160.
    pub resolution: Option<i32>,
    pub source: Option<String>,
    pub codec: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr: Option<String>,
    pub audio: Option<String>,
    pub size: i64,
    pub group: Option<String>,
    pub score: Option<i32>,
    pub added: Option<DateTime<Utc>>,
    pub file_name: String,
    pub formats: Vec<String>,
    pub cutoff_met: bool,
    /// What processing did or does to the file, as on a movie's file. Null for a file no run
    /// names.
    #[serde(default)]
    pub processing: Option<FileProcessing>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct EpisodeRecord {
    pub number: i32,
    pub title: Option<String>,
    pub air_date: Option<String>,
    pub runtime: Option<i32>,
    pub overview: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still: Option<String>,
    /// `available`, `downloading`, `missing` or `unaired`.
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<EpisodeFile>,
}

/// One season with its episodes (`GET /api/v1/metadata/shows/{tmdb}/seasons/{n}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SeasonRecord {
    /// `show-<tmdb>`.
    pub show: String,
    pub number: i32,
    pub name: Option<String>,
    pub air_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    pub overview: Option<String>,
    pub have: i32,
    pub total: i32,
    pub episodes: Vec<EpisodeRecord>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct PersonLinks {
    pub tmdb: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imdb: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

/// A person's credits as cards, most popular first: a cast card carries `role`, a crew card
/// `job`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Credits {
    pub cast: Vec<Card>,
    pub crew: Vec<Card>,
}

/// A person (`GET /api/v1/metadata/people/{id}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PersonRecord {
    /// `person-<tmdb>`.
    pub id: String,
    pub name: String,
    pub photo: Option<String>,
    /// `Acting`, `Directing`, `Writing`.
    pub known_for: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub born: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub died: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub birthplace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub biography: Option<String>,
    pub aka: Vec<String>,
    pub links: PersonLinks,
    pub credits: Credits,
    pub rich: bool,
}

/// A collection (`GET /api/v1/metadata/collections/{id}`), its parts in release order, each
/// with its library state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CollectionRecord {
    /// `collection-<tmdb>`.
    pub id: String,
    pub name: String,
    pub overview: Option<String>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub parts: Vec<Card>,
    /// False while only the library's own parts are known (TMDB's record is not fetched yet).
    pub rich: bool,
}

/// The collections the library has a part of (`GET /api/v1/collections`), by id.
pub type CollectionMap = BTreeMap<String, CollectionRecord>;

/// A browse list (`GET /api/v1/discover/{list}`): `{title, kind, items}`, with the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct BrowseList {
    /// The list's key: `trending`, `genre-movie-28`, `studio-420`.
    pub list: String,
    pub title: String,
    /// `movie`, `show`, or `all` for a list of both.
    pub kind: String,
    pub items: Vec<Card>,
    pub page: u32,
    pub total_pages: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct PersonCard {
    /// `person-<tmdb>`.
    pub id: String,
    pub name: String,
    pub photo: Option<String>,
    pub known_for: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CollectionCard {
    /// `collection-<tmdb>`.
    pub id: String,
    pub name: String,
    pub poster: Option<String>,
}

/// An album found by a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct AlbumCard {
    /// `album-<rating key>`.
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    /// `artist-<rating key>`.
    pub artist_id: Option<String>,
    pub year: Option<i32>,
    /// An address the browser can load.
    pub cover: Option<String>,
}

/// An artist found by a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ArtistCard {
    /// `artist-<rating key>`.
    pub id: String,
    pub name: String,
    /// An address the browser can load.
    pub photo: Option<String>,
}

/// One answer for the search field (`GET /api/v1/search?q=`): what the library has first
/// (movies and shows, each with its state), titles outside it (with their state when they
/// are requested or downloading), people, collections, and the music the library has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct SearchAnswer {
    pub query: String,
    pub library: Vec<Card>,
    pub titles: Vec<Card>,
    pub people: Vec<PersonCard>,
    pub collections: Vec<CollectionCard>,
    #[serde(default)]
    pub albums: Vec<AlbumCard>,
    #[serde(default)]
    pub artists: Vec<ArtistCard>,
}

/// One title of the Discover billboard: a trending title with its backdrop and the facts the
/// billboard shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Billboard {
    /// `movie-<tmdb>` or `show-<tmdb>`.
    pub id: String,
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub logo: Option<String>,
    pub overview: Option<String>,
    pub tagline: Option<String>,
    pub certification: Option<String>,
    /// Minutes.
    pub runtime: Option<i32>,
    pub genres: Vec<String>,
    pub rating: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// One row of Discover; its `id` is the key of its full list (`GET /api/v1/discover/{id}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct DiscoverRow {
    pub id: String,
    pub title: String,
    pub items: Vec<Card>,
}

/// A genre, a studio or a network as Discover shows it; `key` is its list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct BrowseTile {
    /// TMDB's id of the genre, company or network.
    pub id: i64,
    /// `genre-movie-28`, `studio-420`, `network-49`.
    pub key: String,
    pub name: String,
    /// A genre: the backdrop of a title in it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<String>,
    /// A studio or a network: its logo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct DiscoverGenres {
    pub movie: Vec<BrowseTile>,
    pub show: Vec<BrowseTile>,
}

/// Discover (`GET /api/v1/discover`): the billboard, the rows (trending, popular, upcoming,
/// on the air, recent requests), the genres, and the studios and networks the library has
/// most titles of.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct DiscoverPage {
    pub billboard: Vec<Billboard>,
    pub rows: Vec<DiscoverRow>,
    pub genres: DiscoverGenres,
    pub studios: Vec<BrowseTile>,
    pub networks: Vec<BrowseTile>,
}

/// What started a processing run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RunTrigger {
    /// `rule` (an import rule), `backlog`, `tag` (a tag on the title in its manager), `manual`,
    /// `api` (sent on demand, from Singularr or by another program) or `other`.
    pub kind: String,
    /// The same in words: `Import rule`, `By hand`, `Singularr`, `API`, or `Tag` and the
    /// tag's name.
    pub label: String,
    /// The service's own text.
    pub raw: String,
}

/// A file a processing run names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RunFile {
    /// The file's name.
    pub file: String,
    /// Where it is, as the service names it.
    pub path: String,
    /// Bytes; null when the service does not say.
    pub size: Option<i64>,
}

/// A file an imported result replaced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RunReplaced {
    pub file: String,
    pub path: String,
    pub size: Option<i64>,
    /// Whether it was still on disk when the service looked.
    pub still_there: Option<bool>,
}

/// One event of a processing run, in the service's words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RunEvent {
    pub at: DateTime<Utc>,
    pub event: String,
    /// The part of the service that did it.
    pub actor: Option<String>,
    /// The first of the service's note, reason, error or detail that is set.
    pub note: Option<String>,
}

/// A worker that is busy now. Every field but the service is as the worker's service reports
/// it, and null when it does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RunWorker {
    /// The integration id of the worker's service.
    pub service: String,
    /// The node it runs on, by name.
    pub node: Option<String>,
    /// The path it works on.
    pub file: Option<String>,
    /// 0 to 100.
    pub percent: Option<f64>,
    pub fps: Option<f64>,
    /// The time left, as the service writes it: `0:12:00`.
    pub eta: Option<String>,
    pub status: Option<String>,
    /// The kind of worker, as the service names it.
    pub kind: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
}

/// A library of a worker service, by the worker's id of it and its name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RunLibrary {
    pub id: String,
    pub name: String,
}

/// A flow of a worker service: what it does to a file, step by step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct RunFlow {
    pub id: String,
    pub name: String,
    /// What it does, in its author's words. Only where one run is read by its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
    /// The names of its steps, in order. Only where one run is read by its id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<String>>,
}

/// Where a run is in the worker's own records: the library whose folder its file waits in, the
/// flow that library runs and, for an open run, where the file stands there now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct RunInWorker {
    /// The integration id of the worker service.
    pub service: String,
    /// The worker library whose folder is the longest one the run's queue path lies below.
    pub library: Option<RunLibrary>,
    /// That library's flow.
    pub flow: Option<RunFlow>,
    /// Asked now, for an open run only: `queued`, `working` (a worker is on it), `done`,
    /// `failed`, `notRequired`, `held` (the worker holds it back for a while) or `absent` (the
    /// worker has no record of the file). Null for a closed run, and when it was not asked.
    pub state: Option<String>,
    /// The same in the worker's own words.
    pub words: Option<String>,
    /// When the worker was asked.
    pub checked_at: Option<DateTime<Utc>>,
}

/// One run of a processing service on one file: a job that rewrites the file after its import
/// (a conversion, a re-encode, an audio rebuild) and has the result imported in its place.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProcessingRun {
    /// `<integration id>:<the service's id of the job>`.
    pub id: String,
    /// The integration id of the service whose job it is.
    pub service: String,
    /// The flow, by the service's own id of it, and in the words of the service's adapter.
    pub flow: String,
    pub flow_label: String,
    pub trigger: RunTrigger,
    /// `staged` (waiting for a worker), `working` (a worker is on it), `verifying` (what the
    /// worker made is being checked), `imported`, `failed`, `refused`, `pruned` or
    /// `superseded`.
    pub state: String,
    /// When it started.
    pub at: DateTime<Utc>,
    /// When the service last changed it.
    pub updated: Option<DateTime<Utc>>,
    /// When it closed; null while it is open.
    pub finished: Option<DateTime<Utc>>,
    /// Seconds from `at` to `finished`.
    pub took: Option<i64>,
    /// The episode its file name reads as, for a run linked to a show.
    pub episode: Option<EpisodeRef>,
    pub source: Option<RunFile>,
    /// What the worker made, before it is imported.
    pub output: Option<RunFile>,
    /// The imported file.
    pub result: Option<RunFile>,
    /// Bytes saved: the source's size less the result's, for an imported run.
    pub saved: Option<i64>,
    pub replaced: Vec<RunReplaced>,
    /// Why it was refused or failed, in the service's words.
    pub detail: Option<String>,
    /// Open for longer than the stuck threshold with no worker on it.
    pub stuck: bool,
    /// The worker that is on it.
    pub worker: Option<RunWorker>,
    /// Oldest first.
    pub events: Vec<RunEvent>,
    /// Where it is in the worker's own records. Null when no worker is configured or nothing
    /// is known.
    #[serde(default)]
    pub in_worker: Option<RunInWorker>,
    /// A stuck run: why, in one sentence, when the worker's records say. Null otherwise.
    #[serde(default)]
    pub stuck_why: Option<String>,
}

/// The processing runs linked to a title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TitleProcessing {
    /// Newest first, at most 50.
    pub runs: Vec<ProcessingRun>,
    /// How many of all its runs are open.
    pub open: i64,
    /// How many runs it has in all.
    pub total: i64,
}

/// What processing did or does to a file of the library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct FileProcessing {
    /// `processed`: the file is the result of an imported run. `processing`: an open run has
    /// the file as its source.
    pub state: String,
    /// The id of that run.
    pub run: String,
}
