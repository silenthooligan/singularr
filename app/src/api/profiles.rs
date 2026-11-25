//! Quality profiles, lists and where the policy comes from (`docs/profiles.md`).
//!
//! `GET /api/v1/profiles` and `/api/v1/lists` answer the model of the policy in force: the saved
//! model, or the pinned policy file read into the model (read-only, with Import). A change is
//! the owner's, and only while the saved model is the source: each is a new version
//! (`policy::saved`), put in force at once, and the library's files are assessed again.
//! `POST /api/v1/profiles/preview` ranks releases under a draft profile without saving it.

use super::routes::{ApiJson, ApiPath, ApiQuery};
use super::state::AppState;
use crate::catalog::ingest::Assess;
use crate::catalog::store::{self as catalog_store, HeldFile, ReleaseTags};
use crate::core::auth::Caller;
use crate::core::error::AppError;
use crate::core::schema::ProblemDoc;
use crate::policy::saved::{self, Change, Origin, SaveError, Saved};
use crate::policy::{ActiveEditions, ActivePolicy, PolicySource, held_context};
use crate::releases::assess::Assessor;
use crate::settings::PatchError;
use crate::settings::model::SettingsPatch;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use singularr_core::policy::{EvalContext, Lane, Policy, evaluate};
use singularr_core::profile::{
    self, Group, List, ListKind, Model, Profile, Rule, STRIP, Stance, slug, source_label, unique_id,
};
use singularr_core::release::{self, ParsedRelease, Resolution};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use utoipa::OpenApi;

/// The problem document that says why a request was not answered.
pub struct Refused(Box<Response>);

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        *self.0
    }
}

/// An answer, or the problem document that says why not.
type Answer = Result<Response, Refused>;

fn fail(e: impl Into<AppError>) -> Refused {
    Refused(Box::new(e.into().into_response()))
}

fn problem(status: StatusCode, code: &str, title: &str, detail: impl Into<String>) -> Refused {
    let mut p = crate::core::http::Problem::new(status, code, title);
    p.detail = Some(detail.into());
    Refused(Box::new(p.into_response()))
}

fn save_failed(e: SaveError) -> Refused {
    match e {
        SaveError::Invalid(why) => fail(AppError::BadRequest(why)),
        SaveError::NotFound(why) => fail(AppError::NotFound(why)),
        SaveError::Conflict(why) => {
            problem(StatusCode::CONFLICT, "singularr.conflict", "Conflict", why)
        }
        SaveError::Body(why) => fail(AppError::Internal(why)),
        SaveError::Db(e) => fail(AppError::Db(e)),
    }
}

fn owner_only(caller: &Caller) -> Result<(), Refused> {
    if matches!(caller, Caller::Owner { .. }) {
        Ok(())
    } else {
        Err(fail(AppError::Forbidden(
            "Only the owner may change quality profiles, lists and naming.".to_string(),
        )))
    }
}

/// The problem a change meets while the policy file is in force: it names the file.
fn pinned(state: &AppState) -> Refused {
    let file = &state.config.policy_path;
    let detail = match state.settings.env_of("system.policyFile") {
        Some(var) => format!("The policy file {file} ({var}) is in force. Import it to edit."),
        None => format!("The policy file {file} is in force. Import it to edit."),
    };
    problem(
        StatusCode::CONFLICT,
        "singularr.policy_file",
        "Read-only",
        detail,
    )
}

/// Where the policy comes from.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourceView {
    /// `file`: the policy file, read-only, with Import. `saved`: the saved model, editable.
    pub kind: String,
    /// The policy file.
    pub file: String,
    /// The variable that names the file, when it is set.
    pub file_env: Option<String>,
    /// Where the choice of source comes from: `default`, `environment` or `saved`.
    pub setting_source: String,
    /// The variable that fixes the source, when one does; it cannot be changed here then.
    pub setting_env: Option<String>,
    /// The newest saved version, when and by whom it was saved.
    pub version: Option<i64>,
    pub saved_at: Option<DateTime<Utc>>,
    pub saved_by: Option<String>,
}

fn source_view(
    state: &AppState,
    caller: &Caller,
    origin: Origin,
    latest: Option<&Saved>,
) -> SourceView {
    let field = state.settings.field(saved::SOURCE, caller);
    SourceView {
        kind: origin.as_str().to_string(),
        file: state.config.policy_path.clone(),
        file_env: state.settings.env_of("system.policyFile"),
        setting_source: field
            .as_ref()
            .map_or_else(|| "default".to_string(), |f| f.source.clone()),
        setting_env: field.and_then(|f| f.env),
        version: latest.map(|s| s.version),
        saved_at: latest.map(|s| s.saved_at),
        saved_by: latest.map(|s| s.saved_by.clone()),
    }
}

/// The model of the policy in force, and the newest saved version.
struct Current {
    origin: Origin,
    model: Model,
    latest: Option<Saved>,
}

impl Current {
    fn editable(&self) -> bool {
        self.origin == Origin::Saved
    }
}

async fn current(state: &AppState) -> Result<Current, Refused> {
    let origin = saved::origin(&state.settings);
    let latest = saved::latest(&state.pool).await.map_err(save_failed)?;
    let model = match (&latest, origin) {
        (Some(s), Origin::Saved) => s.model.clone(),
        _ => {
            // The policy in force read into the model; the naming as saved.
            let active = state.policy.current();
            let mut m = saved::imported(
                &active.policy,
                &state.config,
                latest.as_ref().map(|s| &s.model),
            );
            m.fill();
            m
        }
    };
    Ok(Current {
        origin,
        model,
        latest,
    })
}

/// Assess the library's files again under the policy now in force, in the background.
fn reassess_later(state: &AppState, cause: Option<i64>) {
    let (editions, _) = crate::policy::load_editions(&state.config.editions_path)
        .unwrap_or_else(|_| (ActiveEditions::shipped(), None));
    let assess: Arc<dyn Assess> = Arc::new(Assessor::live(
        state.policy.clone(),
        Arc::new(editions),
        env!("SINGULARR_BUILD_ID"),
    ));
    let (pool, journal) = (state.pool.clone(), state.journal.clone());
    tokio::spawn(async move {
        match crate::catalog::sync::reassess(&pool, &journal, assess, cause).await {
            Ok(r) => tracing::info!(
                examined = r.examined,
                updated = r.updated,
                score_changed = r.score_changed,
                "files assessed again under the policy now in force"
            ),
            Err(e) => tracing::warn!(error = %e, "assessing the files again failed"),
        }
    });
}

