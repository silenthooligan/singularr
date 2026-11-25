//! The router of Singularr Requests, built from the request routes alone.
//!
//! No owner route is registered here, so none can answer: `/api/v1/...`, `/metrics`,
//! `/api/docs`, the owner pages and their server functions all answer 404. The identity headers
//! of the edge (`X-Auth-Request-*`, `X-Edge-Token`) and `Authorization` are removed from
//! every request before a handler runs, and no handler reads them: on this face a person is
//! their session cookie and nothing else, the owner included.

use super::html::{self, Card, Here, SeasonRow, Shell};
use super::plex::PlexSignIn;
use super::session::{
    self, Limiter, PIN_COOKIE, PIN_SECS, SESSION_COOKIE, SESSION_DAYS, form_pairs, own_cookies,
    same_origin, set_cookie, token_sha256,
};
use crate::catalog::model::TitleKind;
use crate::core::config::RequestsFaceConfig;
use crate::engines::model::DiscoverList;
use crate::metadata::tmdb::Tmdb;
use crate::requests::model::{FaceRules, FaceSession};
use crate::requests::store as requests_store;
use axum::extract::{ConnectInfo, Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Router};
use chrono::Utc;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

/// The content security policy of every page: no script at all, styles, fonts and the manifest
/// from this face, images from here and TMDB, forms only to this face and to Plex's sign-in.
pub const CSP: &str = "default-src 'none'; style-src 'self'; img-src 'self' https://image.tmdb.org; font-src 'self'; manifest-src 'self'; form-action 'self' https://app.plex.tv; base-uri 'none'; frame-ancestors 'none'";

/// New requests one person may make in an hour (the database refuses more than 30).
pub const REQUESTS_PER_HOUR: usize = 10;

/// Form and query keys that would ask for 4K. The face is HD only; a request that carries one
/// is refused.
pub const FOUR_K_KEYS: &[&str] = &["4k", "is4k", "is_4k", "uhd", "quality", "tier"];

pub struct FaceInner {
    pub config: RequestsFaceConfig,
    pub pool: sqlx::PgPool,
    pub plex: PlexSignIn,
    pub tmdb: Tmdb,
    /// Sign-ins started per client, and over all clients.
    pub signins: Limiter,
    pub signins_all: Limiter,
    /// Checks of a PIN per client.
    pub polls: Limiter,
}

#[derive(Clone)]
pub struct FaceState(pub Arc<FaceInner>);

impl std::ops::Deref for FaceState {
    type Target = FaceInner;
    fn deref(&self) -> &FaceInner {
        &self.0
    }
}

impl FaceState {
    pub fn new(
        config: RequestsFaceConfig,
        pool: sqlx::PgPool,
        out: crate::core::outbound::Outbound,
    ) -> FaceState {
        // A stable name for this installation at Plex, derived from the secret one way.
        let client_id = token_sha256(&format!(
            "singularr-requests plex client|{}",
            config.session_secret
        ))[..32]
            .to_string();
        FaceState(Arc::new(FaceInner {
            plex: PlexSignIn::new(client_id, out.clone()),
            tmdb: Tmdb::new(config.tmdb_token.clone(), out),
            config,
            pool,
            signins: Limiter::new(5, Duration::from_secs(600)),
            signins_all: Limiter::new(60, Duration::from_secs(600)),
            polls: Limiter::new(120, Duration::from_secs(600)),
        }))
    }
}

/// The whole router of the face.
pub fn router(state: FaceState) -> Router {
    Router::new()
        .route("/", get(discover))
        .route("/signin", get(signin_page).post(signin_start))
        .route("/signin/plex", get(signin_back))
        .route("/signout", post(signout))
        .route("/search", get(search))
        .route("/movie/{tmdb}", get(movie_page))
        .route("/show/{tmdb}", get(show_page))
        .route("/movie/{tmdb}/request", post(movie_request))
        .route("/show/{tmdb}/request", post(show_request))
        .route("/requests", get(my_requests))
        .route("/assets/requests.css", get(stylesheet))
        .route("/assets/fonts/{file}", get(font))
        .route("/brand/{file}", get(brand))
        .route("/manifest.webmanifest", get(manifest))
        .route("/robots.txt", get(robots))
        .route("/healthz", get(|| async { (StatusCode::OK, "ok") }))
        .route("/readyz", get(ready))
        .fallback(not_found)
        .layer(axum::middleware::from_fn(face_headers))
        .with_state(state)
}

