//! The catalog as the API and the web app see it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use singularr_core::{ParsedRelease, Verdict};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum TitleKind {
    Movie,
    Show,
}

impl TitleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TitleKind::Movie => "movie",
            TitleKind::Show => "show",
        }
    }

    pub fn parse(s: &str) -> Option<TitleKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "movie" | "movies" => Some(TitleKind::Movie),
            "show" | "shows" | "series" => Some(TitleKind::Show),
            _ => None,
        }
    }
}

/// A library: the set of files one media server plays from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct Library {
    /// The library id. Until libraries come from the media server, the id of the Radarr or
    /// Sonarr integration that manages it.
    pub id: String,
    pub name: String,
    /// `movie` or `show`.
    pub kind: Option<String>,
    /// The integration it belongs to: the Plex server for its sections, a Radarr or Sonarr
    /// integration for the library of its own.
    pub integration: Option<String>,
    /// The section key, for a library of the Plex server.
    pub section_key: Option<String>,
    /// The Radarr or Sonarr integration that manages a section (none for music and home
    /// video).
    pub managed_by: Option<String>,
    /// The section's folders as the server names them.
    pub locations: Vec<String>,
    /// The folders its titles live under, as the engines report them.
    pub root_folders: Vec<String>,
    /// The engines that manage it today.
    pub engines: Vec<String>,
    pub titles: i64,
    pub copies: i64,
    pub size_bytes: i64,
}

/// Filters of `GET /api/v1/titles`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct TitleQuery {
    /// `movie` or `show`.
    pub kind: Option<String>,
    /// A library id: titles that library holds.
    pub library: Option<String>,
    /// Words of the title, or an id: `tmdb:603`, `tvdb:81189`, `tt0133093`.
    pub q: Option<String>,
    /// Titles with a copy in this lane.
    pub lane: Option<String>,
    /// `true`: titles with a protected copy. `false`: titles without one.
    pub protected: Option<bool>,
    /// `true`: titles that hold a copy in a 2160p lane. `false`: titles that hold none.
    pub has_uhd: Option<bool>,
    /// Titles that hold this edition: a key of `rules/editions.toml` (`open-matte`), its label
    /// or an alias (`Open Matte`, `DC`), or an `other:` key. Files no engine tracks count.
    pub edition: Option<String>,
    /// Titles with a copy whose quality comes from this source: `remux`, `bluray`, `webdl`,
    /// `webrip`, `hdtv`. The quality is the one the copy is scored as.
    pub quality: Option<String>,
    /// `true`: titles that lack files (a movie with no copy, a show with fewer episode files
    /// than episodes counted toward completion). `false`: titles that lack none.
    pub missing: Option<bool>,
    /// The title's status as the engines give it: `continuing`, `ended`, `upcoming`,
    /// `released`.
    pub status: Option<String>,
    /// `true`: shows with a next airing still ahead. `false`: titles with none.
    pub airing: Option<bool>,
    /// `added` (the default: newest copy first), `title`, `year`, `score` (the best copy's
    /// score), `size`, `copies` or `next` (the next airing, soonest first).
    pub sort: Option<String>,
    /// `asc` or `desc`. Left out, each sort has its natural order: titles ascending,
    /// everything else descending.
    pub order: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// What a title is, as the arrs describe it in their title lists. A field is absent until an
/// engine gives it. In the API these fields sit next to the title's other fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct TitleAbout {
    pub overview: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    pub runtime_minutes: Option<i32>,
    /// The age rating: `PG-13`, `TV-MA`.
    pub certification: Option<String>,
    /// Movies: the studio.
    pub studio: Option<String>,
    /// Shows: the network.
    pub network: Option<String>,
    /// Movies: the collection the movie belongs to, by name.
    pub collection: Option<String>,
    /// Movies: that collection's TMDB id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub collection_tmdb_id: Option<i64>,
    /// Movies: the title in its original language, when the manager gives one that differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ssr", sqlx(default))]
    pub original_title: Option<String>,
    /// From 0 to 10, to one decimal. Movies: TMDB's rating. Shows: the rating Sonarr gives.
    pub rating: Option<f64>,
    /// Movies: IMDb's rating, from 0 to 10.
    pub imdb_rating: Option<f64>,
    /// A wide background image, next to `poster_url`.
    pub fanart_url: Option<String>,
    /// A banner image. Shows have one more often than movies.
    pub banner_url: Option<String>,
    /// Shows: when the first episode aired.
    pub first_aired: Option<DateTime<Utc>>,
    /// Shows: when the next episode airs. Absent when none is scheduled.
    pub next_airing: Option<DateTime<Utc>>,
    /// Movies: the theatrical release.
    pub in_cinemas: Option<DateTime<Utc>>,
    /// Movies: the digital release.
    pub digital_release: Option<DateTime<Utc>>,
}

