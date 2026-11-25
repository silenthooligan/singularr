//! The discover lists: trending, popular movies, popular shows and upcoming, from the Seerr
//! integration (which reads them from TMDB), kept for
//! 30 minutes, each title marked with what Singularr's own catalog, downloads and requests say
//! about it. The marks are worked out for every answer, so they are never older than the
//! catalog.

use super::model::{DiscoverItem, DiscoverQuery, DiscoverResponse, DiscoverRow};
use super::store;
use crate::catalog::store as catalog_store;
use crate::core::error::{AppError, AppResult};
use crate::engines::model::DiscoverList;
use crate::engines::seerr::{BACKDROP_SIZE, POSTER_SIZE, image_url};
use crate::engines::{DiscoverPage, RequestSite};
use crate::lifecycle::store as downloads_store;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a fetched list is served before Seerr is asked again.
pub const TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Clone)]
struct Cached {
    at: Instant,
    fetched_at: DateTime<Utc>,
    page: DiscoverPage,
}

/// The fetched lists, by list and page.
#[derive(Default)]
pub struct DiscoverCache {
    entries: Mutex<HashMap<(DiscoverList, u32), Cached>>,
}

impl DiscoverCache {
    fn get(&self, key: (DiscoverList, u32)) -> Option<Cached> {
        self.entries.lock().unwrap().get(&key).cloned()
    }

    fn put(&self, key: (DiscoverList, u32), page: DiscoverPage) -> Cached {
        let cached = Cached {
            at: Instant::now(),
            fetched_at: Utc::now(),
            page,
        };
        self.entries.lock().unwrap().insert(key, cached.clone());
        cached
    }
}

/// A list as it can be served: fetched now or within the last 30 minutes, an older copy when
/// Seerr does not answer (stale), or nothing with the reason.
struct Fetched {
    list: DiscoverList,
    cached: Option<Cached>,
    error: Option<String>,
    stale: bool,
}

async fn fetch(
    site: Option<&Arc<dyn RequestSite>>,
    cache: &DiscoverCache,
    list: DiscoverList,
    page: u32,
) -> Fetched {
    let key = (list, page);
    let held = cache.get(key);
    if let Some(c) = held.as_ref().filter(|c| c.at.elapsed() < TTL) {
        return Fetched {
            list,
            cached: Some(c.clone()),
            error: None,
            stale: false,
        };
    }
    let Some(site) = site else {
        return Fetched {
            list,
            cached: None,
            error: Some("No Seerr integration is configured (SEERR_URL and SEERR_API_KEY).".into()),
            stale: false,
        };
    };
    match site.discover(list, page).await {
        Ok(fresh) => Fetched {
            list,
            cached: Some(cache.put(key, fresh)),
            error: None,
            stale: false,
        },
        Err(e) => {
            tracing::warn!(site = site.id(), list = list.as_str(), error = %e, "a discover list could not be read");
            Fetched {
                list,
                stale: held.is_some(),
                cached: held,
                error: Some(e.to_string()),
            }
        }
    }
}

/// What Singularr holds of each title, by kind and TMDB id: the library status, the catalog
/// title and the newest open request.
type Marks = HashMap<(String, i64), (&'static str, Option<i64>, Option<i64>)>;

/// The library status of titles by TMDB id, from the catalog, the downloads and the requests.
///
/// - `in_library`: the catalog holds a copy.
/// - `downloading`: the catalog knows the title and an arr's download for it is in progress.
/// - `requested`: an open request, or an arr wants the title and holds no copy.
/// - `none`: nothing.
pub async fn library_status(pool: &PgPool, keys: &[(String, i64)]) -> Result<Marks, sqlx::Error> {
    let tmdb_ids: Vec<i64> = keys.iter().map(|(_, id)| *id).collect();
    let links = catalog_store::title_links(pool, &tmdb_ids, &[]).await?;
    let ids: Vec<i64> = links.iter().map(|l| l.id).collect();
    let downloading = downloads_store::downloading_titles(pool, &ids).await?;
    let open = store::open_requests(pool, &tmdb_ids).await?;
    let mut marks = Marks::new();
    for (kind, tmdb) in keys {
        let link = links
            .iter()
            .find(|l| l.kind == *kind && l.tmdb_id == Some(*tmdb));
        let request = open
            .iter()
            .find(|(k, t, _)| k == kind && t == tmdb)
            .map(|(_, _, id)| *id);
        let status = match link {
            Some(l) if l.copies > 0 => "in_library",
            Some(l) if downloading.contains(&l.id) => "downloading",
            Some(_) => "requested",
            None if request.is_some() => "requested",
            None => "none",
        };
        marks.insert((kind.clone(), *tmdb), (status, link.map(|l| l.id), request));
    }
    Ok(marks)
}

pub async fn discover(
    pool: &PgPool,
    sites: &[Arc<dyn RequestSite>],
    cache: &DiscoverCache,
    q: &DiscoverQuery,
) -> AppResult<DiscoverResponse> {
    let lists: Vec<DiscoverList> = match q.list.as_deref().map(str::trim).filter(|l| !l.is_empty())
    {
        Some(name) => vec![DiscoverList::parse(name).ok_or_else(|| {
            AppError::bad_request(format!(
                "list is one of trending, popular_movies, popular_shows, upcoming_movies, not `{name}`"
            ))
        })?],
        None => DiscoverList::ALL.to_vec(),
    };
    let page = q.page.unwrap_or(1).clamp(1, 500);
    let site = sites.first();
    let fetched =
        futures::future::join_all(lists.iter().map(|l| fetch(site, cache, *l, page))).await;

    let mut keys: Vec<(String, i64)> = fetched
        .iter()
        .filter_map(|f| f.cached.as_ref())
        .flat_map(|c| c.page.entries.iter())
        .map(|e| (e.kind.as_str().to_string(), e.tmdb_id))
        .collect();
    keys.sort();
    keys.dedup();
    let marks = library_status(pool, &keys).await?;

    let rows = fetched
        .into_iter()
        .map(|f| {
            let items: Vec<DiscoverItem> = f
                .cached
                .as_ref()
                .map(|c| {
                    c.page
                        .entries
                        .iter()
                        .map(|e| {
                            let kind = e.kind.as_str().to_string();
                            let (status, title_id, request_id) = marks
                                .get(&(kind.clone(), e.tmdb_id))
                                .copied()
                                .unwrap_or(("none", None, None));
                            DiscoverItem {
                                kind,
                                tmdb_id: e.tmdb_id,
                                title: e.title.clone(),
                                year: e.year,
                                overview: e.overview.clone(),
                                rating: e.rating,
                                poster_url: image_url(e.poster_path.as_deref(), POSTER_SIZE),
                                backdrop_url: image_url(e.backdrop_path.as_deref(), BACKDROP_SIZE),
                                library_status: status.to_string(),
                                title_id,
                                request_id,
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            DiscoverRow {
                list: f.list,
                label: f.list.label().to_string(),
                total: f.cached.as_ref().map(|c| c.page.total_results).unwrap_or(0),
                page: f.cached.as_ref().map(|c| c.page.page).unwrap_or(page),
                total_pages: f.cached.as_ref().map(|c| c.page.total_pages).unwrap_or(0),
                fetched_at: f.cached.as_ref().map(|c| c.fetched_at),
                error: f.error,
                stale: f.stale,
                items,
            }
        })
        .collect();
    Ok(DiscoverResponse {
        site: site.map(|s| s.site().to_string()),
        lists: rows,
    })
}