/// Remove the edge's identity headers before anything runs; add the security headers after.
async fn face_headers(mut req: Request, next: Next) -> Response {
    let drop: Vec<header::HeaderName> = req
        .headers()
        .keys()
        .filter(|n| {
            let n = n.as_str();
            n.starts_with("x-auth-request-")
                || n.starts_with("x-forwarded-user")
                || n.starts_with("x-forwarded-email")
                || n.starts_with("x-forwarded-preferred-username")
                || n == "x-edge-token"
                || n == "authorization"
        })
        .cloned()
        .collect();
    for name in drop {
        req.headers_mut().remove(name);
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static("same-origin"),
    );
    h.insert(
        "permissions-policy",
        HeaderValue::from_static("camera=(), microphone=(), geolocation=(), interest-cohort=()"),
    );
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

// Responses -------------------------------------------------------------------------------------

fn html_response(status: StatusCode, body: String) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        body,
    )
        .into_response()
}

fn see_other(location: &str, cookies: &[String]) -> Response {
    let mut res = (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, location.to_string())],
    )
        .into_response();
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}

fn with_cookies(mut res: Response, cookies: &[String]) -> Response {
    for c in cookies {
        if let Ok(v) = HeaderValue::from_str(c) {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}

fn failed(e: impl std::fmt::Display, what: &str) -> Response {
    tracing::error!(error = %e, "{what}");
    html_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        html::message(None, "Something went wrong", "Try again in a moment."),
    )
}

// Who is asking ------------------------------------------------------------------------------------

/// The session token of the request's cookie, when it has the shape of one.
fn token_of(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    own_cookies(cookie)
        .remove(SESSION_COOKIE)
        .filter(|t| session::plausible_token(t))
}

/// The signed-in person, from the database's view of the cookie's session.
async fn person(
    state: &FaceState,
    headers: &HeaderMap,
) -> Result<Option<(FaceSession, String)>, sqlx::Error> {
    let Some(token) = token_of(headers) else {
        return Ok(None);
    };
    Ok(requests_store::face_session(&state.pool, &token)
        .await?
        .map(|s| (s, token)))
}

/// The client a rate limit counts: the first address the edge forwarded, else the peer.
fn client_key(headers: &HeaderMap, peer: Option<&ConnectInfo<SocketAddr>>) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .or_else(|| peer.map(|p| p.0.ip().to_string()))
        .unwrap_or_else(|| "unknown".to_string())
}

fn is_same_origin(state: &FaceState, headers: &HeaderMap) -> bool {
    let text = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    same_origin(
        text("sec-fetch-site"),
        text("origin"),
        &state.config.public_url,
    )
}

async fn rules(state: &FaceState) -> Option<FaceRules> {
    requests_store::face_rules(&state.pool).await.ok().flatten()
}

fn shell<'a>(person: &'a FaceSession, rules: Option<&'a FaceRules>, here: Here) -> Shell<'a> {
    Shell {
        person,
        owner_url: rules.and_then(|r| r.owner_url.as_deref()),
        here,
    }
}

/// The state of each title of a list: (kind, TMDB id) to its state.
async fn states(state: &FaceState, ids: &[i64]) -> HashMap<(String, i64), String> {
    match requests_store::face_availability(&state.pool, ids).await {
        Ok(rows) => rows.into_iter().map(|(k, id, s)| ((k, id), s)).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "the titles' states could not be read");
            HashMap::new()
        }
    }
}

macro_rules! signed_in {
    ($state:expr, $headers:expr) => {
        match person(&$state, &$headers).await {
            Ok(Some(found)) => found,
            Ok(None) => return see_other("/signin", &[]),
            Err(e) => return failed(e, "the session could not be read"),
        }
    };
}

// Pages ----------------------------------------------------------------------------------------------

