//! The routes behind the pages the owner manages with, each in the shape of the web client's
//! sample file for it (`web/data/<name>.json`): the workspace record, the library tables,
//! Activity, Requests, the calendar and Home. `routes::router` merges them with one line and
//! `routes::openapi` merges their document.
//!
//! Every route needs a caller with `read` (the layer in `routes::router`) and only reads: the
//! database, and what only the managers know (their calendar, history and wanted lists) with
//! GET through the engine ports, kept a short time in [`PageCache`].

mod activity;
mod cache;
mod calendar;
pub(in crate::api) mod cards;
mod home;
pub(in crate::api) mod labels;
mod library;
mod managers;
pub mod model;
mod requests;
mod workspace;

pub use cache::PageCache;

use super::routes::ApiQuery;
use super::state::AppState;
use crate::catalog::model::TitleKind;
use crate::core::auth::Caller;
use crate::core::error::AppError;
use crate::core::schema::ProblemDoc;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use model::{Activity, Calendar, Home, LibraryTable, RequestsPage, Workspace};
use sha2::{Digest, Sha256};
use utoipa::OpenApi;

type ApiResult<T> = Result<Json<T>, AppError>;

/// The workspace record every page starts from: the name, the caller, the media server's
/// sections with what they hold, the integrations, the sidebar's counts, the server, the
/// portal, Sign out, what needs attention, and the profiles.
#[utoipa::path(get, path = "/api/v1/workspace", tag = "pages",
    responses((status = 200, body = Workspace), (status = 401, body = ProblemDoc)))]
async fn workspace_route(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Workspace> {
    Ok(Json(workspace::workspace(&state, &caller).await?))
}

/// A library table, built at most every 30 seconds, with its tag. A client that sends the tag
/// back (`If-None-Match`) and still has it gets 304.
async fn library_answer(
    state: &AppState,
    headers: &HeaderMap,
    kind: TitleKind,
) -> Result<Response, AppError> {
    let slot = match kind {
        TitleKind::Movie => &state.pages.movies,
        TitleKind::Show => &state.pages.shows,
    };
    let built = slot
        .get(|| async {
            let table = library::table(&state.pool, kind, &state.policy.current().policy).await?;
            let body = serde_json::to_vec(&table)
                .map_err(|e| AppError::Internal(format!("the table could not be written: {e}")))?;
            let digest = Sha256::digest(&body);
            let etag = format!("\"{}\"", hex::encode(&digest[..12]));
            Ok::<_, AppError>((cache::Built { body, etag }, cache::LIBRARY_KEEP))
        })
        .await?;
    let etag = HeaderValue::from_str(&built.etag)
        .map_err(|e| AppError::Internal(format!("the tag could not be sent: {e}")))?;
    let cache_control = HeaderValue::from_static("private, max-age=30");
    let fresh = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == built.etag));
    if fresh {
        return Ok((
            StatusCode::NOT_MODIFIED,
            [(header::ETAG, etag), (header::CACHE_CONTROL, cache_control)],
        )
            .into_response());
    }
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::ETAG, etag),
            (header::CACHE_CONTROL, cache_control),
        ],
        built.body.clone(),
    )
        .into_response())
}

/// Every movie as one row of `{columns, rows}`: facts, state, profile, the lead file's
/// quality, editions, files, size, score, plays. Cacheable: `ETag` and 30 seconds.
#[utoipa::path(get, path = "/api/v1/library/movies", tag = "pages",
    responses((status = 200, body = LibraryTable), (status = 304, description = "The tag sent is current"), (status = 401, body = ProblemDoc)))]
async fn library_movies(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    library_answer(&state, &headers, TitleKind::Movie).await
}

/// Every show as one row of `{columns, rows}`: facts, status, seasons, episodes and what the
/// library has of them, the next airing, state, profile, size, plays. Cacheable: `ETag` and
/// 30 seconds.
#[utoipa::path(get, path = "/api/v1/library/shows", tag = "pages",
    responses((status = 200, body = LibraryTable), (status = 304, description = "The tag sent is current"), (status = 401, body = ProblemDoc)))]
async fn library_shows(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    library_answer(&state, &headers, TitleKind::Show).await
}

/// Activity's queue: every download the server tracks, with the managers' queue facts, the
/// release's score and trace in the title's profile, and the journal's timeline. Each row says
/// whether it is a movie or a show. The managers are not asked for a history or a wanted list.
/// (Activity's record is three addresses, none ending in `/activity`: EasyPrivacy blocks a
/// first-party request whose address ends there.)
#[utoipa::path(get, path = "/api/v1/activity/queue", tag = "pages",
    responses((status = 200, description = "`{queue}`", body = Activity), (status = 401, body = ProblemDoc)))]
