//! What the policy routes send and receive.

use crate::core::model::Page;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The policy in force, as data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct PolicyView {
    /// SHA-256 of the policy text.
    pub hash: String,
    pub name: String,
    pub version: String,
    /// `file:<path>` or `builtin` (the fallback compiled into the binary).
    pub source: String,
    pub loaded_at: DateTime<Utc>,
    /// Traits, lanes and protections, as `rules/policy.toml` defines them.
    #[cfg_attr(feature = "ssr", schema(value_type = Object))]
    pub policy: singularr_core::Policy,
    /// The policy text as loaded.
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct SimulateRequest {
    /// The whole policy as TOML text.
    pub policy: String,
    /// Only list changes in this lane. Counts always cover every lane.
    #[serde(default)]
    pub lane: Option<String>,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub per_page: Option<u32>,
}

/// One trait whose contribution to a copy's score differs between the two policies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TraitChange {
    pub trait_id: String,
    pub label: String,
    /// Points under the active policy, 0 when the trait did not score.
    pub before: i32,
    /// Points under the proposed policy.
    pub after: i32,
    /// In plain words: `now matches`, `no longer matches`, `points 100 to 50`.
    pub what: String,
}

/// One copy whose score would change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ScoreChange {
    pub copy_id: i64,
    pub title_id: i64,
    pub title: String,
    pub library: String,
    pub lane: String,
    pub release_name: String,
    /// Absent: the active policy has no such lane.
    pub old_score: Option<i32>,
    /// Absent: the proposed policy has no such lane.
    pub new_score: Option<i32>,
    pub delta: i32,
    /// The traits behind the difference, largest effect first.
    pub traits: Vec<TraitChange>,
    /// The ladder position (0 is best; absent: the lane does not take the quality).
    pub old_rank: Option<usize>,
    pub new_rank: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct LaneCount {
    pub lane: String,
    pub copies: i64,
    pub changed: i64,
    pub raised: i64,
    pub lowered: i64,
}

/// What a proposed policy would change. Nothing is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct SimulateResponse {
    /// The proposed policy's name, version and hash.
    pub name: String,
    pub version: String,
    pub hash: String,
    /// The active policy's hash, which the proposal is compared with.
    pub active_hash: String,
    pub copies_evaluated: i64,
    /// Copies that have no lane, or whose lane the proposed policy does not have.
    pub copies_without_lane: i64,
    pub changed: i64,
    pub raised: i64,
    pub lowered: i64,
    /// Copies whose quality enters or leaves its lane's ladder, or moves on it.
    pub rank_changed: i64,
    pub by_lane: Vec<LaneCount>,
    /// The copies whose score changes, largest change first.
    pub changes: Page<ScoreChange>,
}
