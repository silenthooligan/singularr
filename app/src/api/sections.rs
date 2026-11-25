//! The `/api/v1` routes of the Requests, Discover, Music and Home Movies sections, and the
//! summary the sidebar shows. `routes::router` merges them with one line and `routes::openapi`
//! merges their document, so these sections and the rest of the API change apart.
//!
//! Every route needs a caller with `read` (the layer in `routes::router`). The home movies
//! writes and the two scans also need `library`. Nothing here writes to an engine: the one
//! write is the owner's annotation of a tape, in Singularr's own tables.

use super::model::{EngineSync, Summary};
use super::routes::{ApiJson, ApiPath, ApiQuery};
use super::state::AppState;
use crate::core::auth::Caller;
use crate::core::error::AppError;
use crate::core::model::{Page, ScanReport, Scope};
use crate::core::schema::ProblemDoc;
use crate::core::store as core_store;
use crate::engines::service::statuses;
use crate::homemovies::model::{InboxQuery, Tape, TapePage, TapePatch, TapeQuery};
use crate::homemovies::service as homemovies;
use crate::lifecycle::service as downloads;
use crate::music::model::{AlbumPage, AlbumQuery, ArtistQuery, ArtistSummary};
use crate::music::service as music;
use crate::requests::model::{IntentQuery, RequestDetail, RequestIntent};
use crate::requests::service as requests;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use utoipa::OpenApi;

type ApiResult<T> = Result<Json<T>, AppError>;

// `GET /api/v1/requests` is the Requests page's list, in `api::pages`.

