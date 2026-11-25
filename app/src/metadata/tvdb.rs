//! TheTVDB (API v4). A key signs in for a token (the one POST, to `/v4/login`); everything
//! else is GET.

use super::model::ProviderInfo;
use super::{Found, MetadataError, MetadataProvider, TitleIds, seasons_from_episodes, year_of};
use crate::catalog::model::TitleKind;
use crate::core::outbound::{Body, OutRequest, Outbound, TVDB_HOST, expect_ok};
use async_trait::async_trait;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;

pub struct Tvdb {
    key: Option<String>,
    out: Outbound,
    token: Mutex<Option<String>>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: Option<T>,
}

#[derive(Deserialize)]
struct RawLogin {
    token: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawEpisode {
    season_number: Option<i32>,
    aired: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSeries {
    id: i64,
    name: Option<String>,
    first_aired: Option<String>,
    year: Option<String>,
    #[serde(default)]
    episodes: Vec<RawEpisode>,
}

#[derive(Deserialize)]
struct RawMovie {
    id: i64,
    name: Option<String>,
    year: Option<String>,
}

#[derive(Deserialize)]
struct RawId {
    id: i64,
}

#[derive(Deserialize)]
struct RawRemote {
    series: Option<RawId>,
    movie: Option<RawId>,
}

impl Tvdb {
    pub fn new(key: Option<String>, out: Outbound) -> Self {
        Tvdb {
            key,
            out,
            token: Mutex::new(None),
        }
    }

    async fn login(&self) -> Result<String, MetadataError> {
        let key = self.key.as_deref().ok_or_else(|| {
            MetadataError::Unsupported("SINGULARR_TVDB_KEY is not set".to_string())
        })?;
        let url = format!("https://{TVDB_HOST}/v4/login");
        let body = serde_json::json!({ "apikey": key }).to_string();
        let res = self
            .out
            .send(OutRequest::post(&url, Body::Json(body)))
            .await?;
        if res.status == 401 {
            return Err(MetadataError::Refused(
                "TVDB answered 401 to the sign-in".to_string(),
            ));
        }
        expect_ok(&url, &res)?;
        let parsed: Envelope<RawLogin> =
            serde_json::from_slice(&res.body).map_err(|e| MetadataError::Decode(e.to_string()))?;
        parsed
            .data
            .map(|d| d.token)
            .ok_or_else(|| MetadataError::Decode("the sign-in answer holds no token".to_string()))
    }

    /// GET with the session token, signing in first when there is none and once more when
    /// the token is refused. `Ok(None)` for a 404.
    async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<Option<T>, MetadataError> {
        let url = format!("https://{TVDB_HOST}/v4{path}");
        let mut token = self.token.lock().await;
        for attempt in 0..2 {
            let bearer = match token.as_deref() {
                Some(t) => t.to_string(),
                None => {
                    let fresh = self.login().await?;
                    *token = Some(fresh.clone());
                    fresh
                }
            };
            let res = self
                .out
                .send(OutRequest::get(&url).header("Authorization", format!("Bearer {bearer}")))
                .await?;
            match res.status {
                401 if attempt == 0 => *token = None,
                401 => return Err(MetadataError::Refused("TVDB answered 401".to_string())),
                404 => return Ok(None),
                _ => {
                    expect_ok(&url, &res)?;
                    let parsed: Envelope<T> = serde_json::from_slice(&res.body)
                        .map_err(|e| MetadataError::Decode(e.to_string()))?;
                    return Ok(parsed.data);
                }
            }
        }
        Ok(None)
    }

    async fn remote(&self, imdb: &str) -> Result<Option<RawRemote>, MetadataError> {
        let found: Option<Vec<RawRemote>> = self.get(&format!("/search/remoteid/{imdb}")).await?;
        Ok(found.and_then(|list| list.into_iter().next()))
    }
}

#[async_trait]
impl MetadataProvider for Tvdb {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "tvdb".to_string(),
            name: "TheTVDB".to_string(),
            configured: self.key.is_some(),
            needs: Some("SINGULARR_TVDB_KEY".to_string()),
            kinds: vec!["movie".to_string(), "show".to_string()],
            host: TVDB_HOST.to_string(),
        }
    }

    async fn lookup(&self, ids: &TitleIds) -> Result<Option<Found>, MetadataError> {
        let needs_imdb = || {
            MetadataError::Unsupported(match ids.kind {
                TitleKind::Show => "TVDB needs a tvdb id or an imdb id for a show".to_string(),
                TitleKind::Movie => "TVDB needs an imdb id for a movie".to_string(),
            })
        };
        match ids.kind {
            TitleKind::Show => {
                let id = match (ids.tvdb, ids.imdb.as_deref()) {
                    (Some(id), _) => id,
                    (None, Some(imdb)) => match self.remote(imdb).await?.and_then(|r| r.series) {
                        Some(series) => series.id,
                        None => return Ok(None),
                    },
                    (None, None) => return Err(needs_imdb()),
                };
                let series: Option<RawSeries> = self
                    .get(&format!("/series/{id}/extended?meta=episodes&short=true"))
                    .await?;
                Ok(series.map(|s| Found {
                    provider_id: s.id.to_string(),
                    title: s.name.unwrap_or_default(),
                    year: s
                        .year
                        .as_deref()
                        .and_then(|y| y.parse().ok())
                        .or_else(|| year_of(s.first_aired.as_deref())),
                    seasons: seasons_from_episodes(
                        s.episodes
                            .into_iter()
                            .filter_map(|e| Some((e.season_number?, e.aired))),
                    ),
                }))
            }
            TitleKind::Movie => {
                let imdb = ids.imdb.as_deref().ok_or_else(needs_imdb)?;
                let Some(movie) = self.remote(imdb).await?.and_then(|r| r.movie) else {
                    return Ok(None);
                };
                let found: Option<RawMovie> = self.get(&format!("/movies/{}", movie.id)).await?;
                Ok(found.map(|m| Found {
                    provider_id: m.id.to_string(),
                    title: m.name.unwrap_or_default(),
                    year: m.year.as_deref().and_then(|y| y.parse().ok()),
                    seasons: Vec::new(),
                }))
            }
        }
    }
}
