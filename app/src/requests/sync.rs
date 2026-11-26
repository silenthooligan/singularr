//! The requests loop: every request on each Seerr site, every few minutes (one loop per site).
//!
//! A pass reads the site's whole request list, newest first, in pages with a short pause
//! between them; looks up the titles it has not seen (at most a few hundred a pass, each title
//! once, for both sites); links each request to the catalog title; and writes the difference
//! with one journal event per change, in one transaction.
//!
//! A request missing from the listing is removed only when the pass is sure it read
//! everything: no unreadable entry, the site's total the same at the first and the last page,
//! and as many requests as that total. A listing far shorter than what is stored is refused.

use super::diff::{Change, RequestFacts, diff, implausible};
use super::rules;
use super::store::{self, MediaWrite};
use crate::catalog::model::TitleKind;
use crate::catalog::store::{self as catalog_store, TitleLink};
use crate::core::journal::{Journal, event};
use crate::core::outbound::OutboundError;
use crate::engines::model::DiscoverList;
use crate::engines::service::PassOk;
use crate::engines::{EngineError, RequestSite, SiteRequest};
use chrono::SubsecRound;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most pages one pass reads (50,000 requests at the default page size).
const MAX_PAGES: u32 = 500;
const PAGE_PAUSE: Duration = Duration::from_millis(200);

/// How a pass reads a site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassConfig {
    /// Requests asked for per page.
    pub page_size: u32,
    /// Titles looked up per pass. The first passes after a fresh start fill the cache over a
    /// few runs instead of asking Seerr about every title at once.
    pub lookups: usize,
}

