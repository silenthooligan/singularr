//! `GET /api/v1/rules`: the rules in force as the Rules page draws them (`web/data/rules.json`):
//! the policy's profiles, traits and protections, the edition list, the request rules and what
//! leaves the library. `routes::router` merges the route with one line.
//!
//! The policy in force is the saved model of quality profiles and lists, changed through
//! `api::profiles`, or the policy file `SINGULARR_POLICY` names, read-only; this route reads
//! either and changes nothing. `POST /api/v1/policy/simulate` replays a proposed policy across
//! the library. What the server cannot know yet is left empty, never invented: no processing
//! flows, no naming (`api::naming` has the conventions), no counts of what retention would
//! touch.

use super::state::AppState;
use crate::catalog::store as catalog_store;
use crate::core::config::{Config, IntegrationType};
use crate::core::error::AppError;
use crate::core::schema::ProblemDoc;
use crate::policy::{ActivePolicy, PolicySource};
use crate::requests::rules as request_rules;
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use singularr_core::EditionList;
use singularr_core::policy::Condition;
use std::collections::BTreeMap;
use utoipa::OpenApi;

/// The rules in force.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RulesView {
    pub name: String,
    pub version: String,
    /// SHA-256 of the policy text.
    pub hash: String,
    pub loaded_at: DateTime<Utc>,
    /// `default`: the product's own policy (the file the image carries, or the copy built
    /// into the program when that file is missing). `file`: the policy file `SINGULARR_POLICY`
    /// names. `saved`: the saved model of quality profiles and lists.
    pub source: String,
    /// The policy file, when one is read.
    pub file: Option<String>,
    /// The variable that names the file, when it is set.
    pub env: Option<String>,
    /// True while the saved model is the source: it is changed through `/api/v1/profiles` and
    /// `/api/v1/lists`, never through this route.
    pub editable: bool,
    pub profiles: Vec<ProfileView>,
    pub traits: Vec<TraitView>,
    pub editions: Vec<EditionView>,
    /// Where the edition list comes from: `file` with `editionsFile`, or `default` (built in).
    pub editions_source: String,
    pub editions_file: Option<String>,
    pub protections: Vec<ProtectionView>,
    pub request_rules: Vec<RequestRuleView>,
    pub retention: Vec<RetentionView>,
    /// Flows that run on a file after import. Empty: this version reads no processing engine.
    #[schema(value_type = Vec<Object>)]
    pub processing: Vec<serde_json::Value>,
    /// How files and folders are named. Empty: in stage 1 the managers name files.
    #[schema(value_type = Vec<Object>)]
    pub naming: Vec<serde_json::Value>,
}

/// A profile (a lane in the policy): the quality a library aims for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileView {
    /// The lane's id: `movies-hd`.
    pub id: String,
    /// The manager's profile it stands for (`HD-1080p`), else the label.
    pub name: String,
    pub label: String,
    /// `movie` or `show`, from the manager it stands for.
    pub kind: Option<String>,
    /// Quality steps, best first; qualities in one step rank the same.
    pub ladder: Vec<Vec<String>>,
    pub min_score: i32,
    pub upgrade_step: i32,
    /// Upgrades stop once a file scores this. Absent: no cutoff score.
    pub cutoff: Option<i32>,
    pub cutoff_quality: Option<String>,
    /// The managers' profiles it stands for: `radarr:HD-1080p`.
    pub arr_profiles: Vec<String>,
    /// Titles the managers place in it.
    pub titles: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TraitView {
    pub id: String,
    pub label: String,
    pub category: String,
    /// Why the rule exists.
    pub reason: String,
    /// The managers' custom formats it stands for, by name.
    pub formats: Vec<String>,
    /// Points per profile id.
    pub points: BTreeMap<String, i32>,
    /// The condition, as the policy writes it.
    #[schema(value_type = Object)]
    pub when: Condition,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EditionView {
    pub key: String,
    pub label: String,
    pub aliases: Vec<String>,
    /// A file whose name names no edition is this edition.
    pub default: bool,
}

/// What may replace the file the library has.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProtectionView {
    pub id: String,
    pub label: String,
    pub reason: String,
    /// It applies to a title that carries one of these tags.
    pub tags: Vec<String>,
    /// It applies when the file the library has matches this.
    #[schema(value_type = Option<Object>)]
    pub current: Option<Condition>,
    /// A candidate must match this to replace the file. Absent: nothing replaces it.
    #[schema(value_type = Option<Object>)]
    pub allow: Option<Condition>,
    /// Titles with a file it guards now.
    pub titles: i64,
}