async fn discover(State(state): State<FaceState>, headers: HeaderMap) -> Response {
    let (me, _) = signed_in!(state, headers);
    let rules = rules(&state).await;
    let items = match requests_store::face_discover(&state.pool).await {
        Ok(items) => items,
        Err(e) => return failed(e, "the discover lists could not be read"),
    };
    let ids: Vec<i64> = items.iter().map(|i| i.tmdb_id).collect();
    let marks = states(&state, &ids).await;
    let lists: Vec<(&str, Vec<Card<'_>>)> = DiscoverList::ALL
        .iter()
        .map(|list| {
            let cards = items
                .iter()
                .filter(|i| i.list == list.as_str())
                .map(|i| Card {
                    kind: &i.kind,
                    tmdb_id: i.tmdb_id,
                    title: &i.title,
                    year: i.year,
                    poster_path: i.poster_path.as_deref(),
                    state: marks.get(&(i.kind.clone(), i.tmdb_id)).map(String::as_str),
                })
                .collect();
            (list.label(), cards)
        })
        .collect();
    html_response(
        StatusCode::OK,
        html::discover(&shell(&me, rules.as_ref(), Here::Discover), &lists),
    )
}

async fn search(
    State(state): State<FaceState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (me, _) = signed_in!(state, headers);
    let rules = rules(&state).await;
    let words: String = q
        .get("q")
        .map(|s| s.trim().chars().take(100).collect())
        .unwrap_or_default();
    let shell = shell(&me, rules.as_ref(), Here::Search);
    if words.is_empty() {
        return html_response(StatusCode::OK, html::search(&shell, "", None, None));
    }
    let found = match state.tmdb.search(&words, 1).await {
        Ok(found) => found,
        Err(e) => {
            tracing::warn!(error = %e, "a search failed");
            return html_response(
                StatusCode::OK,
                html::search(
                    &shell,
                    &words,
                    None,
                    Some("Search unavailable. Try again in a moment."),
                ),
            );
        }
    };
    let ids: Vec<i64> = found.iter().map(|r| r.tmdb_id).collect();
    let marks = states(&state, &ids).await;
    let cards: Vec<Card<'_>> = found
        .iter()
        .map(|r| {
            let kind = r.kind.as_str();
            Card {
                kind,
                tmdb_id: r.tmdb_id,
                title: &r.title,
                year: r.year,
                poster_path: r.poster_path.as_deref(),
                state: marks
                    .get(&(kind.to_string(), r.tmdb_id))
                    .map(String::as_str),
            }
        })
        .collect();
    html_response(
        StatusCode::OK,
        html::search(&shell, &words, Some(&cards), None),
    )
}

async fn title_page(
    state: &FaceState,
    headers: &HeaderMap,
    kind: TitleKind,
    tmdb_id: i64,
    message: Option<&str>,
) -> Response {
    let (me, _) = signed_in!(state, headers);
    let rules = rules(state).await;
    let shell = shell(&me, rules.as_ref(), Here::Title);
    let t = match state.tmdb.details(kind, tmdb_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return html_response(
                StatusCode::NOT_FOUND,
                html::message(Some(&shell), "Not found", "Title not found."),
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "a title could not be read");
            return html_response(
                StatusCode::BAD_GATEWAY,
                html::message(
                    Some(&shell),
                    "Not answering",
                    "Couldn't load the title. Try again in a moment.",
                ),
            );
        }
    };
    let marks = states(state, &[tmdb_id]).await;
    let title_state = marks.get(&(kind.as_str().to_string(), tmdb_id)).cloned();
    let season_states: HashMap<i32, String> = if kind == TitleKind::Show {
        requests_store::face_seasons(&state.pool, tmdb_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect()
    } else {
        HashMap::new()
    };
    let seasons: Vec<SeasonRow<'_>> = t
        .seasons
        .iter()
        .map(|s| SeasonRow {
            number: s.number,
            name: s.name.as_deref(),
            episodes: s.episodes,
            state: season_states.get(&s.number).map(String::as_str),
        })
        .collect();
    html_response(
        StatusCode::OK,
        html::title(
            &shell,
            &t,
            title_state.as_deref(),
            &seasons,
            rules.as_ref(),
            message,
        ),
    )
}

async fn movie_page(
    State(state): State<FaceState>,
    headers: HeaderMap,
    Path(tmdb): Path<i64>,
) -> Response {
    title_page(&state, &headers, TitleKind::Movie, tmdb, None).await
}

async fn show_page(
    State(state): State<FaceState>,
    headers: HeaderMap,
    Path(tmdb): Path<i64>,
) -> Response {
    title_page(&state, &headers, TitleKind::Show, tmdb, None).await
}

/// A request for a title: checked, then recorded as an intent. HD only.
async fn request_title(
    state: &FaceState,
    headers: &HeaderMap,
    raw_query: Option<&str>,
    body: &str,
    kind: TitleKind,
    tmdb_id: i64,
) -> Response {
    if !is_same_origin(state, headers) {
        return html_response(
            StatusCode::FORBIDDEN,
            html::message(None, "Refused", "Request from another site."),
        );
    }
    let pairs = form_pairs(body);
    let query = form_pairs(raw_query.unwrap_or(""));
    if pairs
        .iter()
        .chain(query.iter())
        .any(|(k, _)| FOUR_K_KEYS.contains(&k.to_ascii_lowercase().as_str()))
    {
        return html_response(
            StatusCode::BAD_REQUEST,
            html::message(None, "HD only", "Requests are HD only."),
        );
    }
    let (me, token) = signed_in!(state, headers);
    let t = match state.tmdb.details(kind, tmdb_id).await {
        Ok(Some(t)) => t,
        Ok(None) => {
            return html_response(
                StatusCode::NOT_FOUND,
                html::message(None, "Not found", "Title not found."),
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "a title could not be read for a request");
            return title_page(
                state,
                headers,
                kind,
                tmdb_id,
                Some("Couldn't check the title; nothing was requested. Try again in a moment."),
            )
            .await;
        }
    };
    // Already here, or already asked for: say so, ask nothing.
    let marks = states(state, &[tmdb_id]).await;
    let mut seasons: Vec<i32> = Vec::new();
    if kind == TitleKind::Show {
        let listed: Vec<i32> = t.seasons.iter().map(|s| s.number).collect();
        let taken: HashMap<i32, String> = requests_store::face_seasons(&state.pool, tmdb_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .collect();
        seasons = pairs
            .iter()
            .filter(|(k, _)| k == "seasons")
            .filter_map(|(_, v)| v.trim().parse::<i32>().ok())
            .filter(|n| listed.contains(n) && !taken.contains_key(n))
            .collect();
        seasons.sort_unstable();
        seasons.dedup();
        if seasons.is_empty() {
            return title_page(
                state,
                headers,
                kind,
                tmdb_id,
                Some("Choose a season that is not available or requested."),
            )
            .await;
        }
    } else if marks.contains_key(&(kind.as_str().to_string(), tmdb_id)) {
        return title_page(
            state,
            headers,
            kind,
            tmdb_id,
            Some("Already available or requested."),
        )
        .await;
    }
    let mine = match requests_store::face_my_requests(&state.pool, &token).await {
        Ok(rows) => rows,
        Err(e) => return failed(e, "the person's requests could not be read"),
    };
    let hour_ago = Utc::now() - chrono::Duration::hours(1);
    let recent = mine
        .iter()
        .filter(|r| r.source == "intent" && r.asked_at > hour_ago)
        .count();
    if recent >= REQUESTS_PER_HOUR {
        return title_page(
            state,
            headers,
            kind,
            tmdb_id,
            Some("Too many requests in the last hour. Try again later."),
        )
        .await;
    }
    let write = requests_store::IntentWrite {
        kind: kind.as_str(),
        tmdb_id,
        seasons: &seasons,
        title: Some(&t.title),
        year: t.year,
    };
    if let Err(e) = requests_store::insert_intent(&state.pool, &token, &write).await {
        tracing::warn!(error = %e, person = me.plex_account_id, "an intent was refused");
        return title_page(
            state,
            headers,
            kind,
            tmdb_id,
            Some("Couldn't record the request. Try again later."),
        )
        .await;
    }
    see_other("/requests?asked=1", &[])
}

async fn movie_request(
    State(state): State<FaceState>,
    headers: HeaderMap,
    Path(tmdb): Path<i64>,
    req: Request,
) -> Response {
    let query = req.uri().query().map(str::to_string);
    let body = match axum::body::to_bytes(req.into_body(), 16 * 1024).await {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => {
            return html_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                html::message(None, "Too large", "Form too large."),
            );
        }
    };
    request_title(
        &state,
        &headers,
        query.as_deref(),
        &body,
        TitleKind::Movie,
        tmdb,
    )
    .await
}

async fn show_request(
    State(state): State<FaceState>,
    headers: HeaderMap,
    Path(tmdb): Path<i64>,
    req: Request,
) -> Response {
    let query = req.uri().query().map(str::to_string);
    let body = match axum::body::to_bytes(req.into_body(), 16 * 1024).await {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => {
            return html_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                html::message(None, "Too large", "Form too large."),
            );
        }
    };
    request_title(
        &state,
        &headers,
        query.as_deref(),
        &body,
        TitleKind::Show,
        tmdb,
    )
    .await
}

