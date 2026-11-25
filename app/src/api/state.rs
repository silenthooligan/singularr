//! What every request handler can reach, built once at startup.

use super::model::{LaneSummary, Overview, PolicySummary};
use crate::catalog::store as catalog_store;
use crate::catalog::store::{LibraryWrite, Pruned};
use crate::core::auth::{self, Caller};
use crate::core::config::{ArrKind, Config, IntegrationType};
use crate::core::error::AppResult;
use crate::core::identity::Allowlist;
use crate::core::journal::Journal;
use crate::core::journal::event;
use crate::core::model::{Event, EventQuery, Page, Paging};
use crate::core::store as core_store;
use crate::engines::model::Health;
use crate::engines::service::{EngineSet, statuses};
use crate::lifecycle::model::DownloadQuery;
use crate::lifecycle::service as downloads;
use crate::metadata::service::ProviderSet;
use crate::policy::PolicyHandle;
use axum::http::HeaderMap;
use sqlx::PgPool;
use std::ops::Deref;
use std::sync::Arc;

pub struct Inner {
    pub config: Config,
    pub pool: PgPool,
    pub journal: Journal,
    pub policy: PolicyHandle,
    pub engines: Arc<EngineSet>,
    pub metadata: Arc<ProviderSet>,
    /// The addresses that are the owner (`SINGULARR_OWNER_EMAILS`).
    pub owners: Allowlist,
    /// The request rules and the discover cache.
    pub requests: Arc<crate::requests::service::Requests>,
    /// The music folder.
    pub music: Arc<crate::music::service::MusicLibrary>,
    /// The home movies folder.
    pub home_movies: Arc<crate::homemovies::service::HomeMovies>,
    /// Every setting with its source, and the values saved here.
    pub settings: Arc<crate::settings::Settings>,
    /// What the page routes keep a short time (`api::pages`).
    pub pages: super::pages::PageCache,
    /// Search: built in, or Meilisearch when configured.
    pub search: Arc<crate::search::Search>,
    /// The notification channels: their changes, their tests and their deliveries.
    pub notify: crate::notify::service::Notifier,
}

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

impl AppState {
    pub fn new(inner: Inner) -> Self {
        AppState(Arc::new(inner))
    }

    /// Who sent these headers. Call only behind `strip_untrusted_identity`.
    pub async fn caller(&self, headers: &HeaderMap) -> AppResult<Caller> {
        auth::identify(headers, &self.pool, &self.owners).await
    }

    /// The policy's lanes placed in the libraries of the arrs they stand for.
    pub fn lane_slots(&self) -> Vec<crate::catalog::service::LaneSlot> {
        crate::catalog::service::lane_slots(&self.policy.current().policy, &self.config.arrs)
    }

    pub fn policy_summary(&self) -> PolicySummary {
        let p = self.policy.current();
        PolicySummary {
            hash: p.hash.clone(),
            name: p.policy.name.clone(),
            version: p.policy.version.clone(),
            source: p.source.label(),
            loaded_at: p.loaded_at,
            traits: p.policy.traits.len(),
            lanes: p
                .policy
                .lanes
                .iter()
                .map(|l| LaneSummary {
                    id: l.id.clone(),
                    label: l.label.clone(),
                })
                .collect(),
            protections: p.policy.protections.len(),
        }
    }

