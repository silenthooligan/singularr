//! The Movie Database (API v3, bearer token). GET only.

use super::model::{ProviderInfo, SeasonInfo};
use super::{Found, MetadataError, MetadataProvider, TitleIds, year_of};
use crate::catalog::model::TitleKind;
use crate::core::outbound::{OutRequest, Outbound, TMDB_HOST, encode_component};
use crate::engines::seerr::tmdb_path;
use async_trait::async_trait;
use serde::Deserialize;
use serde::de::DeserializeOwned;

pub struct Tmdb {
    token: Option<String>,
    out: Outbound,
}

#[derive(Deserialize)]
struct RawSeason {
    season_number: i32,
    episode_count: Option<i32>,
    air_date: Option<String>,
}

#[derive(Deserialize)]
struct RawTv {
    id: i64,
    name: Option<String>,
    first_air_date: Option<String>,
    #[serde(default)]
    seasons: Vec<RawSeason>,
}

#[derive(Deserialize)]
struct RawMovie {
    id: i64,
    title: Option<String>,
    release_date: Option<String>,
}

#[derive(Deserialize)]
struct RawId {
    id: i64,
}

#[derive(Deserialize)]
struct RawFind {
    #[serde(default)]
    tv_results: Vec<RawId>,
    #[serde(default)]
    movie_results: Vec<RawId>,
}

/// One result of a search: a movie or a show.
#[derive(Debug, Clone, PartialEq)]
pub struct TmdbResult {
    pub kind: TitleKind,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    /// A TMDB image path, `/abc.jpg`.
    pub poster_path: Option<String>,
}

/// One season of a show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmdbSeason {
    pub number: i32,
    pub name: Option<String>,
    pub episodes: i32,
    pub air_date: Option<String>,
}

/// What a title page shows of a title.
#[derive(Debug, Clone, PartialEq)]
pub struct TmdbTitle {
    pub kind: TitleKind,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<i32>,
    pub overview: Option<String>,
    pub poster_path: Option<String>,
    pub backdrop_path: Option<String>,
    pub genres: Vec<String>,
    /// Minutes, for a movie.
    pub runtime: Option<i32>,
    /// The seasons of a show, specials (season 0) left out.
    pub seasons: Vec<TmdbSeason>,
}

#[derive(Deserialize)]
struct RawGenre {
    name: Option<String>,
}

#[derive(Deserialize)]
struct RawSearchResult {
    id: Option<i64>,
    media_type: Option<String>,
    title: Option<String>,
    name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
}

#[derive(Deserialize)]
struct RawSearch {
    #[serde(default)]
    results: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawDetailSeason {
    season_number: i32,
    name: Option<String>,
    episode_count: Option<i32>,
    air_date: Option<String>,
}

#[derive(Deserialize)]
struct RawDetail {
    id: i64,
    title: Option<String>,
    name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    overview: Option<String>,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    #[serde(default)]
    genres: Vec<RawGenre>,
    runtime: Option<i32>,
    #[serde(default)]
    seasons: Vec<RawDetailSeason>,
}

fn text(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

impl Tmdb {
    pub fn new(token: Option<String>, out: Outbound) -> Self {
        Tmdb { token, out }
    }

    /// Movies and shows whose names match the words, best first (TMDB's multi search, adult
    /// titles left out). People are left out.
    pub async fn search(&self, query: &str, page: u32) -> Result<Vec<TmdbResult>, MetadataError> {
        let q = query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let found: Option<RawSearch> = self
            .get(&format!(
                "/search/multi?query={}&page={}&include_adult=false",
                encode_component(q),
                page.clamp(1, 20)
            ))
            .await?;
        Ok(found
            .map(|f| f.results)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value::<RawSearchResult>(v).ok())
            .filter_map(|r| {
                let kind = match r.media_type.as_deref()? {
                    "movie" => TitleKind::Movie,
                    "tv" => TitleKind::Show,
                    _ => return None,
                };
                let title = text(r.title.or(r.name))?;
                Some(TmdbResult {
                    kind,
                    tmdb_id: r.id.filter(|n| *n > 0)?,
                    title,
                    year: year_of(r.release_date.or(r.first_air_date).as_deref()),
                    overview: text(r.overview),
                    poster_path: tmdb_path(r.poster_path),
                })
            })
            .collect())
    }

    /// One title's page facts. `Ok(None)` when TMDB has no such title.
    pub async fn details(
        &self,
        kind: TitleKind,
        tmdb_id: i64,
    ) -> Result<Option<TmdbTitle>, MetadataError> {
        let path = match kind {
            TitleKind::Movie => format!("/movie/{tmdb_id}"),
            TitleKind::Show => format!("/tv/{tmdb_id}"),
        };
        let raw: Option<RawDetail> = self.get(&path).await?;
        Ok(raw.map(|d| TmdbTitle {
            kind,
            tmdb_id: d.id,
            title: text(d.title.or(d.name)).unwrap_or_else(|| format!("TMDB {tmdb_id}")),
            year: year_of(d.release_date.or(d.first_air_date).as_deref()),
            overview: text(d.overview),
            poster_path: tmdb_path(d.poster_path),
            backdrop_path: tmdb_path(d.backdrop_path),
            genres: d.genres.into_iter().filter_map(|g| text(g.name)).collect(),
            runtime: d.runtime.filter(|m| *m > 0),
            seasons: d
                .seasons
                .into_iter()
                .filter(|s| s.season_number > 0)
                .map(|s| TmdbSeason {
                    number: s.season_number,
                    name: text(s.name),
                    episodes: s.episode_count.unwrap_or(0),
                    air_date: text(s.air_date),
                })
                .collect(),
        }))
    }

    /// TMDB's answer to a path (with its query) as JSON. `Ok(None)` for a 404.
    pub async fn get_json(&self, path: &str) -> Result<Option<serde_json::Value>, MetadataError> {
        self.get(path).await
    }

    /// `Ok(None)` for a 404: TMDB has no such record.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, MetadataError> {
        let token = self.token.as_deref().ok_or_else(|| {
            MetadataError::Unsupported("SINGULARR_TMDB_TOKEN is not set".to_string())
        })?;
        let url = format!("https://{TMDB_HOST}/3{path}");
        let res = self
            .out
            .send(
                OutRequest::get(&url)
                    .header("Authorization", format!("Bearer {token}"))
                    .header("Accept", "application/json"),
            )
            .await?;
        match res.status {
            404 => Ok(None),
            401 | 403 => Err(MetadataError::Refused(format!(
                "TMDB answered {}",
                res.status
            ))),
            _ => {
                crate::core::outbound::expect_ok(&url, &res)?;
                serde_json::from_slice(&res.body)
                    .map(Some)
                    .map_err(|e| MetadataError::Decode(e.to_string()))
            }
        }
    }

