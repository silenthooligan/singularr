//! The `/api/v1` search routes: one answer for the search field (`/search`) and which search
//! is in force (`/search/status`).
//!
//! What the library has comes from the search port (`search::Search`: Meilisearch when it is
//! configured and answers, else built in), each title with its state. Titles outside the
//! library, people and collections come from TMDB's multi search; a TMDB title the library has
//! joins the library's list. Every route needs a caller with `read`.

use super::metadata::mark;
use super::routes::ApiQuery;
use super::state::AppState;
use crate::core::error::AppError;
use crate::core::schema::ProblemDoc;
use crate::metadata::record::{AlbumCard, ArtistCard, Card, CollectionCard, SearchAnswer};
use crate::search::model::{SearchDoc, SearchStatus};
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use utoipa::OpenApi;

type ApiResult<T> = Result<Json<T>, AppError>;

/// Hits asked of the search port per kind, and titles in the library's list.
const PER_KIND: usize = 20;

#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// The words.
    pub q: Option<String>,
    /// Only one kind of result: `movie`, `show`, `title` (both), `person`, `collection`,
    /// `album`, `artist` or `music` (albums and artists). Everything when absent.
    pub kind: Option<String>,
}

/// What one search asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Wanted {
    /// Of `movie` and `show`.
    titles: Vec<&'static str>,
    people: bool,
    collections: bool,
    albums: bool,
    artists: bool,
}

fn wanted(kind: Option<&str>) -> Result<Wanted, AppError> {
    let none = Wanted {
        titles: Vec::new(),
        people: false,
        collections: false,
        albums: false,
        artists: false,
    };
    let k = kind.map(|k| k.trim().to_ascii_lowercase());
    Ok(match k.as_deref() {
        None | Some("") | Some("all") => Wanted {
            titles: vec!["movie", "show"],
            people: true,
            collections: true,
            albums: true,
            artists: true,
        },
        Some("movie") => Wanted {
            titles: vec!["movie"],
            ..none
        },
        Some("show") | Some("tv") => Wanted {
            titles: vec!["show"],
            ..none
        },
        Some("title") | Some("titles") => Wanted {
            titles: vec!["movie", "show"],
            ..none
        },
        Some("person") | Some("people") => Wanted {
            people: true,
            ..none
        },
        Some("collection") | Some("collections") => Wanted {
            collections: true,
            ..none
        },
        Some("album") | Some("albums") => Wanted {
            albums: true,
            ..none
        },
        Some("artist") | Some("artists") => Wanted {
            artists: true,
            ..none
        },
        Some("music") => Wanted {
            albums: true,
            artists: true,
            ..none
        },
        Some(other) => {
            return Err(AppError::bad_request(format!(
                "`kind` is movie, show, title, person, collection, album, artist or music, not `{other}`"
            )));
        }
    })
}

fn card(d: &SearchDoc) -> Card {
    Card {
        id: d.id.clone(),
        kind: d.kind.clone(),
        title: d.title.clone(),
        year: d.year,
        poster: d.poster.clone(),
        rating: d.rating,
        state: None,
        role: None,
        job: None,
    }
}

