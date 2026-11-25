//! The Seerr adapter (API v1), read-only, for a Seerr integration.
//!
//! Endpoints used, all GET, with the key in `X-Api-Key`: `/status`, `/request/count`,
//! `/request?take=&skip=&sort=added&filter=all`, `/user?take=&skip=&sort=created`,
//! `/movie/{tmdbId}`, `/tv/{tmdbId}`, `/discover/trending`, `/discover/movies`,
//! `/discover/tv` and `/discover/movies/upcoming`.
//!
//! The requester is read as a display name only. Seerr's own `displayName` falls back to the
//! account's email address when the account has no user name, so a name that holds an `@` is
//! never taken; such an account is named `user <id>`. The types here have no field for an
//! address, so none can be kept by accident.
//!
//! Answers are read leniently: a field of an unexpected shape is no value. A request without
//! an id, a kind or a date is counted as unreadable instead of failing the page.

use super::arr::{lenient, null_as_default};
use super::model::DiscoverList;
use super::{
    DiscoverEntry, DiscoverPage, EngineError, EngineInfo, RequestSite, SiteMedia, SiteRequest,
    SiteRequestCount, SiteRequestPage, SiteUser, SiteUserPage,
};
use crate::catalog::model::TitleKind;
use crate::core::config::SeerrConfig;
use crate::core::outbound::{OutRequest, OutResponse, Outbound, expect_ok};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;

pub const API: &str = "/api/v1";
/// The kind of engine a Seerr site is, in the engine list.
pub const KIND: &str = "seerr";
pub use super::model::SITE;
/// TMDB's image host, with the two sizes Singularr shows.
pub const TMDB_IMAGES: &str = "https://image.tmdb.org/t/p/";
pub const POSTER_SIZE: &str = "w342";
pub const BACKDROP_SIZE: &str = "w1280";

pub struct Seerr {
    id: String,
    base: String,
    api_key: String,
    out: Outbound,
}