/// Put a policy in force and have the files assessed again.
async fn activate(state: &AppState, active: ActivePolicy) -> Result<(), Refused> {
    let event = saved::put_in_force(&state.pool, &state.journal, &state.policy, active)
        .await
        .map_err(fail)?;
    reassess_later(state, event.map(|e| e.id));
    Ok(())
}

/// Put in force what the policy source setting says now: the policy file, or the saved model
/// (which starts as the file, imported, when nothing is saved yet).
pub async fn apply_source(state: &AppState) -> Result<(), AppError> {
    let active = match saved::origin(&state.settings) {
        Origin::File => {
            crate::policy::load(&state.config.policy_path)
                .map_err(|e| AppError::BadRequest(e.to_string()))?
                .policy
        }
        Origin::Saved => {
            let in_force = state.policy.current();
            saved::starting_policy(
                &state.pool,
                &state.journal,
                &state.settings,
                &state.config,
                (*in_force).clone(),
            )
            .await
            .map_err(AppError::Internal)?
        }
    };
    let event = saved::put_in_force(&state.pool, &state.journal, &state.policy, active).await?;
    reassess_later(state, event.map(|e| e.id));
    Ok(())
}

/// A change of the saved model by the owner, put in force at once.
async fn edit<T>(
    state: &AppState,
    caller: &Caller,
    action: &str,
    subject: &str,
    f: impl FnOnce(&mut Model) -> Result<T, SaveError>,
) -> Result<(T, Saved), Refused> {
    owner_only(caller)?;
    if saved::origin(&state.settings) == Origin::File {
        return Err(pinned(state));
    }
    let actor = caller.actor();
    let in_force = state.policy.current();
    let config = &state.config;
    let what = Change {
        action,
        subject,
        actor: &actor,
    };
    let (out, saved, active) = saved::change(
        &state.pool,
        &state.journal,
        what,
        || saved::imported(&in_force.policy, config, None),
        f,
    )
    .await
    .map_err(save_failed)?;
    activate(state, active).await?;
    Ok((out, saved))
}

async fn lane_titles(state: &AppState) -> Result<BTreeMap<String, i64>, Refused> {
    Ok(catalog_store::lane_title_counts(&state.pool)
        .await
        .map_err(fail)?
        .into_iter()
        .collect())
}

/// A profile as the pages read it: its answers, the line that says it, and its titles.
fn profile_json(model: &Model, p: &Profile, titles: &BTreeMap<String, i64>) -> Value {
    let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("summary".to_string(), json!(p.summary(model)));
        o.insert(
            "titles".to_string(),
            json!(titles.get(&p.id).copied().unwrap_or(0)),
        );
    }
    v
}

/// The numbers a profile compiles to, for whoever asks for them.
fn lane_json(policy: &Policy, lane: &Lane) -> Value {
    let label = |id: &str| {
        policy
            .traits
            .iter()
            .find(|t| t.id == id)
            .map_or_else(|| id.to_string(), |t| t.label.clone())
    };
    let mut points: Vec<(&String, i32)> = lane.points.iter().map(|(k, v)| (k, *v)).collect();
    points.sort_by_key(|(_, p)| std::cmp::Reverse(p.abs()));
    json!({
        "ladder": lane.ladder,
        "minScore": lane.min_score,
        "upgradeStep": lane.upgrade_step,
        "cutoff": (lane.cutoff_score != i32::MAX).then_some(lane.cutoff_score),
        "cutoffQuality": lane.cutoff_quality,
        "points": points
            .iter()
            .map(|(id, p)| json!({"trait": id, "label": label(id), "points": p}))
            .collect::<Vec<_>>(),
    })
}

