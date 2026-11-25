//! The `/api/v1` routes. Every handler is a few lines over a service function; the web
//! app's server functions call the same service functions.

use super::model::Overview;
use super::state::AppState;
use crate::catalog::model::{
    CopyQuery, CopyView, TitleDetail, TitleEdition, TitleQuery, TitleSummary,
};
use crate::catalog::service as catalog;
use crate::core::auth::{self, Caller};
use crate::core::error::AppError;
use crate::core::model::{
    ApiKeyInfo, CreatedApiKey, Event, EventQuery, Listing, NewApiKey, Page, Scope, Whoami,
};
use crate::core::schema::ProblemDoc;
use crate::core::store as core_store;
use crate::engines::model::{Health, ShadowParseRequest, ShadowParseResponse};
use crate::engines::service::shadow_parse;
use crate::lifecycle::model::{Download, DownloadQuery};
use crate::lifecycle::service as downloads;
use crate::metadata::model::{CompareQuery, CompareResponse, ProviderInfo};
use crate::metadata::service::compare;
use crate::policy::model::{PolicyView, SimulateRequest, SimulateResponse};
use crate::releases::model::{
    CopyVerdict, ParseRequest, ParseResponse, ScoreRequest, ScoreResponse, ShadowQuery,
    ShadowResponse,
};
use crate::releases::service as releases;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::Next;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use futures::Stream;
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};

type ApiResult<T> = Result<Json<T>, AppError>;

// Extractors that answer a malformed request with a problem document -------------------------

#[derive(FromRequest)]
#[from_request(via(Json), rejection(AppError))]
pub struct ApiJson<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(AppError))]
pub struct ApiQuery<T>(pub T);

#[derive(FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(AppError))]
pub struct ApiPath<T>(pub T);

impl From<JsonRejection> for AppError {
    fn from(r: JsonRejection) -> Self {
        AppError::BadRequest(format!("Invalid JSON body: {}", r.body_text()))
    }
}

impl From<QueryRejection> for AppError {
    fn from(r: QueryRejection) -> Self {
        AppError::BadRequest(format!("Invalid query string: {}", r.body_text()))
    }
}

impl From<PathRejection> for AppError {
    fn from(r: PathRejection) -> Self {
        AppError::BadRequest(format!("Invalid path: {}", r.body_text()))
    }
}

/// Every route below this layer needs a caller with the `read` scope: the owner's browser
/// session through the edge, or an API key. The caller is left in the request for handlers
/// that need more than `read`.
async fn require_read(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let caller = match state.caller(req.headers()).await {
        Ok(caller) => caller,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = caller.require(Scope::Read) {
        return e.into_response();
    }
    req.extensions_mut().insert(caller);
    next.run(req).await
}

/// Key management is for the owner, or a key that holds `admin`.
fn require_admin(caller: &Caller) -> Result<(), AppError> {
    match caller {
        Caller::Owner { .. } => Ok(()),
        key => key.require(Scope::Admin),
    }
}

// Handlers ---------------------------------------------------------------------------------------

/// Liveness and engine reachability. No sign-in needed.
#[utoipa::path(get, path = "/api/v1/health", tag = "system", security(()),
    responses((status = 200, description = "The program is up. `status` says whether everything it depends on is.", body = Health)))]
async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(state.health().await)
}

/// Who the caller is and which scopes they hold.
#[utoipa::path(get, path = "/api/v1/whoami", tag = "system",
    responses((status = 200, body = Whoami), (status = 401, body = ProblemDoc)))]
async fn whoami(Extension(caller): Extension<Caller>) -> Json<Whoami> {
    Json(caller.whoami())
}

/// What needs attention: counts, engine state, stuck downloads, orphans, shadow differences.
#[utoipa::path(get, path = "/api/v1/overview", tag = "system",
    responses((status = 200, body = Overview), (status = 401, body = ProblemDoc)))]
async fn overview(State(state): State<AppState>) -> ApiResult<Overview> {
    Ok(Json(state.overview().await?))
}