/// A rule a request meets before anything is searched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestRuleView {
    pub id: String,
    pub label: String,
    pub description: String,
    pub enabled: bool,
    #[schema(value_type = Option<Object>)]
    pub value: Option<serde_json::Value>,
    pub unit: Option<String>,
    /// Days a limit counts over; absent for a rule that is not a limit over time.
    pub period: Option<i64>,
    /// `default` or `environment` (with `env`), as in the settings.
    pub source: String,
    pub env: Option<String>,
    /// The setting that shows it in Settings, People and access.
    pub setting: Option<String>,
}

/// What leaves the library, and when. Until stage 4 a rule only reports.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RetentionView {
    pub id: String,
    pub label: String,
    pub rule: String,
    pub action: String,
    pub enabled: bool,
    /// What it would touch now. Absent: not counted yet (plays are not read).
    pub matches: Option<i64>,
}

/// What the rules view is made of.
pub struct RulesInput<'a> {
    pub policy: &'a ActivePolicy,
    pub editions: &'a EditionList,
    /// The edition list's file, `None` for the copy built in.
    pub editions_file: Option<String>,
    pub config: &'a Config,
    /// The variables that fix the policy file and the request rules, by setting key.
    pub env: BTreeMap<&'static str, String>,
    pub lane_titles: BTreeMap<String, i64>,
    pub protection_titles: BTreeMap<String, i64>,
}

/// The kind of title a manager's profile is for: the integration's type.
fn kind_of(config: &Config, arr_profile: &str) -> Option<String> {
    let id = arr_profile.split_once(':')?.0;
    let kind = config
        .integration(id)
        .map(|i| i.kind)
        .or_else(|| IntegrationType::parse(id))?;
    match kind {
        IntegrationType::Radarr => Some("movie".to_string()),
        IntegrationType::Sonarr => Some("show".to_string()),
        _ => None,
    }
}