async fn my_requests(
    State(state): State<FaceState>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let (me, token) = signed_in!(state, headers);
    let rules = rules(&state).await;
    let items = match requests_store::face_my_requests(&state.pool, &token).await {
        Ok(items) => items,
        Err(e) => return failed(e, "the person's requests could not be read"),
    };
    // Just asked: say what, in the person's words, and that nothing is sent yet.
    let asked = q
        .contains_key("asked")
        .then(|| match html::requested(&items) {
            Some(words) => format!("{words}. Test run: nothing is sent."),
            None => "Recorded. Test run: nothing is sent.".to_string(),
        });
    let message = asked.as_deref();
    html_response(
        StatusCode::OK,
        html::my_requests(
            &shell(&me, rules.as_ref(), Here::MyRequests),
            &items,
            message,
        ),
    )
}

// The identity's files ---------------------------------------------------------------------------

/// A file the face hands out as it is, kept a day by the browser.
fn kept(bytes: &'static [u8], content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        bytes,
    )
        .into_response()
}

/// The portal's marks, icons and share card, from `web/brand` in the repository.
pub fn brand_file(name: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match name {
        "requests-dark.svg" => (
            include_bytes!("../../../web/brand/requests-dark.svg").as_slice(),
            "image/svg+xml",
        ),
        "favicon.ico" => (
            include_bytes!("../../../web/brand/favicon.ico").as_slice(),
            "image/x-icon",
        ),
        "favicon.svg" => (
            include_bytes!("../../../web/brand/favicon.svg").as_slice(),
            "image/svg+xml",
        ),
        "apple-touch-icon.png" => (
            include_bytes!("../../../web/brand/apple-touch-icon.png").as_slice(),
            "image/png",
        ),
        "icon-192.png" => (
            include_bytes!("../../../web/brand/icon-192.png").as_slice(),
            "image/png",
        ),
        "icon-512.png" => (
            include_bytes!("../../../web/brand/icon-512.png").as_slice(),
            "image/png",
        ),
        "share-card.png" => (
            include_bytes!("../../../web/brand/share-card.png").as_slice(),
            "image/png",
        ),
        _ => return None,
    })
}

