//! The `/api/v1` routes that ask a manager to search, grab, import or remove
//! (`crate::acquire`).
//!
//! Every route is the owner's alone. The interactive search is a read and answers under read
//! access too; every other route is an act: 403 `singularr.not_allowed` while Singularr may only
//! read the manager, before anything is asked of it, and a journal event naming the title and
//! the release once the manager took it. A second search of a title while one runs answers 409
//! `singularr.busy`. `core::outbound` holds the same line on the way out, each act in the one
//! form the arr adapter declares.

use super::processing::{SYNC_OFF, not_allowed, problem};
use super::routes::{ApiJson, ApiPath, ApiQuery};
use super::state::AppState;
use crate::acquire::model::{ActAnswer, ReleaseList, SearchManyAnswer};
use crate::acquire::service::{AcquireError, Asker, WantedItem};
use crate::core::auth::Caller;
use crate::core::error::AppError;
use crate::core::schema::ProblemDoc;
use crate::policy::ActivePolicy;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use utoipa::{IntoParams, OpenApi, ToSchema};

/// `GET .../releases`: a show's season, and one episode of it.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReleasesQuery {
    /// A show's season. Needed for a show.
    pub season: Option<i32>,
    /// One episode of that season.
    pub episode: Option<i32>,
}

/// `POST .../grab`: a release the last search of the title found.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrabRequest {
    pub guid: String,
    pub indexer_id: i64,
}

/// `POST .../search`: the movie; a show, one of its seasons, or episodes of a season.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    #[serde(default)]
    pub season: Option<i32>,
    /// Episode numbers of `season`.
    #[serde(default)]
    pub episodes: Vec<i32>,
}

/// `DELETE /api/v1/activity/queue/{id}`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RemoveQuery {
    /// Keep the release from being grabbed again. Default false.
    pub blocklist: Option<bool>,
}

/// One title of a search from the wanted list: a movie, or one episode of a show.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchItem {
    /// `movie-603`, `show-1396`.
    pub title: String,
    pub season: Option<i32>,
    pub episode: Option<i32>,
}

/// `POST /api/v1/search`: at most 50 titles and episodes.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchManyRequest {
    pub items: Vec<SearchItem>,
}

/// The answer of an ask that did not go through.
fn refused(e: AcquireError) -> Response {
    match e {
        AcquireError::NotFound(m) => AppError::not_found(m).into_response(),
        AcquireError::BadRequest(m) => AppError::bad_request(m).into_response(),
        AcquireError::NotAllowed(m) => not_allowed(m),
        AcquireError::Busy(m) => problem(StatusCode::CONFLICT, "singularr.busy", "Busy", m),
        AcquireError::NotOffered(m) => problem(
            StatusCode::CONFLICT,
            "singularr.not_offered",
            "Not offered",
            m,
        ),
        AcquireError::Refused(m) => problem(
            StatusCode::UNPROCESSABLE_ENTITY,
            "singularr.refused",
            "Refused",
            m,
        ),
        AcquireError::Upstream(m) => AppError::Upstream(m).into_response(),
        AcquireError::Db(e) => AppError::from(e).into_response(),
    }
}

/// Why an ask is not made: not the owner, or the integrations are not read. `None`: it may be.
fn not_the_owner(state: &AppState, caller: &Caller) -> Option<Response> {
    if !matches!(caller, Caller::Owner { .. }) {
        return Some(
            AppError::Forbidden("Only the owner can ask the managers.".to_string()).into_response(),
        );
    }
    if !state.config.sync.enabled {
        return Some(problem(
            StatusCode::CONFLICT,
            "singularr.conflict",
            "Not asked",
            format!("{SYNC_OFF}."),
        ));
    }
    None
}

fn asker<'a>(state: &'a AppState, policy: &'a ActivePolicy, actor: &'a str) -> Asker<'a> {
    Asker {
        pool: &state.pool,
        journal: &state.journal,
        engines: &state.engines,
        config: &state.config,
        policy,
        actor,
    }
}