/// The view of the rules in force. Pure.
pub fn rules_view(input: &RulesInput<'_>) -> RulesView {
    let p = &input.policy.policy;
    let configured = input.env.get("system.policyFile").cloned();
    let (source, file) = match &input.policy.source {
        PolicySource::Builtin => ("default", None),
        PolicySource::File(path) if configured.is_some() => ("file", Some(path.clone())),
        PolicySource::File(path) => ("default", Some(path.clone())),
        PolicySource::Saved(_) => ("saved", None),
    };
    let profiles = p
        .lanes
        .iter()
        .map(|l| ProfileView {
            id: l.id.clone(),
            name: l
                .arr_profiles
                .first()
                .and_then(|a| a.split_once(':'))
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| l.label.clone()),
            label: l.label.clone(),
            kind: l
                .arr_profiles
                .first()
                .and_then(|a| kind_of(input.config, a)),
            ladder: l.ladder.clone(),
            min_score: l.min_score,
            upgrade_step: l.upgrade_step,
            cutoff: (l.cutoff_score != i32::MAX).then_some(l.cutoff_score),
            cutoff_quality: l.cutoff_quality.clone(),
            arr_profiles: l.arr_profiles.clone(),
            titles: input.lane_titles.get(&l.id).copied().unwrap_or(0),
        })
        .collect();
    let traits = p
        .traits
        .iter()
        .map(|t| {
            let mut formats: Vec<String> = t
                .arr_formats
                .iter()
                .map(|f| f.split_once(':').map(|(_, n)| n).unwrap_or(f).to_string())
                .collect();
            formats.sort();
            formats.dedup();
            TraitView {
                id: t.id.clone(),
                label: t.label.clone(),
                category: t.category.clone(),
                reason: t.why.clone(),
                formats,
                points: p
                    .lanes
                    .iter()
                    .filter_map(|l| l.points.get(&t.id).map(|n| (l.id.clone(), *n)))
                    .collect(),
                when: t.when.clone(),
            }
        })
        .collect();
    let editions = input
        .editions
        .editions
        .iter()
        .map(|e| EditionView {
            key: e.key.clone(),
            label: e.label.clone(),
            aliases: e.aliases.clone(),
            default: e.default,
        })
        .collect();
    let protections = p
        .protections
        .iter()
        .map(|pr| ProtectionView {
            id: pr.id.clone(),
            label: pr.label.clone(),
            reason: pr.why.clone(),
            tags: pr.tags.clone(),
            current: pr.held.clone(),
            allow: pr.allow.clone(),
            titles: input.protection_titles.get(&pr.id).copied().unwrap_or(0),
        })
        .collect();
    let r = &input.config.request_rules;
    let from = |key: &str| match input.env.get(key) {
        Some(var) => ("environment".to_string(), Some(var.clone())),
        None => ("default".to_string(), None),
    };
    let (gate_source, gate_env) = from("people.seasonGate");
    let (max_source, max_env) = from("people.maxSeasons");
    let (owners_source, owners_env) = from("people.requestOwners");
    let request_rules = vec![
        RequestRuleView {
            id: "season-gate".to_string(),
            label: "Season limit".to_string(),
            description: format!(
                "At most {} of one show per member; movies and 4K not limited",
                if r.max_seasons == 1 {
                    "1 season".to_string()
                } else {
                    format!("{} seasons", r.max_seasons)
                }
            ),
            enabled: r.season_gate,
            value: Some(serde_json::json!(r.max_seasons)),
            unit: Some("seasons".to_string()),
            period: None,
            source: if gate_env.is_some() {
                gate_source
            } else {
                max_source
            },
            env: gate_env.or(max_env),
            setting: Some("people.maxSeasons".to_string()),
        },
        RequestRuleView {
            id: "owners-exempt".to_string(),
            label: "Owner exemption".to_string(),
            description: "Owner's Seerr accounts skip the season limit and retention".to_string(),
            enabled: !r.owners.is_empty(),
            value: Some(serde_json::json!(
                r.owners.iter().map(|id| id.to_string()).collect::<Vec<_>>()
            )),
            unit: Some("accounts".to_string()),
            period: None,
            source: owners_source,
            env: owners_env,
            setting: Some("people.requestOwners".to_string()),
        },
        RequestRuleView {
            id: "hd-only".to_string(),
            label: "Portal quality".to_string(),
            description: "HD only; 4K is requested from the workspace".to_string(),
            enabled: true,
            value: None,
            unit: None,
            period: None,
            source: "default".to_string(),
            env: None,
            setting: Some("portal.quality".to_string()),
        },
    ];
    let retention = vec![
        RetentionView {
            id: "watched-requests".to_string(),
            label: "Watched requests".to_string(),
            rule: format!(
                "Season removed {} d after watched to the end",
                request_rules::WATCHED_DAYS
            ),
            action: "Remove files".to_string(),
            enabled: r.season_gate,
            matches: None,
        },
        RetentionView {
            id: "unwatched-requests".to_string(),
            label: "Unwatched requests".to_string(),
            rule: format!(
                "Season unplayed for {} d removed {} d later",
                request_rules::UNPLAYED_DAYS,
                request_rules::UNPLAYED_REMOVAL_DAYS
            ),
            action: "Remove files".to_string(),
            enabled: r.season_gate,
            matches: None,
        },
    ];
    RulesView {
        name: p.name.clone(),
        version: p.version.clone(),
        hash: input.policy.hash.clone(),
        loaded_at: input.policy.loaded_at,
        source: source.to_string(),
        file,
        env: configured,
        editable: matches!(input.policy.source, PolicySource::Saved(_)),
        profiles,
        traits,
        editions,
        editions_source: if input.editions_file.is_some() {
            "file"
        } else {
            "default"
        }
        .to_string(),
        editions_file: input.editions_file.clone(),
        protections,
        request_rules,
        retention,
        processing: Vec::new(),
        naming: Vec::new(),
    }
}

/// The rules in force: profiles, traits, editions, protections, the request rules and what
/// leaves the library, with where the policy comes from. Read-only: the rules are files, a
/// change is a commit, and `POST /api/v1/policy/simulate` shows what it would change first.
#[utoipa::path(get, path = "/api/v1/rules", tag = "policy",
    responses((status = 200, body = RulesView), (status = 401, body = ProblemDoc)))]
async fn rules(State(state): State<AppState>) -> Result<Json<RulesView>, AppError> {
    // The edition list as the file holds it; the program's own copy when there is none.
    let (editions, _warning) = crate::policy::load_editions(&state.config.editions_path)
        .unwrap_or_else(|_| (crate::policy::ActiveEditions::shipped(), None));
    let editions_file = match &editions.source {
        PolicySource::File(path) => Some(path.clone()),
        PolicySource::Builtin | PolicySource::Saved(_) => None,
    };
    let mut env = BTreeMap::new();
    for key in [
        "system.policyFile",
        "people.seasonGate",
        "people.maxSeasons",
        "people.requestOwners",
    ] {
        if let Some(var) = state.settings.env_of(key) {
            env.insert(key, var);
        }
    }
    let active = state.policy.current();
    let input = RulesInput {
        policy: &active,
        editions: &editions.list,
        editions_file,
        config: &state.config,
        env,
        lane_titles: catalog_store::lane_title_counts(&state.pool)
            .await?
            .into_iter()
            .collect(),
        protection_titles: catalog_store::protection_title_counts(&state.pool)
            .await?
            .into_iter()
            .collect(),
    };
    Ok(Json(rules_view(&input)))
}

/// The rules route, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new().route("/rules", get(rules))
}