/// The requests asked for on Singularr Requests (intents), newest first, each with what the full
/// process made of it. R1 is shadow: `outcome.would_send` is what would go to Seerr, and
/// nothing did.
#[utoipa::path(get, path = "/api/v1/requests/intents", tag = "requests", params(IntentQuery),
    responses((status = 200, body = Page<RequestIntent>), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn request_intents(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<IntentQuery>,
) -> ApiResult<Page<RequestIntent>> {
    Ok(Json(
        crate::requests::intents::list(&state.pool, q.state.as_deref(), q.page, q.per_page).await?,
    ))
}

/// One request, with the other requests for the same title on either site.
#[utoipa::path(get, path = "/api/v1/requests/{id}", tag = "requests", params(("id" = i64, Path, description = "The request's id in Singularr")),
    responses((status = 200, body = RequestDetail), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn request_one(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<i64>,
) -> ApiResult<RequestDetail> {
    Ok(Json(
        requests::get(&state.pool, &state.requests.rules, id).await?,
    ))
}

/// The artists of the music library, by name, with totals over their albums.
#[utoipa::path(get, path = "/api/v1/music/artists", tag = "music", params(ArtistQuery),
    responses((status = 200, body = Page<ArtistSummary>), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn music_artists(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<ArtistQuery>,
) -> ApiResult<Page<ArtistSummary>> {
    Ok(Json(music::artists(&state.pool, &q).await?))
}

/// The albums of the music library, filtered, sorted and paged, with the library's totals.
#[utoipa::path(get, path = "/api/v1/music/albums", tag = "music", params(AlbumQuery),
    responses((status = 200, body = AlbumPage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn music_albums(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<AlbumQuery>,
) -> ApiResult<AlbumPage> {
    Ok(Json(music::albums(&state.pool, &q).await?))
}

/// The album's cover image from its folder, with the content type of its extension. Only a
/// file under the music folder is served, at most 16 MiB.
/// An album of a media server (`album-<item key>`, `album-<server>_<item key>`) has its cover
/// made by that server, resized to `?size=` pixels (64 to 1200, default 600).
#[utoipa::path(get, path = "/api/v1/music/albums/{id}/cover", tag = "music", params(("id" = String, Path, description = "`album-<item key>` or `album-<server>_<item key>` (a media server), or the folder album's id"), super::server::ImageQuery),
    responses((status = 200, description = "The image (image/jpeg, image/png, image/webp, image/gif or image/bmp)"), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn music_cover(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<super::server::ImageQuery>,
) -> Result<Response, AppError> {
    if id.starts_with("album-") {
        return super::server::server_image(&state, "album", &id, q.size).await;
    }
    let id: i64 = id
        .parse()
        .map_err(|_| AppError::bad_request(format!("`{id}` is not an album id")))?;
    let (bytes, content_type) = music::cover(&state.pool, &state.music, id).await?;
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "private, max-age=3600"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response())
}

/// Scan the music folder now. Needs the `library` scope. Answers when the scan is done.
#[utoipa::path(post, path = "/api/v1/music/scan", tag = "music",
    responses((status = 200, body = ScanReport), (status = 403, body = ProblemDoc), (status = 502, description = "The folder could not be read", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn music_scan(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<ScanReport> {
    caller.require(Scope::Library)?;
    Ok(Json(
        music::scan(&state.pool, &state.journal, &state.music, &caller.actor()).await?,
    ))
}

/// The tapes, filtered and paged, oldest first, with the years, people and places of all
/// tapes for the filter lists and the totals.
#[utoipa::path(get, path = "/api/v1/home-movies", tag = "home_movies", params(TapeQuery),
    responses((status = 200, body = TapePage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn home_movies_list(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<TapeQuery>,
) -> ApiResult<TapePage> {
    Ok(Json(homemovies::list(&state.pool, &q).await?))
}

/// Tapes that were never annotated, newest first.
#[utoipa::path(get, path = "/api/v1/home-movies/inbox", tag = "home_movies", params(InboxQuery),
    responses((status = 200, body = Page<Tape>), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn home_movies_inbox(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<InboxQuery>,
) -> ApiResult<Page<Tape>> {
    Ok(Json(homemovies::inbox(&state.pool, &q).await?))
}

/// One tape: what the owner set, what the file name says, and the effective values.
#[utoipa::path(get, path = "/api/v1/home-movies/{id}", tag = "home_movies", params(("id" = i64, Path, description = "The tape's id")),
    responses((status = 200, body = Tape), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn home_movie(State(state): State<AppState>, ApiPath(id): ApiPath<i64>) -> ApiResult<Tape> {
    Ok(Json(homemovies::get(&state.pool, id).await?))
}

/// Annotate a tape: title, years, uncertain, chapters, people, places, private, notes. A
/// field left out keeps its value, `null` goes back to what the file name says. Any patch, an
/// empty one too, takes the tape out of the inbox. Needs the `library` scope. Writes only to
/// Singularr's own tables.
#[utoipa::path(patch, path = "/api/v1/home-movies/{id}", tag = "home_movies", request_body = TapePatch,
    params(("id" = i64, Path, description = "The tape's id")),
    responses((status = 200, body = Tape), (status = 400, body = ProblemDoc), (status = 403, body = ProblemDoc), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn home_movie_patch(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
    ApiJson(patch): ApiJson<TapePatch>,
) -> ApiResult<Tape> {
    caller.require(Scope::Library)?;
    Ok(Json(
        homemovies::patch(&state.pool, &state.journal, &caller.actor(), id, &patch).await?,
    ))
}

/// Scan the home movies folder now. Needs the `library` scope. Answers when the scan is done.
#[utoipa::path(post, path = "/api/v1/home-movies/scan", tag = "home_movies",
    responses((status = 200, body = ScanReport), (status = 403, body = ProblemDoc), (status = 502, description = "The folder could not be read", body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn home_movies_scan(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<ScanReport> {
    caller.require(Scope::Library)?;
    Ok(Json(
        homemovies::scan(
            &state.pool,
            &state.journal,
            &state.home_movies,
            &caller.actor(),
        )
        .await?,
    ))
}

/// What the sidebar counts: pending requests, stuck downloads, orphans, the home movies
/// inbox, every engine's last sync and the last scan of each folder.
#[utoipa::path(get, path = "/api/v1/summary", tag = "system",
    responses((status = 200, body = Summary), (status = 401, body = ProblemDoc)))]
async fn summary(State(state): State<AppState>) -> ApiResult<Summary> {
    let counts = downloads::counts(&state.pool).await?;
    let engines = statuses(&state.pool, &state.engines)
        .await?
        .into_iter()
        .map(|e| EngineSync {
            engine: e.engine,
            kind: e.kind,
            library: e.library,
            configured: e.configured,
            ok: e.ok,
            last_sync_at: e.last_sync_at,
            last_ok_at: e.last_ok_at,
        })
        .collect();
    Ok(Json(Summary {
        requests_pending: requests::pending(&state.pool).await?,
        downloads_stuck: counts.stuck,
        downloads_orphans: counts.orphans,
        downloads_active: counts.active,
        home_movies_inbox: homemovies::inbox_count(&state.pool).await?,
        engines,
        folders: vec![
            music::state(&state.pool, &state.music).await?,
            homemovies::state(&state.pool, &state.home_movies).await?,
        ],
        latest_event_id: core_store::latest_event_id(&state.pool).await?,
        sync_enabled: state.config.sync.enabled,
    }))
}

/// The sections' routes, relative to `/api/v1`.
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/requests/intents", get(request_intents))
        .route("/requests/{id}", get(request_one))
        .route("/music/artists", get(music_artists))
        .route("/music/albums", get(music_albums))
        .route("/music/albums/{id}/cover", get(music_cover))
        .route("/music/scan", post(music_scan))
        .route("/home-movies", get(home_movies_list))
        .route("/home-movies/inbox", get(home_movies_inbox))
        .route("/home-movies/scan", post(home_movies_scan))
        .route("/home-movies/{id}", get(home_movie).patch(home_movie_patch))
        .route("/summary", get(summary))
}

#[derive(OpenApi)]
#[openapi(
    paths(
        request_intents, request_one, music_artists, music_albums, music_cover, music_scan,
        home_movies_list, home_movies_inbox, home_movie,
        home_movie_patch, home_movies_scan, summary
    ),
    tags(
        (name = "requests", description = "Requests on both Seerr sites, the season gate and the cleanup"),
        (name = "music", description = "Artists, albums, tracks and covers from the music folder"),
        (name = "home_movies", description = "The family tapes and the owner's annotations")
    )
)]
struct SectionsDoc;

/// The sections' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    SectionsDoc::openapi()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_openapi_document_lists_the_section_routes() {
        let doc = serde_json::to_value(super::super::routes::openapi()).unwrap();
        let paths = doc["paths"].as_object().unwrap();
        for (path, method) in [
            ("/api/v1/requests", "get"),
            ("/api/v1/requests/{id}", "get"),
            ("/api/v1/requests/intents", "get"),
            ("/api/v1/music/artists", "get"),
            ("/api/v1/music/artists/{id}", "get"),
            ("/api/v1/music/albums", "get"),
            ("/api/v1/music/albums/{id}", "get"),
            ("/api/v1/music/albums/{id}/cover", "get"),
            ("/api/v1/music/scan", "post"),
            ("/api/v1/home-movies", "get"),
            ("/api/v1/home-movies/inbox", "get"),
            ("/api/v1/home-movies/{id}", "get"),
            ("/api/v1/home-movies/{id}", "patch"),
            ("/api/v1/home-movies/scan", "post"),
            ("/api/v1/summary", "get"),
        ] {
            assert!(
                paths.get(path).and_then(|p| p.get(method)).is_some(),
                "{method} {path} is missing from the OpenAPI document"
            );
        }
        let schemas = doc["components"]["schemas"].as_object().unwrap();
        for name in [
            "Request",
            "RequestIntent",
            "SeasonGate",
            "Cleanup",
            "AlbumSummary",
            "Track",
            "Tape",
            "TapePatch",
            "Summary",
            "FolderScan",
        ] {
            assert!(schemas.contains_key(name), "schema {name} is missing");
        }
        let tags: Vec<&str> = doc["tags"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert!(tags.contains(&"music") && tags.contains(&"catalog"));
        // The new routes sit under the same sign-in as the rest.
        assert!(doc["paths"]["/api/v1/requests"]["get"]["security"].is_null());
    }
}