impl Seerr {
    /// `None` when the site has no key.
    pub fn new(config: &SeerrConfig, out: Outbound) -> Option<Self> {
        Some(Seerr {
            id: config.id.clone(),
            base: config.url.clone(),
            api_key: config.api_key.clone()?,
            out,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{API}{path}", self.base)
    }

    async fn fetch(&self, path: &str) -> Result<OutResponse, EngineError> {
        let req = OutRequest::get(self.url(path)).header("X-Api-Key", self.api_key.clone());
        Ok(self.out.send(req).await?)
    }

    fn decode<T: DeserializeOwned>(
        &self,
        what: &'static str,
        body: &[u8],
    ) -> Result<T, EngineError> {
        serde_json::from_slice(body).map_err(|e| EngineError::Decode {
            engine: self.id.to_string(),
            what,
            message: e.to_string(),
        })
    }

    async fn get<T: DeserializeOwned>(
        &self,
        what: &'static str,
        path: &str,
    ) -> Result<T, EngineError> {
        let res = self.fetch(path).await?;
        expect_ok(&self.url(path), &res)?;
        self.decode(what, &res.body)
    }
}

/// The full URL of a TMDB image path in one size.
pub fn image_url(path: Option<&str>, size: &str) -> Option<String> {
    path.map(|p| format!("{TMDB_IMAGES}{size}{p}"))
}

/// A TMDB image path as Seerr or TMDB gives it (`/abc123.jpg`), or nothing. Anything else is
/// dropped, so a URL built from it always points at TMDB's image host.
pub fn tmdb_path(path: Option<String>) -> Option<String> {
    let p = path?.trim().to_string();
    let ok = p.len() > 1
        && p.len() <= 200
        && p.starts_with('/')
        && !p.contains("..")
        && p[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    ok.then_some(p)
}

/// The year of a date Seerr gives as text (`2004-05-31`).
fn year_of(date: Option<&str>) -> Option<i32> {
    let year: i32 = date?.trim().get(..4)?.parse().ok()?;
    (1850..=2200).contains(&year).then_some(year)
}

fn kind_of(media_type: Option<&str>) -> Option<TitleKind> {
    match media_type?.trim() {
        "movie" => Some(TitleKind::Movie),
        "tv" => Some(TitleKind::Show),
        _ => None,
    }
}

// What Seerr sends. Every field is optional: a field a Seerr version does not send must not
// fail the page.

#[derive(Deserialize)]
struct RawStatus {
    #[serde(default, deserialize_with = "lenient")]
    version: Option<String>,
}

#[derive(Deserialize)]
struct RawCount {
    #[serde(default, deserialize_with = "lenient")]
    total: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    movie: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    tv: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    pending: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    approved: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    declined: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    processing: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    available: Option<i64>,
}

#[derive(Deserialize)]
struct RawPageInfo {
    #[serde(default, deserialize_with = "lenient")]
    results: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequestPage {
    #[serde(default, deserialize_with = "lenient")]
    page_info: Option<RawPageInfo>,
    #[serde(default, deserialize_with = "null_as_default")]
    results: Vec<serde_json::Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawMedia {
    #[serde(default, deserialize_with = "lenient")]
    tmdb_id: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    tvdb_id: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    status: Option<i32>,
    #[serde(default, deserialize_with = "lenient")]
    status4k: Option<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawSeason {
    #[serde(default, deserialize_with = "lenient")]
    season_number: Option<i32>,
}

/// The requesting account. No address field: Seerr sends `email`, and it is not read.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawUser {
    #[serde(default, deserialize_with = "lenient")]
    id: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    display_name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    username: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    plex_username: Option<String>,
    #[serde(default, deserialize_with = "number_or_text")]
    plex_id: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    user_type: Option<i32>,
}

/// A number Seerr may send as a number or as text (the Plex id has been both).
fn number_or_text<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    })
}

/// One account as Singularr keeps it, or `None` without an id.
fn site_user(raw: RawUser) -> Option<SiteUser> {
    let seerr_user_id = raw.id.filter(|n| *n > 0)?;
    let clean = |v: &Option<String>| {
        v.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.contains('@'))
            .map(|s| s.chars().take(80).collect::<String>())
    };
    Some(SiteUser {
        seerr_user_id,
        display_name: display_name(&raw),
        plex_id: raw.plex_id.filter(|n| *n > 0),
        plex_username: clean(&raw.plex_username),
        user_type: raw.user_type,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequest {
    #[serde(default, deserialize_with = "lenient")]
    id: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    status: Option<i32>,
    #[serde(rename = "type", default, deserialize_with = "lenient")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    is4k: Option<bool>,
    #[serde(default, deserialize_with = "lenient")]
    created_at: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "lenient")]
    updated_at: Option<DateTime<Utc>>,
    #[serde(default, deserialize_with = "lenient")]
    media: Option<RawMedia>,
    #[serde(default, deserialize_with = "lenient")]
    seasons: Option<Vec<RawSeason>>,
    #[serde(default, deserialize_with = "lenient")]
    requested_by: Option<RawUser>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawExternalIds {
    #[serde(default, deserialize_with = "lenient")]
    tvdb_id: Option<i64>,
}

/// A movie or a show: the title lookups and the discover lists share this shape.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawTitle {
    #[serde(default, deserialize_with = "lenient")]
    id: Option<i64>,
    #[serde(default, deserialize_with = "lenient")]
    media_type: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    title: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    name: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    release_date: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    first_air_date: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    overview: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    vote_average: Option<f64>,
    #[serde(default, deserialize_with = "lenient")]
    poster_path: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    backdrop_path: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    external_ids: Option<RawExternalIds>,
}

impl RawTitle {
    fn name(&self) -> Option<String> {
        self.title
            .as_deref()
            .or(self.name.as_deref())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    }

    fn year(&self) -> Option<i32> {
        year_of(
            self.release_date
                .as_deref()
                .or(self.first_air_date.as_deref()),
        )
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDiscover {
    #[serde(default, deserialize_with = "lenient")]
    page: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    total_pages: Option<u32>,
    #[serde(default, deserialize_with = "lenient")]
    total_results: Option<i64>,
    #[serde(default, deserialize_with = "null_as_default")]
    results: Vec<serde_json::Value>,
}

/// The name a request shows for its requester. Seerr's display name first, then the account's
/// user names; a value with an `@` is an address and is never taken. Without a usable name the
/// account is `user <id>`.
fn display_name(user: &RawUser) -> String {
    let name = [&user.display_name, &user.username, &user.plex_username]
        .into_iter()
        .flatten()
        .map(|n| n.trim())
        .find(|n| !n.is_empty() && !n.contains('@'))
        .map(|n| n.chars().take(80).collect::<String>());
    match (name, user.id) {
        (Some(name), _) => name,
        (None, Some(id)) => format!("user {id}"),
        (None, None) => "unknown".to_string(),
    }
}

/// One request as Singularr keeps it, or `None` when it cannot be read.
fn site_request(raw: RawRequest) -> Option<SiteRequest> {
    let kind = kind_of(raw.kind.as_deref())?;
    let created_at = raw.created_at.or(raw.updated_at)?;
    let is_4k = raw.is4k.unwrap_or(false);
    let media = raw.media.as_ref();
    let media_status = media
        .and_then(|m| if is_4k { m.status4k } else { m.status })
        .unwrap_or(1);
    let user = raw.requested_by.unwrap_or_default();
    let mut seasons: Vec<i32> = raw
        .seasons
        .unwrap_or_default()
        .into_iter()
        .filter_map(|s| s.season_number)
        .collect();
    seasons.sort_unstable();
    seasons.dedup();
    Some(SiteRequest {
        seerr_id: raw.id?,
        kind,
        is_4k,
        request_status: raw.status.unwrap_or(0),
        media_status,
        tmdb_id: media.and_then(|m| m.tmdb_id).filter(|n| *n > 0),
        tvdb_id: media.and_then(|m| m.tvdb_id).filter(|n| *n > 0),
        requested_by: display_name(&user),
        requested_by_id: user.id,
        created_at,
        updated_at: raw.updated_at,
        seasons: if kind == TitleKind::Show {
            seasons
        } else {
            Vec::new()
        },
    })
}

fn discover_path(list: DiscoverList, page: u32) -> String {
    let base = match list {
        DiscoverList::Trending => "/discover/trending",
        DiscoverList::PopularMovies => "/discover/movies",
        DiscoverList::PopularShows => "/discover/tv",
        DiscoverList::UpcomingMovies => "/discover/movies/upcoming",
    };
    format!("{base}?page={page}")
}

/// The kind a list's entries have when an entry does not say. Trending mixes kinds, and
/// people, so an entry there must say.
fn list_kind(list: DiscoverList) -> Option<TitleKind> {
    match list {
        DiscoverList::Trending => None,
        DiscoverList::PopularMovies | DiscoverList::UpcomingMovies => Some(TitleKind::Movie),
        DiscoverList::PopularShows => Some(TitleKind::Show),
    }
}

fn discover_entry(list: DiscoverList, raw: RawTitle) -> Option<DiscoverEntry> {
    // People and collections in the trending list are not titles.
    let kind = match raw.media_type.as_deref() {
        Some(t) => kind_of(Some(t))?,
        None => list_kind(list)?,
    };
    Some(DiscoverEntry {
        kind,
        tmdb_id: raw.id.filter(|n| *n > 0)?,
        title: raw.name()?,
        year: raw.year(),
        overview: raw
            .overview
            .as_deref()
            .map(str::trim)
            .filter(|o| !o.is_empty())
            .map(str::to_string),
        rating: raw
            .vote_average
            .filter(|v| v.is_finite() && (0.0..=10.0).contains(v))
            .map(|v| (v * 10.0).round() / 10.0),
        poster_path: tmdb_path(raw.poster_path),
        backdrop_path: tmdb_path(raw.backdrop_path),
    })
}

#[async_trait]
impl RequestSite for Seerr {
    fn id(&self) -> &str {
        &self.id
    }

    fn site(&self) -> &str {
        SITE
    }

    async fn status(&self) -> Result<EngineInfo, EngineError> {
        let raw: RawStatus = self.get("status", "/status").await?;
        Ok(EngineInfo {
            app: Some("Seerr".to_string()),
            version: raw.version,
        })
    }

    async fn count(&self) -> Result<SiteRequestCount, EngineError> {
        let raw: RawCount = self.get("request count", "/request/count").await?;
        Ok(SiteRequestCount {
            total: raw.total.unwrap_or(0),
            movie: raw.movie,
            tv: raw.tv,
            pending: raw.pending,
            approved: raw.approved,
            declined: raw.declined,
            processing: raw.processing,
            available: raw.available,
        })
    }

    async fn requests(&self, take: u32, skip: u32) -> Result<SiteRequestPage, EngineError> {
        let raw: RawRequestPage = self
            .get(
                "requests",
                &format!("/request?take={take}&skip={skip}&sort=added&filter=all"),
            )
            .await?;
        let listed = raw.results.len();
        let requests: Vec<SiteRequest> = raw
            .results
            .into_iter()
            .filter_map(|v| serde_json::from_value::<RawRequest>(v).ok())
            .filter_map(site_request)
            .collect();
        Ok(SiteRequestPage {
            unreadable: listed - requests.len(),
            total: raw
                .page_info
                .and_then(|p| p.results)
                .unwrap_or(requests.len() as i64),
            requests,
        })
    }

    async fn users(&self, take: u32, skip: u32) -> Result<SiteUserPage, EngineError> {
        let raw: RawRequestPage = self
            .get(
                "users",
                &format!("/user?take={take}&skip={skip}&sort=created"),
            )
            .await?;
        let listed = raw.results.len();
        let users: Vec<SiteUser> = raw
            .results
            .into_iter()
            .filter_map(|v| serde_json::from_value::<RawUser>(v).ok())
            .filter_map(site_user)
            .collect();
        Ok(SiteUserPage {
            unreadable: listed - users.len(),
            total: raw
                .page_info
                .and_then(|p| p.results)
                .unwrap_or(users.len() as i64),
            users,
        })
    }

    async fn media(&self, kind: TitleKind, tmdb_id: i64) -> Result<Option<SiteMedia>, EngineError> {
        let path = match kind {
            TitleKind::Movie => format!("/movie/{tmdb_id}"),
            TitleKind::Show => format!("/tv/{tmdb_id}"),
        };
        let res = self.fetch(&path).await?;
        if res.status == 404 {
            return Ok(None);
        }
        expect_ok(&self.url(&path), &res)?;
        let raw: RawTitle = self.decode("title", &res.body)?;
        let Some(title) = raw.name() else {
            return Ok(None);
        };
        Ok(Some(SiteMedia {
            year: raw.year(),
            tvdb_id: raw
                .external_ids
                .as_ref()
                .and_then(|e| e.tvdb_id)
                .filter(|n| *n > 0),
            poster_path: tmdb_path(raw.poster_path),
            backdrop_path: tmdb_path(raw.backdrop_path),
            title,
        }))
    }

    async fn discover(&self, list: DiscoverList, page: u32) -> Result<DiscoverPage, EngineError> {
        let page = page.max(1);
        let raw: RawDiscover = self.get("discover", &discover_path(list, page)).await?;
        let entries = raw
            .results
            .into_iter()
            .filter_map(|v| serde_json::from_value::<RawTitle>(v).ok())
            .filter_map(|t| discover_entry(list, t))
            .collect();
        Ok(DiscoverPage {
            page: raw.page.unwrap_or(page),
            total_pages: raw.total_pages.unwrap_or(1),
            total_results: raw.total_results.unwrap_or(0),
            entries,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Body, Method, OutboundError};
    use std::sync::Arc;

    const BASE: &str = "http://192.0.2.62:5055";

    fn allow() -> Allowlist {
        let mut allow = Allowlist::default();
        allow.allow_engine("seerr", BASE, &[]).unwrap();
        allow
    }

    fn seerr(recorded: Arc<Recorded>) -> Seerr {
        let config = SeerrConfig {
            id: "seerr".to_string(),
            url: BASE.to_string(),
            api_key: Some("test-key".to_string()),
        };
        Seerr::new(&config, Outbound::with_transport(allow(), recorded)).unwrap()
    }

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    #[tokio::test]
    async fn accounts_are_read_without_addresses() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v1/user?take=50&skip=0",
            200,
            &fixture("seerr_users.json"),
        );
        let page = seerr(recorded.clone()).users(50, 0).await.unwrap();
        assert_eq!((page.total, page.unreadable), (4, 1));
        assert_eq!(page.users.len(), 3);
        let owner = &page.users[0];
        assert_eq!(
            (
                owner.seerr_user_id,
                owner.display_name.as_str(),
                owner.plex_id
            ),
            (1, "Owner Account", Some(1_000_001))
        );
        // A friend whose display name is an address is named by a user name instead.
        let friend = &page.users[1];
        assert_eq!(friend.display_name, "brightfriend");
        assert_eq!(friend.plex_username.as_deref(), Some("brightfriend"));
        assert_eq!(
            friend.plex_id,
            Some(1_000_007),
            "an id sent as text reads too"
        );
        assert_eq!(friend.user_type, Some(1));
        // A local account has no Plex id.
        assert_eq!(page.users[2].plex_id, None);
        let debug = format!("{:?}", page.users);
        assert!(!debug.contains('@'), "no address is kept: {debug}");
        assert_eq!(recorded.methods_seen(), vec![Method::Get]);
    }

    /// The safety guarantee toward Seerr: approving, declining, requesting and clearing are
    /// refused before anything is sent.
    #[tokio::test]
    async fn nothing_but_get_reaches_a_seerr_site() {
        let allow = allow();
        for (method, path) in [
            (Method::Post, "/api/v1/request"),
            (Method::Post, "/api/v1/request/2031/approve"),
            (Method::Post, "/api/v1/request/2031/decline"),
            (Method::Put, "/api/v1/request/2031"),
            (Method::Delete, "/api/v1/request/2031"),
            (Method::Delete, "/api/v1/media/77"),
            (Method::Post, "/api/v1/media/77/available"),
        ] {
            let err = allow.check(method, &format!("{BASE}{path}")).unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{} {path} must be refused, got {err:?}",
                method.as_str()
            );
        }
        let recorded = Recorded::new();
        let out = Outbound::with_transport(allow, recorded.clone());
        let err = out
            .send(OutRequest::post(
                format!("{BASE}/api/v1/request/2031/approve"),
                Body::None,
            ))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a listed call"));
        assert!(recorded.seen.lock().unwrap().is_empty(), "nothing was sent");
    }

    #[tokio::test]
    async fn requests_are_read_with_display_names_only() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v1/request?take=100&skip=0",
            200,
            &fixture("seerr_requests.json"),
        );
        let s = seerr(recorded.clone());
        let page = s.requests(100, 0).await.unwrap();
        assert_eq!(page.total, 8);
        assert_eq!(page.requests.len(), 8);
        assert_eq!(page.unreadable, 0);
        let first = &page.requests[0];
        assert_eq!(
            (first.seerr_id, first.kind, first.request_status),
            (2031, TitleKind::Show, 1)
        );
        assert_eq!(first.seasons, vec![1, 2, 3, 4]);
        assert_eq!(first.requested_by, "maple.reed");
        assert_eq!(first.tvdb_id, Some(253463));
        // A 4K request carries the 4K tier's status, not the HD copy's.
        let uhd = &page.requests[1];
        assert!(uhd.is_4k);
        assert_eq!(uhd.media_status, 1);
        assert!(uhd.seasons.is_empty());
        // An account known only by its address is named by its id.
        let anonymous = page.requests.iter().find(|r| r.seerr_id == 1970).unwrap();
        assert_eq!(anonymous.requested_by, "user 9");
        let all = format!("{:?}", page.requests);
        assert!(!all.contains('@'), "no address is read: {all}");
        assert_eq!(recorded.methods_seen(), vec![Method::Get]);
        let urls = recorded.urls_seen();
        assert!(urls[0].contains("sort=added") && urls[0].contains("filter=all"));
    }

    #[tokio::test]
    async fn an_unreadable_request_is_counted_not_fatal() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v1/request?",
            200,
            r#"{"pageInfo":{"results":3},"results":[
                {"id":1,"type":"movie","status":2,"createdAt":"2026-11-01T10:00:00.000Z","media":{"tmdbId":5,"status":3}},
                {"type":"movie","status":2},
                {"id":3,"type":"music","status":2,"createdAt":"2026-11-01T10:00:00.000Z"}
            ]}"#,
        );
        let page = seerr(recorded).requests(100, 0).await.unwrap();
        assert_eq!(
            (page.requests.len(), page.unreadable, page.total),
            (1, 2, 3)
        );
        assert_eq!(page.requests[0].requested_by, "unknown");
    }

    #[tokio::test]
    async fn status_count_lookups_and_discover() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            "/api/v1/status",
            200,
            &fixture("seerr_status.json"),
        );
        recorded.answer(
            Method::Get,
            "/api/v1/request/count",
            200,
            &fixture("seerr_request_count.json"),
        );
        let media: serde_json::Value = serde_json::from_str(&fixture("seerr_media.json")).unwrap();
        for (path, body) in media.as_object().unwrap() {
            recorded.answer(
                Method::Get,
                &format!("/api/v1/{path}"),
                200,
                &body.to_string(),
            );
        }
        recorded.answer(Method::Get, "/api/v1/movie/424242", 404, "{}");
        let discover: serde_json::Value =
            serde_json::from_str(&fixture("seerr_discover.json")).unwrap();
        for (path, body) in discover.as_object().unwrap() {
            recorded.answer(
                Method::Get,
                &format!("/api/v1/{path}"),
                200,
                &body.to_string(),
            );
        }
        let s = seerr(recorded.clone());
        assert_eq!(s.status().await.unwrap().version.as_deref(), Some("2.7.3"));
        let count = s.count().await.unwrap();
        assert_eq!((count.total, count.pending), (8, Some(2)));

        let movie = s.media(TitleKind::Movie, 687163).await.unwrap().unwrap();
        assert_eq!(movie.title, "Project Hail Mary");
        assert_eq!(movie.year, Some(2026));
        assert_eq!(movie.poster_path.as_deref(), Some("/phm-poster.jpg"));
        let show = s.media(TitleKind::Show, 42009).await.unwrap().unwrap();
        assert_eq!(
            (show.title.as_str(), show.year),
            ("Black Mirror", Some(2011))
        );
        assert_eq!(show.tvdb_id, Some(253463));
        assert_eq!(s.media(TitleKind::Movie, 424242).await.unwrap(), None);

        let trending = s.discover(DiscoverList::Trending, 1).await.unwrap();
        assert_eq!(trending.total_results, 10000);
        assert!(
            trending.entries.iter().all(|e| e.title != "A Person"),
            "people are not titles"
        );
        assert!(trending.entries.iter().any(|e| e.kind == TitleKind::Show));
        let shows = s.discover(DiscoverList::PopularShows, 1).await.unwrap();
        assert!(shows.entries.iter().all(|e| e.kind == TitleKind::Show));
        let upcoming = s.discover(DiscoverList::UpcomingMovies, 1).await.unwrap();
        assert!(!upcoming.entries.is_empty());
        assert!(
            recorded.methods_seen().iter().all(|m| *m == Method::Get),
            "only GET was sent"
        );
        assert!(
            recorded
                .urls_seen()
                .iter()
                .any(|u| u.ends_with("/api/v1/discover/movies/upcoming?page=1"))
        );
    }

    #[test]
    fn names_never_take_an_address() {
        let user = |display: Option<&str>, plex: Option<&str>, id: Option<i64>| RawUser {
            id,
            display_name: display.map(str::to_string),
            username: None,
            plex_username: plex.map(str::to_string),
            ..RawUser::default()
        };
        assert_eq!(
            display_name(&user(Some("homeowner"), None, Some(1))),
            "homeowner"
        );
        assert_eq!(
            display_name(&user(Some("friend@example.org"), Some("friend"), Some(3))),
            "friend"
        );
        assert_eq!(
            display_name(&user(Some("friend@example.org"), None, Some(3))),
            "user 3"
        );
        assert_eq!(display_name(&user(Some("  "), None, None)), "unknown");
    }

    #[test]
    fn image_paths_stay_on_tmdb() {
        assert_eq!(
            tmdb_path(Some("/abc123.jpg".into())).as_deref(),
            Some("/abc123.jpg")
        );
        for bad in [
            "https://evil.example/x.jpg",
            "/../x.jpg",
            "abc.jpg",
            "/a b.jpg",
            "/",
        ] {
            assert_eq!(tmdb_path(Some(bad.into())), None, "{bad}");
        }
        assert_eq!(
            image_url(Some("/p.jpg"), POSTER_SIZE).as_deref(),
            Some("https://image.tmdb.org/t/p/w342/p.jpg")
        );
        assert_eq!(year_of(Some("2004-05-31")), Some(2004));
        assert_eq!(year_of(Some("")), None);
        assert_eq!(year_of(Some("0000-00-00")), None);
    }
}
