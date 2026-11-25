//! The `/api/v1/processing/rules` routes: Singularr's own processing rules, their versions, what
//! a draft would match now, what each rule sent or would have sent, and a rule run on request.
//!
//! Reading needs a caller with `read`. Saving, removing and running a rule are the owner's
//! alone, each journaled (`processing_rule_saved`, `processing_rule_removed`; a send is a
//! `processing_sent` event in the rule's name). A rule is checked when it is saved: its service
//! is a configured processing service and its flow one that service offers, its conditions ask
//! something of the file, and the traits it names are the policy's.

use super::processing::{RunTitle, not_allowed, problem, read_only_words};
use super::routes::{ApiJson, ApiPath, ApiQuery};
use super::state::AppState;
use crate::core::auth::Caller;
use crate::core::error::{AppError, AppResult};
use crate::core::journal::event;
use crate::core::outbound::Access;
use crate::core::schema::ProblemDoc;
use crate::processing::model::{Run, file_name};
use crate::processing::rules::{
    self, ON_IMPORT, ON_REQUEST, REQUEST_MAX, RuleBody, RuleConditions, RuleRow,
};
use crate::processing::store;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use utoipa::{IntoParams, OpenApi, ToSchema};

/// Files a preview lists, at most; `count` says how many match.
const PREVIEW_SHOWN: usize = 100;
/// Entries of a log on one page by default, and at most.
const LOG_DEFAULT: i64 = 100;
const LOG_MAX: i64 = 500;
/// The six ways a rule's entry ends.
const OUTCOMES: [&str; 6] = ["shadow", "held", "skipped", "sent", "refused", "failed"];

/// How many entries of each outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct OutcomeCounts {
    pub shadow: i64,
    pub held: i64,
    pub skipped: i64,
    pub sent: i64,
    pub refused: i64,
    pub failed: i64,
}

impl OutcomeCounts {
    fn add(&mut self, outcome: &str, n: i64) {
        let slot = match outcome {
            "shadow" => &mut self.shadow,
            "held" => &mut self.held,
            "skipped" => &mut self.skipped,
            "sent" => &mut self.sent,
            "refused" => &mut self.refused,
            _ => &mut self.failed,
        };
        *slot += n;
    }
}

/// A rule, with its service's and flow's names and what its log holds.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuleView {
    pub id: i64,
    pub name: String,
    pub enabled: bool,
    /// `import` or `request`.
    pub when: String,
    pub conditions: RuleConditions,
    /// The service's integration id, and its name.
    pub service: String,
    pub service_name: String,
    pub flow: String,
    pub flow_label: String,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub created_by: String,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
    /// What its log holds, by outcome.
    pub counts: OutcomeCounts,
    /// When it last matched a file.
    pub last_match: Option<DateTime<Utc>>,
}

/// One version of a rule as it was saved.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VersionView {
    pub version: i32,
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
    pub saved_at: DateTime<Utc>,
    pub saved_by: String,
}

/// `GET /api/v1/processing/rules/{id}`: a rule and every version it had, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RuleDetail {
    #[serde(flatten)]
    pub rule: RuleView,
    pub versions: Vec<VersionView>,
}

/// `GET /api/v1/processing/rules`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct RuleList {
    pub rules: Vec<RuleView>,
}

/// `POST /api/v1/processing/rules/preview`: a draft's conditions.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    pub conditions: RuleConditions,
}

/// A file of the library a rule matches.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct MatchView {
    pub title: RunTitle,
    /// The file's name and its whole path.
    pub file: String,
    pub path: Option<String>,
    pub library: String,
    pub size: i64,
    /// Why it matches, in the condition language's words.
    pub evidence: String,
}

/// What a draft matches now.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PreviewAnswer {
    /// Every file it matches.
    pub count: usize,
    /// The first hundred of them, in the catalog's order.
    pub files: Vec<MatchView>,
}

/// What the service did on its own for the file of an entry: its newest run of that file that
/// no rule sent.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServiceDid {
    pub run: String,
    pub flow: String,
    pub state: String,
    pub at: DateTime<Utc>,
    /// What started it, in the service's own words.
    pub trigger: String,
}