#[derive(OpenApi)]
#[openapi(paths(rules))]
struct RulesDoc;

/// The rules route's part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    RulesDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ActivePolicy;

    fn config(extra: &[(&'static str, &'static str)]) -> Config {
        let mut e = BTreeMap::from([("DATABASE_URL", "postgres://x@127.0.0.1:1/x")]);
        e.extend(extra.iter().copied());
        Config::from(&e).unwrap()
    }

    fn view(policy: &ActivePolicy, config: &Config) -> RulesView {
        rules_view(&RulesInput {
            policy,
            editions: EditionList::shipped(),
            editions_file: None,
            config,
            env: BTreeMap::new(),
            lane_titles: BTreeMap::from([("movies-hd".to_string(), 12)]),
            protection_titles: BTreeMap::from([("open-matte".to_string(), 3)]),
        })
    }

    /// The keys the Rules page reads, from a reduced copy of its sample (`web/data/rules.json`).
    #[test]
    fn the_rules_answer_the_page_s_shape() {
        let sample: serde_json::Value =
            serde_json::from_str(include_str!("../../tests/fixtures/contract_rules.json")).unwrap();
        let policy = ActivePolicy::builtin();
        let answer = serde_json::to_value(view(&policy, &config(&[]))).unwrap();
        for (key, value) in sample
            .as_object()
            .unwrap()
            .iter()
            .filter(|(k, _)| !k.starts_with('_'))
        {
            let ours = &answer[key];
            assert!(!ours.is_null(), "the answer has no `{key}`");
            if let (Some(theirs), Some(mine)) = (value.as_array(), ours.as_array()) {
                let (Some(theirs), Some(mine)) = (theirs.first(), mine.first()) else {
                    continue;
                };
                for field in theirs.as_object().unwrap().keys() {
                    assert!(
                        mine.get(field).is_some(),
                        "`{key}[]` has no `{field}`: {mine}"
                    );
                }
            }
        }
    }

    #[test]
    fn profiles_traits_and_protections_come_from_the_policy() {
        let policy = ActivePolicy::builtin();
        let v = view(&policy, &config(&[("RADARR_URL", "http://10.0.0.5:7878")]));
        assert_eq!(v.source, "default");
        assert!(!v.editable);
        let hd = v.profiles.iter().find(|p| p.id == "movies-hd").unwrap();
        assert_eq!(hd.kind.as_deref(), Some("movie"));
        assert_eq!(hd.titles, 12);
        assert!(!hd.ladder.is_empty());
        assert_eq!(
            policy.policy.lanes.len(),
            v.profiles.len(),
            "one profile per lane"
        );
        let shows = v.profiles.iter().find(|p| p.id == "shows-hd").unwrap();
        assert_eq!(shows.kind.as_deref(), Some("show"));
        for t in &v.traits {
            let lane_points: usize = policy
                .policy
                .lanes
                .iter()
                .filter(|l| l.points.contains_key(&t.id))
                .count();
            assert_eq!(t.points.len(), lane_points, "{}", t.id);
        }
        let om = v.protections.iter().find(|p| p.id == "open-matte").unwrap();
        assert_eq!(om.titles, 3);
        let current = serde_json::to_value(&om.current).unwrap();
        assert_eq!(current["edition"][0], "Open Matte");
        assert!(v.editions.iter().any(|e| e.default));
        assert!(v.processing.is_empty() && v.naming.is_empty());
    }

    #[test]
    fn the_request_rules_follow_the_configuration() {
        let policy = ActivePolicy::builtin();
        let v = view(
            &policy,
            &config(&[
                ("SINGULARR_REQUEST_MAX_SEASONS", "3"),
                ("SINGULARR_REQUEST_SEASON_GATE", "off"),
            ]),
        );
        let gate = &v.request_rules[0];
        assert_eq!(gate.id, "season-gate");
        assert!(!gate.enabled);
        assert_eq!(gate.value, Some(serde_json::json!(3)));
        assert!(gate.description.contains("3 seasons"));
        assert!(
            v.retention
                .iter()
                .all(|r| !r.enabled && r.matches.is_none())
        );
        let json = serde_json::to_value(&v).unwrap();
        assert!(json["requestRules"].is_array() && json["retention"].is_array());
        for r in json["requestRules"].as_array().unwrap() {
            let text = r["description"].as_str().unwrap();
            assert!(!text.contains('\u{2014}') && !text.contains('\u{2013}'));
        }
    }

    #[test]
    fn the_openapi_document_lists_the_rules_route() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        assert!(doc["paths"]["/api/v1/rules"]["get"].is_object());
        assert!(doc["components"]["schemas"]["RulesView"].is_object());
    }
}