/// Titles, filtered, sorted and paged. `total` counts every title that matches.
#[utoipa::path(get, path = "/api/v1/titles", tag = "catalog", params(TitleQuery),
    responses((status = 200, body = Page<TitleSummary>), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn titles(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<TitleQuery>,
) -> ApiResult<Page<TitleSummary>> {
    Ok(Json(
        catalog::titles(&state.pool, &state.lane_slots(), &q).await?,
    ))
}

/// One title with its copies, verdicts, protections, what each engine says about it, and
/// the lanes in which it holds nothing.
#[utoipa::path(get, path = "/api/v1/titles/{id}", tag = "catalog", params(("id" = i64, Path, description = "The title's id")),
    responses((status = 200, body = TitleDetail), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title(State(state): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<TitleDetail> {
    Ok(Json(
        catalog::title(&state.pool, &state.lane_slots(), id).await?,
    ))
}

/// A title's copies in every library, paged.
#[utoipa::path(get, path = "/api/v1/titles/{id}/copies", tag = "catalog",
    params(("id" = i64, Path, description = "The title's id"), CopyQuery),
    responses((status = 200, body = Page<CopyView>), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title_copies(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<CopyQuery>,
) -> ApiResult<Page<CopyView>> {
    Ok(Json(catalog::title_copies(&state.pool, id, &q).await?))
}

/// A title's editions: each with its label, whether it is wanted, whether a library plays it
/// first, and the files that hold it (copies, and files in its folders no engine tracks).
/// Movies only in stage 1; a show answers an empty list.
#[utoipa::path(get, path = "/api/v1/titles/{id}/editions", tag = "catalog", params(("id" = i64, Path, description = "The title's id")),
    responses((status = 200, body = Listing<TitleEdition>), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title_editions(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<Listing<TitleEdition>> {
    catalog::title_kind(&state.pool, id).await?;
    Ok(Json(Listing::new(
        catalog::title_editions(&state.pool, id).await?,
    )))
}

/// The score trace of a held file: every rule that scored, with its evidence.
#[utoipa::path(get, path = "/api/v1/copies/{id}/verdict", tag = "catalog", params(("id" = i64, Path, description = "The copy's id")),
    responses((status = 200, body = CopyVerdict), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn copy_verdict(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<CopyVerdict> {
    Ok(Json(releases::copy_verdict(
        catalog::copy(&state.pool, id).await?,
    )))
}

/// Read a release name into fields, with the token behind each field.
#[utoipa::path(post, path = "/api/v1/releases/parse", tag = "releases", request_body = ParseRequest,
    responses((status = 200, body = ParseResponse), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn releases_parse(ApiJson(req): ApiJson<ParseRequest>) -> ApiResult<ParseResponse> {
    Ok(Json(releases::parse(&req)?))
}

/// Score a release name in a lane, and decide whether it would replace a held copy.
#[utoipa::path(post, path = "/api/v1/releases/score", tag = "releases", request_body = ScoreRequest,
    responses((status = 200, body = ScoreResponse), (status = 400, body = ProblemDoc), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn releases_score(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<ScoreRequest>,
) -> ApiResult<ScoreResponse> {
    Ok(Json(
        releases::score(&state.pool, &state.policy.current(), &req).await?,
    ))
}

/// Copies where Singularr's parse or score differs from what the arr recorded, with counts
/// per kind of difference.
#[utoipa::path(get, path = "/api/v1/releases/shadow", tag = "releases", params(ShadowQuery),
    responses((status = 200, body = ShadowResponse), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn releases_shadow(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<ShadowQuery>,
) -> ApiResult<ShadowResponse> {
    Ok(Json(
        releases::shadow(&state.pool, &state.engines.arr_ids(), &q).await?,
    ))
}

/// Ask an arr's own parser about a name (a GET to the arr) and put its reading next to
/// Singularr's.
#[utoipa::path(post, path = "/api/v1/releases/shadow/parse", tag = "releases", request_body = ShadowParseRequest,
    responses((status = 200, body = ShadowParseResponse), (status = 400, body = ProblemDoc), (status = 502, description = "The arr did not answer", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn releases_shadow_parse(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<ShadowParseRequest>,
) -> ApiResult<ShadowParseResponse> {
    Ok(Json(
        shadow_parse(&state.engines, &state.policy.current(), &req).await?,
    ))
}

/// Every download in the client, its owner, its state and why it is stuck.
#[utoipa::path(get, path = "/api/v1/downloads", tag = "downloads", params(DownloadQuery),
    responses((status = 200, body = Page<Download>), (status = 401, body = ProblemDoc)))]
async fn downloads_list(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<DownloadQuery>,
) -> ApiResult<Page<Download>> {
    Ok(Json(downloads::downloads(&state.pool, &q).await?))
}

/// Downloads no engine owns, and queue items with no title behind them.
#[utoipa::path(get, path = "/api/v1/downloads/orphans", tag = "downloads", params(DownloadQuery),
    responses((status = 200, body = Page<Download>), (status = 401, body = ProblemDoc)))]
async fn downloads_orphans(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<DownloadQuery>,
) -> ApiResult<Page<Download>> {
    Ok(Json(downloads::orphans(&state.pool, &q).await?))
}

/// The active policy as data, with its hash and when it was loaded.
#[utoipa::path(get, path = "/api/v1/policy", tag = "policy",
    responses((status = 200, body = PolicyView), (status = 401, body = ProblemDoc)))]
async fn policy(State(state): State<AppState>) -> Json<PolicyView> {
    Json(state.policy.current().view())
}

/// Score every stored copy with a proposed policy and report what would change. Nothing
/// is stored.
#[utoipa::path(post, path = "/api/v1/policy/simulate", tag = "policy", request_body = SimulateRequest,
    responses((status = 200, body = SimulateResponse), (status = 400, description = "The proposed policy is not valid", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn policy_simulate(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<SimulateRequest>,
) -> ApiResult<SimulateResponse> {
    Ok(Json(
        releases::simulate(&state.pool, &state.policy.current(), &req).await?,
    ))
}

/// The metadata providers and whether each is configured.
#[utoipa::path(get, path = "/api/v1/metadata/providers", tag = "metadata",
    responses((status = 200, body = Listing<ProviderInfo>), (status = 401, body = ProblemDoc)))]
async fn metadata_providers(State(state): State<AppState>) -> Json<Listing<ProviderInfo>> {
    Json(Listing::new(state.metadata.list()))
}

/// One title as each provider has it: title, year, and per season the episode count and
/// first air date.
#[utoipa::path(get, path = "/api/v1/metadata/compare", tag = "metadata", params(CompareQuery),
    responses((status = 200, body = CompareResponse), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn metadata_compare(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<CompareQuery>,
) -> ApiResult<CompareResponse> {
    Ok(Json(compare(&state.metadata, &q).await?))
}

/// The journal: every change, newest first, filterable.
#[utoipa::path(get, path = "/api/v1/events", tag = "journal", params(EventQuery),
    responses((status = 200, body = Page<Event>), (status = 401, body = ProblemDoc)))]
async fn events(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<EventQuery>,
) -> ApiResult<Page<Event>> {
    Ok(Json(state.events(&q).await?))
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
struct StreamQuery {
    /// Replay events after this id before going live. The `Last-Event-ID` header does the
    /// same and wins when both are sent.
    after: Option<i64>,
}

fn frame(e: &Event) -> SseEvent {
    SseEvent::default()
        .id(e.id.to_string())
        .event(e.kind.clone())
        .data(serde_json::to_string(e).unwrap_or_else(|_| "{}".to_string()))
}

/// The journal as it happens (Server-Sent Events). Each event carries its journal id as
/// the SSE id, its kind as the SSE event name and the journal entry as JSON data. A client
/// that reconnects with `Last-Event-ID` gets everything it missed first.
#[utoipa::path(get, path = "/api/v1/events/stream", tag = "journal", params(StreamQuery,
        ("Last-Event-ID" = Option<i64>, Header, description = "The id of the last event the client saw")),
    responses((status = 200, description = "An event stream", content_type = "text/event-stream", body = String), (status = 401, body = ProblemDoc)))]
async fn events_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiQuery(q): ApiQuery<StreamQuery>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let resume = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<i64>().ok())
        .or(q.after);
    // Subscribe before reading the table, so nothing committed in between is lost; events
    // that arrive both ways are dropped by id.
    let mut live = state.journal.subscribe();
    let pool = state.pool.clone();
    let stream = async_stream::stream! {
        let latest = core_store::latest_event_id(&pool).await.unwrap_or(0);
        let mut sent = resume.unwrap_or(latest);
        yield Ok(SseEvent::default()
            .event("connected")
            .data(serde_json::json!({ "latest": latest, "resume_from": resume }).to_string()));
        let mut catch_up = resume.is_some();
        loop {
            if catch_up {
                // Read what is in the table after the last event sent, page by page.
                loop {
                    match core_store::events_after(&pool, sent, 500).await {
                        Ok(batch) if batch.is_empty() => break,
                        Ok(batch) => {
                            for e in &batch {
                                sent = e.id;
                                yield Ok(frame(e));
                            }
                        }
                        Err(_) => {
                            yield Ok(SseEvent::default().event("error").data("{\"detail\":\"Couldn't load the journal\"}"));
                            break;
                        }
                    }
                }
                catch_up = false;
            }
            match live.recv().await {
                Ok(e) if e.id > sent => {
                    sent = e.id;
                    yield Ok(frame(&e));
                }
                Ok(_) => {}
                Err(RecvError::Lagged(missed)) => {
                    // The reader fell behind the live channel: say so, then read the gap.
                    yield Ok(SseEvent::default()
                        .event("lagged")
                        .data(serde_json::json!({ "missed": missed, "resync_from": sent }).to_string()));
                    catch_up = true;
                }
                Err(RecvError::Closed) => break,
            }
        }
    };
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("heartbeat"),
    )
}

/// API keys, newest first. Secrets are never listed.
#[utoipa::path(get, path = "/api/v1/keys", tag = "keys",
    responses((status = 200, body = Listing<ApiKeyInfo>), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn keys_list(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Listing<ApiKeyInfo>> {
    require_admin(&caller)?;
    Ok(Json(Listing::new(auth::list_keys(&state.pool).await?)))
}

/// Create an API key. The secret is in this answer and nowhere else.
#[utoipa::path(post, path = "/api/v1/keys", tag = "keys", request_body = NewApiKey,
    responses((status = 201, body = CreatedApiKey), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn keys_create(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(new): ApiJson<NewApiKey>,
) -> Result<(StatusCode, Json<CreatedApiKey>), AppError> {
    require_admin(&caller)?;
    let created = auth::create_key(&state.pool, &state.journal, &caller, &new).await?;
    Ok((StatusCode::CREATED, Json(created)))
}

/// Revoke an API key. It stops working at once.
#[utoipa::path(delete, path = "/api/v1/keys/{id}", tag = "keys", params(("id" = i64, Path, description = "The key's id")),
    responses((status = 200, body = ApiKeyInfo), (status = 404, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn keys_revoke(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<ApiKeyInfo> {
    require_admin(&caller)?;
    Ok(Json(
        auth::revoke_key(&state.pool, &state.journal, &caller, id).await?,
    ))
}

/// This document.
#[utoipa::path(get, path = "/api/v1/openapi.json", tag = "system",
    responses((status = 200, description = "The OpenAPI 3.1 description of this API", body = Object), (status = 401, body = ProblemDoc)))]
async fn openapi_json() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        openapi().to_json().unwrap_or_else(|_| "{}".to_string()),
    )
        .into_response()
}

struct KeyScheme;

impl Modify for KeyScheme {
    fn modify(&self, doc: &mut utoipa::openapi::OpenApi) {
        let components = doc.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "api_key",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("mq_<key>")
                    .description(Some(
                        "An API key created at /api, sent as `Authorization: Bearer mq_...`. \
                         A browser session through the edge needs no key.",
                    ))
                    .build(),
            ),
        );
    }
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Singularr API",
        description = "Singularr, a media manager for a media server (Plex, Jellyfin or Emby). It reads the configured integrations (the media servers, Radarr, Sonarr, the download clients, Seerr, the processing services), builds one catalog, scores every file with the policy in force, and reports. It acts on an integration only where the owner allows it (`access`), and the one act so far is sending a file to a processing flow. Errors are RFC 9457 problem documents. Lists take `page` and `per_page` (at most 200)."
    ),
    paths(
        health, whoami, overview, titles, title, title_copies, title_editions,
        copy_verdict,
        releases_parse, releases_score, releases_shadow, releases_shadow_parse,
        downloads_list, downloads_orphans, policy, policy_simulate, metadata_providers,
        metadata_compare, events, events_stream, keys_list, keys_create, keys_revoke,
        openapi_json
    ),
    components(schemas(
        crate::core::schema::ParsedReleaseDoc,
        crate::core::schema::ParseNoteDoc,
        crate::core::schema::VerdictDoc,
        crate::core::schema::TraceItemDoc,
        crate::core::schema::DecisionDoc,
        ProblemDoc
    )),
    security(("api_key" = [])),
    modifiers(&KeyScheme),
    tags(
        (name = "system", description = "Health, the integrations, the caller, the overview"),
        (name = "catalog", description = "Libraries, titles and copies"),
        (name = "releases", description = "Parse, score, decide, and the shadow comparison with the arrs"),
        (name = "downloads", description = "Downloads, their owners, stuck ones and orphans"),
        (name = "policy", description = "The rules in force, and simulation of a proposed policy"),
        (name = "metadata", description = "Rich title, season, person and collection records, browse lists and search, from TMDB through the cache; the metadata providers compared"),
        (name = "journal", description = "Every change, as history and as a live stream"),
        (name = "keys", description = "API keys")
    )
)]
struct ApiDoc;