    /// The TMDB id for the title, from the id given or through `/find`.
    async fn resolve(&self, ids: &TitleIds) -> Result<Option<i64>, MetadataError> {
        if let Some(id) = ids.tmdb {
            return Ok(Some(id));
        }
        let (external, source) = match (ids.kind, ids.tvdb, ids.imdb.as_deref()) {
            (TitleKind::Show, Some(tvdb), _) => (tvdb.to_string(), "tvdb_id"),
            (_, _, Some(imdb)) => (imdb.to_string(), "imdb_id"),
            _ => {
                return Err(MetadataError::Unsupported(
                    "TMDB needs a tmdb id, an imdb id, or for a show a tvdb id".to_string(),
                ));
            }
        };
        let found: Option<RawFind> = self
            .get(&format!("/find/{external}?external_source={source}"))
            .await?;
        Ok(found.and_then(|f| {
            let list = match ids.kind {
                TitleKind::Show => f.tv_results,
                TitleKind::Movie => f.movie_results,
            };
            list.first().map(|r| r.id)
        }))
    }
}

#[async_trait]
impl MetadataProvider for Tmdb {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "tmdb".to_string(),
            name: "The Movie Database".to_string(),
            configured: self.token.is_some(),
            needs: Some("SINGULARR_TMDB_TOKEN".to_string()),
            kinds: vec!["movie".to_string(), "show".to_string()],
            host: TMDB_HOST.to_string(),
        }
    }

    async fn lookup(&self, ids: &TitleIds) -> Result<Option<Found>, MetadataError> {
        let Some(id) = self.resolve(ids).await? else {
            return Ok(None);
        };
        match ids.kind {
            TitleKind::Show => {
                let tv: Option<RawTv> = self.get(&format!("/tv/{id}")).await?;
                Ok(tv.map(|tv| Found {
                    provider_id: tv.id.to_string(),
                    title: tv.name.unwrap_or_default(),
                    year: year_of(tv.first_air_date.as_deref()),
                    seasons: tv
                        .seasons
                        .into_iter()
                        .map(|s| SeasonInfo {
                            season: s.season_number,
                            episodes: s.episode_count.unwrap_or(0),
                            first_air_date: s.air_date.filter(|d| !d.is_empty()),
                        })
                        .collect(),
                }))
            }
            TitleKind::Movie => {
                let movie: Option<RawMovie> = self.get(&format!("/movie/{id}")).await?;
                Ok(movie.map(|m| Found {
                    provider_id: m.id.to_string(),
                    title: m.title.unwrap_or_default(),
                    year: year_of(m.release_date.as_deref()),
                    seasons: Vec::new(),
                }))
            }
        }
    }
}
