//! What the release routes send and receive.

use crate::catalog::model::{ArrReading, DiffCounts, SingularrReading, VerdictView};
use crate::core::model::Page;
use serde::{Deserialize, Serialize};
use singularr_core::{Decision, ParsedRelease};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ParseRequest {
    /// A release name, a scene name or a file name.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ParseResponse {
    /// The fields read from the name. `notes` names the token behind each one.
    #[cfg_attr(feature = "ssr", schema(value_type = crate::core::schema::ParsedReleaseDoc))]
    pub parsed: ParsedRelease,
    /// Source and resolution together, for example `WEBDL-1080p`, when both were read.
    pub quality: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ScoreRequest {
    pub name: String,
    /// The lane to score in. May be left out when `held_copy` is given: its lane is used.
    #[serde(default)]
    pub lane: Option<String>,
    /// A copy id. The release is then judged as a candidate to replace that copy.
    #[serde(default)]
    pub held_copy: Option<i64>,
    /// The release's size, for traits that look at it.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// Tags of the title, for protections that key on them. With `held_copy` the stored
    /// tags of its title are used when this is left out.
    #[serde(default)]
    pub title_tags: Option<Vec<String>>,
}

/// The held copy a candidate was compared with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct HeldCopy {
    pub copy_id: i64,
    pub title_id: i64,
    pub title: String,
    pub library: String,
    pub lane: Option<String>,
    pub release_name: String,
    pub protections: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ScoreResponse {
    pub lane: String,
    #[cfg_attr(feature = "ssr", schema(value_type = crate::core::schema::ParsedReleaseDoc))]
    pub parsed: ParsedRelease,
    /// The candidate's verdict. Its `ladder_steps` holds for `decision.candidate` and
    /// `decision.held` too: they are scored in the same lane.
    #[cfg_attr(feature = "ssr", schema(value_type = crate::core::schema::VerdictDoc))]
    pub verdict: VerdictView,
    /// Whether the release would be taken: against `held` when a held copy was named,
    /// otherwise as if nothing were held.
    #[cfg_attr(feature = "ssr", schema(value_type = crate::core::schema::DecisionDoc))]
    pub decision: Decision,
    pub held: Option<HeldCopy>,
    /// The hash of the policy that scored it.
    pub policy_hash: String,
}

/// The score trace of a held file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct CopyVerdict {
    pub copy_id: i64,
    pub title_id: i64,
    pub title: String,
    pub library: String,
    pub lane: Option<String>,
    pub release_name: String,
    /// Where the parsed name came from: `scene_name`, `original_path` or `library_name`.
    pub name_source: String,
    /// Absent when the copy has no lane in the active policy.
    #[cfg_attr(feature = "ssr", schema(value_type = Option<crate::core::schema::VerdictDoc>))]
    pub verdict: Option<VerdictView>,
    pub protections: Vec<String>,
    pub arr: ArrReading,
    pub singularr: SingularrReading,
    pub differs: Vec<String>,
    /// Custom formats the arr matched for which Singularr matched no trait.
    pub arr_only_formats: Vec<String>,
    /// Scored traits Singularr matched whose custom format the arr did not match.
    pub singularr_only_traits: Vec<String>,
}

/// Filters of `GET /api/v1/releases/shadow`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct ShadowQuery {
    /// One arr integration, by id.
    pub arr: Option<String>,
    /// The kind of difference to list: `quality`, `group`, `edition`, `score`, `any`
    /// (the default, also written `true`) or `all` for every copy, agreeing or not.
    pub disagree: Option<String>,
    pub lane: Option<String>,
    /// Words of the title or of the release name.
    pub q: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// One copy, Singularr's reading next to the arr's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ShadowRow {
    pub copy_id: i64,
    pub title_id: i64,
    pub title: String,
    pub library: String,
    pub engine: String,
    pub lane: Option<String>,
    pub season_number: Option<i32>,
    pub episode_numbers: Vec<i32>,
    pub release_name: String,
    pub name_source: String,
    pub arr: ArrReading,
    pub singularr: SingularrReading,
    /// Any of `quality`, `group`, `edition`, `score`.
    pub differs: Vec<String>,
    /// Custom formats the arr matched for which Singularr matched no trait.
    pub arr_only_formats: Vec<String>,
    /// Scored traits Singularr matched whose custom format the arr did not match.
    pub singularr_only_traits: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct EngineDiffCounts {
    pub engine: String,
    pub counts: DiffCounts,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ShadowResponse {
    /// Counts for the engine asked for, or for all engines.
    pub counts: DiffCounts,
    pub by_engine: Vec<EngineDiffCounts>,
    pub rows: Page<ShadowRow>,
}