/// What the library has first (from the search port, each title with its state), then titles
/// outside it, people and collections (TMDB's multi search), then albums and artists. Without
/// TMDB the outside lists are empty; without Meilisearch, or when it does not answer, the
/// built-in search answers.
#[utoipa::path(get, path = "/api/v1/search", tag = "metadata", params(SearchQuery),
    responses((status = 200, body = SearchAnswer), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn search(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<SearchQuery>,
) -> ApiResult<SearchAnswer> {
    let words = q.q.as_deref().map(str::trim).unwrap_or("");
    if words.is_empty() {
        return Err(AppError::bad_request("`q` is empty."));
    }
    if words.chars().count() > 200 {
        return Err(AppError::bad_request("`q` is too long."));
    }
    let want = wanted(q.kind.as_deref())?;
    let mut kinds: Vec<String> = want.titles.iter().map(|k| k.to_string()).collect();
    for (on, kind) in [
        (want.collections, "collection"),
        (want.albums, "album"),
        (want.artists, "artist"),
    ] {
        if on {
            kinds.push(kind.to_string());
        }
    }
    let found = state.search.find(words, &kinds, PER_KIND).await?;
    let mut library: Vec<(f64, Card)> = Vec::new();
    let mut collections: Vec<CollectionCard> = Vec::new();
    let mut albums: Vec<AlbumCard> = Vec::new();
    let mut artists: Vec<ArtistCard> = Vec::new();
    for h in &found.hits {
        let d = &h.doc;
        match d.kind.as_str() {
            "movie" | "show" => library.push((h.score, card(d))),
            "collection" => collections.push(CollectionCard {
                id: d.id.clone(),
                name: d.title.clone(),
                poster: d.poster.clone(),
            }),
            "album" => albums.push(AlbumCard {
                id: d.id.clone(),
                title: d.title.clone(),
                artist: d.artist.clone(),
                artist_id: d.parent.clone(),
                year: d.year,
                cover: d.poster.clone(),
            }),
            "artist" => artists.push(ArtistCard {
                id: d.id.clone(),
                name: d.title.clone(),
                photo: d.poster.clone(),
            }),
            _ => {}
        }
    }
    // Movies and shows in one list, the best first.
    library.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut library: Vec<Card> = library.into_iter().map(|(_, c)| c).take(PER_KIND).collect();

    let outside = if want.titles.is_empty() && !want.people && !want.collections {
        None
    } else {
        match state.metadata.records(&state.pool).search(words).await {
            Ok(a) => Some(a),
            Err(AppError::Upstream(why)) => {
                tracing::warn!(error = %why, "search without TMDB");
                None
            }
            Err(e) => return Err(e),
        }
    };
    let (mut titles, people, outside_collections) = match outside {
        Some(a) => (a.titles, a.people, a.collections),
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    titles.retain(|t| want.titles.iter().any(|k| *k == t.kind));
    let mut lists = [&mut library, &mut titles];
    mark(&state.pool, &mut lists).await?;
    // A TMDB title the library has belongs to the library's list.
    let mut outside_titles = Vec::new();
    for t in titles {
        if library.iter().any(|l| l.id == t.id) {
            continue;
        }
        match t.state.as_deref() {
            Some("available") | Some("partial") => library.push(t),
            _ => outside_titles.push(t),
        }
    }
    if want.collections {
        for c in outside_collections {
            if !collections.iter().any(|x| x.id == c.id) {
                collections.push(c);
            }
        }
    }
    Ok(Json(SearchAnswer {
        query: words.to_string(),
        library,
        titles: outside_titles,
        people: if want.people { people } else { Vec::new() },
        collections,
        albums,
        artists,
    }))
}

/// Which search is in force: the provider answering now (`postgresql` built in, or
/// `meilisearch`), the one the configuration asks for, the documents in its index, when it was
/// last written and how it stands. Meilisearch that is configured and does not answer shows
/// as `warn`, with built-in search in force.
#[utoipa::path(get, path = "/api/v1/search/status", tag = "metadata",
    responses((status = 200, body = SearchStatus), (status = 401, body = ProblemDoc)))]
async fn status(State(state): State<AppState>) -> ApiResult<SearchStatus> {
    Ok(Json(state.search.status().await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/search", get(search))
        .route("/search/status", get(status))
}

#[derive(OpenApi)]
#[openapi(paths(search, status))]
struct SearchRoutesDoc;

/// The search routes' part of the OpenAPI document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    SearchRoutesDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_narrows_what_is_asked() {
        assert_eq!(wanted(None).unwrap().titles, vec!["movie", "show"]);
        let m = wanted(Some("music")).unwrap();
        assert!(m.albums && m.artists && m.titles.is_empty() && !m.people);
        assert_eq!(wanted(Some("Show")).unwrap().titles, vec!["show"]);
        assert!(wanted(Some("person")).unwrap().people);
        assert!(wanted(Some("everything")).is_err());
    }
}