/// One entry of a rule's log.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogView {
    pub at: DateTime<Utc>,
    /// `shadow`, `held`, `skipped`, `sent`, `refused` or `failed`.
    pub outcome: String,
    /// Why it was held, skipped, refused or failed, in a few words.
    pub detail: Option<String>,
    pub evidence: Option<String>,
    pub file: String,
    pub path: String,
    pub title: Option<RunTitle>,
    pub flow: String,
    /// The version of the rule that decided.
    pub version: i32,
    /// The import the file came with; null for a run on request.
    pub event: Option<i64>,
    /// The run a send opened.
    pub run: Option<String>,
    /// What the service did on its own for the same file. Null when it did nothing known.
    pub service_did: Option<ServiceDid>,
}

/// `GET /api/v1/processing/rules/{id}/log`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LogPage {
    pub entries: Vec<LogView>,
    pub counts: OutcomeCounts,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LogQuery {
    /// Entries, newest first: 100 when left out, at most 500.
    #[param(value_type = Option<i64>)]
    pub limit: Option<String>,
}

/// `POST /api/v1/processing/rules/{id}/run`: what a run on request did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RunAnswer {
    /// Files the rule matches now.
    pub matched: usize,
    /// The ones this run took: each is an entry of the log.
    pub logged: usize,
    pub sent: usize,
    /// Matching files not sent before that this run did not take.
    pub left: usize,
}

/// The names of a rule's service and flow, from the configuration and the adapter.
fn names(state: &AppState, rule: &RuleRow) -> (String, String) {
    let service = state
        .config
        .integration(&rule.service)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| rule.service.clone());
    let flow = state
        .engines
        .processor(&rule.service)
        .map(|p| p.flow_label(&rule.flow))
        .unwrap_or_else(|| rule.flow.clone());
    (service, flow)
}

fn view(state: &AppState, rule: &RuleRow, counts: &[store::LogCount]) -> RuleView {
    let (service_name, flow_label) = names(state, rule);
    let mut c = OutcomeCounts::default();
    let mut last: Option<DateTime<Utc>> = None;
    for n in counts.iter().filter(|n| n.rule_id == rule.id) {
        c.add(&n.outcome, n.entries);
        last = last.max(n.last);
    }
    RuleView {
        id: rule.id,
        name: rule.name.clone(),
        enabled: rule.enabled,
        when: rule.run_on.clone(),
        conditions: rule.conditions.clone(),
        service: rule.service.clone(),
        service_name,
        flow: rule.flow.clone(),
        flow_label,
        version: rule.version,
        created_at: rule.created_at,
        created_by: rule.created_by.clone(),
        updated_at: rule.updated_at,
        updated_by: rule.updated_by.clone(),
        counts: c,
        last_match: last,
    }
}

fn owner_only(caller: &Caller, what: &str) -> AppResult<()> {
    if matches!(caller, Caller::Owner { .. }) {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!("Only the owner can {what}.")))
    }
}

/// A rule as it may be saved: its words trimmed, and every part checked.
fn checked(state: &AppState, mut body: RuleBody) -> AppResult<RuleBody> {
    body.name = body.name.trim().to_string();
    body.service = body.service.trim().to_string();
    body.flow = body.flow.trim().to_string();
    if body.name.is_empty() || body.name.chars().count() > 120 {
        return Err(AppError::bad_request("`name` is 1 to 120 characters."));
    }
    if body.when != ON_IMPORT && body.when != ON_REQUEST {
        return Err(AppError::bad_request(format!(
            "`when` is import or request, not `{}`.",
            body.when
        )));
    }
    let processor = state.engines.processor(&body.service).ok_or_else(|| {
        AppError::bad_request(format!(
            "`service` is a configured processing service, not `{}`.",
            body.service
        ))
    })?;
    let flows = processor.flows();
    if !flows.iter().any(|f| f.id == body.flow) {
        let ids: Vec<&str> = flows.iter().map(|f| f.id.as_str()).collect();
        return Err(AppError::bad_request(if ids.is_empty() {
            format!("`{}` offers no flow.", body.service)
        } else {
            format!("`flow` is one of {}, not `{}`.", ids.join(", "), body.flow)
        }));
    }
    let c = &body.conditions;
    if c.is_empty() {
        return Err(AppError::bad_request(
            "A rule needs at least one condition.",
        ));
    }
    let active = state.policy.current();
    let policy = &active.policy;
    if let Some(unknown) = c
        .traits
        .iter()
        .find(|t| !policy.traits.iter().any(|p| &p.id == *t))
    {
        return Err(AppError::bad_request(format!(
            "There is no trait `{unknown}` in the policy."
        )));
    }
    if [c.min_bitrate, c.max_bitrate]
        .into_iter()
        .flatten()
        .any(|b| !b.is_finite() || b < 0.0)
    {
        return Err(AppError::bad_request(
            "A bitrate is a number of Mbit/s, 0 or more.",
        ));
    }
    Ok(body)
}