async fn activity_queue(State(state): State<AppState>) -> ApiResult<Activity> {
    Ok(Json(
        activity::activity(&state, activity::Parts::QUEUE).await?,
    ))
}

/// Activity's history: the managers' history, newest first, at most 500 rows, an upgrade as one
/// row. Each row says whether it is a movie or a show.
#[utoipa::path(get, path = "/api/v1/activity/history", tag = "pages",
    responses((status = 200, description = "`{history}`", body = Activity), (status = 401, body = ProblemDoc)))]
async fn activity_history(State(state): State<AppState>) -> ApiResult<Activity> {
    Ok(Json(
        activity::activity(&state, activity::Parts::HISTORY).await?,
    ))
}

/// Activity's wanted list: the managers' missing titles and episodes, and those held below the
/// profile's cutoff. Each row says whether it is a movie or a show.
#[utoipa::path(get, path = "/api/v1/activity/wanted", tag = "pages",
    responses((status = 200, description = "`{wanted}`", body = Activity), (status = 401, body = ProblemDoc)))]
async fn activity_wanted(State(state): State<AppState>) -> ApiResult<Activity> {
    Ok(Json(
        activity::activity(&state, activity::Parts::WANTED).await?,
    ))
}

/// Every request, newest first: the title, what was asked (the movie or seasons), the target,
/// who asked, the status in the client's words, the season gate, the downloads; and the people
/// who ask.
#[utoipa::path(get, path = "/api/v1/requests", tag = "pages",
    responses((status = 200, body = RequestsPage), (status = 401, body = ProblemDoc)))]
async fn requests_route(State(state): State<AppState>) -> ApiResult<RequestsPage> {
    Ok(Json(requests::requests(&state).await?))
}

/// Episodes airing and movies releasing between two days, from the managers' calendars, each
/// with its state. Without days: the Monday of last week and four weeks on.
#[utoipa::path(get, path = "/api/v1/calendar", tag = "pages", params(calendar::CalendarQuery),
    responses((status = 200, body = Calendar), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn calendar_route(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<calendar::CalendarQuery>,
) -> ApiResult<Calendar> {
    Ok(Json(calendar::calendar(&state, &q).await?))
}

/// The state of the server on one page: the counts, what needs attention, what was added
/// (movies and episodes apart), the next seven days, the queue by kind, the requests waiting
/// and what plays now.
#[utoipa::path(get, path = "/api/v1/home", tag = "pages",
    responses((status = 200, body = Home), (status = 401, body = ProblemDoc)))]
async fn home_route(State(state): State<AppState>) -> ApiResult<Home> {
    Ok(Json(home::home(&state).await?))
}

/// The page routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/workspace", get(workspace_route))
        .route("/library/movies", get(library_movies))
        .route("/library/shows", get(library_shows))
        .route("/activity/queue", get(activity_queue))
        .route("/activity/history", get(activity_history))
        .route("/activity/wanted", get(activity_wanted))
        .route("/requests", get(requests_route))
        .route("/calendar", get(calendar_route))
        .route("/home", get(home_route))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        workspace_route, library_movies, library_shows, activity_queue, activity_history,
        activity_wanted, requests_route, calendar_route, home_route
    ),
    tags((name = "pages", description = "What the workspace's pages read, in the shapes of the web client's data"))
)]
struct PagesDoc;

/// The page routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    PagesDoc::openapi()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_openapi_document_lists_the_page_routes() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        let paths = doc["paths"].as_object().unwrap();
        for path in [
            "/api/v1/workspace",
            "/api/v1/library/movies",
            "/api/v1/library/shows",
            "/api/v1/activity/queue",
            "/api/v1/activity/history",
            "/api/v1/activity/wanted",
            "/api/v1/requests",
            "/api/v1/calendar",
            "/api/v1/home",
        ] {
            assert!(
                paths.get(path).and_then(|p| p.get("get")).is_some(),
                "GET {path} is missing from the OpenAPI document"
            );
        }
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for name in [
            "Workspace",
            "LibraryTable",
            "Activity",
            "QueueRow",
            "HistoryRow",
            "WantedRow",
            "RequestsPage",
            "RequestItem",
            "Calendar",
            "CalendarItem",
            "Home",
            "TitleCard",
        ] {
            assert!(schemas.contains_key(name), "schema {name} is missing");
        }
    }
}
