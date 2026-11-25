//! What the API and the web app ask about requests.

use super::diff::{RequestFacts, StoredRequest};
use super::discover::DiscoverCache;
use super::model::{RelatedRequest, Request, RequestDetail, RequestPage, RequestQuery, Requester};
use super::rules::{self, RuleInput, Rules};
use super::store::{self, Filter, RequestRow};
use crate::core::config::RequestRulesConfig;
use crate::core::error::{AppError, AppResult};
use crate::core::model::Paging;
use crate::engines::seerr::{BACKDROP_SIZE, POSTER_SIZE, SITE, image_url};
use sqlx::PgPool;

/// What the request routes need beyond the database: the rules and the discover cache.
pub struct Requests {
    pub rules: Rules,
    pub discover: DiscoverCache,
}

impl Requests {
    pub fn new(config: &RequestRulesConfig) -> Self {
        Requests {
            rules: Rules {
                owners: config.owners.clone(),
                max_seasons: config.max_seasons,
                season_gate: config.season_gate,
            },
            discover: DiscoverCache::default(),
        }
    }
}

/// What the rules read of a stored request.
pub fn rule_input(f: &RequestFacts) -> RuleInput<'_> {
    RuleInput {
        seerr_id: f.seerr_id,
        site: &f.site,
        kind: &f.kind,
        is_4k: f.is_4k,
        request_status: &f.request_status,
        media_status: &f.media_status,
        requested_by: &f.requested_by,
        requested_by_id: f.requested_by_id,
        tmdb_id: f.tmdb_id,
        created_at: f.requested_at,
        seasons: &f.seasons,
    }
}

fn view(row: RequestRow, rules: &Rules, others: &[RuleInput<'_>]) -> Request {
    let f = &row.facts;
    let me = rule_input(f);
    let gate = rules::season_gate(rules, &me, others);
    let cleanup = rules::cleanup(rules, &me);
    let owner = rules.is_owner(f.requested_by_id);
    let f = row.facts;
    Request {
        id: row.id,
        site: f.site,
        seerr_id: f.seerr_id,
        kind: f.kind,
        tmdb_id: f.tmdb_id,
        tvdb_id: f.tvdb_id,
        is_4k: f.is_4k,
        status: f.status,
        request_status: f.request_status,
        media_status: f.media_status,
        requested_by: f.requested_by,
        owner,
        created_at: f.requested_at,
        updated_at: f.seerr_updated_at,
        seasons: f.seasons,
        title: row.title,
        year: row.year,
        poster_url: image_url(row.poster_path.as_deref(), POSTER_SIZE),
        backdrop_url: image_url(row.backdrop_path.as_deref(), BACKDROP_SIZE),
        title_id: f.title_id,
        gate,
        cleanup,
        first_seen_at: row.first_seen_at,
        synced_at: row.updated_at,
    }
}

/// The views of these rows, each with the requests the season gate counts for it.
async fn views(pool: &PgPool, rules: &Rules, rows: Vec<RequestRow>) -> AppResult<Vec<Request>> {
    let mut shows: Vec<i64> = rows
        .iter()
        .filter(|r| r.facts.kind == "show")
        .filter_map(|r| r.facts.tmdb_id)
        .collect();
    shows.sort_unstable();
    shows.dedup();
    let siblings: Vec<StoredRequest> = store::show_requests(pool, &shows).await?;
    let others: Vec<RuleInput<'_>> = siblings.iter().map(|s| rule_input(&s.facts)).collect();
    Ok(rows.into_iter().map(|r| view(r, rules, &others)).collect())
}

fn filter(q: &RequestQuery) -> AppResult<Filter> {
    let mut statuses = Vec::new();
    for s in q
        .status
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
    {
        if !rules::STATUSES.contains(&s.as_str()) {
            return Err(AppError::bad_request(format!(
                "status is one or more of {}, not `{s}`",
                rules::STATUSES.join(", ")
            )));
        }
        statuses.push(s);
    }
    let site = match q.site.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => None,
        Some(s) if s.eq_ignore_ascii_case(SITE) => Some(SITE.to_string()),
        Some(s) => {
            return Err(AppError::bad_request(format!(
                "site is `{SITE}`, not `{s}`"
            )));
        }
    };
    let kind = match q
        .kind
        .as_deref()
        .map(|k| k.trim().to_ascii_lowercase())
        .filter(|k| !k.is_empty())
    {
        None => None,
        Some(k) if k == "movie" => Some("movie".to_string()),
        Some(k) if k == "show" || k == "tv" => Some("show".to_string()),
        Some(k) => {
            return Err(AppError::bad_request(format!(
                "kind is `movie` or `show`, not `{k}`"
            )));
        }
    };
    Ok(Filter {
        statuses,
        site,
        person: q
            .person
            .as_deref()
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty()),
        kind,
        words: q
            .q
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_string)
            .collect(),
        title_id: q.title,
    })
}

/// Requests on both sites, filtered and paged, with counts per status and the requesters.
pub async fn list(pool: &PgPool, rules: &Rules, q: &RequestQuery) -> AppResult<RequestPage> {
    let f = filter(q)?;
    let sort = q.sort.as_deref().map(str::trim).unwrap_or("created");
    let (name, reversed) = match sort.strip_prefix('-') {
        Some(rest) => (rest, true),
        None => (sort, false),
    };
    let name = if name.is_empty() { "created" } else { name };
    if !["created", "updated", "title"].contains(&name) {
        return Err(AppError::bad_request(format!(
            "sort is `created`, `updated` or `title`, not `{sort}`"
        )));
    }
    let paging = Paging::new(q.page, q.per_page);
    let (rows, total) = store::list(pool, &f, name, reversed, paging).await?;
    let counts = store::counts(pool, &f).await?;
    let people = store::people(pool, &f)
        .await?
        .into_iter()
        .map(|(name, requests, accounts)| Requester {
            // A name is the owner's only when every account behind it is: a friend who took
            // the owner's display name does not turn the owner's row into theirs.
            owner: !accounts.is_empty()
                && accounts.iter().all(|a| {
                    a.split_once(':')
                        .and_then(|(_, id)| id.parse::<i64>().ok())
                        .is_some_and(|id| rules.is_owner(Some(id)))
                }),
            name,
            requests,
        })
        .collect();
    Ok(RequestPage {
        items: views(pool, rules, rows).await?,
        page: paging.page,
        per_page: paging.per_page,
        total,
        counts,
        people,
    })
}

/// One request, with the other requests for the same title on either site.
pub async fn get(pool: &PgPool, rules: &Rules, id: i64) -> AppResult<RequestDetail> {
    let row = store::get(pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no request {id}.")))?;
    let related = match row.facts.tmdb_id {
        Some(tmdb) => store::related(pool, &row.facts.kind, tmdb, id).await?,
        None => Vec::new(),
    };
    let request = views(pool, rules, vec![row])
        .await?
        .pop()
        .ok_or_else(|| AppError::Internal("the request view was not built".to_string()))?;
    Ok(RequestDetail {
        request,
        related: related
            .into_iter()
            .map(|s| RelatedRequest {
                id: s.id,
                site: s.facts.site,
                status: s.facts.status,
                requested_by: s.facts.requested_by,
                is_4k: s.facts.is_4k,
                seasons: s.facts.seasons,
                created_at: s.facts.requested_at,
            })
            .collect(),
    })
}

/// Pending requests on both sites, for the sidebar.
pub async fn pending(pool: &PgPool) -> AppResult<i64> {
    Ok(store::pending_count(pool).await?)
}