/// Save a rule's change with its event in one transaction.
async fn journaled(
    state: &AppState,
    actor: &str,
    kind: &str,
    rule: &RuleRow,
    extra: serde_json::Value,
) -> AppResult<()> {
    let mut tx = state.journal.begin(&state.pool).await?;
    let mut body = json!({
        "rule": rule.id, "name": rule.name, "version": rule.version, "enabled": rule.enabled,
        "when": rule.run_on, "service": rule.service, "flow": rule.flow,
    });
    if let (Some(into), serde_json::Value::Object(more)) = (body.as_object_mut(), extra) {
        into.extend(more);
    }
    tx.append_one(event(kind, "rule", rule.id, actor, body))
        .await?;
    tx.commit().await?;
    Ok(())
}

// Handlers ------------------------------------------------------------------------------------

/// Every processing rule, oldest first, with what its log holds.
#[utoipa::path(get, path = "/api/v1/processing/rules", tag = "processing",
    responses((status = 200, body = RuleList), (status = 401, body = ProblemDoc)))]
async fn rules_list(State(state): State<AppState>) -> AppResult<Json<RuleList>> {
    let counts = store::log_counts(&state.pool).await?;
    let rules = store::rules(&state.pool).await?;
    Ok(Json(RuleList {
        rules: rules.iter().map(|r| view(&state, r, &counts)).collect(),
    }))
}

