//! The rich metadata records from TMDB: titles, seasons, people, collections, browse lists and
//! search, in the shapes of [`super::record`].
//!
//! One TMDB call per record where `append_to_response` allows (a title's credits, videos,
//! keywords, release dates or ratings, where to watch, recommendations, similar titles,
//! external ids and logos come with it; a show's credits are its aggregate credits, over every
//! season). Every answer is kept in `metadata_cache`: titles, seasons, people and collections
//! for a week, lists and searches for a day. A refresh that fails serves the stale answer.
//!
//! The cache holds TMDB's answers as they came, keyed by the exact path asked, and a record is
//! mapped from the stored answer on every read. So a cached title gains a field the moment
//! this module maps it, and a change of what a path appends is a new key, fetched once.
//!
//! The library state of each card is not TMDB's to say; the API layer fills it from the
//! catalog and the requests.

use super::MetadataError;
use super::record::{
    Billboard, BrowseList, BrowseTile, Card, CastCredit, CollectionCard, CollectionRecord,
    CollectionRef, Company, Credits, CrewCredit, Dates, DiscoverPage, DiscoverRow, EpisodeRecord,
    Links, PersonCard, PersonLinks, PersonRecord, PersonRef, Ratings, Score, SearchAnswer,
    SeasonRecord, SeasonSummary, TitleCredits, TitleRecord, Video, WatchProvider,
};
use super::store;
use super::tmdb::Tmdb;
use crate::catalog::model::TitleKind;
use crate::core::error::{AppError, AppResult};
use crate::core::outbound::encode_component;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

/// How long an answer is fresh, in days.
pub const RECORD_TTL: i64 = 7;
pub const LIST_TTL: i64 = 1;

/// Cast and crew entries on a title record (the credits route has them all), and cards in a
/// row.
pub const CAST_MAX: usize = 40;
pub const CREW_MAX: usize = 40;
const CARDS_MAX: usize = 20;
/// Titles on the Discover billboard.
const BILLBOARD_MAX: usize = 6;

/// Discover's rows from TMDB, in order.
pub const DISCOVER_LISTS: [ListKey; 5] = [
    ListKey::Trending,
    ListKey::PopularMovies,
    ListKey::UpcomingMovies,
    ListKey::PopularShows,
    ListKey::OnTheAir,
];

/// The TMDB path of a title record with everything appended.
pub fn title_path(kind: TitleKind, tmdb: i64) -> String {
    match kind {
        TitleKind::Movie => format!(
            "/movie/{tmdb}?append_to_response=credits,videos,keywords,release_dates,watch/providers,recommendations,similar,external_ids,images&include_image_language=en,null"
        ),
        TitleKind::Show => format!(
            "/tv/{tmdb}?append_to_response=aggregate_credits,videos,keywords,content_ratings,watch/providers,recommendations,similar,external_ids,images&include_image_language=en,null"
        ),
    }
}

pub fn season_path(tmdb: i64, number: i32) -> String {
    format!("/tv/{tmdb}/season/{number}")
}

pub fn person_path(tmdb: i64) -> String {
    format!("/person/{tmdb}?append_to_response=combined_credits,external_ids")
}

pub fn collection_path(tmdb: i64) -> String {
    format!("/collection/{tmdb}")
}

/// A browse list by its key, as `GET /api/v1/discover/{list}` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListKey {
    Trending,
    PopularMovies,
    UpcomingMovies,
    PopularShows,
    OnTheAir,
    Genre(TitleKind, i64),
    Studio(i64),
    Network(i64),
}

impl ListKey {
    pub fn parse(text: &str) -> Option<ListKey> {
        let t = text.trim().to_ascii_lowercase();
        let num = |rest: &str| rest.parse::<i64>().ok().filter(|n| *n > 0);
        Some(match t.as_str() {
            "trending" => ListKey::Trending,
            "popular-movies" => ListKey::PopularMovies,
            "upcoming-movies" => ListKey::UpcomingMovies,
            "popular-shows" => ListKey::PopularShows,
            "on-the-air" => ListKey::OnTheAir,
            _ => {
                if let Some(rest) = t.strip_prefix("genre-movie-") {
                    ListKey::Genre(TitleKind::Movie, num(rest)?)
                } else if let Some(rest) = t.strip_prefix("genre-show-") {
                    ListKey::Genre(TitleKind::Show, num(rest)?)
                } else if let Some(rest) = t.strip_prefix("studio-") {
                    ListKey::Studio(num(rest)?)
                } else {
                    ListKey::Network(num(t.strip_prefix("network-")?)?)
                }
            }
        })
    }

    pub fn key(self) -> String {
        match self {
            ListKey::Trending => "trending".to_string(),
            ListKey::PopularMovies => "popular-movies".to_string(),
            ListKey::UpcomingMovies => "upcoming-movies".to_string(),
            ListKey::PopularShows => "popular-shows".to_string(),
            ListKey::OnTheAir => "on-the-air".to_string(),
            ListKey::Genre(TitleKind::Movie, id) => format!("genre-movie-{id}"),
            ListKey::Genre(TitleKind::Show, id) => format!("genre-show-{id}"),
            ListKey::Studio(id) => format!("studio-{id}"),
            ListKey::Network(id) => format!("network-{id}"),
        }
    }

    /// The kind of every item, when the list has one kind.
    pub fn kind(self) -> Option<TitleKind> {
        match self {
            ListKey::Trending => None,
            ListKey::PopularMovies | ListKey::UpcomingMovies | ListKey::Studio(_) => {
                Some(TitleKind::Movie)
            }
            ListKey::PopularShows | ListKey::OnTheAir | ListKey::Network(_) => {
                Some(TitleKind::Show)
            }
            ListKey::Genre(kind, _) => Some(kind),
        }
    }

    pub fn path(self, page: u32, region: &str) -> String {
        let p = page.clamp(1, 500);
        match self {
            ListKey::Trending => format!("/trending/all/week?page={p}"),
            ListKey::PopularMovies => format!("/movie/popular?page={p}&region={region}"),
            ListKey::UpcomingMovies => format!("/movie/upcoming?page={p}&region={region}"),
            ListKey::PopularShows => format!("/tv/popular?page={p}"),
            ListKey::OnTheAir => format!("/tv/on_the_air?page={p}"),
            ListKey::Genre(TitleKind::Movie, id) => {
                format!("/discover/movie?with_genres={id}&sort_by=popularity.desc&page={p}")
            }
            ListKey::Genre(TitleKind::Show, id) => {
                format!("/discover/tv?with_genres={id}&sort_by=popularity.desc&page={p}")
            }
            ListKey::Studio(id) => {
                format!("/discover/movie?with_companies={id}&sort_by=popularity.desc&page={p}")
            }
            ListKey::Network(id) => {
                format!("/discover/tv?with_networks={id}&sort_by=popularity.desc&page={p}")
            }
        }
    }
}

