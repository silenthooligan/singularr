//! Metadata providers behind one port: TMDB, TVDB and TVmaze, and the rich records the
//! interface reads (titles, seasons, people, collections, lists, search), from TMDB through
//! the module's cache (`metadata_cache`).

pub mod model;
pub mod record;

#[cfg(feature = "ssr")]
pub mod records;
#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod tmdb;
#[cfg(feature = "ssr")]
pub mod tvdb;
#[cfg(feature = "ssr")]
pub mod tvmaze;

#[cfg(feature = "ssr")]
pub use port::*;

#[cfg(feature = "ssr")]
mod port {
    use super::model::{ProviderInfo, SeasonInfo};
    use crate::catalog::model::TitleKind;
    use crate::core::outbound::OutboundError;
    use async_trait::async_trait;

    /// The ids a title is known by. A provider uses the one it understands.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TitleIds {
        pub kind: TitleKind,
        pub tmdb: Option<i64>,
        pub tvdb: Option<i64>,
        pub imdb: Option<String>,
    }

    /// What a provider knows about a title.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Found {
        /// The provider's own id.
        pub provider_id: String,
        pub title: String,
        pub year: Option<i32>,
        pub seasons: Vec<SeasonInfo>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum MetadataError {
        #[error(transparent)]
        Outbound(#[from] OutboundError),
        #[error("the provider's answer could not be read: {0}")]
        Decode(String),
        /// The provider cannot look this up: it has no such kind, or none of the ids fit.
        #[error("{0}")]
        Unsupported(String),
        #[error("the provider refused the key: {0}")]
        Refused(String),
    }

    #[async_trait]
    pub trait MetadataProvider: Send + Sync {
        fn info(&self) -> ProviderInfo;
        /// `Ok(None)`: the provider answered and has no such title.
        async fn lookup(&self, ids: &TitleIds) -> Result<Option<Found>, MetadataError>;
    }

    /// The year of a date written `YYYY-MM-DD`.
    pub fn year_of(date: Option<&str>) -> Option<i32> {
        date?.get(..4)?.parse().ok().filter(|y| *y > 1800)
    }

    /// Count episodes per season from `(season, air date)` pairs, with each season's
    /// earliest air date.
    pub fn seasons_from_episodes(
        episodes: impl Iterator<Item = (i32, Option<String>)>,
    ) -> Vec<SeasonInfo> {
        let mut by_season: std::collections::BTreeMap<i32, SeasonInfo> =
            std::collections::BTreeMap::new();
        for (season, aired) in episodes {
            let entry = by_season.entry(season).or_insert_with(|| SeasonInfo {
                season,
                episodes: 0,
                first_air_date: None,
            });
            entry.episodes += 1;
            let aired = aired.filter(|d| !d.is_empty());
            if let Some(date) = aired
                && entry
                    .first_air_date
                    .as_ref()
                    .is_none_or(|first| date < *first)
            {
                entry.first_air_date = Some(date);
            }
        }
        by_season.into_values().collect()
    }
}
