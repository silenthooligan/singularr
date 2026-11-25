//! The items the kinds hand over that the catalog keeps: what a title and a file are as an
//! engine reports them, the kind of a title, and an arr's own reading of a name.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// What a title is, as the arrs describe it in their title lists. A field is absent until an
/// engine gives it. In the API these fields sit next to the title's other fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "sqlx", derive(sqlx::FromRow))]
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
    #[cfg_attr(feature = "sqlx", sqlx(default))]
    pub collection_tmdb_id: Option<i64>,
    /// Movies: the title in its original language, when the manager gives one that differs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "sqlx", sqlx(default))]
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
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// The engine a snapshot came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineRef {
    pub id: String,
    pub library: String,
    pub kind: TitleKind,
}

/// One season of a show as the engine reports it: the same shape the API returns.
pub use EngineSeason as SeasonFacts;

/// One title as an engine reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TitleFacts {
    pub engine_title_id: i64,
    pub kind: TitleKind,
    pub title: String,
    pub sort_title: Option<String>,
    pub year: Option<i32>,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub original_language: Option<String>,
    pub poster_url: Option<String>,
    pub status: Option<String>,
    pub path: Option<String>,
    pub monitored: bool,
    pub profile: Option<String>,
    pub lane: Option<String>,
    pub tags: Vec<String>,
    pub added_at: Option<DateTime<Utc>>,
    pub file_count: i32,
    pub size_bytes: i64,
    pub seasons: Vec<SeasonFacts>,
    /// Shows: the engine's episode counts over the whole show (monitored and aired, or held;
    /// and every episode it knows of). `file_count` is the number of episodes with a file.
    pub episode_count: Option<i32>,
    pub total_episode_count: Option<i32>,
    /// The descriptive facts: overview, genres, runtime, rating, images, dates.
    pub about: TitleAbout,
    /// The title's files when the engine lists them with the title (movies). `None` when
    /// files are read per title (shows).
    #[serde(skip)]
    pub files: Option<Vec<FileFacts>>,
}

/// One file as an engine reports it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileFacts {
    pub engine_file_id: i64,
    pub season_number: Option<i32>,
    /// Episodes the file holds, read from the library file name by the engine adapter.
    pub episode_numbers: Vec<i32>,
    pub episode_name: Option<String>,
    pub relative_path: String,
    pub scene_name: Option<String>,
    pub original_path: Option<String>,
    pub size_bytes: i64,
    pub date_added: Option<DateTime<Utc>>,
    pub arr_quality: Option<String>,
    pub arr_group: Option<String>,
    /// `None` when the engine records no edition at all (Sonarr).
    pub arr_edition: Option<String>,
    /// The arr's revision marks the file as a proper (a revision above 1) or a repack.
    pub arr_proper: bool,
    pub arr_repack: bool,
    pub arr_custom_formats: Vec<String>,
    pub arr_score: Option<i32>,
    pub languages: Vec<String>,
    pub media_info: Option<serde_json::Value>,
}

/// The outside lists Seerr serves on Discover, from TMDB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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

/// What an arr's own parser makes of a release name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
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