impl Default for PassConfig {
    fn default() -> Self {
        PassConfig {
            page_size: 100,
            lookups: 250,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequestReport {
    pub listed: usize,
    pub added: usize,
    pub status_changed: usize,
    pub updated: usize,
    pub removed: usize,
    pub lookups: usize,
    pub lookups_failed: usize,
    /// The pass could not be sure it read every request, so it removed none.
    pub removals_held: bool,
    pub events: usize,
}

fn link(r: &SiteRequest, links: &[TitleLink]) -> Option<i64> {
    let kind = r.kind.as_str();
    let by = |pick: fn(&TitleLink) -> Option<i64>, id: Option<i64>| {
        id.and_then(|id| links.iter().find(|l| l.kind == kind && pick(l) == Some(id)))
    };
    by(|l| l.tmdb_id, r.tmdb_id)
        .or_else(|| by(|l| l.tvdb_id, r.tvdb_id))
        .map(|l| l.id)
}

fn facts(site: &str, r: &SiteRequest, links: &[TitleLink]) -> RequestFacts {
    RequestFacts {
        site: site.to_string(),
        seerr_id: r.seerr_id,
        kind: r.kind.as_str().to_string(),
        tmdb_id: r.tmdb_id,
        tvdb_id: r.tvdb_id,
        is_4k: r.is_4k,
        request_status: rules::request_status(r.request_status).to_string(),
        media_status: rules::media_status(r.media_status).to_string(),
        status: rules::status_word(r.request_status, r.media_status).to_string(),
        requested_by: r.requested_by.clone(),
        requested_by_id: r.requested_by_id,
        // The database keeps microseconds; a finer time would read back as a change.
        requested_at: r.created_at.trunc_subsecs(6),
        seerr_updated_at: r.updated_at.map(|t| t.trunc_subsecs(6)),
        seasons: r.seasons.clone(),
        title_id: link(r, links),
    }
}

fn body(f: &RequestFacts, title: Option<&str>) -> Value {
    json!({
        "site": f.site,
        "seerr_id": f.seerr_id,
        "kind": f.kind,
        "title": title,
        "tmdb_id": f.tmdb_id,
        "title_id": f.title_id,
        "requested_by": f.requested_by,
        "status": f.status,
        "is_4k": f.is_4k,
        "seasons": f.seasons,
    })
}

/// Look up the titles of these requests that are not in the cache yet, at most `limit`.
/// Returns the lookups made and the ones that failed.
async fn look_up(
    pool: &PgPool,
    site: &Arc<dyn RequestSite>,
    listed: &[SiteRequest],
    limit: usize,
) -> Result<(usize, usize), String> {
    let known: HashSet<(String, i64)> = store::known_media(pool)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();
    let mut wanted: Vec<(TitleKind, i64)> = Vec::new();
    for r in listed {
        if let Some(tmdb) = r.tmdb_id {
            let key = (r.kind, tmdb);
            if !known.contains(&(r.kind.as_str().to_string(), tmdb)) && !wanted.contains(&key) {
                wanted.push(key);
            }
        }
    }
    let (mut made, mut failed, mut unreachable) = (0usize, 0usize, 0usize);
    for (kind, tmdb) in wanted.into_iter().take(limit) {
        let write = match site.media(kind, tmdb).await {
            Ok(Some(m)) => {
                store::upsert_media(
                    pool,
                    kind.as_str(),
                    tmdb,
                    &MediaWrite {
                        title: Some(&m.title),
                        year: m.year,
                        poster_path: m.poster_path.as_deref(),
                        backdrop_path: m.backdrop_path.as_deref(),
                        tvdb_id: m.tvdb_id,
                        error: None,
                    },
                )
                .await
            }
            Ok(None) => {
                failed += 1;
                store::upsert_media(
                    pool,
                    kind.as_str(),
                    tmdb,
                    &MediaWrite {
                        error: Some("the site does not know this title"),
                        ..MediaWrite::default()
                    },
                )
                .await
            }
            // The site's calls pause after repeated failures: the rest waits for the next pass.
            Err(EngineError::Outbound(OutboundError::CircuitOpen { .. })) => break,
            Err(EngineError::Outbound(OutboundError::Transport { .. })) => {
                // The site stopped answering: the rest waits for the next pass.
                unreachable += 1;
                if unreachable >= 3 {
                    break;
                }
                continue;
            }
            Err(e) => {
                failed += 1;
                let message = e.to_string();
                store::upsert_media(
                    pool,
                    kind.as_str(),
                    tmdb,
                    &MediaWrite {
                        error: Some(&message),
                        ..MediaWrite::default()
                    },
                )
                .await
            }
        };
        write.map_err(|e| e.to_string())?;
        made += 1;
    }
    Ok((made, failed))
}

/// One pass over one site.
pub async fn requests_pass(
    pool: &PgPool,
    journal: &Journal,
    site: &Arc<dyn RequestSite>,
    config: PassConfig,
) -> Result<(PassOk, RequestReport), String> {
    let started = Instant::now();
    let info = site.status().await.map_err(|e| e.to_string())?;
    // The status call is small and constant: its round trip is the site's latency.
    let latency = started.elapsed();
    let count = site.count().await.map_err(|e| e.to_string())?;

    let page_size = config.page_size.max(1);
    let mut listed: Vec<SiteRequest> = Vec::new();
    let (mut unreadable, mut first_total, mut last_total) = (0usize, None, 0i64);
    let mut complete = false;
    let mut skip = 0u32;
    for page in 0..MAX_PAGES {
        if page > 0 {
            tokio::time::sleep(PAGE_PAUSE).await;
        }
        let got = site
            .requests(page_size, skip)
            .await
            .map_err(|e| e.to_string())?;
        if first_total.is_none() {
            first_total = Some(got.total);
        }
        last_total = got.total;
        let on_page = got.requests.len() + got.unreadable;
        unreadable += got.unreadable;
        listed.extend(got.requests);
        skip = skip.saturating_add(page_size);
        if on_page < page_size as usize || i64::from(skip) >= got.total {
            complete = true;
            break;
        }
    }
    // A request listed twice (a new one pushed the list down while it was read) counts once.
    let mut seen = HashSet::new();
    listed.retain(|r| seen.insert(r.seerr_id));
    let sure = complete
        && unreadable == 0
        && first_total == Some(last_total)
        && listed.len() as i64 == last_total;

    let (lookups, lookups_failed) = look_up(pool, site, &listed, config.lookups).await?;

    let tmdb_ids: Vec<i64> = listed.iter().filter_map(|r| r.tmdb_id).collect();
    let tvdb_ids: Vec<i64> = listed
        .iter()
        .filter(|r| r.kind == TitleKind::Show)
        .filter_map(|r| r.tvdb_id)
        .collect();
    let links = catalog_store::title_links(pool, &tmdb_ids, &tvdb_ids)
        .await
        .map_err(|e| e.to_string())?;
    let titles: HashMap<(String, i64), String> = store::media_titles(pool, &tmdb_ids)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter_map(|(kind, tmdb, title)| title.map(|t| ((kind, tmdb), t)))
        .collect();
    let incoming: Vec<RequestFacts> = listed
        .iter()
        .map(|r| facts(site.site(), r, &links))
        .collect();

    let mut report = apply(
        pool,
        journal,
        site.id(),
        site.site(),
        &incoming,
        sure,
        &titles,
    )
    .await
    .map_err(|e| e.to_string())?;
    report.lookups = lookups;
    report.lookups_failed = lookups_failed;
    if report.changed() {
        tracing::info!(
            site = site.id(),
            added = report.added,
            status_changed = report.status_changed,
            removed = report.removed,
            removals_held = report.removals_held,
            "requests updated"
        );
    }
    let unknown = store::unknown_titles(pool).await.unwrap_or(0);
    Ok((
        PassOk {
            took: latency,
            version: info.version,
            counts: json!({
                "requests": report.listed,
                "requests_on_site": count.total,
                "pending_on_site": count.pending,
                "unreadable": unreadable,
                "titles_looked_up": lookups,
                "titles_waiting": unknown,
            }),
        },
        report,
    ))
}

impl RequestReport {
    pub fn changed(&self) -> bool {
        self.added + self.status_changed + self.updated + self.removed > 0
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("{site} listed {listed} requests where {known} are known; the answer was not applied")]
    Implausible {
        site: String,
        known: usize,
        listed: usize,
    },
}

/// Write what one site lists now, with one journal event per new, moved or removed request.
pub async fn apply(
    pool: &PgPool,
    journal: &Journal,
    engine: &str,
    site: &str,
    incoming: &[RequestFacts],
    removals: bool,
    titles: &HashMap<(String, i64), String>,
) -> Result<RequestReport, ApplyError> {
    let actor = format!("sync:{engine}");
    let title_of = |f: &RequestFacts| {
        f.tmdb_id
            .and_then(|t| titles.get(&(f.kind.clone(), t)))
            .map(String::as_str)
    };
    let mut tx = journal.begin(pool).await?;
    let stored = store::stored(tx.conn(), site).await?;
    if implausible(stored.len(), incoming.len()) {
        return Err(ApplyError::Implausible {
            site: site.to_string(),
            known: stored.len(),
            listed: incoming.len(),
        });
    }
    let changes = diff(&stored, incoming, removals);
    let mut report = RequestReport {
        listed: incoming.len(),
        removals_held: !removals,
        ..RequestReport::default()
    };
    let mut events = Vec::new();
    for change in &changes {
        match change {
            Change::Add(i) => {
                let f = &incoming[*i];
                let id = store::insert(tx.conn(), f).await?;
                report.added += 1;
                events.push(event(
                    "request_added",
                    "request",
                    id,
                    &actor,
                    body(f, title_of(f)),
                ));
            }
            Change::Status { id, new, from } => {
                let f = &incoming[*new];
                store::update(tx.conn(), *id, f).await?;
                report.status_changed += 1;
                let mut b = body(f, title_of(f));
                b["from"] = json!(from);
                b["to"] = json!(f.status);
                events.push(event("request_status_changed", "request", id, &actor, b));
            }
            Change::Update { id, new } => {
                store::update(tx.conn(), *id, &incoming[*new]).await?;
                report.updated += 1;
            }
            Change::Remove(id) => {
                store::delete(tx.conn(), *id).await?;
                report.removed += 1;
                if let Some(old) = stored.iter().find(|s| s.id == *id) {
                    events.push(event(
                        "request_removed",
                        "request",
                        id,
                        &actor,
                        body(&old.facts, title_of(&old.facts)),
                    ));
                }
            }
        }
    }
    report.events = events.len();
    tx.append(events).await?;
    tx.commit().await?;
    Ok(report)
}

#[derive(Debug, thiserror::Error)]
pub enum UsersError {
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// What one accounts pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsersReport {
    pub read: usize,
    pub unreadable: usize,
    pub removed: usize,
    /// Every page was read and the listing is plausible, so accounts it lacks were removed.
    pub complete: bool,
}

/// Read a site's accounts into `seerr_users`: who may sign in to Singularr Requests, and which
/// Seerr account a request goes out as. Read-only toward Seerr (GET `/user`). Accounts the
/// site no longer lists are removed only when every page was read and the listing is not far
/// shorter than what is stored.
pub async fn users_pass(
    pool: &PgPool,
    site: &Arc<dyn RequestSite>,
    rules: &super::rules::Rules,
) -> Result<UsersReport, UsersError> {
    const TAKE: u32 = 100;
    let mut users = Vec::new();
    let mut unreadable = 0;
    let mut total = 0;
    let mut complete = false;
    for page in 0..50u32 {
        let got = site.users(TAKE, page * TAKE).await?;
        total = got.total;
        unreadable += got.unreadable;
        let n = got.users.len() + got.unreadable;
        users.extend(got.users);
        if n < TAKE as usize || (users.len() + unreadable) as i64 >= total {
            complete = true;
            break;
        }
    }
    let known = store::seerr_user_count(pool, site.site()).await?;
    let plausible = !(known >= 20 && (users.len() as i64) * 2 < known);
    let complete = complete && unreadable == 0 && users.len() as i64 == total && plausible;
    let rows: Vec<store::SeerrUserWrite> = users
        .into_iter()
        .map(|u| store::SeerrUserWrite {
            is_owner: rules.is_owner(Some(u.seerr_user_id)),
            seerr_user_id: u.seerr_user_id,
            display_name: u.display_name,
            plex_id: u.plex_id,
            plex_username: u.plex_username,
            user_type: u.user_type,
        })
        .collect();
    let mut tx = pool.begin().await?;
    let (read, removed) = store::write_seerr_users(&mut tx, site.site(), &rows, complete).await?;
    tx.commit().await?;
    Ok(UsersReport {
        read,
        unreadable,
        removed,
        complete,
    })
}

/// Read the four discover lists (first page each) into `discover_items`, so the face can show
/// them without a Seerr key. A list Seerr does not give keeps its stored copy.
pub async fn discover_pass(pool: &PgPool, site: &Arc<dyn RequestSite>) -> usize {
    let mut stored = 0;
    for list in DiscoverList::ALL {
        let page = match site.discover(list, 1).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(site = site.id(), list = list.as_str(), error = %e, "a discover list was not stored");
                continue;
            }
        };
        let items: Vec<store::DiscoverWrite> = page
            .entries
            .into_iter()
            .map(|e| store::DiscoverWrite {
                kind: e.kind.as_str().to_string(),
                tmdb_id: e.tmdb_id,
                title: e.title,
                year: e.year,
                overview: e.overview,
                rating: e.rating,
                poster_path: e.poster_path,
                backdrop_path: e.backdrop_path,
            })
            .collect();
        let written = async {
            let mut tx = pool.begin().await?;
            store::replace_discover(&mut tx, list.as_str(), &items).await?;
            tx.commit().await
        }
        .await;
        match written {
            Ok(()) => stored += 1,
            Err(e) => {
                tracing::warn!(list = list.as_str(), error = %e, "a discover list was not stored")
            }
        }
    }
    stored
}