/// One season of a show as one engine reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct EngineSeason {
    pub season_number: i32,
    pub monitored: bool,
    /// Episodes the engine counts toward completion: monitored and aired, or held.
    pub episode_count: i32,
    /// Every episode the engine knows of in the season, aired or not.
    pub total_episode_count: i32,
    /// Episodes with a file.
    pub episode_file_count: i32,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct TitleSummary {
    pub id: i64,
    /// `movie` or `show`.
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    pub poster_url: Option<String>,
    pub status: Option<String>,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    /// Libraries that hold the title.
    pub libraries: Vec<String>,
    /// Lanes its copies are scored in.
    pub lanes: Vec<String>,
    pub copies: i64,
    pub size_bytes: i64,
    /// A protection applies to at least one of its copies.
    pub protected: bool,
    pub added_at: Option<DateTime<Utc>>,
    /// The highest score among its copies.
    pub best_score: Option<i32>,
    /// When its newest copy was added.
    pub newest_copy_at: Option<DateTime<Utc>>,
    /// Shows: the seasons, without the specials. Absent for a movie.
    pub season_count: Option<i32>,
    /// Shows: the episodes that count toward completion (monitored and aired, or held), as
    /// the largest count any library reports.
    pub episode_count: Option<i32>,
    /// Shows: episodes with a file, in the library that holds the most.
    pub episode_file_count: Option<i32>,
    #[serde(flatten)]
    #[cfg_attr(feature = "ssr", sqlx(flatten))]
    pub about: TitleAbout,
    /// The copy that stands for the title in a list: its best scored copy. Absent when the
    /// title holds no copy.
    #[serde(default)]
    #[cfg_attr(feature = "ssr", sqlx(skip))]
    pub lead: Option<LeadCopy>,
}

/// The copy that stands for a title in a list: the highest score, then the newest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct LeadCopy {
    pub copy_id: i64,
    #[serde(skip)]
    pub title_id: i64,
    pub library: String,
    pub lane: Option<String>,
    /// The quality it is scored as: `WEBDL-1080p`.
    pub quality: Option<String>,
    /// The release group it is scored as.
    pub group: Option<String>,
    /// The first edition it is scored as: `Open Matte`.
    pub edition: Option<String>,
    pub score: Option<i32>,
    pub size_bytes: i64,
    pub date_added: Option<DateTime<Utc>>,
    /// Ids of the protections that guard it.
    pub protections: Vec<String>,
}

/// Totals over the titles of one kind.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct KindTotals {
    pub titles: i64,
    /// Titles that hold at least one copy.
    pub with_file: i64,
    /// Shows: episodes counted toward completion, summed over the shows.
    pub episodes: i64,
    /// Shows: episodes with a file, summed over the shows.
    pub episode_files: i64,
    pub size_bytes: i64,
}

/// A value and how many files have it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamedCount {
    pub name: String,
    pub count: i64,
}

/// What the files of one season of a show are, over every library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeasonFiles {
    pub season_number: i32,
    /// Qualities the files are scored as, most files first.
    pub qualities: Vec<NamedCount>,
    /// Release groups, most files first.
    pub groups: Vec<NamedCount>,
    pub size_bytes: i64,
    /// When the newest file came in.
    pub last_added: Option<DateTime<Utc>>,
}

/// The name of an episode, read from its library file name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct EpisodeName {
    pub season_number: i32,
    pub episode_number: i32,
    pub name: Option<String>,
}

/// What one engine says about a title.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct EngineFacts {
    /// The arr integration id.
    pub engine: String,
    pub library: String,
    /// The engine's own id for the title.
    pub engine_title_id: i64,
    pub path: Option<String>,
    pub monitored: bool,
    /// The engine's quality profile.
    pub profile: Option<String>,
    /// The lane that profile maps to.
    pub lane: Option<String>,
    pub tags: Vec<String>,
    pub status: Option<String>,
    pub added_at: Option<DateTime<Utc>>,
    pub file_count: i32,
    pub size_bytes: i64,
    /// Shows: the episodes this engine counts toward completion (monitored and aired, or
    /// held). `file_count` says how many episodes have a file.
    pub episode_count: Option<i32>,
    /// Shows: every episode this engine knows of, aired or not.
    pub total_episode_count: Option<i32>,
    /// Shows: the same per season, as this engine reports it.
    #[cfg_attr(feature = "ssr", sqlx(json))]
    pub seasons: Vec<EngineSeason>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct Season {
    pub season_number: i32,
    /// Episodes that count toward completion (monitored and aired, or held): the largest
    /// count any engine reports.
    pub episode_count: i32,
    /// Every episode the engines know of in this season, aired or not.
    pub total_episode_count: i32,
    /// Episodes with a file, in the library that holds the most. `engines[].seasons` has
    /// the count of each library.
    pub episode_file_count: i32,
    /// Files held for this season, over all libraries.
    pub copies: i64,
}

