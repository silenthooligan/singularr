//! Search over what the library has (movies, shows, collections, albums and artists), behind
//! one port with two providers:
//!
//! - **Built in** (`builtin`): PostgreSQL over the module's own copy of the documents
//!   (`search_documents`), matching words and word prefixes, and similar words where the
//!   database has the trigram extension. Always written, always there: it needs nothing a plain
//!   install does not have.
//! - **Meilisearch** (`meili`): used when `SINGULARR_SEARCH_URL` and `SINGULARR_SEARCH_KEY` are
//!   set. Typo tolerant and ranked by Meilisearch. Its index is `<SINGULARR_SEARCH_PREFIX>library`.
//!
//! The index pass (`index`) writes the documents to both, in full at start and again after the
//! catalog or the music changes (and every `SINGULARR_SEARCH_INDEX_SECS`). A search Meilisearch
//! cannot answer is answered built in, and the status says so; nothing fails without it.
//!
//! The module sits between `homemovies` and `face` in the layer order: it reads the catalog,
//! the music read model and the metadata cache through their own functions, and keeps its SQL
//! in `store.rs`.

pub mod model;

#[cfg(feature = "ssr")]
pub mod builtin;
#[cfg(feature = "ssr")]
pub mod index;
#[cfg(feature = "ssr")]
pub mod meili;
#[cfg(feature = "ssr")]
pub mod store;

#[cfg(feature = "ssr")]
pub use service::*;

#[cfg(feature = "ssr")]
mod service {
    use super::builtin::Builtin;
    use super::meili::Meili;
    use super::model::{SearchDoc, SearchStatus};
    use super::store;
    use crate::core::config::Config;
    use crate::core::error::{AppError, AppResult};
    use crate::core::outbound::{Outbound, OutboundError};
    use async_trait::async_trait;
    use sqlx::PgPool;
    use std::sync::Mutex;

    pub const BUILTIN: &str = "postgresql";
    pub const MEILISEARCH: &str = "meilisearch";

    /// The kinds of document, in the order a search answer lists them.
    pub const KINDS: [&str; 5] = ["movie", "show", "collection", "album", "artist"];

    #[derive(Debug, thiserror::Error)]
    pub enum SearchError {
        #[error("the search index could not be read or written: {0}")]
        Database(#[from] sqlx::Error),
        #[error(transparent)]
        Outbound(#[from] OutboundError),
        #[error("Meilisearch answered: {0}")]
        Meili(String),
    }

    /// A document found, with the provider's score (compared only within one answer).
    #[derive(Debug, Clone, PartialEq)]
    pub struct Hit {
        pub doc: SearchDoc,
        pub score: f64,
    }

    #[async_trait]
    pub trait SearchProvider: Send + Sync {
        /// `postgresql` or `meilisearch`.
        fn id(&self) -> &'static str;
        /// Replace every document with these. Returns the documents the index holds now.
        async fn replace(&self, docs: &[SearchDoc]) -> Result<u64, SearchError>;
        /// The best documents for the words, at most `limit` of each kind asked, best first
        /// within a kind.
        async fn search(
            &self,
            query: &str,
            kinds: &[String],
            limit: usize,
        ) -> Result<Vec<Hit>, SearchError>;
        /// Documents in the index.
        async fn documents(&self) -> Result<i64, SearchError>;
    }

    /// Hits and the provider that found them.
    pub struct Found {
        pub hits: Vec<Hit>,
        pub provider: &'static str,
    }

    /// What one index pass did, per provider.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
    pub struct IndexReport {
        pub documents: usize,
        pub builtin: Result<u64, String>,
        pub meilisearch: Option<Result<u64, String>>,
    }

    /// The search service: the built-in provider, and Meilisearch in front of it when it is
    /// configured.
    pub struct Search {
        pool: PgPool,
        builtin: Builtin,
        meili: Option<Meili>,
        /// The last failure of Meilisearch, cleared by its next success.
        meili_error: Mutex<Option<String>>,
        /// The first media server, whose albums and artists have the short ids.
        first_server: Option<String>,
    }

    impl Search {
        pub fn new(pool: PgPool, config: &Config, out: &Outbound) -> Search {
            let search = &config.search;
            Search {
                builtin: Builtin::new(pool.clone()),
                meili: search
                    .meilisearch()
                    .map(|(url, key)| Meili::new(url, key, &search.prefix, out.clone())),
                pool,
                meili_error: Mutex::new(None),
                first_server: config.first_media_server().map(str::to_string),
            }
        }

        /// The first media server's id: its albums and artists are `album-<key>`, any other
        /// server's `album-<server>_<key>` (`music::model::server_music_id`).
        pub fn first_server(&self) -> Option<&str> {
            self.first_server.as_deref()
        }

        /// The provider the configuration asks for.
        pub fn configured(&self) -> &'static str {
            if self.meili.is_some() {
                MEILISEARCH
            } else {
                BUILTIN
            }
        }

        fn failing(&self) -> Option<String> {
            self.meili_error.lock().ok().and_then(|e| e.clone())
        }

        fn remember(&self, error: Option<String>) {
            if let Ok(mut slot) = self.meili_error.lock() {
                *slot = error;
            }
        }

        /// The provider answering now: Meilisearch when it is configured and its last call
        /// went through, else built in.
        pub fn provider(&self) -> &'static str {
            if self.meili.is_some() && self.failing().is_none() {
                MEILISEARCH
            } else {
                BUILTIN
            }
        }