/// The two fonts, from `web/css/fonts` in the repository.
pub fn font_file(name: &str) -> Option<&'static [u8]> {
    Some(match name {
        "DMSans.ttf" => include_bytes!("../../../web/css/fonts/DMSans.ttf").as_slice(),
        "IBMPlexMono-Regular.ttf" => {
            include_bytes!("../../../web/css/fonts/IBMPlexMono-Regular.ttf").as_slice()
        }
        _ => return None,
    })
}

async fn brand(Path(file): Path<String>) -> Response {
    match brand_file(&file) {
        Some((bytes, kind)) => kept(bytes, kind),
        None => not_found().await,
    }
}

async fn font(Path(file): Path<String>) -> Response {
    match font_file(&file) {
        Some(bytes) => kept(bytes, "font/ttf"),
        None => not_found().await,
    }
}

/// The portal as an app on a phone's home screen.
pub const MANIFEST: &str = r##"{
  "name": "Singularr Requests",
  "short_name": "Requests",
  "start_url": "/",
  "scope": "/",
  "display": "standalone",
  "background_color": "#0e0d15",
  "theme_color": "#0e0d15",
  "icons": [
    {"src": "/brand/icon-192.png", "sizes": "192x192", "type": "image/png"},
    {"src": "/brand/icon-512.png", "sizes": "512x512", "type": "image/png"},
    {"src": "/brand/favicon.svg", "sizes": "any", "type": "image/svg+xml"}
  ]
}
"##;