/// What the arr recorded for a file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ArrReading {
    pub quality: Option<String>,
    pub group: Option<String>,
    pub edition: Option<String>,
    pub custom_formats: Vec<String>,
    pub score: Option<i32>,
}

/// What Singularr reads from the same name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct SingularrReading {
    pub quality: Option<String>,
    pub group: Option<String>,
    pub edition: Option<String>,
    pub score: Option<i32>,
}

/// A verdict as the API returns it: the core's verdict, with the number of steps of the
/// lane's ladder next to `rank`, so a client can draw "step 2 of 5".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerdictView {
    #[serde(flatten)]
    pub verdict: Verdict,
    /// Steps in the lane's quality ladder. Absent when the lane is not in the policy.
    #[serde(default)]
    pub ladder_steps: Option<usize>,
}

impl std::ops::Deref for VerdictView {
    type Target = Verdict;

    fn deref(&self) -> &Verdict {
        &self.verdict
    }
}

/// A lane of a library in which a title holds nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct EmptyLane {
    pub lane: String,
    pub label: String,
    pub library: String,
    /// The lane takes 2160p qualities.
    pub uhd: bool,
}

/// The edition a copy belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CopyEdition {
    /// A key of `rules/editions.toml` (`directors-cut`), or `other:<slug>` for a named cut the
    /// list does not know.
    pub key: String,
    /// `Director's Cut`.
    pub label: String,
    /// The name or the arr's record names the edition. False: nothing does, and the copy is
    /// the default edition (Theatrical).
    pub marked: bool,
}

/// One file that holds an edition of a title: a copy an engine tracks, or a file in the
/// title's folder that no engine tracks (listed, never acted on).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct EditionHolding {
    /// The library id.
    pub library: String,
    /// The engine whose folder holds the file.
    pub engine: String,
    /// The copy, when the engine tracks the file.
    pub copy_id: Option<i64>,
    /// False: the engine does not track the file; the folder scan found it.
    pub tracked: bool,
    pub lane: Option<String>,
    /// The quality the copy is scored as: `Bluray-1080p`. Absent for a file no engine tracks.
    pub quality: Option<String>,
    /// `2160p`, `1080p`.
    pub resolution: Option<String>,
    /// The file name, below the title's folder.
    pub file_name: String,
    pub size_bytes: i64,
    /// The edition the file name carries in Plex naming (`{edition-..}`), which is what Plex
    /// shows in its version picker. Absent:
    /// the server shows the file without an edition name.
    pub plays_as: Option<String>,
}

/// One edition of a title: whether it is wanted, whether a library plays it first, and where
/// it is held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TitleEdition {
    pub key: String,
    pub label: String,
    /// A lane holds one copy per wanted edition. By default a title wants the editions it
    /// holds.
    pub wanted: bool,
    /// The edition a library plays first: the Theatrical when it is held, else the held edition
    /// first in the list's order, unless the owner chose.
    pub is_default: bool,
    /// `owner` when the owner set this edition's row; absent when it follows what is held.
    pub set_by: Option<String>,
    pub held: Vec<EditionHolding>,
}

/// The last scan of the movie folders for files no engine tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct FolderScanStatus {
    pub roots: Vec<String>,
    pub ok: bool,
    pub last_scan_at: DateTime<Utc>,
    pub last_ok_at: Option<DateTime<Utc>>,
    pub took_ms: Option<i64>,
    pub error: Option<String>,
    /// Folders read, video files seen, files no engine tracks, folders that could not be read.
    #[cfg_attr(feature = "ssr", schema(value_type = Object))]
    pub counts: serde_json::Value,
}