/// The record service: TMDB through the cache.
pub struct Records<'a> {
    pub tmdb: &'a Tmdb,
    pub pool: &'a PgPool,
    /// The region for release dates, ratings and where to watch: `US`.
    pub region: String,
    /// The language TMDB answers in: `en-US`.
    pub language: String,
}

/// A TMDB path asked in a language: `language=` added, and the language's own images first.
/// English (`en-US`, TMDB's own default) leaves the path as it is, so an English installation
/// keeps its cache keys.
pub fn localized(path: &str, language: &str) -> String {
    if language.is_empty() || language.eq_ignore_ascii_case("en-US") {
        return path.to_string();
    }
    let short = language.split('-').next().unwrap_or(language);
    let path = path.replace(
        "include_image_language=en,null",
        &format!("include_image_language={short},en,null"),
    );
    let sep = if path.contains('?') { '&' } else { '?' };
    format!("{path}{sep}language={language}")
}

fn upstream(e: MetadataError) -> AppError {
    match e {
        MetadataError::Unsupported(why) => AppError::Upstream(format!("TMDB unavailable: {why}.")),
        other => AppError::Upstream(format!("TMDB did not answer: {other}")),
    }
}

impl Records<'_> {
    /// One TMDB answer through the cache. `Ok(None)`: TMDB has no such record.
    pub async fn cached(&self, path: &str, ttl_days: i64) -> AppResult<Option<Value>> {
        let asked = localized(path, &self.language);
        let path = asked.as_str();
        let hit = store::get(self.pool, path).await?;
        if let Some(h) = &hit
            && Utc::now() - h.fetched_at < Duration::days(ttl_days)
        {
            return Ok(h.body.clone());
        }
        match self.tmdb.get_json(path).await {
            Ok(body) => {
                store::put(self.pool, path, body.as_ref()).await?;
                Ok(body)
            }
            Err(e) => match hit {
                Some(h) => {
                    tracing::warn!(path, error = %e, "TMDB did not answer; the stored record is served");
                    Ok(h.body)
                }
                None => Err(upstream(e)),
            },
        }
    }

    async fn need(&self, path: &str, ttl: i64, what: &str) -> AppResult<Value> {
        self.cached(path, ttl)
            .await?
            .ok_or_else(|| AppError::not_found(format!("TMDB has no {what}.")))
    }

    /// Stored answers by path, fresh or not, without asking TMDB. A path never asked is left
    /// out; one TMDB had no record for maps to `None`.
    pub async fn stored(
        &self,
        paths: &[String],
    ) -> AppResult<std::collections::HashMap<String, Option<Value>>> {
        Ok(store::get_many(self.pool, paths)
            .await?
            .into_iter()
            .map(|(key, c)| (key, c.body))
            .collect())
    }

    /// Ask TMDB for these paths where the stored answer is missing or older than `ttl_days`,
    /// a few at a time, at most `max`. Returns how many were asked. A failure is logged and
    /// skipped: warming is best effort.
    pub async fn warm(&self, paths: &[String], ttl_days: i64, max: usize) -> usize {
        use futures::StreamExt;
        let stale: Vec<String> = match store::stale_of(self.pool, paths, ttl_days).await {
            Ok(list) => list.into_iter().take(max).collect(),
            Err(e) => {
                tracing::warn!(error = %e, "the metadata cache could not be read for warming");
                return 0;
            }
        };
        let asked = stale.len();
        futures::stream::iter(stale)
            .for_each_concurrent(4, |path| async move {
                if let Err(e) = self.cached(&path, ttl_days).await {
                    tracing::debug!(path, error = %e, "a record could not be warmed");
                }
            })
            .await;
        asked
    }

    pub async fn title(&self, kind: TitleKind, tmdb: i64) -> AppResult<TitleRecord> {
        let raw = self
            .need(
                &title_path(kind, tmdb),
                RECORD_TTL,
                &format!("{} {tmdb}", kind.as_str()),
            )
            .await?;
        Ok(title_record(kind, &raw, &self.region))
    }

    /// Everything of a title's cast and crew, from the same stored answer as its record.
    pub async fn credits(&self, kind: TitleKind, tmdb: i64) -> AppResult<TitleCredits> {
        let raw = self
            .need(
                &title_path(kind, tmdb),
                RECORD_TTL,
                &format!("{} {tmdb}", kind.as_str()),
            )
            .await?;
        Ok(title_credits(kind, &raw))
    }

    pub async fn season(&self, tmdb: i64, number: i32) -> AppResult<SeasonRecord> {
        let raw = self
            .need(
                &season_path(tmdb, number),
                RECORD_TTL,
                &format!("season {number} of show {tmdb}"),
            )
            .await?;
        Ok(season_record(tmdb, &raw))
    }

    pub async fn person(&self, tmdb: i64) -> AppResult<PersonRecord> {
        let raw = self
            .need(&person_path(tmdb), RECORD_TTL, &format!("person {tmdb}"))
            .await?;
        Ok(person_record(&raw))
    }

    pub async fn collection(&self, tmdb: i64) -> AppResult<CollectionRecord> {
        let raw = self
            .need(
                &collection_path(tmdb),
                RECORD_TTL,
                &format!("collection {tmdb}"),
            )
            .await?;
        Ok(collection_record(&raw))
    }

    /// The list's title: a fixed name, or the genre, studio or network by its TMDB name.
    async fn list_title(&self, list: ListKey) -> AppResult<String> {
        Ok(match list {
            ListKey::Trending => "Trending".to_string(),
            ListKey::PopularMovies => "Popular movies".to_string(),
            ListKey::UpcomingMovies => "Upcoming movies".to_string(),
            ListKey::PopularShows => "Popular shows".to_string(),
            ListKey::OnTheAir => "On the air".to_string(),
            ListKey::Genre(kind, id) => {
                let path = match kind {
                    TitleKind::Movie => "/genre/movie/list",
                    TitleKind::Show => "/genre/tv/list",
                };
                self.cached(path, LIST_TTL)
                    .await?
                    .and_then(|v| {
                        arr(&v, "genres")
                            .iter()
                            .find(|g| int(g, "id") == Some(id))
                            .and_then(|g| text(g, "name"))
                    })
                    .unwrap_or_else(|| format!("Genre {id}"))
            }
            ListKey::Studio(id) => self
                .cached(&format!("/company/{id}"), RECORD_TTL)
                .await?
                .and_then(|v| text(&v, "name"))
                .unwrap_or_else(|| format!("Studio {id}")),
            ListKey::Network(id) => self
                .cached(&format!("/network/{id}"), RECORD_TTL)
                .await?
                .and_then(|v| text(&v, "name"))
                .unwrap_or_else(|| format!("Network {id}")),
        })
    }

    /// A genre list as (id, name): `/genre/movie/list` or `/genre/tv/list`.
    pub async fn genre_names(&self, kind: TitleKind) -> AppResult<Vec<(i64, String)>> {
        let path = match kind {
            TitleKind::Movie => "/genre/movie/list",
            TitleKind::Show => "/genre/tv/list",
        };
        Ok(self
            .cached(path, LIST_TTL)
            .await?
            .map(|v| {
                arr(&v, "genres")
                    .iter()
                    .filter_map(|g| Some((int(g, "id")?, text(g, "name")?)))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Discover from TMDB: the billboard (trending titles with a backdrop, each from its
    /// record), the rows, and the genres, each with the backdrop of a title in it from those
    /// rows. A part TMDB cannot give is left empty; library states are the API layer's.
    pub async fn discover(&self) -> DiscoverPage {
        let mut page = DiscoverPage::default();
        let mut backdrops: HashMap<(&'static str, i64), String> = HashMap::new();
        let mut trending: Vec<(TitleKind, i64)> = Vec::new();
        for list in DISCOVER_LISTS {
            let raw = match self.cached(&list.path(1, &self.region), LIST_TTL).await {
                Ok(Some(v)) => v,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!(list = %list.key(), error = %e, "a Discover row could not be read");
                    continue;
                }
            };
            for r in arr(&raw, "results") {
                let Some(kind) = kind_of(r.get("media_type").and_then(Value::as_str), list.kind())
                else {
                    continue;
                };
                let Some(backdrop) = image(r, "backdrop_path") else {
                    continue;
                };
                for g in arr(r, "genre_ids").iter().filter_map(Value::as_i64) {
                    backdrops
                        .entry((kind.as_str(), g))
                        .or_insert_with(|| backdrop.clone());
                }
                if list == ListKey::Trending
                    && trending.len() < BILLBOARD_MAX
                    && let Some(id) = int(r, "id").filter(|n| *n > 0)
                {
                    trending.push((kind, id));
                }
            }
            let title = self.list_title(list).await.unwrap_or_else(|_| list.key());
            page.rows.push(DiscoverRow {
                id: list.key(),
                title,
                items: cards(arr(&raw, "results"), list.kind(), CARDS_MAX),
            });
        }
        for kind in [TitleKind::Movie, TitleKind::Show] {
            let tiles: Vec<BrowseTile> = match self.genre_names(kind).await {
                Ok(names) => names
                    .into_iter()
                    .map(|(id, name)| BrowseTile {
                        id,
                        key: ListKey::Genre(kind, id).key(),
                        name,
                        backdrop: backdrops.get(&(kind.as_str(), id)).cloned(),
                        logo: None,
                    })
                    .collect(),
                Err(e) => {
                    tracing::warn!(error = %e, "the genres could not be read");
                    Vec::new()
                }
            };
            match kind {
                TitleKind::Movie => page.genres.movie = tiles,
                TitleKind::Show => page.genres.show = tiles,
            }
        }
        let records =
            futures::future::join_all(trending.iter().map(|(kind, id)| self.title(*kind, *id)))
                .await;
        page.billboard = records
            .into_iter()
            .filter_map(Result::ok)
            .map(|r| Billboard {
                rating: r.ratings.tmdb.as_ref().map(|s| s.score),
                id: r.id,
                kind: r.kind,
                title: r.title,
                year: r.year,
                poster: r.poster,
                backdrop: r.backdrop,
                logo: r.logo,
                overview: r.overview,
                tagline: r.tagline,
                certification: r.certification,
                runtime: r.runtime,
                genres: r.genres,
                state: None,
            })
            .collect();
        page
    }

    pub async fn browse(&self, list: ListKey, page: u32) -> AppResult<BrowseList> {
        let raw = self
            .cached(&list.path(page, &self.region), LIST_TTL)
            .await?
            .unwrap_or(Value::Null);
        Ok(BrowseList {
            list: list.key(),
            title: self.list_title(list).await?,
            kind: list.kind().map(|k| k.as_str()).unwrap_or("all").to_string(),
            items: cards(arr(&raw, "results"), list.kind(), usize::MAX),
            page: int(&raw, "page").unwrap_or(1).clamp(1, 500) as u32,
            total_pages: int(&raw, "total_pages").unwrap_or(0).clamp(0, 500) as u32,
        })
    }

    /// Titles, people and collections outside the library for these words. The library's own
    /// titles are the API layer's to add.
    pub async fn search(&self, query: &str) -> AppResult<SearchAnswer> {
        let q = encode_component(query.trim());
        let multi = self
            .cached(
                &format!("/search/multi?query={q}&include_adult=false&page=1"),
                LIST_TTL,
            )
            .await?
            .unwrap_or(Value::Null);
        let collections = self
            .cached(&format!("/search/collection?query={q}&page=1"), LIST_TTL)
            .await?
            .unwrap_or(Value::Null);
        Ok(search_answer(query.trim(), &multi, &collections))
    }
}

// Reading TMDB's JSON ---------------------------------------------------------------------------

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn int(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

fn arr<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// An image path as TMDB gives it: `/abc.jpg`.
fn image(v: &Value, key: &str) -> Option<String> {
    text(v, key).filter(|p| p.starts_with('/'))
}

/// The date part of `2026-01-02` or `2026-01-02T00:00:00.000Z`.
fn date(v: &Value, key: &str) -> Option<String> {
    text(v, key)
        .and_then(|d| d.get(..10).map(str::to_string))
        .filter(|d| d.len() == 10 && d.as_bytes()[4] == b'-')
}

fn year(d: Option<&str>) -> Option<i32> {
    d?.get(..4)?.parse().ok().filter(|y| *y > 1800)
}

fn kind_of(media_type: Option<&str>, default: Option<TitleKind>) -> Option<TitleKind> {
    match media_type {
        Some("movie") => Some(TitleKind::Movie),
        Some("tv") => Some(TitleKind::Show),
        Some(_) => None,
        None => default,
    }
}

/// The id of a title as the interface names it: `movie-603`, `show-1396`.
pub fn title_id(kind: TitleKind, tmdb: i64) -> String {
    format!("{}-{tmdb}", kind.as_str())
}

/// One card from a TMDB result. `None` for a person or a result without an id.
pub fn card(v: &Value, default: Option<TitleKind>) -> Option<Card> {
    let kind = kind_of(v.get("media_type").and_then(Value::as_str), default)?;
    let id = int(v, "id").filter(|n| *n > 0)?;
    let title = text(v, "title").or_else(|| text(v, "name"))?;
    let released = date(v, "release_date").or_else(|| date(v, "first_air_date"));
    Some(Card {
        id: title_id(kind, id),
        kind: kind.as_str().to_string(),
        title,
        year: year(released.as_deref()),
        poster: image(v, "poster_path"),
        rating: v
            .get("vote_average")
            .and_then(Value::as_f64)
            .filter(|r| *r > 0.0),
        state: None,
        role: None,
        job: None,
    })
}

/// Cards from a list of results, once each, at most `max`.
pub fn cards(results: &[Value], default: Option<TitleKind>, max: usize) -> Vec<Card> {
    let mut out: Vec<Card> = Vec::new();
    for c in results.iter().filter_map(|v| card(v, default)) {
        if out.len() >= max {
            break;
        }
        if !out.iter().any(|o| o.id == c.id) {
            out.push(c);
        }
    }
    out
}

fn person_id(v: &Value) -> Option<String> {
    int(v, "id")
        .filter(|n| *n > 0)
        .map(|id| format!("person-{id}"))
}

/// Studios (`browse` = `studio`) or networks (`network`), each with the key of its list.
fn companies(list: &[Value], browse: &str) -> Vec<Company> {
    list.iter()
        .filter_map(|c| {
            let id = int(c, "id").filter(|n| *n > 0)?;
            Some(Company {
                id,
                name: text(c, "name")?,
                logo: image(c, "logo_path"),
                browse: Some(format!("{browse}-{id}")),
            })
        })
        .collect()
}

/// The region's entry of a per-region list (`release_dates`, `content_ratings`).
fn region_entry<'v>(list: &'v [Value], region: &str) -> Option<&'v Value> {
    list.iter().find(|r| {
        r.get("iso_3166_1")
            .and_then(Value::as_str)
            .is_some_and(|c| c.eq_ignore_ascii_case(region))
    })
}

/// A movie's certification and release dates in the region. TMDB's release types: 1
/// premiere, 2 limited, 3 theatrical, 4 digital, 5 physical, 6 TV.
fn movie_dates(raw: &Value, region: &str) -> (Option<String>, Dates) {
    let entries = raw
        .get("release_dates")
        .map(|r| arr(r, "results"))
        .and_then(|list| region_entry(list, region))
        .map(|e| arr(e, "release_dates"))
        .unwrap_or(&[]);
    let first_of = |types: &[i64]| -> Option<String> {
        let mut dates: Vec<String> = entries
            .iter()
            .filter(|d| int(d, "type").is_some_and(|t| types.contains(&t)))
            .filter_map(|d| date(d, "release_date"))
            .collect();
        dates.sort();
        dates.into_iter().next()
    };
    let certification = [3, 2, 1, 4, 5, 6].iter().find_map(|t| {
        entries
            .iter()
            .filter(|d| int(d, "type") == Some(*t))
            .find_map(|d| text(d, "certification"))
    });
    let dates = Dates {
        theatrical: first_of(&[3])
            .or_else(|| first_of(&[2]))
            .or_else(|| date(raw, "release_date")),
        digital: first_of(&[4]),
        physical: first_of(&[5]),
        ..Dates::default()
    };
    (certification, dates)
}

fn videos(raw: &Value) -> Vec<Video> {
    raw.get("videos")
        .map(|v| arr(v, "results"))
        .unwrap_or(&[])
        .iter()
        .filter_map(|v| {
            let site = text(v, "site")?;
            if site != "YouTube" && site != "Vimeo" {
                return None;
            }
            Some(Video {
                site,
                key: text(v, "key")?,
                name: text(v, "name").unwrap_or_default(),
                kind: text(v, "type").unwrap_or_else(|| "Video".to_string()),
            })
        })
        .take(30)
        .collect()
}

fn watch(raw: &Value, region: &str) -> Vec<WatchProvider> {
    let Some(entry) = raw
        .get("watch/providers")
        .and_then(|w| w.get("results"))
        .and_then(Value::as_object)
        .and_then(|m| {
            m.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(region))
                .map(|(_, v)| v)
        })
    else {
        return Vec::new();
    };
    let mut out: Vec<WatchProvider> = Vec::new();
    for key in ["flatrate", "free", "ads"] {
        for p in arr(entry, key) {
            if let Some(name) = text(p, "provider_name")
                && !out.iter().any(|o| o.name == name)
            {
                out.push(WatchProvider {
                    name,
                    logo: image(p, "logo_path"),
                });
            }
        }
    }
    out
}

/// A show's credits over all its seasons (`aggregate_credits`), when TMDB gave them; else the
/// title's `credits` (a movie, or a stored answer from before).
fn credit_list<'v>(raw: &'v Value, part: &str) -> (&'v [Value], bool) {
    match raw.get("aggregate_credits").map(|c| arr(c, part)) {
        Some(list) if !list.is_empty() => (list, true),
        _ => (
            raw.get("credits").map(|c| arr(c, part)).unwrap_or(&[]),
            false,
        ),
    }
}

fn count(v: &Value, key: &str) -> Option<i32> {
    int(v, key)
        .and_then(|n| i32::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// The whole cast in billing order, once per person. A show's comes from its aggregate
/// credits: the characters a person played, most episodes first, and how many episodes.
pub fn cast(raw: &Value) -> Vec<CastCredit> {
    let (list, aggregated) = credit_list(raw, "cast");
    let mut list: Vec<&Value> = list.iter().collect();
    // Stable: people TMDB gives no order keep their place after the ordered ones.
    list.sort_by_key(|c| int(c, "order").unwrap_or(i64::MAX));
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<CastCredit> = Vec::new();
    for c in list {
        let (Some(id), Some(name)) = (person_id(c), text(c, "name")) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let (role, episodes) = if aggregated {
            let mut roles: Vec<&Value> = arr(c, "roles").iter().collect();
            roles.sort_by_key(|r| std::cmp::Reverse(int(r, "episode_count").unwrap_or(0)));
            let mut names: Vec<String> = Vec::new();
            for r in roles {
                if let Some(n) = text(r, "character")
                    && !names.contains(&n)
                {
                    names.push(n);
                }
            }
            (names.join(", "), count(c, "total_episode_count"))
        } else {
            (text(c, "character").unwrap_or_default(), None)
        };
        out.push(CastCredit {
            id,
            name,
            role,
            photo: image(c, "profile_path"),
            episodes,
        });
    }
    out
}

/// How much a job matters on a title page, lower first: directing, creating, writing,
/// producing, music, photography, editing, casting, design, then the rest by department.
fn job_rank(job: &str, dept: &str) -> u8 {
    match job {
        "Director" | "Co-Director" => 0,
        "Creator" | "Showrunner" | "Developer" => 1,
        "Screenplay"
        | "Writer"
        | "Teleplay"
        | "Story"
        | "Novel"
        | "Characters"
        | "Author"
        | "Book"
        | "Original Story"
        | "Comic Book"
        | "Head Writer"
        | "Original Series Creator" => 2,
        "Executive Producer" | "Producer" => 3,
        "Original Music Composer" | "Music" | "Composer" | "Main Title Theme Composer" => 4,
        "Director of Photography" | "Cinematography" => 5,
        "Editor" => 6,
        "Casting" | "Casting Director" => 7,
        "Production Design" | "Costume Design" => 8,
        _ => match dept {
            "Directing" => 9,
            "Writing" => 10,
            "Production" => 11,
            "Sound" => 12,
            "Camera" => 13,
            "Editing" => 14,
            "Art" => 15,
            "Costume & Make-Up" => 16,
            "Visual Effects" => 17,
            _ => 18,
        },
    }
}

/// The whole crew, once per person and job, the key jobs first; within a job the people with
/// the most episodes (a show), then TMDB's order.
pub fn crew(raw: &Value) -> Vec<CrewCredit> {
    let (list, aggregated) = credit_list(raw, "crew");
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut out: Vec<CrewCredit> = Vec::new();
    for c in list {
        let (Some(id), Some(name)) = (person_id(c), text(c, "name")) else {
            continue;
        };
        let dept = text(c, "department").unwrap_or_default();
        let jobs: Vec<(String, Option<i32>)> = if aggregated {
            arr(c, "jobs")
                .iter()
                .filter_map(|j| Some((text(j, "job")?, count(j, "episode_count"))))
                .collect()
        } else {
            text(c, "job").map(|j| (j, None)).into_iter().collect()
        };
        for (job, episodes) in jobs {
            if !seen.insert((id.clone(), job.clone())) {
                continue;
            }
            out.push(CrewCredit {
                id: id.clone(),
                name: name.clone(),
                job,
                dept: dept.clone(),
                photo: image(c, "profile_path"),
                episodes,
            });
        }
    }
    out.sort_by_key(|c| {
        (
            job_rank(&c.job, &c.dept),
            std::cmp::Reverse(c.episodes.unwrap_or(0)),
        )
    });
    out
}

/// Everything of a title's cast and crew.
pub fn title_credits(kind: TitleKind, raw: &Value) -> TitleCredits {
    TitleCredits {
        id: title_id(kind, int(raw, "id").unwrap_or(0)),
        cast: cast(raw),
        crew: crew(raw),
    }
}

/// Aired episodes of a season: every one before the season of the last episode aired, up to
/// that episode in its season, none after. Without that episode, the season's own air date
/// decides.
fn aired_in(raw: &Value, number: i64, total: i32, air_date: Option<&str>) -> i32 {
    let last = raw.get("last_episode_to_air");
    match (
        last.and_then(|e| int(e, "season_number")),
        last.and_then(|e| int(e, "episode_number")),
    ) {
        (Some(season), _) if number < season => total,
        (Some(season), Some(episode)) if number == season => {
            i32::try_from(episode).unwrap_or(total).clamp(0, total)
        }
        (Some(_), _) => 0,
        (None, _) => {
            let today = Utc::now().date_naive().to_string();
            if air_date.is_some_and(|d| *d <= *today) {
                total
            } else {
                0
            }
        }
    }
}

fn logo(raw: &Value) -> Option<String> {
    let logos = raw.get("images").map(|i| arr(i, "logos")).unwrap_or(&[]);
    logos
        .iter()
        .find(|l| l.get("iso_639_1").and_then(Value::as_str) == Some("en"))
        .or_else(|| logos.first())
        .and_then(|l| image(l, "file_path"))
}

/// A title record from TMDB's answer with everything appended. `library` is left empty.
pub fn title_record(kind: TitleKind, raw: &Value, region: &str) -> TitleRecord {
    let tmdb = int(raw, "id").unwrap_or(0);
    let external = raw.get("external_ids").cloned().unwrap_or(Value::Null);
    let imdb = text(raw, "imdb_id").or_else(|| text(&external, "imdb_id"));
    let tvdb = int(&external, "tvdb_id").filter(|n| *n > 0);
    let title = text(raw, "title")
        .or_else(|| text(raw, "name"))
        .unwrap_or_else(|| format!("TMDB {tmdb}"));
    let original = text(raw, "original_title").or_else(|| text(raw, "original_name"));
    let (certification, dates, runtime) = match kind {
        TitleKind::Movie => {
            let (c, d) = movie_dates(raw, region);
            (c, d, int(raw, "runtime").filter(|m| *m > 0))
        }
        TitleKind::Show => {
            let c = raw
                .get("content_ratings")
                .map(|r| arr(r, "results"))
                .and_then(|list| region_entry(list, region))
                .and_then(|e| text(e, "rating"));
            let d = Dates {
                first: date(raw, "first_air_date"),
                last: date(raw, "last_air_date"),
                next: raw
                    .get("next_episode_to_air")
                    .and_then(|n| date(n, "air_date")),
                ..Dates::default()
            };
            let runtime = arr(raw, "episode_run_time")
                .iter()
                .find_map(Value::as_i64)
                .or_else(|| {
                    raw.get("last_episode_to_air")
                        .and_then(|e| int(e, "runtime"))
                })
                .filter(|m| *m > 0);
            (c, d, runtime)
        }
    };
    let released = dates.theatrical.clone().or_else(|| dates.first.clone());
    let votes = int(raw, "vote_count").unwrap_or(0);
    let keywords_list = raw
        .get("keywords")
        .map(|k| {
            let list = arr(k, "keywords");
            if list.is_empty() {
                arr(k, "results")
            } else {
                list
            }
        })
        .unwrap_or(&[]);
    let kind_word = match kind {
        TitleKind::Movie => "movie",
        TitleKind::Show => "tv",
    };
    let trakt_kind = match kind {
        TitleKind::Movie => "movie",
        TitleKind::Show => "show",
    };
    let studios = companies(arr(raw, "production_companies"), "studio");
    let networks = (kind == TitleKind::Show).then(|| companies(arr(raw, "networks"), "network"));
    let collection = raw.get("belongs_to_collection").and_then(|c| {
        Some(CollectionRef {
            id: format!("collection-{}", int(c, "id").filter(|n| *n > 0)?),
            name: text(c, "name")?,
            poster: image(c, "poster_path"),
            backdrop: image(c, "backdrop_path"),
        })
    });
    let mut cast_list = cast(raw);
    let cast_total = cast_list.len();
    cast_list.truncate(CAST_MAX);
    let mut crew_list = crew(raw);
    let crew_total = crew_list.len();
    crew_list.truncate(CREW_MAX);
    TitleRecord {
        id: title_id(kind, tmdb),
        kind: kind.as_str().to_string(),
        tmdb,
        imdb: imdb.clone(),
        tvdb,
        year: year(released.as_deref()),
        overview: text(raw, "overview"),
        certification,
        runtime: runtime.and_then(|m| i32::try_from(m).ok()),
        genres: arr(raw, "genres")
            .iter()
            .filter_map(|g| text(g, "name"))
            .collect(),
        poster: image(raw, "poster_path"),
        backdrop: image(raw, "backdrop_path"),
        ratings: Ratings {
            tmdb: (votes > 0).then(|| Score {
                score: raw
                    .get("vote_average")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0),
                votes: Some(votes),
            }),
            ..Ratings::default()
        },
        links: Links {
            tmdb: format!("https://www.themoviedb.org/{kind_word}/{tmdb}"),
            imdb: imdb
                .as_deref()
                .map(|id| format!("https://www.imdb.com/title/{id}/")),
            tvdb: tvdb.map(|id| format!("https://thetvdb.com/dereferrer/series/{id}")),
            trakt: Some(format!(
                "https://trakt.tv/search/tmdb/{tmdb}?id_type={trakt_kind}"
            )),
            letterboxd: (kind == TitleKind::Movie)
                .then(|| format!("https://letterboxd.com/tmdb/{tmdb}/")),
            homepage: text(raw, "homepage"),
        },
        rich: true,
        state: None,
        library: None,
        original_title: original.filter(|o| *o != title),
        title,
        tagline: text(raw, "tagline"),
        status: text(raw, "status"),
        dates,
        budget: int(raw, "budget").filter(|n| *n > 0),
        revenue: int(raw, "revenue").filter(|n| *n > 0),
        language: text(raw, "original_language"),
        countries: match kind {
            TitleKind::Movie => arr(raw, "production_countries")
                .iter()
                .filter_map(|c| text(c, "iso_3166_1"))
                .collect(),
            TitleKind::Show => arr(raw, "origin_country")
                .iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect(),
        },
        studio: match kind {
            TitleKind::Movie => studios.first().map(|s| s.name.clone()),
            TitleKind::Show => None,
        },
        studios,
        network: networks
            .as_ref()
            .and_then(|n| n.first())
            .map(|n| n.name.clone()),
        networks,
        keywords: keywords_list
            .iter()
            .filter_map(|k| text(k, "name"))
            .collect(),
        collection_name: collection.as_ref().map(|c| c.name.clone()),
        collection,
        logo: logo(raw),
        videos: videos(raw),
        watch: watch(raw, region),
        crew: crew_list,
        crew_total,
        cast: cast_list,
        cast_total,
        creators: (kind == TitleKind::Show).then(|| {
            arr(raw, "created_by")
                .iter()
                .filter_map(|c| {
                    Some(PersonRef {
                        id: person_id(c)?,
                        name: text(c, "name")?,
                        photo: image(c, "profile_path"),
                    })
                })
                .collect()
        }),
        recommendations: cards(
            raw.get("recommendations")
                .map(|r| arr(r, "results"))
                .unwrap_or(&[]),
            Some(kind),
            CARDS_MAX,
        ),
        similar: cards(
            raw.get("similar").map(|r| arr(r, "results")).unwrap_or(&[]),
            Some(kind),
            CARDS_MAX,
        ),
        history: Vec::new(),
        queue: Vec::new(),
        requests: Vec::new(),
        processing: None,
        plays: None,
        seasons: (kind == TitleKind::Show).then(|| {
            arr(raw, "seasons")
                .iter()
                .filter(|s| int(s, "season_number").is_some_and(|n| n > 0))
                .filter_map(|s| {
                    let number = int(s, "season_number")?;
                    let total = int(s, "episode_count")
                        .and_then(|n| i32::try_from(n).ok())
                        .unwrap_or(0);
                    let air_date = date(s, "air_date");
                    Some(SeasonSummary {
                        number: i32::try_from(number).ok()?,
                        name: text(s, "name"),
                        aired: aired_in(raw, number, total, air_date.as_deref()),
                        air_date,
                        poster: image(s, "poster_path"),
                        have: 0,
                        total,
                        in_library: Vec::new(),
                    })
                })
                .collect()
        }),
    }
}

/// A season record from TMDB's season answer. Each episode is `unaired` or `missing` until
/// the API layer marks what the library holds.
pub fn season_record(tmdb: i64, raw: &Value) -> SeasonRecord {
    let today = Utc::now().date_naive().to_string();
    let episodes: Vec<EpisodeRecord> = arr(raw, "episodes")
        .iter()
        .filter_map(|e| {
            let air_date = date(e, "air_date");
            Some(EpisodeRecord {
                number: i32::try_from(int(e, "episode_number")?).ok()?,
                title: text(e, "name"),
                runtime: int(e, "runtime").and_then(|m| i32::try_from(m).ok()),
                overview: text(e, "overview"),
                still: image(e, "still_path"),
                state: if air_date.as_deref().is_none_or(|d| *d > *today) {
                    "unaired"
                } else {
                    "missing"
                }
                .to_string(),
                air_date,
                file: None,
            })
        })
        .collect();
    SeasonRecord {
        show: title_id(TitleKind::Show, tmdb),
        number: int(raw, "season_number")
            .and_then(|n| i32::try_from(n).ok())
            .unwrap_or(0),
        name: text(raw, "name"),
        air_date: date(raw, "air_date"),
        poster: image(raw, "poster_path"),
        overview: text(raw, "overview"),
        have: 0,
        total: i32::try_from(episodes.len()).unwrap_or(i32::MAX),
        episodes,
    }
}

/// A person's credit cards, once per title: the most popular first, then the newest (the page
/// sorts by year or title itself; "popular" is the order given).
fn credit_cards(list: &[Value], extra: &str) -> Vec<Card> {
    let mut out: Vec<Card> = Vec::new();
    let mut popularity: Vec<f64> = Vec::new();
    for v in list {
        let Some(mut c) = card(v, None) else {
            continue;
        };
        let what = text(v, extra);
        let pop = v.get("popularity").and_then(Value::as_f64).unwrap_or(0.0);
        if let Some(at) = out.iter().position(|o| o.id == c.id) {
            popularity[at] = popularity[at].max(pop);
            let existing = &mut out[at];
            // One card per title; a second role or job joins the first.
            let slot = if extra == "character" {
                &mut existing.role
            } else {
                &mut existing.job
            };
            if let (Some(w), Some(have)) = (what, slot.as_mut())
                && !have.split(", ").any(|x| x == w)
            {
                have.push_str(", ");
                have.push_str(&w);
            }
            continue;
        }
        if extra == "character" {
            c.role = what;
        } else {
            c.job = what;
        }
        out.push(c);
        popularity.push(pop);
    }
    let mut ranked: Vec<(f64, Card)> = popularity.into_iter().zip(out).collect();
    ranked.sort_by(|(pa, a), (pb, b)| pb.total_cmp(pa).then_with(|| b.year.cmp(&a.year)));
    ranked.into_iter().map(|(_, c)| c).collect()
}

pub fn person_record(raw: &Value) -> PersonRecord {
    let tmdb = int(raw, "id").unwrap_or(0);
    let external = raw.get("external_ids").cloned().unwrap_or(Value::Null);
    let imdb = text(raw, "imdb_id").or_else(|| text(&external, "imdb_id"));
    let credits = raw.get("combined_credits").cloned().unwrap_or(Value::Null);
    PersonRecord {
        id: format!("person-{tmdb}"),
        name: text(raw, "name").unwrap_or_else(|| format!("TMDB person {tmdb}")),
        photo: image(raw, "profile_path"),
        known_for: text(raw, "known_for_department"),
        born: date(raw, "birthday"),
        died: date(raw, "deathday"),
        birthplace: text(raw, "place_of_birth"),
        biography: text(raw, "biography"),
        aka: arr(raw, "also_known_as")
            .iter()
            .filter_map(|a| a.as_str().map(str::to_string))
            .collect(),
        links: PersonLinks {
            tmdb: format!("https://www.themoviedb.org/person/{tmdb}"),
            imdb: imdb.map(|id| format!("https://www.imdb.com/name/{id}/")),
            homepage: text(raw, "homepage").filter(|h| h.starts_with("http")),
        },
        credits: Credits {
            cast: credit_cards(arr(&credits, "cast"), "character"),
            crew: credit_cards(arr(&credits, "crew"), "job"),
        },
        rich: true,
    }
}

pub fn collection_record(raw: &Value) -> CollectionRecord {
    let tmdb = int(raw, "id").unwrap_or(0);
    let mut parts = cards(arr(raw, "parts"), Some(TitleKind::Movie), usize::MAX);
    parts.sort_by(|a, b| a.year.unwrap_or(i32::MAX).cmp(&b.year.unwrap_or(i32::MAX)));
    CollectionRecord {
        id: format!("collection-{tmdb}"),
        name: text(raw, "name").unwrap_or_else(|| format!("Collection {tmdb}")),
        overview: text(raw, "overview"),
        poster: image(raw, "poster_path"),
        backdrop: image(raw, "backdrop_path"),
        parts,
        rich: true,
    }
}

pub fn search_answer(query: &str, multi: &Value, collections: &Value) -> SearchAnswer {
    let results = arr(multi, "results");
    SearchAnswer {
        query: query.to_string(),
        library: Vec::new(),
        titles: cards(results, None, CARDS_MAX),
        people: results
            .iter()
            .filter(|r| r.get("media_type").and_then(Value::as_str) == Some("person"))
            .filter_map(|p| {
                Some(PersonCard {
                    id: person_id(p)?,
                    name: text(p, "name")?,
                    photo: image(p, "profile_path"),
                    known_for: text(p, "known_for_department"),
                })
            })
            .take(CARDS_MAX)
            .collect(),
        collections: arr(collections, "results")
            .iter()
            .filter_map(|c| {
                Some(CollectionCard {
                    id: format!("collection-{}", int(c, "id")?),
                    name: text(c, "name")?,
                    poster: image(c, "poster_path"),
                })
            })
            .take(CARDS_MAX)
            .collect(),
        albums: Vec::new(),
        artists: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn a_movie_record_carries_everything_appended() {
        let r = title_record(TitleKind::Movie, &fixture("tmdb_movie_603.json"), "US");
        assert_eq!(
            (r.id.as_str(), r.kind.as_str(), r.tmdb),
            ("movie-603", "movie", 603)
        );
        assert_eq!(r.imdb.as_deref(), Some("tt0133093"));
        assert_eq!((r.title.as_str(), r.year), ("The Matrix", Some(1999)));
        assert_eq!(r.certification.as_deref(), Some("R"));
        assert_eq!(r.runtime, Some(136));
        assert_eq!(r.genres, vec!["Action", "Science Fiction"]);
        assert_eq!(r.ratings.tmdb.as_ref().and_then(|s| s.votes), Some(26000));
        assert_eq!(r.dates.theatrical.as_deref(), Some("1999-03-31"));
        assert_eq!(r.dates.digital.as_deref(), Some("1999-09-21"));
        assert_eq!(r.dates.physical.as_deref(), Some("1999-09-21"));
        assert_eq!(r.budget, Some(63_000_000));
        assert_eq!(r.countries, vec!["US"]);
        assert_eq!(r.studios[0].name, "Warner Bros. Pictures");
        assert_eq!(r.studio.as_deref(), Some("Warner Bros. Pictures"));
        assert_eq!(
            r.studios[0].browse,
            Some(format!("studio-{}", r.studios[0].id))
        );
        assert!(r.networks.is_none() && r.creators.is_none() && r.seasons.is_none());
        assert_eq!(
            r.keywords,
            vec!["simulated reality", "artificial intelligence"]
        );
        let c = r.collection.as_ref().unwrap();
        assert_eq!(
            (c.id.as_str(), c.name.as_str()),
            ("collection-2344", "The Matrix Collection")
        );
        assert_eq!(r.logo.as_deref(), Some("/logo-en.png"));
        assert_eq!(r.videos.len(), 1, "only YouTube and Vimeo");
        assert_eq!(r.videos[0].kind, "Trailer");
        assert_eq!(r.watch.len(), 2);
        assert_eq!(r.watch[0].name, "Max");
        assert_eq!(r.cast[0].name, "Keanu Reeves");
        assert_eq!(r.cast[0].id, "person-6384");
        assert_eq!(r.cast[0].role, "Neo");
        assert_eq!(r.cast_total, r.cast.len());
        assert_eq!(r.cast[0].episodes, None, "a movie has no episode counts");
        assert_eq!(r.crew_total, r.crew.len());
        assert_eq!(r.crew[0].dept, "Directing");
        assert_eq!(r.collection_name.as_deref(), Some("The Matrix Collection"));
        assert_eq!(r.crew.iter().filter(|c| c.job == "Director").count(), 2);
        assert_eq!(r.recommendations[0].id, "movie-604");
        assert_eq!(r.similar.len(), 1);
        assert_eq!(
            r.links.letterboxd.as_deref(),
            Some("https://letterboxd.com/tmdb/603/")
        );
        assert_eq!(
            r.links.imdb.as_deref(),
            Some("https://www.imdb.com/title/tt0133093/")
        );
        assert!(r.library.is_none() && r.rich);
        // The field names are the prototype's.
        let json = serde_json::to_value(&r).unwrap();
        assert!(json.get("library").is_some_and(Value::is_null));
        assert!(json.get("originalTitle").is_none(), "the same as the title");
        assert_eq!(json["videos"][0]["type"], "Trailer");
        assert!(json.get("castTotal").is_some() && json.get("crewTotal").is_some());
        assert!(
            json.get("history").is_none(),
            "left out when there is nothing"
        );
        assert!(json["ratings"]["tmdb"]["votes"].is_number());
    }

    #[test]
    fn a_show_record_has_networks_creators_and_seasons() {
        let r = title_record(TitleKind::Show, &fixture("tmdb_tv_1396.json"), "US");
        assert_eq!((r.id.as_str(), r.tvdb), ("show-1396", Some(81189)));
        assert_eq!(r.certification.as_deref(), Some("TV-MA"));
        assert_eq!(r.dates.first.as_deref(), Some("2008-01-20"));
        assert_eq!(r.dates.last.as_deref(), Some("2013-09-29"));
        assert_eq!(r.dates.next, None);
        assert_eq!(r.runtime, Some(47));
        assert_eq!(r.networks.as_ref().unwrap()[0].name, "AMC");
        assert_eq!(
            r.networks.as_ref().unwrap()[0].browse.as_deref(),
            Some("network-174")
        );
        assert_eq!(r.network.as_deref(), Some("AMC"));
        assert_eq!(r.creators.as_ref().unwrap()[0].id, "person-66633");
        let seasons = r.seasons.as_ref().unwrap();
        assert_eq!(seasons.len(), 2, "specials are left out");
        assert_eq!((seasons[0].number, seasons[0].total), (1, 7));
        assert_eq!(
            (seasons[0].aired, seasons[1].aired),
            (7, 13),
            "the last episode aired is in season 2"
        );
        // A show's cast is its aggregate cast, over every season, with episode counts.
        assert_eq!(r.cast_total, 3);
        assert_eq!(
            (r.cast[1].name.as_str(), r.cast[1].episodes),
            ("Aaron Paul", Some(62))
        );
        assert_eq!(r.cast[2].role, "Steven Gomez, Gomez");
        assert_eq!(r.cast[2].photo, None);
        // The crew, a person and a job each, the key jobs first.
        let jobs: Vec<(&str, &str)> = r
            .crew
            .iter()
            .map(|c| (c.name.as_str(), c.job.as_str()))
            .collect();
        assert_eq!(
            jobs,
            vec![
                ("Michelle MacLaren", "Director"),
                ("Vince Gilligan", "Writer"),
                ("Vince Gilligan", "Executive Producer"),
                ("Dave Porter", "Original Music Composer"),
                ("Grip Person", "Key Grip"),
            ]
        );
        assert_eq!(r.crew[2].episodes, Some(62));
        assert_eq!(r.crew_total, 5);
        let credits = title_credits(TitleKind::Show, &fixture("tmdb_tv_1396.json"));
        assert_eq!(credits.id, "show-1396");
        assert_eq!((credits.cast.len(), credits.crew.len()), (3, 5));
        assert_eq!(r.keywords, vec!["drug dealer"]);
        assert!(r.links.letterboxd.is_none());
        assert_eq!(
            r.links.tvdb.as_deref(),
            Some("https://thetvdb.com/dereferrer/series/81189")
        );
        let json = serde_json::to_value(&r).unwrap();
        assert_eq!(json["seasons"][0]["airDate"], "2008-01-20");
        assert!(json["seasons"][0]["inLibrary"].is_array());
        assert_eq!(json["crew"][0]["dept"], "Directing");
    }

    #[test]
    fn a_season_lists_its_episodes_with_stills() {
        let s = season_record(1396, &fixture("tmdb_tv_1396_season_1.json"));
        assert_eq!((s.show.as_str(), s.number, s.total), ("show-1396", 1, 3));
        assert_eq!(s.episodes[0].title.as_deref(), Some("Pilot"));
        assert_eq!(s.episodes[0].still.as_deref(), Some("/still1.jpg"));
        assert_eq!(s.episodes[0].state, "missing");
        assert_eq!(s.episodes[2].state, "unaired", "no air date yet");
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["episodes"][0]["airDate"], "2008-01-20");
    }

    #[test]
    fn a_person_has_credits_as_cards_once_per_title() {
        let p = person_record(&fixture("tmdb_person_6384.json"));
        assert_eq!(
            (p.id.as_str(), p.name.as_str()),
            ("person-6384", "Keanu Reeves")
        );
        assert_eq!(p.known_for.as_deref(), Some("Acting"));
        assert_eq!(p.born.as_deref(), Some("1964-09-02"));
        assert_eq!(p.aka, vec!["Киану Ривз"]);
        assert_eq!(
            p.links.imdb.as_deref(),
            Some("https://www.imdb.com/name/nm0000206/")
        );
        let ids: Vec<&str> = p.credits.cast.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["movie-603", "show-9999"],
            "newest first, once each"
        );
        assert_eq!(p.credits.cast[1].role.as_deref(), Some("Himself, Narrator"));
        assert_eq!(p.credits.crew[0].job.as_deref(), Some("Producer"));
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["knownFor"], "Acting");
        assert_eq!(json["credits"]["cast"][0]["role"], "Neo");
    }

    #[test]
    fn a_collection_lists_its_parts_in_order() {
        let c = collection_record(&fixture("tmdb_collection_2344.json"));
        assert_eq!(c.id, "collection-2344");
        assert!(c.rich);
        let years: Vec<Option<i32>> = c.parts.iter().map(|p| p.year).collect();
        assert_eq!(years, vec![Some(1999), Some(2003), Some(2021)]);
    }

    #[test]
    fn lists_and_search_skip_people_where_titles_belong() {
        let raw = fixture("tmdb_trending.json");
        let items = cards(arr(&raw, "results"), None, usize::MAX);
        assert_eq!(
            items.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
            vec!["movie-603", "show-1396"]
        );
        let found = search_answer(
            "matrix",
            &fixture("tmdb_search_multi.json"),
            &fixture("tmdb_search_collection.json"),
        );
        assert_eq!(found.titles.len(), 1);
        assert_eq!(found.people[0].id, "person-6384");
        assert_eq!(found.collections[0].id, "collection-2344");
        for (text, key) in [
            ("trending", Some("trending")),
            ("Genre-Movie-28", Some("genre-movie-28")),
            ("genre-show-18", Some("genre-show-18")),
            ("studio-420", Some("studio-420")),
            ("network-49", Some("network-49")),
            ("on-the-air", Some("on-the-air")),
            ("genre-movie-x", None),
            ("studio-0", None),
            ("popular", None),
        ] {
            assert_eq!(
                ListKey::parse(text).map(ListKey::key).as_deref(),
                key,
                "{text}"
            );
        }
        assert_eq!(
            ListKey::parse("network-49").unwrap().path(2, "US"),
            "/discover/tv?with_networks=49&sort_by=popularity.desc&page=2"
        );
    }
}
