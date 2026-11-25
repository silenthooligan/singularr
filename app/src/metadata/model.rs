//! What the metadata routes send and receive.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ProviderInfo {
    /// `tmdb`, `tvdb` or `tvmaze`.
    pub id: String,
    pub name: String,
    /// False when the provider needs a key that is not set. It is then not called.
    pub configured: bool,
    /// The variable that configures it, when it needs one.
    pub needs: Option<String>,
    /// What it can look up: `movie`, `show`.
    pub kinds: Vec<String>,
    /// The host Singularr calls for it.
    pub host: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct SeasonInfo {
    pub season: i32,
    pub episodes: i32,
    /// `YYYY-MM-DD`.
    pub first_air_date: Option<String>,
}

/// One provider's answer about one title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ProviderTitle {
    pub provider: String,
    /// `ok`, `not_found`, `unconfigured`, `unsupported` or `error`.
    pub status: String,
    /// Why, for every status but `ok`.
    pub message: Option<String>,
    pub provider_id: Option<String>,
    pub title: Option<String>,
    pub year: Option<i32>,
    pub seasons: Vec<SeasonInfo>,
}

/// Filters of `GET /api/v1/metadata/compare`. Give at least one id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct CompareQuery {
    /// `show` (the default) or `movie`.
    pub kind: Option<String>,
    pub tmdb: Option<i64>,
    pub tvdb: Option<i64>,
    /// For example `tt0903747`.
    pub imdb: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CompareResponse {
    pub kind: String,
    pub tmdb: Option<i64>,
    pub tvdb: Option<i64>,
    pub imdb: Option<String>,
    pub providers: Vec<ProviderTitle>,
    /// Where the providers that answered disagree, in plain sentences.
    pub differences: Vec<String>,
}