        /// Search Meilisearch when it is configured, else (or when it fails) built in.
        pub async fn find(&self, query: &str, kinds: &[String], limit: usize) -> AppResult<Found> {
            if let Some(m) = &self.meili {
                match m.search(query, kinds, limit).await {
                    Ok(hits) => {
                        self.remember(None);
                        return Ok(Found {
                            hits,
                            provider: MEILISEARCH,
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Meilisearch did not answer; built-in search answers");
                        self.remember(Some(e.to_string()));
                    }
                }
            }
            let hits = self
                .builtin
                .search(query, kinds, limit)
                .await
                .map_err(|e| AppError::Internal(e.to_string()))?;
            Ok(Found {
                hits,
                provider: BUILTIN,
            })
        }

        /// Write these documents to every provider, and record how each went.
        pub async fn index(&self, docs: &[SearchDoc]) -> IndexReport {
            let builtin = self.builtin.replace(docs).await.map_err(|e| e.to_string());
            let recorded = match &builtin {
                Ok(n) => {
                    store::write_state(
                        &self.pool,
                        BUILTIN,
                        Some(i64::try_from(*n).unwrap_or(i64::MAX)),
                        true,
                        "ok",
                        None,
                    )
                    .await
                }
                Err(e) => {
                    store::write_state(&self.pool, BUILTIN, None, false, "down", Some(e)).await
                }
            };
            if let Err(e) = recorded {
                tracing::warn!(error = %e, "the search state could not be recorded");
            }
            let meilisearch = match &self.meili {
                None => None,
                Some(m) => {
                    let result = m.replace(docs).await.map_err(|e| e.to_string());
                    let recorded = match &result {
                        Ok(n) => {
                            self.remember(None);
                            store::write_state(
                                &self.pool,
                                MEILISEARCH,
                                Some(i64::try_from(*n).unwrap_or(i64::MAX)),
                                true,
                                "ok",
                                None,
                            )
                            .await
                        }
                        Err(e) => {
                            self.remember(Some(e.clone()));
                            store::write_state(
                                &self.pool,
                                MEILISEARCH,
                                None,
                                false,
                                "down",
                                Some(e),
                            )
                            .await
                        }
                    };
                    if let Err(e) = recorded {
                        tracing::warn!(error = %e, "the search state could not be recorded");
                    }
                    Some(result)
                }
            };
            IndexReport {
                documents: docs.len(),
                builtin,
                meilisearch,
            }
        }

        /// Which search is in force and how its index stands.
        pub async fn status(&self) -> AppResult<SearchStatus> {
            let typo_tolerant = self.builtin.trigram().await;
            let provider = self.provider();
            let row = store::read_state(&self.pool, provider).await?;
            let documents = match provider {
                BUILTIN => store::count(&self.pool).await?,
                _ => row.as_ref().map(|r| r.documents).unwrap_or(0),
            };
            let (state, detail) = match (&self.meili, self.failing(), &row) {
                (Some(_), Some(why), _) => (
                    "warn".to_string(),
                    Some(format!(
                        "Meilisearch offline ({why}); built-in search in use"
                    )),
                ),
                (_, _, None) => ("warn".to_string(), Some("Index not written".to_string())),
                (_, _, Some(r)) => (r.state.clone(), r.detail.clone()),
            };
            Ok(SearchStatus {
                provider: provider.to_string(),
                configured: self.configured().to_string(),
                documents,
                last_indexed: row.and_then(|r| r.last_indexed),
                state,
                detail,
                typo_tolerant,
            })
        }
    }
}