/// What a profile can answer: the options of each group, the sources, the resolutions.
fn options() -> Value {
    let groups: Vec<Value> = Group::ALL
        .iter()
        .map(|g| {
            json!({
                "id": g,
                "answer": g.answer(),
                "label": g.label(),
                "choices": profile::choices(*g)
                    .map(|c| json!({"key": c.key, "label": c.label}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({
        "groups": groups,
        "sources": STRIP
            .iter()
            .map(|s| json!({"key": s, "label": source_label(*s)}))
            .collect::<Vec<_>>(),
        "resolutions": [Resolution::R720, Resolution::R1080, Resolution::R2160],
        "stances": [Stance::Prefer, Stance::Allow, Stance::Avoid, Stance::Block],
    })
}

/// `GET /api/v1/profiles`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfilesView {
    pub source: SourceView,
    /// Changes are taken: the saved model is the source.
    pub editable: bool,
    /// The saved version in force, when the saved model is the source.
    pub version: Option<i64>,
    /// Each profile's answers, with `summary` (its one line) and `titles`.
    #[schema(value_type = Vec<Object>)]
    pub profiles: Vec<Value>,
    /// The lists a profile ranks: `{id, name, kind, members}` (a count).
    #[schema(value_type = Vec<Object>)]
    pub lists: Vec<Value>,
    /// The advanced rules: `{id, name, category, why, when}`.
    #[schema(value_type = Vec<Object>)]
    pub rules: Vec<Value>,
    /// The groups of options with their choices, the sources, resolutions and stances.
    #[schema(value_type = Object)]
    pub options: Value,
}

/// Every quality profile, with what a profile can answer and where the policy comes from.
#[utoipa::path(get, path = "/api/v1/profiles", tag = "policy",
    responses((status = 200, body = ProfilesView), (status = 401, body = ProblemDoc)))]
async fn profiles_list(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> Answer {
    let cur = current(&state).await?;
    let titles = lane_titles(&state).await?;
    let view = ProfilesView {
        source: source_view(&state, &caller, cur.origin, cur.latest.as_ref()),
        editable: cur.editable(),
        version: cur
            .latest
            .as_ref()
            .filter(|_| cur.editable())
            .map(|s| s.version),
        profiles: cur
            .model
            .profiles
            .iter()
            .map(|p| profile_json(&cur.model, p, &titles))
            .collect(),
        lists: cur
            .model
            .lists
            .iter()
            .map(|l| json!({"id": l.id, "name": l.name, "kind": l.kind, "members": l.members.len()}))
            .collect(),
        rules: cur
            .model
            .rules
            .iter()
            .map(|r| json!({"id": r.id, "name": r.name, "category": r.category, "why": r.why, "when": r.when}))
            .collect(),
        options: options(),
    };
    Ok(Json(view).into_response())
}

/// One quality profile, with the lane it compiles to under `compiled`.
#[utoipa::path(get, path = "/api/v1/profiles/{id}", tag = "policy",
    params(("id" = String, Path, description = "The profile's id")),
    responses((status = 200, description = "The profile", body = Object), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn profile_get(State(state): State<AppState>, ApiPath(id): ApiPath<String>) -> Answer {
    let cur = current(&state).await?;
    let p = cur.model.profile(&id).ok_or_else(|| {
        fail(AppError::not_found(format!(
            "There is no quality profile `{id}`."
        )))
    })?;
    let titles = lane_titles(&state).await?;
    let mut v = profile_json(&cur.model, p, &titles);
    let compiled = profile::compile(&cur.model, "preview")
        .map_err(|e| fail(AppError::BadRequest(e.to_string())))?;
    if let (Some(lane), Some(o)) = (compiled.lane(&id), v.as_object_mut()) {
        o.insert("compiled".to_string(), lane_json(&compiled, lane));
        o.insert("editable".to_string(), json!(cur.editable()));
    }
    Ok(Json(v).into_response())
}

fn not_found_profile(id: &str) -> SaveError {
    SaveError::NotFound(format!("There is no quality profile `{id}`."))
}

/// Add a quality profile. The answers it leaves out take their defaults; `id` comes from the
/// name when it is left out.
#[utoipa::path(post, path = "/api/v1/profiles", tag = "policy", request_body = Object,
    responses((status = 201, description = "The profile as saved", body = Object), (status = 400, body = ProblemDoc),
        (status = 403, body = ProblemDoc), (status = 409, description = "The policy file is in force, or the id is taken", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn profile_create(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(mut p): ApiJson<Profile>,
) -> Answer {
    owner_only(&caller)?;
    p.name = p.name.trim().to_string();
    if p.name.is_empty() {
        return Err(fail(AppError::bad_request("`name` is required.")));
    }
    let cur = current(&state).await?;
    let taken: BTreeSet<String> = cur.model.profiles.iter().map(|x| x.id.clone()).collect();
    let asked = p.id.trim().to_string();
    if !asked.is_empty() && taken.contains(&asked) {
        return Err(save_failed(SaveError::Conflict(format!(
            "There is a quality profile `{asked}` already."
        ))));
    }
    let id = if asked.is_empty() {
        unique_id(&slug(&p.name), &taken)
    } else {
        asked
    };
    let subject = format!("profile:{id}");
    let ((), saved) = edit(&state, &caller, "profile_saved", &subject, |m| {
        if m.profile(&id).is_some() {
            return Err(SaveError::Conflict(format!(
                "There is a quality profile `{id}` already."
            )));
        }
        p.id = id.clone();
        p.rev = 1;
        p.fill(m);
        m.profiles.push(p);
        Ok(())
    })
    .await?;
    created_profile(&state, &saved, &id, StatusCode::CREATED).await
}

async fn created_profile(state: &AppState, saved: &Saved, id: &str, status: StatusCode) -> Answer {
    let p = saved
        .model
        .profile(id)
        .ok_or_else(|| save_failed(not_found_profile(id)))?;
    let titles = lane_titles(state).await?;
    Ok((status, Json(profile_json(&saved.model, p, &titles))).into_response())
}

/// Save a quality profile's answers. `id` is the path's.
#[utoipa::path(put, path = "/api/v1/profiles/{id}", tag = "policy", request_body = Object,
    params(("id" = String, Path, description = "The profile's id")),
    responses((status = 200, description = "The profile as saved", body = Object), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn profile_put(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiJson(mut p): ApiJson<Profile>,
) -> Answer {
    p.name = p.name.trim().to_string();
    let subject = format!("profile:{id}");
    let ((), saved) = edit(&state, &caller, "profile_saved", &subject, |m| {
        let at = m
            .profiles
            .iter()
            .position(|x| x.id == id)
            .ok_or_else(|| not_found_profile(&id))?;
        p.id = id.clone();
        p.rev = m.profiles[at].rev + 1;
        p.fill(m);
        m.profiles[at] = p;
        Ok(())
    })
    .await?;
    created_profile(&state, &saved, &id, StatusCode::OK).await
}

/// Delete a quality profile. The last one stays.
#[utoipa::path(delete, path = "/api/v1/profiles/{id}", tag = "policy",
    params(("id" = String, Path, description = "The profile's id")),
    responses((status = 200, description = "`{deleted, version}`", body = Object), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "The policy file is in force, or it is the last profile", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn profile_delete(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> Answer {
    let subject = format!("profile:{id}");
    let ((), saved) = edit(&state, &caller, "profile_deleted", &subject, |m| {
        let before = m.profiles.len();
        m.profiles.retain(|x| x.id != id);
        if m.profiles.len() == before {
            return Err(not_found_profile(&id));
        }
        if m.profiles.is_empty() {
            return Err(SaveError::Conflict(
                "The last quality profile cannot be deleted.".to_string(),
            ));
        }
        Ok(())
    })
    .await?;
    Ok(Json(json!({"deleted": id, "version": saved.version})).into_response())
}

/// A copy of a quality profile, under a new id. The copy stands for no manager's profile.
#[utoipa::path(post, path = "/api/v1/profiles/{id}/duplicate", tag = "policy",
    params(("id" = String, Path, description = "The profile's id")),
    responses((status = 201, description = "The copy", body = Object), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn profile_duplicate(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> Answer {
    owner_only(&caller)?;
    let cur = current(&state).await?;
    let taken: BTreeSet<String> = cur.model.profiles.iter().map(|x| x.id.clone()).collect();
    let new_id = unique_id(&format!("{id}-copy"), &taken);
    let subject = format!("profile:{new_id}");
    let ((), saved) = edit(&state, &caller, "profile_saved", &subject, |m| {
        let source = m
            .profile(&id)
            .cloned()
            .ok_or_else(|| not_found_profile(&id))?;
        if m.profile(&new_id).is_some() {
            return Err(SaveError::Conflict(format!(
                "There is a quality profile `{new_id}` already."
            )));
        }
        m.profiles.push(Profile {
            id: new_id.clone(),
            name: format!("{} copy", source.name),
            rev: 1,
            manager_profiles: Vec::new(),
            ..source
        });
        Ok(())
    })
    .await?;
    created_profile(&state, &saved, &new_id, StatusCode::CREATED).await
}

/// `POST /api/v1/profiles/preview`.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRequest {
    /// The profile as drafted; its answers left out take their defaults.
    #[schema(value_type = Object)]
    pub profile: Profile,
    /// A title whose files the library holds: `movie-603`, `show-1396`, or the catalog's
    /// number.
    #[serde(default)]
    pub title: Option<String>,
    /// Release names to rank as given.
    #[serde(default)]
    pub releases: Vec<String>,
    /// Lists as drafted, in place of the saved ones of the same id.
    #[serde(default)]
    #[schema(value_type = Vec<Object>)]
    pub lists: Vec<List>,
    /// At most this many files of the library (default 12, at most 50).
    #[serde(default)]
    pub limit: Option<i64>,
}

/// One ranked release.
struct Ranked {
    from: &'static str,
    file: Option<String>,
    size: Option<i64>,
    release: ParsedRelease,
    verdict: singularr_core::Verdict,
}

/// The catalog's number of a title: `movie-603`, `show-1396` or the number itself.
async fn title_number(state: &AppState, title: &str) -> Result<Option<i64>, Refused> {
    if let Ok(n) = title.trim().parse::<i64>() {
        return Ok(Some(n));
    }
    let Some((kind, tmdb)) = title.trim().split_once('-') else {
        return Ok(None);
    };
    let Ok(tmdb) = tmdb.parse::<i64>() else {
        return Ok(None);
    };
    let found = catalog_store::titles_with_ids(&state.pool, &[tmdb], &[], &[])
        .await
        .map_err(fail)?;
    Ok(found.into_iter().find(|t| t.kind == kind).map(|t| t.id))
}

fn held_release(f: &HeldFile) -> (ParsedRelease, EvalContext) {
    let release: ParsedRelease = serde_json::from_value(f.held.clone()).unwrap_or_default();
    let ctx = held_context(
        f.size_bytes,
        &f.tags,
        f.original_language.as_deref(),
        &f.languages,
        f.media_info.as_ref(),
    );
    (release, ctx)
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Rank releases under a draft profile, with the reasons in the profile's words. Nothing is
/// saved. The releases are the names given, the files of a title, or else a sample of the
/// library's files of the profile's kind.
#[utoipa::path(post, path = "/api/v1/profiles/preview", tag = "policy", request_body = PreviewRequest,
    responses((status = 200, description = "`{profile, compiled, ranking: [{name, from, file, size, quality, score, rank, accepted, why, reasons}]}`", body = Object),
        (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn profiles_preview(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<PreviewRequest>,
) -> Answer {
    let cur = current(&state).await?;
    let mut model = cur.model;
    for list in req.lists {
        match model.lists.iter_mut().find(|l| l.id == list.id) {
            Some(l) => *l = list,
            None => model.lists.push(list),
        }
    }
    let mut draft = req.profile;
    if draft.id.trim().is_empty() {
        draft.id = "draft".to_string();
    }
    if draft.name.trim().is_empty() {
        draft.name = "Draft".to_string();
    }
    // The draft alone: other profiles do not change how it ranks.
    model.profiles = vec![draft];
    model.fill();
    let policy = profile::compile(&model, "preview")
        .map_err(|e| fail(AppError::BadRequest(e.to_string())))?;
    let p = &model.profiles[0];
    let lane = policy
        .lane(&p.id)
        .ok_or_else(|| fail(AppError::Internal("the draft has no lane".to_string())))?;

    let mut items: Vec<Ranked> = Vec::new();
    let score = |release: ParsedRelease, ctx: &EvalContext| {
        let verdict = evaluate(&policy, lane, &release, ctx);
        (release, verdict)
    };
    for name in req
        .releases
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .take(200)
    {
        let (release, verdict) = score(release::parse(name), &EvalContext::default());
        items.push(Ranked {
            from: "given",
            file: None,
            size: None,
            release,
            verdict,
        });
    }
    let limit = req.limit.unwrap_or(12).clamp(1, 50);
    let files = match req.title.as_deref() {
        Some(title) => match title_number(&state, title).await? {
            Some(id) => catalog_store::held_files_of_title(&state.pool, id, limit)
                .await
                .map_err(fail)?,
            None => {
                return Err(fail(AppError::not_found(format!(
                    "There is no title `{title}` in the library."
                ))));
            }
        },
        None if items.is_empty() => {
            let kind = match p.target.kind {
                profile::TitleKind::Movie => "movie",
                profile::TitleKind::Show => "show",
            };
            catalog_store::held_files_sample(&state.pool, kind, &p.id, limit)
                .await
                .map_err(fail)?
        }
        None => Vec::new(),
    };
    for f in &files {
        let (release, ctx) = held_release(f);
        let (release, verdict) = score(release, &ctx);
        items.push(Ranked {
            from: "library",
            file: Some(file_name(&f.relative_path)),
            size: Some(f.size_bytes),
            release,
            verdict,
        });
    }
    let accepted = |v: &singularr_core::Verdict| v.rank.is_some() && v.score >= lane.min_score;
    items.sort_by(|a, b| {
        accepted(&b.verdict)
            .cmp(&accepted(&a.verdict))
            .then_with(|| {
                a.verdict
                    .rank
                    .unwrap_or(usize::MAX)
                    .cmp(&b.verdict.rank.unwrap_or(usize::MAX))
            })
            .then_with(|| b.verdict.score.cmp(&a.verdict.score))
    });
    let ranking: Vec<Value> = items
        .iter()
        .map(|r| {
            let ok = accepted(&r.verdict);
            let why = if ok {
                None
            } else if r.verdict.rank.is_none() {
                Some("Not on the source strip")
            } else {
                Some("Blocked")
            };
            json!({
                "name": r.release.raw,
                "from": r.from,
                "file": r.file,
                "size": r.size,
                "quality": r.verdict.quality,
                "score": r.verdict.score,
                "rank": r.verdict.rank,
                "accepted": ok,
                "why": why,
                "reasons": profile::explain(&model, p, &r.verdict.trace),
            })
        })
        .collect();
    Ok(Json(json!({
        "profile": p.id,
        "summary": p.summary(&model),
        "compiled": lane_json(&policy, lane),
        "ranking": ranking,
    }))
    .into_response())
}

/// `GET /api/v1/lists`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListsView {
    pub source: SourceView,
    pub editable: bool,
    pub version: Option<i64>,
    /// Each list with `files` (the library's files from its members), `library` (each member
    /// with its files) and `usedBy` (the profiles that prefer, avoid or block it).
    #[schema(value_type = Vec<Object>)]
    pub lists: Vec<Value>,
}

fn list_json(model: &Model, l: &List, tags: &[ReleaseTags]) -> Value {
    let named = |row: &ReleaseTags, same: &dyn Fn(&str) -> bool| match l.kind {
        ListKind::Group => {
            row.release_group.as_deref().is_some_and(same)
                || row.collective.as_deref().is_some_and(same)
        }
        ListKind::Service => row.service.as_deref().is_some_and(same),
    };
    let files: i64 = tags
        .iter()
        .filter(|row| named(row, &|n| l.has(n)))
        .map(|row| row.files)
        .sum();
    let members: Vec<Value> = l
        .members
        .iter()
        .map(|m| {
            let n: i64 = tags
                .iter()
                .filter(|row| named(row, &|x| x.eq_ignore_ascii_case(m)))
                .map(|row| row.files)
                .sum();
            json!({"name": m, "files": n})
        })
        .collect();
    let used: Vec<Value> = model
        .uses_of_list(&l.id)
        .into_iter()
        .filter(|(_, stance)| *stance != Stance::Allow)
        .map(|(p, stance)| json!({"profile": p.id, "name": p.name, "stance": stance}))
        .collect();
    let mut v = serde_json::to_value(l).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.insert("files".to_string(), json!(files));
        o.insert("library".to_string(), json!(members));
        o.insert("usedBy".to_string(), json!(used));
    }
    v
}

/// Every list of release groups and streaming services, with what each means in the library.
#[utoipa::path(get, path = "/api/v1/lists", tag = "policy",
    responses((status = 200, body = ListsView), (status = 401, body = ProblemDoc)))]
async fn lists_get(State(state): State<AppState>, Extension(caller): Extension<Caller>) -> Answer {
    let cur = current(&state).await?;
    let tags = catalog_store::release_tags(&state.pool)
        .await
        .map_err(fail)?;
    let view = ListsView {
        source: source_view(&state, &caller, cur.origin, cur.latest.as_ref()),
        editable: cur.editable(),
        version: cur
            .latest
            .as_ref()
            .filter(|_| cur.editable())
            .map(|s| s.version),
        lists: cur
            .model
            .lists
            .iter()
            .map(|l| list_json(&cur.model, l, &tags))
            .collect(),
    };
    Ok(Json(view).into_response())
}

#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct UnlistedQuery {
    /// At most this many of each (default 100, at most 1000).
    limit: Option<usize>,
}

/// The names seen and in no list of their kind, the most frequent first.
fn unlisted_of(
    model: &Model,
    kind: ListKind,
    tags: &[ReleaseTags],
    downloads: &[ParsedRelease],
    limit: usize,
) -> Vec<Value> {
    let mut seen: BTreeMap<String, (String, i64, i64)> = BTreeMap::new();
    let mut add = |name: &str, files: i64, grabs: i64| {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let e = seen
            .entry(name.to_lowercase())
            .or_insert_with(|| (name.to_string(), 0, 0));
        e.1 += files;
        e.2 += grabs;
    };
    for t in tags {
        let names = match kind {
            ListKind::Group => [t.release_group.as_deref(), t.collective.as_deref()],
            ListKind::Service => [t.service.as_deref(), None],
        };
        for n in names.into_iter().flatten() {
            add(n, t.files, 0);
        }
    }
    for d in downloads {
        let names = match kind {
            ListKind::Group => [d.group.as_deref(), d.collective.as_deref()],
            ListKind::Service => [d.service.as_deref(), None],
        };
        for n in names.into_iter().flatten() {
            add(n, 0, 1);
        }
    }
    let listed = |name: &str| model.lists.iter().any(|l| l.kind == kind && l.has(name));
    let mut out: Vec<(String, i64, i64)> = seen
        .into_values()
        .filter(|(name, _, _)| !listed(name))
        .collect();
    out.sort_by(|a, b| {
        (b.1 + b.2)
            .cmp(&(a.1 + a.2))
            .then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
    });
    out.into_iter()
        .take(limit)
        .map(|(name, files, downloads)| json!({"name": name, "files": files, "downloads": downloads}))
        .collect()
}

/// The release groups and services seen in the library and in the download client that are in
/// no list, with how many files and downloads each has, the most first.
#[utoipa::path(get, path = "/api/v1/lists/unlisted", tag = "policy", params(UnlistedQuery),
    responses((status = 200, description = "`{groups: [{name, files, downloads}], services: [...]}`", body = Object), (status = 401, body = ProblemDoc)))]
async fn lists_unlisted(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<UnlistedQuery>,
) -> Answer {
    let cur = current(&state).await?;
    let tags = catalog_store::release_tags(&state.pool)
        .await
        .map_err(fail)?;
    let names = crate::lifecycle::store::download_names(&state.pool, 5000)
        .await
        .map_err(fail)?;
    let downloads: Vec<ParsedRelease> = names.iter().map(|n| release::parse(n)).collect();
    let limit = q.limit.unwrap_or(100).clamp(1, 1000);
    Ok(Json(json!({
        "groups": unlisted_of(&cur.model, ListKind::Group, &tags, &downloads, limit),
        "services": unlisted_of(&cur.model, ListKind::Service, &tags, &downloads, limit),
    }))
    .into_response())
}

/// Members trimmed, empty ones dropped, each once.
fn clean(list: &mut List) {
    let mut members: Vec<String> = Vec::new();
    for m in &list.members {
        let m = m.trim();
        if !m.is_empty() && !members.iter().any(|x| x.eq_ignore_ascii_case(m)) {
            members.push(m.to_string());
        }
    }
    list.members = members;
    list.name = list.name.trim().to_string();
}

fn not_found_list(id: &str) -> SaveError {
    SaveError::NotFound(format!("There is no list `{id}`."))
}

async fn list_answer(state: &AppState, saved: &Saved, id: &str, status: StatusCode) -> Answer {
    let l = saved
        .model
        .list(id)
        .ok_or_else(|| save_failed(not_found_list(id)))?;
    let tags = catalog_store::release_tags(&state.pool)
        .await
        .map_err(fail)?;
    Ok((status, Json(list_json(&saved.model, l, &tags))).into_response())
}

/// Add a list. Every profile has it as allowed until it is ranked.
#[utoipa::path(post, path = "/api/v1/lists", tag = "policy", request_body = Object,
    responses((status = 201, description = "The list as saved", body = Object), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 409, description = "The policy file is in force, or the id is taken", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn list_create(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(mut list): ApiJson<List>,
) -> Answer {
    owner_only(&caller)?;
    clean(&mut list);
    if list.name.is_empty() {
        return Err(fail(AppError::bad_request("`name` is required.")));
    }
    let cur = current(&state).await?;
    let mut taken: BTreeSet<String> = cur.model.lists.iter().map(|x| x.id.clone()).collect();
    taken.insert("none".to_string());
    let asked = list.id.trim().to_string();
    if !asked.is_empty() && taken.contains(&asked) {
        return Err(save_failed(SaveError::Conflict(format!(
            "There is a list `{asked}` already."
        ))));
    }
    let id = if asked.is_empty() {
        unique_id(&slug(&list.name), &taken)
    } else {
        asked
    };
    let subject = format!("list:{id}");
    let ((), saved) = edit(&state, &caller, "list_saved", &subject, |m| {
        if m.list(&id).is_some() {
            return Err(SaveError::Conflict(format!(
                "There is a list `{id}` already."
            )));
        }
        list.id = id.clone();
        list.rev = 1;
        m.lists.push(list);
        Ok(())
    })
    .await?;
    list_answer(&state, &saved, &id, StatusCode::CREATED).await
}

/// Save a list: its name, kind, members and restriction.
#[utoipa::path(put, path = "/api/v1/lists/{id}", tag = "policy", request_body = Object,
    params(("id" = String, Path, description = "The list's id")),
    responses((status = 200, description = "The list as saved", body = Object), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn list_put(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiJson(mut list): ApiJson<List>,
) -> Answer {
    clean(&mut list);
    let subject = format!("list:{id}");
    let ((), saved) = edit(&state, &caller, "list_saved", &subject, |m| {
        let at = m
            .lists
            .iter()
            .position(|x| x.id == id)
            .ok_or_else(|| not_found_list(&id))?;
        list.id = id.clone();
        list.rev = m.lists[at].rev + 1;
        m.lists[at] = list;
        Ok(())
    })
    .await?;
    list_answer(&state, &saved, &id, StatusCode::OK).await
}

/// Delete a list. The profiles that ranked it no longer do.
#[utoipa::path(delete, path = "/api/v1/lists/{id}", tag = "policy",
    params(("id" = String, Path, description = "The list's id")),
    responses((status = 200, description = "`{deleted, version, profiles}`", body = Object), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn list_delete(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> Answer {
    let subject = format!("list:{id}");
    let (profiles, saved) = edit(&state, &caller, "list_deleted", &subject, |m| {
        let ranked: Vec<String> = m
            .uses_of_list(&id)
            .into_iter()
            .filter(|(_, stance)| *stance != Stance::Allow)
            .map(|(p, _)| p.id.clone())
            .collect();
        let before = m.lists.len();
        m.lists.retain(|x| x.id != id);
        if m.lists.len() == before {
            return Err(not_found_list(&id));
        }
        Ok(ranked)
    })
    .await?;
    Ok(
        Json(json!({"deleted": id, "version": saved.version, "profiles": profiles}))
            .into_response(),
    )
}

fn not_found_rule(id: &str) -> SaveError {
    SaveError::NotFound(format!("There is no advanced rule `{id}`."))
}

/// Add an advanced rule: a condition in the engine's language. Every profile has it as
/// allowed until it gives it a stance.
#[utoipa::path(post, path = "/api/v1/advanced-rules", tag = "policy", request_body = Object,
    responses((status = 201, description = "`{id, version}`", body = Object), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rule_create(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(mut rule): ApiJson<Rule>,
) -> Answer {
    owner_only(&caller)?;
    rule.name = rule.name.trim().to_string();
    if rule.name.is_empty() {
        return Err(fail(AppError::bad_request("`name` is required.")));
    }
    let cur = current(&state).await?;
    let taken: BTreeSet<String> = cur.model.rules.iter().map(|x| x.id.clone()).collect();
    let id = unique_id(&slug(&rule.name), &taken);
    let subject = format!("rule:{id}");
    let ((), saved) = edit(&state, &caller, "rule_saved", &subject, |m| {
        rule.id = id.clone();
        rule.rev = 1;
        m.rules.push(rule);
        Ok(())
    })
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "version": saved.version})),
    )
        .into_response())
}

/// Save an advanced rule.
#[utoipa::path(put, path = "/api/v1/advanced-rules/{id}", tag = "policy", request_body = Object,
    params(("id" = String, Path, description = "The rule's id")),
    responses((status = 200, description = "`{id, version}`", body = Object), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rule_put(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiJson(mut rule): ApiJson<Rule>,
) -> Answer {
    let subject = format!("rule:{id}");
    let ((), saved) = edit(&state, &caller, "rule_saved", &subject, |m| {
        let at = m
            .rules
            .iter()
            .position(|x| x.id == id)
            .ok_or_else(|| not_found_rule(&id))?;
        rule.id = id.clone();
        rule.rev = m.rules[at].rev + 1;
        m.rules[at] = rule;
        Ok(())
    })
    .await?;
    Ok(Json(json!({"id": id, "version": saved.version})).into_response())
}

/// Delete an advanced rule.
#[utoipa::path(delete, path = "/api/v1/advanced-rules/{id}", tag = "policy",
    params(("id" = String, Path, description = "The rule's id")),
    responses((status = 200, description = "`{deleted, version}`", body = Object), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rule_delete(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> Answer {
    let subject = format!("rule:{id}");
    let ((), saved) = edit(&state, &caller, "rule_deleted", &subject, |m| {
        let before = m.rules.len();
        m.rules.retain(|x| x.id != id);
        if m.rules.len() == before {
            return Err(not_found_rule(&id));
        }
        Ok(())
    })
    .await?;
    Ok(Json(json!({"deleted": id, "version": saved.version})).into_response())
}

#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
struct ImportQuery {
    /// `tiers` (the default): groups with close points in every profile share a list, with
    /// their mean points. `exact`: one list per distinct points vector, as the file scores.
    #[param(value_type = Option<String>)]
    mode: Option<profile::ImportMode>,
    /// Save nothing; say what the import would change on the library's files.
    dry_run: Option<bool>,
}

/// A profile's id and the releases it is compared on.
type Sample = (String, Vec<(ParsedRelease, EvalContext)>);

/// The library's files of each kind, as releases, for an import to be compared on: at most
/// `limit` a profile, those of the profile first.
async fn sample_releases(
    state: &AppState,
    file: &Policy,
    limit: i64,
) -> Result<Vec<Sample>, Refused> {
    let kind_of = saved::kind_of(&state.config);
    let mut out: Vec<Sample> = Vec::new();
    for lane in &file.lanes {
        let kind = match kind_of(lane) {
            profile::TitleKind::Movie => "movie",
            profile::TitleKind::Show => "show",
        };
        let files = catalog_store::held_files_sample(&state.pool, kind, &lane.id, limit)
            .await
            .map_err(fail)?;
        out.push((lane.id.clone(), files.iter().map(held_release).collect()));
    }
    Ok(out)
}

/// What an import changes on the library's files, profile by profile: places among the
/// releases taken, and releases taken or refused differently.
fn import_report(file: &Policy, ours: &Policy, samples: &[Sample]) -> profile::RankReport {
    let mut report = profile::RankReport::default();
    for (lane, items) in samples {
        let only = |p: &Policy| Policy {
            lanes: p.lanes.iter().filter(|l| &l.id == lane).cloned().collect(),
            ..p.clone()
        };
        let r = profile::rank_changes(&only(file), &only(ours), items);
        report.releases += r.releases;
        report.moved += r.moved;
        report.accept_changed += r.accept_changed;
        report.moves.extend(r.moves);
    }
    let size = |m: &profile::RankMove| match (m.from, m.to) {
        (Some(a), Some(b)) => a.abs_diff(b),
        _ => usize::MAX,
    };
    report.moves.sort_by_key(|m| std::cmp::Reverse(size(m)));
    report.moves.truncate(10);
    report
}

/// Read the policy file into the model and save it as the next version; from then on the saved
/// model is the source (unless the environment fixes the source). The naming conventions stay.
/// With `dryRun`, nothing is saved: the answer says what the import would change on the
/// library's files.
#[utoipa::path(post, path = "/api/v1/policy/import", tag = "policy", params(ImportQuery),
    responses((status = 200, description = "`{version, profiles, lists, rules, source}`; with dryRun `{mode, lists, profiles, rules, releases, moved, acceptChanged, moves}`", body = Object), (status = 400, description = "The policy file is not valid", body = ProblemDoc),
        (status = 403, body = ProblemDoc), (status = 409, description = "The environment keeps the file in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn policy_import(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<ImportQuery>,
) -> Answer {
    owner_only(&caller)?;
    let mode = q.mode.unwrap_or_default();
    let field = state.settings.field(saved::SOURCE, &caller);
    if let Some(var) = field.as_ref().and_then(|f| f.env.clone())
        && saved::origin(&state.settings) == Origin::File
    {
        return Err(problem(
            StatusCode::CONFLICT,
            "singularr.fixed",
            "Fixed by the environment",
            format!("{var} keeps the policy file in force."),
        ));
    }
    let file = crate::policy::load(&state.config.policy_path)
        .map_err(|e| fail(AppError::BadRequest(e.to_string())))?
        .policy;
    let config = &state.config;
    if q.dry_run == Some(true) {
        let model = saved::imported_as(&file.policy, config, None, mode);
        let ours = profile::compile(&model, "import")
            .map_err(|e| fail(AppError::BadRequest(e.to_string())))?;
        let samples = sample_releases(&state, &file.policy, 200).await?;
        let report = import_report(&file.policy, &ours, &samples);
        return Ok(Json(json!({
            "mode": mode,
            "dryRun": true,
            "lists": model.lists.len(),
            "profiles": model.profiles.len(),
            "rules": model.rules.len(),
            "releases": report.releases,
            "moved": report.moved,
            "acceptChanged": report.accept_changed,
            "moves": report.moves,
        }))
        .into_response());
    }
    let actor = caller.actor();
    let what = Change {
        action: "imported",
        subject: "policy",
        actor: &actor,
    };
    let ((), saved, active) = saved::change(
        &state.pool,
        &state.journal,
        what,
        || saved::imported_as(&file.policy, config, None, mode),
        |m| {
            *m = saved::imported_as(&file.policy, config, Some(&*m), mode);
            Ok(())
        },
    )
    .await
    .map_err(save_failed)?;
    if saved::origin(&state.settings) == Origin::File {
        let patch = SettingsPatch {
            values: BTreeMap::from([(saved::SOURCE.to_string(), json!("saved"))]),
            scope: None,
        };
        state
            .settings
            .patch(&state.pool, &state.journal, &caller, &patch)
            .await
            .map_err(|e| match e {
                PatchError::Db(e) => fail(AppError::Db(e)),
                other => fail(AppError::BadRequest(other.to_string())),
            })?;
    }
    activate(&state, active).await?;
    let latest = Some(saved.clone());
    Ok(Json(json!({
        "version": saved.version,
        "profiles": saved.model.profiles.len(),
        "lists": saved.model.lists.len(),
        "rules": saved.model.rules.len(),
        "source": source_view(&state, &caller, saved::origin(&state.settings), latest.as_ref()),
    }))
    .into_response())
}

/// The model in force in the policy file form: the saved model compiled, or the policy file as
/// it was read.
#[utoipa::path(get, path = "/api/v1/policy/export", tag = "policy",
    responses((status = 200, description = "The policy file (TOML)", content_type = "application/toml", body = String), (status = 401, body = ProblemDoc)))]
async fn policy_export(State(state): State<AppState>) -> Answer {
    let cur = current(&state).await?;
    let (text, name) = match (cur.origin, &cur.latest) {
        (Origin::Saved, Some(s)) => (
            profile::export(&s.model, &format!("v{}", s.version))
                .map_err(|e| fail(AppError::Internal(e.to_string())))?,
            format!("policy-v{}.toml", s.version),
        ),
        _ => (
            state.policy.current().text.clone(),
            "policy.toml".to_string(),
        ),
    };
    Ok((
        [
            (
                header::CONTENT_TYPE,
                "application/toml; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        text,
    )
        .into_response())
}

#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct VersionsQuery {
    /// At most this many, newest first (default 50, at most 500).
    limit: Option<i64>,
}

/// The saved versions, newest first, and which is in force.
#[utoipa::path(get, path = "/api/v1/policy/versions", tag = "policy", params(VersionsQuery),
    responses((status = 200, description = "`{versions: [{version, action, subject, savedBy, savedAt, hash, inForce}]}`", body = Object), (status = 401, body = ProblemDoc)))]
async fn policy_versions(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<VersionsQuery>,
) -> Answer {
    let rows =
        crate::policy::store::model_versions(&state.pool, q.limit.unwrap_or(50).clamp(1, 500))
            .await
            .map_err(fail)?;
    let in_force = match state.policy.current().source {
        PolicySource::Saved(v) => Some(v),
        _ => None,
    };
    let versions: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "version": r.version,
                "action": r.action,
                "subject": r.subject,
                "savedBy": r.saved_by,
                "savedAt": r.saved_at,
                "hash": r.hash,
                "inForce": Some(r.version) == in_force,
            })
        })
        .collect();
    Ok(Json(json!({ "versions": versions })).into_response())
}

/// One saved version with its whole model, to compare with another.
#[utoipa::path(get, path = "/api/v1/policy/versions/{version}", tag = "policy",
    params(("version" = i64, Path, description = "The version")),
    responses((status = 200, description = "`{version, action, subject, savedBy, savedAt, hash, model}`", body = Object), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn policy_version(State(state): State<AppState>, ApiPath(version): ApiPath<i64>) -> Answer {
    let s = saved::at(&state.pool, version)
        .await
        .map_err(save_failed)?
        .ok_or_else(|| {
            fail(AppError::not_found(format!(
                "There is no saved version {version}."
            )))
        })?;
    Ok(Json(json!({
        "version": s.version,
        "action": s.action,
        "subject": s.subject,
        "savedBy": s.saved_by,
        "savedAt": s.saved_at,
        "hash": s.hash,
        "model": s.model,
    }))
    .into_response())
}

/// Save an earlier version again as the newest: the undo of every change after it.
#[utoipa::path(post, path = "/api/v1/policy/versions/{version}/restore", tag = "policy",
    params(("version" = i64, Path, description = "The version to restore")),
    responses((status = 200, description = "`{version, restored}`", body = Object), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "The policy file is in force", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn policy_restore(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(version): ApiPath<i64>,
) -> Answer {
    owner_only(&caller)?;
    if saved::origin(&state.settings) == Origin::File {
        return Err(pinned(&state));
    }
    let (saved, active) = saved::restore(&state.pool, &state.journal, version, &caller.actor())
        .await
        .map_err(save_failed)?;
    activate(&state, active).await?;
    Ok(Json(json!({"version": saved.version, "restored": version})).into_response())
}

/// The routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/profiles", get(profiles_list).post(profile_create))
        .route("/profiles/preview", post(profiles_preview))
        .route(
            "/profiles/{id}",
            get(profile_get).put(profile_put).delete(profile_delete),
        )
        .route("/profiles/{id}/duplicate", post(profile_duplicate))
        .route("/lists", get(lists_get).post(list_create))
        .route("/lists/unlisted", get(lists_unlisted))
        .route("/lists/{id}", put(list_put).delete(list_delete))
        .route("/advanced-rules", post(rule_create))
        .route("/advanced-rules/{id}", put(rule_put).delete(rule_delete))
        .route("/policy/import", post(policy_import))
        .route("/policy/export", get(policy_export))
        .route("/policy/versions", get(policy_versions))
        .route("/policy/versions/{version}", get(policy_version))
        .route("/policy/versions/{version}/restore", post(policy_restore))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        profiles_list,
        profile_get,
        profile_create,
        profile_put,
        profile_delete,
        profile_duplicate,
        profiles_preview,
        lists_get,
        lists_unlisted,
        list_create,
        list_put,
        list_delete,
        rule_create,
        rule_put,
        rule_delete,
        policy_import,
        policy_export,
        policy_versions,
        policy_version,
        policy_restore
    ),
    components(schemas(ProfilesView, ListsView, SourceView, PreviewRequest))
)]
struct ProfilesDoc;

/// These routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    ProfilesDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A group, a collective, a service and how many files carry them.
    type Row<'a> = (Option<&'a str>, Option<&'a str>, Option<&'a str>, i64);

    fn tags(rows: &[Row<'_>]) -> Vec<ReleaseTags> {
        rows.iter()
            .map(|(g, c, s, n)| ReleaseTags {
                release_group: g.map(str::to_string),
                collective: c.map(str::to_string),
                service: s.map(str::to_string),
                files: *n,
            })
            .collect()
    }

    fn model() -> Model {
        Model {
            lists: vec![
                List {
                    id: "tier-1".to_string(),
                    name: "Tier 1".to_string(),
                    kind: ListKind::Group,
                    members: vec!["FLUX".to_string(), "QxR".to_string(), "PmP".to_string()],
                    ..List::default()
                },
                List {
                    id: "services".to_string(),
                    name: "Services".to_string(),
                    kind: ListKind::Service,
                    members: vec!["AMZN".to_string()],
                    ..List::default()
                },
            ],
            ..Model::default()
        }
    }

    #[test]
    fn unlisted_names_are_the_ones_in_no_list_the_most_first() {
        let m = model();
        let rows = tags(&[
            (Some("FLUX"), None, Some("AMZN"), 40),
            (Some("NTb"), None, Some("NF"), 12),
            (Some("Tigole"), Some("QxR"), None, 30),
            (Some("ntb"), None, None, 3),
            (None, None, None, 99),
        ]);
        let downloads = vec![release::parse(
            "Show.S01E01.1080p.HMAX.WEB-DL.DDP5.1.H.264-SMURF",
        )];
        let groups = unlisted_of(&m, ListKind::Group, &rows, &downloads, 10);
        let names: Vec<&str> = groups.iter().map(|g| g["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Tigole", "NTb", "SMURF"]);
        assert_eq!(groups[1]["files"], 15);
        assert_eq!(groups[2]["downloads"], 1);
        let services = unlisted_of(&m, ListKind::Service, &rows, &downloads, 10);
        let names: Vec<&str> = services
            .iter()
            .map(|g| g["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["NF", "HMAX"]);
    }

    #[test]
    fn a_list_counts_its_files_and_says_which_members_have_none() {
        let m = model();
        let rows = tags(&[
            (Some("FLUX"), None, Some("AMZN"), 40),
            (Some("Tigole"), Some("QxR"), None, 30),
        ]);
        let v = list_json(&m, &m.lists[0], &rows);
        assert_eq!(v["files"], 70);
        let by: Vec<(String, i64)> = v["library"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x["name"].as_str().unwrap().to_string(),
                    x["files"].as_i64().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            by,
            [
                ("FLUX".to_string(), 40),
                ("QxR".to_string(), 30),
                ("PmP".to_string(), 0)
            ]
        );
    }

    #[test]
    fn the_openapi_document_lists_the_routes() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        for (path, method) in [
            ("/api/v1/profiles", "get"),
            ("/api/v1/profiles", "post"),
            ("/api/v1/profiles/{id}", "put"),
            ("/api/v1/profiles/preview", "post"),
            ("/api/v1/lists", "get"),
            ("/api/v1/lists/unlisted", "get"),
            ("/api/v1/policy/import", "post"),
            ("/api/v1/policy/export", "get"),
            ("/api/v1/policy/versions", "get"),
        ] {
            assert!(
                doc["paths"][path][method].is_object(),
                "{method} {path} is missing"
            );
        }
    }
}