/// One rule, with every version it had.
#[utoipa::path(get, path = "/api/v1/processing/rules/{id}", tag = "processing",
    params(("id" = i64, Path, description = "The rule's id")),
    responses((status = 200, body = RuleDetail), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rules_one(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<Json<RuleDetail>> {
    let rule = store::rule(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no rule {id}.")))?;
    let counts = store::log_counts(&state.pool).await?;
    let versions = store::rule_versions(&state.pool, id).await?;
    Ok(Json(RuleDetail {
        rule: view(&state, &rule, &counts),
        versions: versions
            .into_iter()
            .map(|v| VersionView {
                version: v.version,
                body: v.body,
                saved_at: v.saved_at,
                saved_by: v.saved_by,
            })
            .collect(),
    }))
}

/// Add a rule. The owner alone. An import rule that is off only watches: its log says what it
/// would have sent.
#[utoipa::path(post, path = "/api/v1/processing/rules", tag = "processing", request_body = RuleBody,
    responses((status = 201, body = RuleView), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rules_add(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(body): ApiJson<RuleBody>,
) -> AppResult<Response> {
    owner_only(&caller, "change processing rules")?;
    let body = checked(&state, body)?;
    let actor = caller.actor();
    let mut conn = state.pool.acquire().await?;
    let rule = store::insert_rule(&mut conn, &body, &actor).await?;
    drop(conn);
    journaled(
        &state,
        &actor,
        "processing_rule_saved",
        &rule,
        json!({"created": true}),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(view(&state, &rule, &[]))).into_response())
}

/// Save a rule again: its next version. The owner alone.
#[utoipa::path(put, path = "/api/v1/processing/rules/{id}", tag = "processing", request_body = RuleBody,
    params(("id" = i64, Path, description = "The rule's id")),
    responses((status = 200, body = RuleView), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rules_save(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(body): ApiJson<RuleBody>,
) -> AppResult<Json<RuleView>> {
    owner_only(&caller, "change processing rules")?;
    let body = checked(&state, body)?;
    let actor = caller.actor();
    let mut conn = state.pool.acquire().await?;
    let rule = store::update_rule(&mut conn, id, &body, &actor)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no rule {id}.")))?;
    drop(conn);
    journaled(
        &state,
        &actor,
        "processing_rule_saved",
        &rule,
        json!({"created": false}),
    )
    .await?;
    let counts = store::log_counts(&state.pool).await?;
    Ok(Json(view(&state, &rule, &counts)))
}

/// Remove a rule: it is never evaluated again; its log and versions stay. The owner alone.
#[utoipa::path(delete, path = "/api/v1/processing/rules/{id}", tag = "processing",
    params(("id" = i64, Path, description = "The rule's id")),
    responses((status = 204, description = "Removed"), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rules_remove(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<StatusCode> {
    owner_only(&caller, "change processing rules")?;
    let actor = caller.actor();
    let mut conn = state.pool.acquire().await?;
    let rule = store::remove_rule(&mut conn, id, &actor)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no rule {id}.")))?;
    drop(conn);
    journaled(&state, &actor, "processing_rule_removed", &rule, json!({})).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The library files a draft's conditions match now, with a count. Nothing is sent or saved.
#[utoipa::path(post, path = "/api/v1/processing/rules/preview", tag = "processing", request_body = PreviewRequest,
    responses((status = 200, body = PreviewAnswer), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rules_preview(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<PreviewRequest>,
) -> AppResult<Json<PreviewAnswer>> {
    let active = state.policy.current();
    let found = rules::matching(&state.pool, &body.conditions, &active.policy).await?;
    let ids: Vec<i64> = found
        .iter()
        .take(PREVIEW_SHOWN)
        .map(|(f, _)| f.title.id)
        .collect();
    let cards: HashMap<i64, RunTitle> = super::processing::run_titles(&state, &ids).await?;
    Ok(Json(PreviewAnswer {
        count: found.len(),
        files: found
            .into_iter()
            .take(PREVIEW_SHOWN)
            .map(|(f, evidence)| MatchView {
                title: cards.get(&f.title.id).cloned().unwrap_or_else(|| RunTitle {
                    id: None,
                    kind: f.title.kind.clone(),
                    title: f.title.title.clone(),
                    year: f.title.year,
                    poster: None,
                }),
                file: f
                    .path
                    .as_deref()
                    .map(|p| file_name(p).to_string())
                    .unwrap_or_default(),
                path: f.path,
                library: f.library,
                size: f.size,
                evidence,
            })
            .collect(),
    }))
}

/// What the service did on its own for each file: its newest run whose source is that file and
/// that no rule sent, by (title, file name).
fn service_runs(runs: Vec<Run>) -> HashMap<(i64, String), ServiceDid> {
    let mut out = HashMap::new();
    for r in runs {
        let (Some(title), Some(source)) = (r.title_id, r.source_path.as_deref()) else {
            continue;
        };
        if r.trigger.starts_with("api:") {
            continue;
        }
        out.entry((title, file_name(source).to_string()))
            .or_insert(ServiceDid {
                run: r.id.clone(),
                flow: r.flow.clone(),
                state: r.state.clone(),
                at: r.created_at,
                trigger: r.trigger.clone(),
            });
    }
    out
}

/// A rule's log, newest first: what it sent and what it would have sent, each entry with what
/// the service did on its own for the same file.
#[utoipa::path(get, path = "/api/v1/processing/rules/{id}/log", tag = "processing", params(("id" = i64, Path, description = "The rule's id"), LogQuery),
    responses((status = 200, body = LogPage), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn rules_log(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<LogQuery>,
) -> AppResult<Json<LogPage>> {
    store::rule(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no rule {id}.")))?;
    let limit = match q.limit.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => LOG_DEFAULT,
        Some(n) => n
            .parse::<i64>()
            .map_err(|_| AppError::bad_request(format!("`limit` is a whole number, not `{n}`.")))?,
    }
    .clamp(1, LOG_MAX);
    let rows = store::rule_log(&state.pool, id, limit).await?;
    let mut counts = OutcomeCounts::default();
    for n in store::log_counts(&state.pool)
        .await?
        .iter()
        .filter(|n| n.rule_id == id && OUTCOMES.contains(&n.outcome.as_str()))
    {
        counts.add(&n.outcome, n.entries);
    }
    let mut titles: Vec<i64> = rows.iter().filter_map(|r| r.title_id).collect();
    titles.sort_unstable();
    titles.dedup();
    let cards = super::processing::run_titles(&state, &titles).await?;
    let did = service_runs(store::of_titles(&state.pool, &titles, 2000).await?);
    Ok(Json(LogPage {
        entries: rows
            .into_iter()
            .map(|r| {
                let file = file_name(&r.path).to_string();
                LogView {
                    service_did: r
                        .title_id
                        .and_then(|t| did.get(&(t, file.clone())))
                        .filter(|d| Some(&d.run) != r.run_id.as_ref())
                        .cloned(),
                    title: r.title_id.and_then(|t| cards.get(&t).cloned()),
                    at: r.at,
                    outcome: r.outcome,
                    detail: r.detail,
                    evidence: r.evidence,
                    file,
                    path: r.path,
                    flow: r.flow,
                    version: r.rule_version,
                    event: r.event_id,
                    run: r.run_id,
                }
            })
            .collect(),
        counts,
    }))
}

/// Run a rule now for the files it matches: the ones not sent before, at most fifty, each
/// decided and logged like an import. The owner alone, for a rule that is on, while Singularr may
/// act on its service.
#[utoipa::path(post, path = "/api/v1/processing/rules/{id}/run", tag = "processing",
    params(("id" = i64, Path, description = "The rule's id")),
    responses((status = 200, body = RunAnswer),
        (status = 403, description = "Not the owner; or `singularr.not_allowed`: the service is read-only", body = ProblemDoc),
        (status = 404, body = ProblemDoc), (status = 409, description = "`singularr.conflict`: the rule is off", body = ProblemDoc),
        (status = 401, body = ProblemDoc)))]
async fn rules_run(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> AppResult<Response> {
    owner_only(&caller, "run processing rules")?;
    let rule = store::rule(&state.pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no rule {id}.")))?;
    if !rule.enabled {
        return Ok(problem(
            StatusCode::CONFLICT,
            "singularr.conflict",
            "Rule is off",
            "Rule is off.",
        ));
    }
    let (name, _) = names(&state, &rule);
    if state.engines.access.of(&rule.service).access != Access::Act {
        return Ok(not_allowed(read_only_words(&name)));
    }
    let active = state.policy.current();
    let ran = rules::request_run(
        &state.pool,
        &state.journal,
        &state.engines,
        &active.policy,
        &rule,
        state.config.sync.enabled,
    )
    .await
    .map_err(AppError::Internal)?;
    debug_assert!(ran.logged <= REQUEST_MAX);
    Ok(Json(RunAnswer {
        matched: ran.matched,
        logged: ran.logged,
        sent: ran.sent,
        left: ran.left,
    })
    .into_response())
}

/// The processing rules routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/processing/rules", get(rules_list).post(rules_add))
        .route("/processing/rules/preview", post(rules_preview))
        .route(
            "/processing/rules/{id}",
            get(rules_one).put(rules_save).delete(rules_remove),
        )
        .route("/processing/rules/{id}/log", get(rules_log))
        .route("/processing/rules/{id}/run", post(rules_run))
}

#[derive(OpenApi)]
#[openapi(paths(
    rules_list,
    rules_one,
    rules_add,
    rules_save,
    rules_remove,
    rules_preview,
    rules_log,
    rules_run
))]
struct RulesDoc;

/// The processing rules routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    RulesDoc::openapi()
}