fn answer<T: serde::Serialize>(result: Result<T, AcquireError>) -> Response {
    match result {
        Ok(v) => Json(v).into_response(),
        Err(e) => refused(e),
    }
}

/// A queue row's download: `q-<hash>`, or the hash.
fn hash_of(id: &str) -> &str {
    id.strip_prefix("q-").unwrap_or(id)
}

/// The manager's interactive search for a title: every release its indexers offer for the
/// movie, a season (`season`) or one episode (`season` and `episode`), each with Singularr's
/// verdict in the title's quality profile beside the manager's, and the files held now. It
/// asks every indexer and can take a minute; it is a read and answers under read access. The
/// owner alone.
#[utoipa::path(get, path = "/api/v1/titles/{id}/releases", tag = "acquire",
    params(("id" = String, Path, description = "The title's id: `movie-603`, `show-1396`, or its catalog number"), ReleasesQuery),
    responses((status = 200, body = ReleaseList), (status = 400, body = ProblemDoc),
        (status = 403, description = "Not the owner", body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "`singularr.busy`: a search of the title runs now", body = ProblemDoc),
        (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title_releases(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<ReleasesQuery>,
) -> Response {
    if let Some(r) = not_the_owner(&state, &caller) {
        return r;
    }
    let policy = state.policy.current();
    let actor = caller.actor();
    let ask = asker(&state, &policy, &actor);
    answer(
        async {
            let t = ask.target(&id).await?;
            ask.releases(&t, q.season, q.episode).await
        }
        .await,
    )
}

/// Grab a release the last search of the title found (kept 30 minutes), by its guid and its
/// indexer. The owner alone, while Singularr may act on the manager.
#[utoipa::path(post, path = "/api/v1/titles/{id}/grab", tag = "acquire",
    params(("id" = String, Path, description = "The title's id")),
    request_body = GrabRequest,
    responses((status = 200, body = ActAnswer), (status = 400, body = ProblemDoc),
        (status = 403, description = "Not the owner; or `singularr.not_allowed`: the manager is read-only", body = ProblemDoc),
        (status = 404, description = "Not in the last search", body = ProblemDoc),
        (status = 422, description = "`singularr.refused`: the manager said no", body = ProblemDoc),
        (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title_grab(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiJson(body): ApiJson<GrabRequest>,
) -> Response {
    if let Some(r) = not_the_owner(&state, &caller) {
        return r;
    }
    let policy = state.policy.current();
    let actor = caller.actor();
    let ask = asker(&state, &policy, &actor);
    answer(
        async {
            let t = ask.target(&id).await?;
            ask.grab(&t, body.guid.trim(), body.indexer_id).await
        }
        .await,
    )
}

/// Ask the manager to search for the title: the movie; a whole show, one season (`season`) or
/// episodes of a season (`season` and `episodes`). The owner alone, while Singularr may act on
/// the manager.
#[utoipa::path(post, path = "/api/v1/titles/{id}/search", tag = "acquire",
    params(("id" = String, Path, description = "The title's id")),
    request_body = SearchRequest,
    responses((status = 200, body = ActAnswer), (status = 400, body = ProblemDoc),
        (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "`singularr.busy`: a search of the title runs now", body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn title_search(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiJson(body): ApiJson<SearchRequest>,
) -> Response {
    if let Some(r) = not_the_owner(&state, &caller) {
        return r;
    }
    let policy = state.policy.current();
    let actor = caller.actor();
    let ask = asker(&state, &policy, &actor);
    answer(
        async {
            let t = ask.target(&id).await?;
            ask.search(&t, body.season, &body.episodes).await
        }
        .await,
    )
}

/// Import a download its manager refused, as it is: the files its manual import offers, in the
/// import mode its download client allows. The owner alone, while Singularr may act on the
/// manager that owns the download.
#[utoipa::path(post, path = "/api/v1/activity/queue/{id}/import", tag = "acquire",
    params(("id" = String, Path, description = "The queue row's id: `q-<hash>`")),
    responses((status = 200, body = ActAnswer), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "`singularr.not_offered`: no file to import", body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn queue_import(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
) -> Response {
    if let Some(r) = not_the_owner(&state, &caller) {
        return r;
    }
    let policy = state.policy.current();
    let actor = caller.actor();
    answer(asker(&state, &policy, &actor).import(hash_of(&id)).await)
}

/// Take a download out of its manager's queue and the download client; with `blocklist=true`
/// its release is not grabbed again. The owner alone, while Singularr may act on the manager
/// that owns the download.
#[utoipa::path(delete, path = "/api/v1/activity/queue/{id}", tag = "acquire",
    params(("id" = String, Path, description = "The queue row's id: `q-<hash>`"), RemoveQuery),
    responses((status = 200, body = ActAnswer), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn queue_remove(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<RemoveQuery>,
) -> Response {
    if let Some(r) = not_the_owner(&state, &caller) {
        return r;
    }
    let policy = state.policy.current();
    let actor = caller.actor();
    answer(
        asker(&state, &policy, &actor)
            .remove(hash_of(&id), q.blocklist.unwrap_or(false))
            .await,
    )
}

/// Search for titles of the wanted list: movies, and single episodes of shows (`season` and
/// `episode`), at most 50, one command per manager. The owner alone, while Singularr may act on
/// every manager named; nothing is sent when one of them is read-only.
#[utoipa::path(post, path = "/api/v1/search", tag = "acquire",
    request_body = SearchManyRequest,
    responses((status = 200, body = SearchManyAnswer), (status = 400, body = ProblemDoc),
        (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc),
        (status = 409, description = "`singularr.busy`: one of the titles is searched now", body = ProblemDoc),
        (status = 422, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn search_many(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(body): ApiJson<SearchManyRequest>,
) -> Response {
    if let Some(r) = not_the_owner(&state, &caller) {
        return r;
    }
    let policy = state.policy.current();
    let actor = caller.actor();
    let items: Vec<WantedItem> = body
        .items
        .into_iter()
        .map(|i| WantedItem {
            title: i.title,
            season: i.season,
            episode: i.episode,
        })
        .collect();
    answer(asker(&state, &policy, &actor).search_many(&items).await)
}

/// The routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/titles/{id}/releases", get(title_releases))
        .route("/titles/{id}/grab", post(title_grab))
        .route("/titles/{id}/search", post(title_search))
        .route("/activity/queue/{id}/import", post(queue_import))
        .route("/activity/queue/{id}", delete(queue_remove))
        .route("/search", post(search_many))
}

#[derive(OpenApi)]
#[openapi(
    paths(title_releases, title_grab, title_search, queue_import, queue_remove, search_many),
    tags((name = "acquire", description = "Search, grab and import through the managers: the releases of a search with Singularr's verdict beside the manager's, and the acts, each by permission"))
)]
struct AcquireDoc;

/// The routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    AcquireDoc::openapi()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_openapi_document_lists_the_acquire_routes() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        let paths = doc["paths"].as_object().unwrap();
        for (path, method) in [
            ("/api/v1/titles/{id}/releases", "get"),
            ("/api/v1/titles/{id}/grab", "post"),
            ("/api/v1/titles/{id}/search", "post"),
            ("/api/v1/activity/queue/{id}/import", "post"),
            ("/api/v1/activity/queue/{id}", "delete"),
            ("/api/v1/search", "post"),
            ("/api/v1/search", "get"),
        ] {
            assert!(
                paths.get(path).and_then(|p| p.get(method)).is_some(),
                "{method} {path} is missing from the OpenAPI document"
            );
        }
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for name in [
            "ReleaseList",
            "ReleaseView",
            "SingularrVerdict",
            "ActAnswer",
            "Refusal",
        ] {
            assert!(schemas.contains_key(name), "schema {name} is missing");
        }
        assert_eq!(super::hash_of("q-abc"), "abc");
        assert_eq!(super::hash_of("abc"), "abc");
    }
}