    /// Liveness and engine reachability. Public, so the reasons for a failure are left out.
    pub async fn health(&self) -> Health {
        let database = core_store::ping(&self.pool).await.is_ok();
        let mut engines = if database {
            statuses(&self.pool, &self.engines)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for e in &mut engines {
            e.last_error = None;
        }
        let all_up = engines.iter().filter(|e| e.configured).all(|e| e.ok);
        Health {
            status: if database && all_up { "ok" } else { "degraded" }.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            database,
            sync_enabled: self.config.sync.enabled,
            policy_hash: self.policy.current().hash.clone(),
            policy_source: self.policy.current().source.label(),
            engines,
        }
    }

    pub async fn overview(&self) -> AppResult<Overview> {
        let attention = |stuck: bool| DownloadQuery {
            stuck: stuck.then_some(true),
            orphan: (!stuck).then_some(true),
            per_page: Some(10),
            ..DownloadQuery::default()
        };
        Ok(Overview {
            catalog: catalog_store::counts(&self.pool).await?,
            downloads: downloads::counts(&self.pool).await?,
            shadow: catalog_store::diff_counts(&self.pool, None).await?,
            engines: statuses(&self.pool, &self.engines).await?,
            stuck: downloads::downloads(&self.pool, &attention(true))
                .await?
                .items,
            orphans: downloads::downloads(&self.pool, &attention(false))
                .await?
                .items,
            policy: self.policy_summary(),
            latest_event_id: core_store::latest_event_id(&self.pool).await?,
            sync_enabled: self.config.sync.enabled,
        })
    }

    pub async fn events(&self, q: &EventQuery) -> AppResult<Page<Event>> {
        let paging = Paging::new(q.page, q.per_page);
        let (items, total) = core_store::list_events(&self.pool, q, paging).await?;
        Ok(Page::new(items, paging, total))
    }
}

/// What the start removed of the integrations the configuration no longer lists.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PruneReport {
    pub catalog: Pruned,
    pub downloads: u64,
    pub requests: u64,
    pub accounts: u64,
    pub engine_state: u64,
    /// Saved permissions to act on an integration.
    pub access: u64,
}

impl PruneReport {
    pub fn is_empty(&self) -> bool {
        let c = &self.catalog;
        c.engines.is_empty()
            && c.libraries == 0
            && self.downloads == 0
            && self.requests == 0
            && self.accounts == 0
            && self.engine_state == 0
            && self.access == 0
    }
}

/// The read models follow the configuration: rows that came from an integration it no longer
/// lists are removed (copies, untracked files, title links, the titles only it held, its
/// downloads, the requests and accounts of a request site that is gone, its engine state, what
/// was saved that Singularr may do with it), in one transaction with one `integrations_pruned`
/// event. The journal keeps its history.
/// Nothing is removed when the configuration lists no integration at all, which is more
/// likely a missing `.env` than a choice.
pub async fn prune_unconfigured(
    pool: &PgPool,
    journal: &Journal,
    config: &Config,
) -> Result<PruneReport, sqlx::Error> {
    let keep = config.integration_ids();
    if keep.is_empty() {
        return Ok(PruneReport::default());
    }
    let mut tx = journal.begin(pool).await?;
    let mut report = PruneReport {
        catalog: catalog_store::prune_engines(tx.conn(), &keep).await?,
        ..PruneReport::default()
    };
    report.downloads = crate::lifecycle::store::prune_owners(tx.conn(), &keep).await?;
    let site = config
        .integrations
        .iter()
        .any(|i| i.kind == IntegrationType::Seerr)
        .then_some(crate::engines::seerr::SITE);
    (report.requests, report.accounts) =
        crate::requests::store::prune_sites(tx.conn(), site).await?;
    report.engine_state = crate::engines::store::prune(tx.conn(), &keep).await?;
    report.access = crate::engines::store::prune_access(tx.conn(), &keep).await?;
    if !report.is_empty() {
        tx.append_one(event(
            "integrations_pruned",
            "config",
            "integrations",
            "config",
            serde_json::to_value(&report).unwrap_or_default(),
        ))
        .await?;
    }
    tx.commit().await?;
    Ok(report)
}

/// The libraries of the arr integrations, with the names the configuration gives them.
pub async fn sync_libraries(pool: &PgPool, config: &Config) -> Result<(), sqlx::Error> {
    let libraries: Vec<LibraryWrite> = config
        .arrs
        .iter()
        .map(|a| LibraryWrite {
            id: a.library.clone(),
            name: config
                .integration(&a.id)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| a.id.clone()),
            kind: match a.kind {
                ArrKind::Radarr => "movie",
                ArrKind::Sonarr => "show",
            }
            .to_string(),
            integration: a.id.clone(),
        })
        .collect();
    catalog_store::upsert_libraries(pool, &libraries).await
}

impl Deref for AppState {
    type Target = Inner;

    fn deref(&self) -> &Inner {
        &self.0
    }
}