async fn manifest() -> Response {
    kept(MANIFEST.as_bytes(), "application/manifest+json")
}

// Sign-in -----------------------------------------------------------------------------------------

async fn signin_page(State(state): State<FaceState>, headers: HeaderMap) -> Response {
    match person(&state, &headers).await {
        Ok(Some(_)) => see_other("/", &[]),
        Ok(None) => html_response(
            StatusCode::OK,
            html::signin_shared(&state.config.public_url),
        ),
        Err(e) => failed(e, "the session could not be read"),
    }
}

async fn signin_start(
    State(state): State<FaceState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Response {
    if !is_same_origin(&state, &headers) {
        return html_response(
            StatusCode::FORBIDDEN,
            html::signin(Some("Sign-in from another site.")),
        );
    }
    let client = client_key(&headers, peer.as_ref().map(|p| &p.0));
    if !state.signins.allow(&client) || !state.signins_all.allow("all") {
        return html_response(
            StatusCode::TOO_MANY_REQUESTS,
            html::signin(Some("Too many sign-ins. Try again in a few minutes.")),
        );
    }
    let pin = match state.plex.create_pin().await {
        Ok(pin) => pin,
        Err(e) => {
            tracing::warn!(error = %e, "plex.tv did not make a sign-in PIN");
            return html_response(
                StatusCode::BAD_GATEWAY,
                html::signin(Some("Plex unavailable. Try again in a moment.")),
            );
        }
    };
    let expires = Utc::now().timestamp() + PIN_SECS;
    let cookie = set_cookie(
        PIN_COOKIE,
        &session::pin_cookie_value(&state.config.session_secret, pin.id, expires),
        PIN_SECS,
    );
    let back = format!("{}/signin/plex", state.config.public_url);
    see_other(&state.plex.auth_url(&pin.code, &back), &[cookie])
}

/// Plex sends the browser back here. The PIN is checked; once approved, the account is named,
/// the Plex token is dropped, and a session starts for an account plexseerr knows.
async fn signin_back(
    State(state): State<FaceState>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> Response {
    let clear_pin = set_cookie(PIN_COOKIE, "", 0);
    let cookie = headers.get(header::COOKIE).and_then(|v| v.to_str().ok());
    let pin_id = own_cookies(cookie).remove(PIN_COOKIE).and_then(|v| {
        session::read_pin_cookie(&state.config.session_secret, &v, Utc::now().timestamp())
    });
    let Some(pin_id) = pin_id else {
        return with_cookies(
            html_response(
                StatusCode::OK,
                html::signin(Some("Sign-in expired. Start again.")),
            ),
            &[clear_pin],
        );
    };
    if !state
        .polls
        .allow(&client_key(&headers, peer.as_ref().map(|p| &p.0)))
    {
        return html_response(
            StatusCode::TOO_MANY_REQUESTS,
            html::signin(Some("Too many checks. Start again in a few minutes.")),
        );
    }
    let token = match state.plex.pin_token(pin_id).await {
        Ok(Some(t)) => t,
        Ok(None) => return html_response(StatusCode::OK, html::waiting()),
        Err(e) => {
            tracing::warn!(error = %e, "plex.tv did not answer about a PIN");
            return html_response(
                StatusCode::BAD_GATEWAY,
                html::signin(Some("Plex unavailable. Try again in a moment.")),
            );
        }
    };
    // The Plex token names the account and is dropped here.
    let account = match state.plex.account(token).await {
        Ok(a) => a,
        Err(e) => {
            tracing::warn!(error = %e, "plex.tv did not name the account");
            return with_cookies(
                html_response(
                    StatusCode::BAD_GATEWAY,
                    html::signin(Some("Plex returned no account. Start again.")),
                ),
                &[clear_pin],
            );
        }
    };
    let known = match requests_store::face_account(&state.pool, account.id).await {
        Ok(k) => k,
        Err(e) => return failed(e, "the sign-in check failed"),
    };
    let Some(known) = known else {
        tracing::info!(
            plex_account = account.id,
            "a sign-in by a Plex account the request site does not know was refused"
        );
        return with_cookies(
            html_response(
                StatusCode::FORBIDDEN,
                html::signin(Some(
                    "This Plex account has no access. The server must be shared with it.",
                )),
            ),
            &[clear_pin],
        );
    };
    let token = session::new_token();
    let expires = Utc::now() + chrono::Duration::days(SESSION_DAYS);
    if let Err(e) = requests_store::insert_session(
        &state.pool,
        &token_sha256(&token),
        known.plex_account_id,
        expires,
    )
    .await
    {
        return failed(e, "a session could not be started");
    }
    tracing::info!(plex_account = known.plex_account_id, "signed in");
    see_other(
        "/",
        &[
            set_cookie(SESSION_COOKIE, &token, SESSION_DAYS * 86_400),
            clear_pin,
        ],
    )
}

async fn signout(State(state): State<FaceState>, headers: HeaderMap) -> Response {
    if !is_same_origin(&state, &headers) {
        return html_response(
            StatusCode::FORBIDDEN,
            html::message(None, "Refused", "Sign-out from another site."),
        );
    }
    if let Some(token) = token_of(&headers)
        && let Err(e) = requests_store::end_session(&state.pool, &token_sha256(&token)).await
    {
        return failed(e, "a session could not be ended");
    }
    see_other("/signin", &[set_cookie(SESSION_COOKIE, "", 0)])
}

// The rest ------------------------------------------------------------------------------------------

async fn stylesheet() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "public, max-age=3600"),
        ],
        html::STYLESHEET.as_str(),
    )
        .into_response()
}

