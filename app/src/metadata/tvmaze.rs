//! TVmaze (public API, no key). Shows only. GET only.

use super::model::ProviderInfo;
use super::{Found, MetadataError, MetadataProvider, TitleIds, seasons_from_episodes, year_of};
use crate::catalog::model::TitleKind;
use crate::core::outbound::{OutRequest, Outbound, TVMAZE_HOST, expect_ok};
use async_trait::async_trait;
use serde::Deserialize;

pub struct Tvmaze {
    out: Outbound,
}

#[derive(Deserialize)]
struct RawEpisode {
    season: Option<i32>,
    airdate: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawEmbedded {
    #[serde(default)]
    episodes: Vec<RawEpisode>,
}

#[derive(Deserialize)]
struct RawShow {
    id: i64,
    name: Option<String>,
    premiered: Option<String>,
    #[serde(rename = "_embedded", default)]
    embedded: RawEmbedded,
}

impl Tvmaze {
    pub fn new(out: Outbound) -> Self {
        Tvmaze { out }
    }

    /// The show id behind a lookup. TVmaze answers a lookup with a redirect to the show;
    /// redirects are not followed blindly, so the id is read from the `Location` header.
    async fn resolve(&self, query: &str) -> Result<Option<i64>, MetadataError> {
        let url = format!("https://{TVMAZE_HOST}/lookup/shows?{query}");
        let res = self.out.send(OutRequest::get(&url)).await?;
        match res.status {
            404 => Ok(None),
            301 | 302 | 307 | 308 => {
                let location = res.header("location").ok_or_else(|| {
                    MetadataError::Decode("a redirect without a Location".to_string())
                })?;
                let id = location
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .and_then(|s| s.split('?').next())
                    .and_then(|s| s.parse::<i64>().ok())
                    .ok_or_else(|| MetadataError::Decode(format!("no show id in `{location}`")))?;
                Ok(Some(id))
            }
            _ => {
                expect_ok(&url, &res)?;
                // Some lookups answer with the show itself.
                let show: RawShow = serde_json::from_slice(&res.body)
                    .map_err(|e| MetadataError::Decode(e.to_string()))?;
                Ok(Some(show.id))
            }
        }
    }
}

#[async_trait]
impl MetadataProvider for Tvmaze {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: "tvmaze".to_string(),
            name: "TVmaze".to_string(),
            configured: true,
            needs: None,
            kinds: vec!["show".to_string()],
            host: TVMAZE_HOST.to_string(),
        }
    }

    async fn lookup(&self, ids: &TitleIds) -> Result<Option<Found>, MetadataError> {
        if ids.kind != TitleKind::Show {
            return Err(MetadataError::Unsupported(
                "TVmaze knows shows only".to_string(),
            ));
        }
        let query = match (ids.tvdb, ids.imdb.as_deref()) {
            (Some(tvdb), _) => format!("thetvdb={tvdb}"),
            (None, Some(imdb)) => format!("imdb={imdb}"),
            (None, None) => {
                return Err(MetadataError::Unsupported(
                    "TVmaze needs a tvdb id or an imdb id".to_string(),
                ));
            }
        };
        let Some(id) = self.resolve(&query).await? else {
            return Ok(None);
        };
        let url = format!("https://{TVMAZE_HOST}/shows/{id}?embed=episodes");
        let res = self.out.send(OutRequest::get(&url)).await?;
        if res.status == 404 {
            return Ok(None);
        }
        expect_ok(&url, &res)?;
        let show: RawShow =
            serde_json::from_slice(&res.body).map_err(|e| MetadataError::Decode(e.to_string()))?;
        Ok(Some(Found {
            provider_id: show.id.to_string(),
            title: show.name.unwrap_or_default(),
            year: year_of(show.premiered.as_deref()),
            seasons: seasons_from_episodes(
                show.embedded
                    .episodes
                    .into_iter()
                    .filter_map(|e| Some((e.season?, e.airdate))),
            ),
        }))
    }
}