/// A copy: one file of one title in one library.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CopyView {
    pub id: i64,
    pub title_id: i64,
    pub title: String,
    /// `movie` or `show`.
    pub kind: String,
    pub year: Option<i32>,
    pub library: String,
    pub engine: String,
    /// The lane the copy is scored in. Absent when the engine's profile maps to no lane.
    pub lane: Option<String>,
    pub season_number: Option<i32>,
    pub episode_numbers: Vec<i32>,
    /// Path below the title's folder.
    pub relative_path: String,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    /// The name Singularr parsed, and where it came from: `scene_name`, `original_path` or
    /// `library_name`.
    pub release_name: String,
    pub name_source: String,
    pub size_bytes: i64,
    pub date_added: Option<DateTime<Utc>>,
    pub languages: Vec<String>,
    #[cfg_attr(feature = "ssr", schema(value_type = Option<Object>))]
    pub media_info: Option<serde_json::Value>,
    pub arr: ArrReading,
    pub singularr: SingularrReading,
    /// Where the two readings differ: any of `quality`, `group`, `edition`, `score`.
    pub differs: Vec<String>,
    /// The name as written, parsed as it stands. The shadow comparison uses this.
    #[cfg_attr(feature = "ssr", schema(value_type = crate::core::schema::ParsedReleaseDoc))]
    pub parsed: ParsedRelease,
    /// The release as held and scored: the arr's recorded group, quality, revision and
    /// edition first, the parsed name for the rest.
    #[cfg_attr(feature = "ssr", schema(value_type = crate::core::schema::ParsedReleaseDoc))]
    pub held: ParsedRelease,
    #[cfg_attr(feature = "ssr", schema(value_type = Option<crate::core::schema::VerdictDoc>))]
    pub verdict: Option<VerdictView>,
    /// Custom formats the arr matched for which Singularr matched no trait.
    pub arr_only_formats: Vec<String>,
    /// Scored traits Singularr matched whose custom format the arr did not match.
    pub singularr_only_traits: Vec<String>,
    /// Ids of the protections that guard this copy.
    pub protections: Vec<String>,
    pub protected: bool,
    pub first_seen_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The edition the copy belongs to.
    #[serde(default)]
    pub edition: CopyEdition,
}

/// One title with everything known about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TitleDetail {
    pub id: i64,
    /// `movie` or `show`.
    pub kind: String,
    pub title: String,
    pub year: Option<i32>,
    pub original_language: Option<String>,
    pub poster_url: Option<String>,
    pub status: Option<String>,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    #[serde(flatten)]
    pub about: TitleAbout,
    pub engines: Vec<EngineFacts>,
    pub seasons: Vec<Season>,
    /// The copies, in every library. A long show is cut at `copies.len()`; `copies_total`
    /// says how many there are, and `/titles/{id}/copies` pages through all of them.
    pub copies: Vec<CopyView>,
    pub copies_total: i64,
    /// Ids of the protections that guard at least one copy.
    pub protections: Vec<String>,
    /// The lanes for this kind of title, per library, in which nothing is held.
    pub empty_lanes: Vec<EmptyLane>,
    /// Movies: the editions the title holds, each with where it is held. Shows: empty until
    /// episodes carry editions.
    #[serde(default)]
    pub editions: Vec<TitleEdition>,
}

/// Filters of `GET /api/v1/titles/{id}/copies` and of the shadow list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct CopyQuery {
    pub library: Option<String>,
    pub season: Option<i32>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// How many copies differ from the arr's reading, by kind of difference.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct DiffCounts {
    pub copies: i64,
    /// Copies with at least one difference.
    pub any: i64,
    pub quality: i64,
    pub group: i64,
    pub edition: i64,
    pub score: i64,
}

/// Totals for the home page and the metrics.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct CatalogCounts {
    pub movies: i64,
    pub shows: i64,
    pub copies: i64,
    pub protected_copies: i64,
    pub size_bytes: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptive_facts_sit_next_to_the_other_fields_of_a_title() {
        let listed: TitleSummary = serde_json::from_value(serde_json::json!({
            "id": 7, "kind": "show", "title": "1883", "libraries": ["sonarr"], "lanes": [],
            "copies": 2, "size_bytes": 10, "protected": false,
            "season_count": 1, "episode_count": 10, "episode_file_count": 2,
            "genres": ["Drama", "Western"], "network": "Paramount+", "rating": 8.7,
            "first_aired": "2021-12-19T05:00:00Z"
        }))
        .unwrap();
        assert_eq!(listed.about.network.as_deref(), Some("Paramount+"));
        assert_eq!(listed.about.genres, vec!["Drama", "Western"]);
        assert_eq!(listed.about.rating, Some(8.7));
        assert!(listed.about.first_aired.is_some());
        assert_eq!(listed.about.overview, None);
        let json = serde_json::to_value(&listed).unwrap();
        assert!(json.get("about").is_none(), "the facts are not nested");
        assert_eq!(json["network"], "Paramount+");
        assert_eq!(json["episode_file_count"], 2);
        assert!(json["next_airing"].is_null());
        // The browser reads the same text back.
        let again: TitleSummary = serde_json::from_value(json).unwrap();
        assert_eq!(again, listed);
    }
}