async fn robots() -> Response {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "User-agent: *\nDisallow: /\n",
    )
        .into_response()
}

async fn ready(State(state): State<FaceState>) -> Response {
    match requests_store::face_rules(&state.pool).await {
        Ok(_) => (StatusCode::OK, "ready").into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "not ready");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
        }
    }
}

async fn not_found() -> Response {
    html_response(
        StatusCode::NOT_FOUND,
        html::message(None, "Not found", "Page not found."),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Outbound};
    use axum::body::Body;
    use std::collections::BTreeMap;
    use tower::ServiceExt;

    /// The face over a database that is never reached: the routes below answer without one.
    fn app() -> Router {
        let config = RequestsFaceConfig::from(&BTreeMap::from([
            (
                "SINGULARR_REQUESTS_DATABASE_URL",
                "postgres://x@127.0.0.1:1/x",
            ),
            (
                "SINGULARR_REQUESTS_SESSION_SECRET",
                "0123456789abcdef0123456789abcdef-test",
            ),
            (
                "SINGULARR_REQUESTS_PUBLIC_URL",
                "https://requests.example.org",
            ),
        ]))
        .unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://x@127.0.0.1:1/x")
            .unwrap();
        let out = Outbound::with_transport(Allowlist::for_requests_face(), Recorded::new());
        router(FaceState::new(config, pool, out))
    }

    async fn get(app: &Router, path: &str, extra: &[(&str, &str)]) -> Response {
        let mut req = Request::builder().uri(path);
        for (n, v) in extra {
            req = req.header(*n, *v);
        }
        app.clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    /// The separation is in the build: no owner route is registered on this face, so each one
    /// answers 404 even with the owner's identity headers, an edge token and an API key.
    #[tokio::test]
    async fn owner_routes_do_not_exist_on_this_face() {
        let app = app();
        let owner = [
            ("x-auth-request-email", "owner@example.com"),
            ("x-auth-request-user", "owner"),
            ("x-forwarded-email", "owner@example.com"),
            ("x-edge-token", "the-edge-token-the-owner-face-accepts"),
            ("authorization", "Bearer mq_0123456789abcdef"),
        ];
        for path in [
            "/api/v1/health",
            "/api/v1/whoami",
            "/api/v1/titles",
            "/api/v1/titles/1/editions",
            "/api/v1/requests",
            "/api/v1/requests/intents",
            "/api/v1/keys",
            "/api/v1/events/stream",
            "/api/v1/openapi.json",
            "/api/docs",
            "/metrics",
            "/sfn/shell",
            "/sfn/requests",
            "/movies",
            "/settings",
            "/home-movies",
            "/music",
            "/journal",
            "/pkg/singularr.js",
        ] {
            let res = get(&app, path, &owner).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
            assert!(res.headers().get(header::CONTENT_SECURITY_POLICY).is_some());
        }
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/sfn/key_create")
                    .header("x-auth-request-email", "owner@example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_owner_headers_do_not_sign_anyone_in() {
        let app = app();
        // Without the face's own cookie every page goes to the sign-in, whatever the edge says.
        let res = get(
            &app,
            "/requests",
            &[
                ("x-auth-request-email", "owner@example.com"),
                ("x-edge-token", "edge"),
                ("cookie", "_oauth2_proxy=a-gate-session; mq_session=x"),
            ],
        )
        .await;
        assert_eq!(res.status(), StatusCode::SEE_OTHER);
        assert_eq!(res.headers()[header::LOCATION], "/signin");
    }

    #[tokio::test]
    async fn requests_are_hd_only_and_same_origin() {
        let app = app();
        let post = |uri: &str, body: &str, site: &str| {
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("sec-fetch-site", site)
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        for (uri, body) in [
            ("/movie/603/request", "is4k=true"),
            ("/movie/603/request", "4K=1"),
            ("/show/1396/request", "seasons=1&is_4k=1"),
            ("/show/1396/request?quality=2160p", "seasons=1"),
        ] {
            let res = app
                .clone()
                .oneshot(post(uri, body, "same-origin"))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::BAD_REQUEST, "{uri} {body}");
        }
        let res = app
            .clone()
            .oneshot(post("/movie/603/request", "", "cross-site"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let res = app
            .clone()
            .oneshot(post("/signin", "", "cross-site"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn every_answer_carries_the_security_headers() {
        let app = app();
        let res = get(&app, "/assets/requests.css", &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        let h = res.headers();
        assert_eq!(h[header::CONTENT_SECURITY_POLICY], CSP);
        assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[header::REFERRER_POLICY], "no-referrer");
        assert!(
            h[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .starts_with("text/css")
        );
        assert!(CSP.contains("script-src") || CSP.starts_with("default-src 'none'"));
        assert!(!CSP.contains("'unsafe-inline'"));
        let res = get(&app, "/healthz", &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers()[header::CACHE_CONTROL], "no-store");
        let res = get(&app, "/robots.txt", &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn the_identity_s_files_are_served_by_the_face() {
        let app = app();
        for (path, kind) in [
            ("/brand/requests-dark.svg", "image/svg+xml"),
            ("/brand/favicon.ico", "image/x-icon"),
            ("/brand/favicon.svg", "image/svg+xml"),
            ("/brand/apple-touch-icon.png", "image/png"),
            ("/brand/icon-192.png", "image/png"),
            ("/brand/icon-512.png", "image/png"),
            ("/brand/share-card.png", "image/png"),
            ("/assets/fonts/DMSans.ttf", "font/ttf"),
            ("/assets/fonts/IBMPlexMono-Regular.ttf", "font/ttf"),
            ("/manifest.webmanifest", "application/manifest+json"),
        ] {
            let res = get(&app, path, &[]).await;
            assert_eq!(res.status(), StatusCode::OK, "{path}");
            let h = res.headers();
            assert_eq!(h[header::CONTENT_TYPE], kind, "{path}");
            assert_eq!(h[header::CACHE_CONTROL], "public, max-age=86400", "{path}");
            assert_eq!(h[header::CONTENT_SECURITY_POLICY], CSP);
        }
        for path in [
            "/brand/missing.png",
            "/brand/..%2fCargo.toml",
            "/assets/fonts/Inter.woff2",
        ] {
            let res = get(&app, path, &[]).await;
            assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
        }
        let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
        assert_eq!(manifest["name"], "Singularr Requests");
        assert_eq!(manifest["background_color"], "#0e0d15");
        assert!(CSP.contains("font-src 'self'") && CSP.contains("manifest-src 'self'"));

        // The sign-in page, where a shared address leads, carries the share card at the
        // portal's own address, and the lockup.
        let res = get(&app, "/signin", &[]).await;
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(
            body.contains(
                r#"<meta property="og:image" content="https://requests.example.org/brand/share-card.png">"#
            ),
            "{body}"
        );
        assert!(body.contains("/brand/requests-dark.svg") && body.contains("Sign in with Plex"));
    }
}