pub fn openapi() -> utoipa::openapi::OpenApi {
    ApiDoc::openapi()
        .merge_from(super::sections::openapi())
        .merge_from(super::metadata::openapi())
        .merge_from(super::server::openapi())
        .merge_from(super::config::openapi())
        .merge_from(super::rules::openapi())
        .merge_from(super::journal::openapi())
        .merge_from(super::pages::openapi())
        .merge_from(super::search::openapi())
        .merge_from(super::processing::openapi())
        .merge_from(super::notifications::openapi())
        .merge_from(super::stats::openapi())
        .merge_from(super::worker::openapi())
        .merge_from(super::processing_rules::openapi())
        .merge_from(super::profiles::openapi())
        .merge_from(super::naming::openapi())
        .merge_from(super::acquire::openapi())
}

/// The reference page. One self-contained document: it draws `/api/v1/openapi.json` with
/// its own script and loads nothing from another host.
async fn docs_page() -> Html<&'static str> {
    Html(include_str!("docs.html"))
}

/// `/api/v1` and `/api/docs`.
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/whoami", get(whoami))
        .route("/overview", get(overview))
        .route("/titles", get(titles))
        .route("/titles/{id}", get(title))
        .route("/titles/{id}/copies", get(title_copies))
        .route("/titles/{id}/editions", get(title_editions))
        .route("/copies/{id}/verdict", get(copy_verdict))
        .route("/releases/parse", post(releases_parse))
        .route("/releases/score", post(releases_score))
        .route("/releases/shadow", get(releases_shadow))
        .route("/releases/shadow/parse", post(releases_shadow_parse))
        .route("/downloads", get(downloads_list))
        .route("/downloads/orphans", get(downloads_orphans))
        .route("/policy", get(policy))
        .route("/policy/simulate", post(policy_simulate))
        .route("/metadata/providers", get(metadata_providers))
        .route("/metadata/compare", get(metadata_compare))
        .route("/events", get(events))
        .route("/events/stream", get(events_stream))
        .route("/keys", get(keys_list).post(keys_create))
        .route("/keys/{id}", delete(keys_revoke))
        .route("/openapi.json", get(openapi_json))
        .merge(super::sections::router())
        .merge(super::metadata::router())
        .merge(super::server::router())
        .merge(super::config::router())
        .merge(super::rules::router())
        .merge(super::journal::router())
        .merge(super::pages::router())
        .merge(super::search::router())
        .merge(super::processing::router())
        .merge(super::notifications::router())
        .merge(super::stats::router())
        .merge(super::worker::router())
        .merge(super::processing_rules::router())
        .merge(super::profiles::router())
        .merge(super::naming::router())
        .merge(super::acquire::router())
        .fallback(|| async { AppError::not_found("Unknown route.") })
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_read,
        ));
    let v1 = Router::new().route("/health", get(health)).merge(protected);
    Router::new()
        .nest("/api/v1", v1)
        .route("/api/docs", get(docs_page))
        // Long answers leave compressed for a caller that takes that; the event stream never.
        .layer(super::compress::layer())
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The methods a route can declare, as axum's method routers and OpenAPI both name them.
    const METHODS: [&str; 5] = ["get", "post", "put", "patch", "delete"];

    /// The routes the OpenAPI document leaves out: the reference page. It is HTML for people. It
    /// draws the document.
    const NOT_IN_THE_DOCUMENT: &[(&str, &str)] = &[("/api/docs", "get")];

    /// A path with every parameter written `{}`: a router and the document may name one
    /// parameter differently.
    fn shape(path: &str) -> String {
        let mut out = String::new();
        let mut inside = false;
        for c in path.chars() {
            match c {
                '{' => {
                    inside = true;
                    out.push_str("{}");
                }
                '}' => inside = false,
                c if !inside => out.push(c),
                _ => {}
            }
        }
        out
    }

    /// The string literal that starts `text`, without its quotes.
    fn literal(text: &str) -> &str {
        let rest = text.strip_prefix('"').expect("a string literal");
        &rest[..rest.find('"').expect("its closing quote")]
    }

    /// The `let NAME = "VALUE";` bindings of a source.
    fn bindings(source: &str) -> BTreeMap<&str, &str> {
        let mut out = BTreeMap::new();
        for part in source.split("let ").skip(1) {
            let name_end = part
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(part.len());
            if let Some(value) = part[name_end..].strip_prefix(" = \"")
                && let Some(end) = value.find("\";")
            {
                out.insert(&part[..name_end], &value[..end]);
            }
        }
        out
    }

    /// The routes of one source, before its tests: `(path, method)`, each path under `/api/v1`
    /// unless it names `/api/` itself. A path is a string literal, a `let` binding of one, or a
    /// `format!` of bindings (the worker's routes).
    fn routes_in(source: &str, out: &mut BTreeSet<(String, &'static str)>) {
        let body = source.split("#[cfg(test)]").next().unwrap_or_default();
        let lets = bindings(body);
        let mut rest = body;
        while let Some(at) = rest.find(".route(") {
            let call = &rest[at + ".route(".len()..];
            let arg = call.trim_start();
            let path = if arg.starts_with('"') {
                literal(arg).to_string()
            } else if let Some(template) = arg.strip_prefix("&format!(") {
                let mut text = literal(template)
                    .replace("{{", "\u{1}")
                    .replace("}}", "\u{2}");
                for (name, value) in &lets {
                    text = text.replace(&format!("{{{name}}}"), value);
                }
                text.replace('\u{1}', "{").replace('\u{2}', "}")
            } else {
                let name_end = arg
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(arg.len());
                lets.get(&arg[..name_end])
                    .unwrap_or_else(|| {
                        panic!("a route at `{}` the scan cannot read", &arg[..name_end])
                    })
                    .to_string()
            };
            // The method router: up to the parenthesis that closes `.route(`.
            let mut depth = 1;
            let mut end = call.len();
            for (i, c) in call.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let router = &call[..end];
            let full = if path.starts_with("/api/") {
                path
            } else {
                format!("/api/v1{path}")
            };
            let mut any_method = false;
            for method in METHODS {
                let named = router.match_indices(&format!("{method}(")).any(|(i, _)| {
                    router[..i]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'))
                });
                if named {
                    out.insert((shape(&full), method));
                    any_method = true;
                }
            }
            // `any()`, `on()`, a service or a method router held in a variable: the scan reads no
            // method from them. Such a route must not slip past the check.
            assert!(
                any_method,
                "the route {full} names no method the scan reads: `{}`",
                router.trim()
            );
            rest = &call[end..];
        }
    }

    /// Every route the API's routers declare is in the OpenAPI document with its method, and
    /// every route the document lists has a router: a scan of the `.route(` calls of every
    /// source under `app/src/api`, as `settings::tests` scans the configuration's source.
    #[test]
    fn every_route_is_in_the_openapi_document_and_every_documented_route_is_routed() {
        let mut files = Vec::new();
        let mut folders = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api")];
        while let Some(folder) = folders.pop() {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    folders.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    files.push(path);
                }
            }
        }
        let mut routed = BTreeSet::new();
        for file in &files {
            routes_in(&std::fs::read_to_string(file).unwrap(), &mut routed);
        }
        assert!(routed.len() > 100, "the scan found {} routes", routed.len());
        // Every module with a router of its own is merged into this one. Only then are the routes
        // the scan counts served.
        let api = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
        let this = include_str!("routes.rs");
        let this = this.split("#[cfg(test)]").next().unwrap_or_default();
        let mut merged = 0;
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            if file.ends_with("routes.rs") || !text.contains("pub fn router(") {
                continue;
            }
            let module: Vec<String> = file
                .strip_prefix(&api)
                .unwrap()
                .with_extension("")
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .filter(|c| c != "mod")
                .collect();
            let module = module.join("::");
            assert!(
                this.contains(&format!(".merge(super::{module}::router())")),
                "{module}::router() is not merged into the API's router"
            );
            merged += 1;
        }
        assert!(merged > 10, "{merged} module routers");
        let doc = serde_json::to_value(openapi()).unwrap();
        let documented: BTreeSet<(String, &'static str)> = doc["paths"]
            .as_object()
            .unwrap()
            .iter()
            .flat_map(|(path, item)| {
                METHODS
                    .into_iter()
                    .filter(|m| item.get(*m).is_some())
                    .map(move |m| (shape(path), m))
            })
            .collect();
        let undocumented: Vec<&(String, &str)> = routed
            .iter()
            .filter(|r| {
                !documented.contains(*r) && !NOT_IN_THE_DOCUMENT.contains(&(r.0.as_str(), r.1))
            })
            .collect();
        assert!(
            undocumented.is_empty(),
            "routes the OpenAPI document leaves out: {undocumented:?}"
        );
        let unrouted: Vec<&(String, &str)> =
            documented.iter().filter(|d| !routed.contains(*d)).collect();
        assert!(
            unrouted.is_empty(),
            "routes the OpenAPI document lists that no router declares: {unrouted:?}"
        );
        // The scan reads every form a path takes here: a literal over two lines, a binding, a
        // `format!` of a binding.
        let mut forms = BTreeSet::new();
        routes_in(
            "let at = \"/w/{id}\"; Router::new().route(at, get(a)).route(\n    \"/x/{n}\",\n    get(b).post(c),\n).route(&format!(\"{at}/n/{{node}}\"), delete(d)).route(\"/api/docs\", get(page))",
            &mut forms,
        );
        assert_eq!(
            forms,
            BTreeSet::from([
                ("/api/docs".to_string(), "get"),
                ("/api/v1/w/{}".to_string(), "get"),
                ("/api/v1/w/{}/n/{}".to_string(), "delete"),
                ("/api/v1/x/{}".to_string(), "get"),
                ("/api/v1/x/{}".to_string(), "post"),
            ])
        );
        // A route whose method the scan cannot read stops the test.
        for unread in [
            "Router::new().route(\"/y\", any(h))",
            "Router::new().route(\"/y\", on(MethodFilter::GET, h))",
            "Router::new().route(\"/y\", get_service(files))",
            "let methods = get(h); Router::new().route(\"/y\", methods)",
        ] {
            let read = std::panic::catch_unwind(|| routes_in(unread, &mut BTreeSet::new()));
            assert!(read.is_err(), "the scan passed over {unread}");
        }
    }

    #[test]
    fn the_openapi_document_lists_every_stage_one_route() {
        let doc = serde_json::to_value(openapi()).unwrap();
        assert!(doc["openapi"].as_str().unwrap().starts_with("3.1"));
        let paths = doc["paths"].as_object().unwrap();
        for (path, method) in [
            ("/api/v1/health", "get"),
            ("/api/v1/libraries", "get"),
            ("/api/v1/titles", "get"),
            ("/api/v1/titles/{id}", "get"),
            ("/api/v1/titles/{id}/copies", "get"),
            ("/api/v1/titles/{id}/editions", "get"),
            ("/api/v1/copies/{id}/verdict", "get"),
            ("/api/v1/releases/parse", "post"),
            ("/api/v1/releases/score", "post"),
            ("/api/v1/releases/shadow", "get"),
            ("/api/v1/releases/shadow/parse", "post"),
            ("/api/v1/downloads", "get"),
            ("/api/v1/downloads/orphans", "get"),
            ("/api/v1/policy", "get"),
            ("/api/v1/policy/simulate", "post"),
            ("/api/v1/metadata/providers", "get"),
            ("/api/v1/metadata/compare", "get"),
            ("/api/v1/events", "get"),
            ("/api/v1/events/stream", "get"),
            ("/api/v1/keys", "get"),
            ("/api/v1/keys", "post"),
            ("/api/v1/keys/{id}", "delete"),
            ("/api/v1/openapi.json", "get"),
        ] {
            assert!(
                paths.get(path).and_then(|p| p.get(method)).is_some(),
                "{method} {path} is missing from the OpenAPI document"
            );
        }
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for name in [
            "ParsedRelease",
            "Verdict",
            "TraceItem",
            "Decision",
            "Problem",
            "CopyView",
            "TitleEdition",
            "EditionHolding",
            "Download",
            "Event",
        ] {
            assert!(schemas.contains_key(name), "schema {name} is missing");
        }
        assert_eq!(
            doc["components"]["securitySchemes"]["api_key"]["scheme"],
            "bearer"
        );
        // The health route is the one that needs no sign-in.
        assert_eq!(
            doc["paths"]["/api/v1/health"]["get"]["security"],
            serde_json::json!([{}])
        );
    }

    /// The OpenAPI document as `/api/v1/openapi.json` serves it, byte for byte: its SHA-256 and
    /// its length are pinned in `tests/fixtures/openapi.sha256`. A move of types between crates
    /// leaves it as it was. A change to the API changes the pin. The failure names the new one.
    #[test]
    fn the_openapi_document_is_the_pinned_one() {
        use sha2::{Digest, Sha256};
        let text = openapi().to_json().unwrap();
        let found = format!(
            "{}  {}",
            hex::encode(Sha256::digest(text.as_bytes())),
            text.len()
        );
        let pinned = include_str!("../../tests/fixtures/openapi.sha256").trim();
        assert_eq!(
            found, pinned,
            "the OpenAPI document is not the pinned one. Its pin now: {found}"
        );
    }

    #[test]
    fn the_reference_page_loads_nothing_from_another_host() {
        let page = include_str!("docs.html");
        assert!(page.contains("/api/v1/openapi.json"));
        for needle in [
            "http://",
            "https://",
            "//cdn",
            "<link rel=\"stylesheet\" href",
        ] {
            assert!(
                !page.contains(needle),
                "the reference page must not reference `{needle}`"
            );
        }
    }

    #[test]
    fn key_management_needs_the_owner_or_admin() {
        let owner = Caller::Owner {
            email: "o@example.com".into(),
        };
        let admin = Caller::Key {
            id: 1,
            name: "a".into(),
            scopes: vec![Scope::Admin],
        };
        let reader = Caller::Key {
            id: 2,
            name: "r".into(),
            scopes: vec![Scope::Read, Scope::Settings],
        };
        assert!(require_admin(&owner).is_ok());
        assert!(require_admin(&admin).is_ok());
        assert!(matches!(
            require_admin(&reader),
            Err(AppError::Forbidden(_))
        ));
    }
}
