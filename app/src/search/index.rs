//! The index pass: every movie, show, collection, album and artist the library has, as search
//! documents, written to every provider. It is the job `singularr:index` of the runtime. It runs
//! in full shortly after each start and every `SINGULARR_SEARCH_INDEX_SECS`. A journal event of
//! the catalog or the music asks for it again after a quiet half minute: a sync pass is indexed
//! once. Each pass also fetches the TMDB records of collections not stored yet, a few at a time.
//! The collections list fills up this way.

use super::IndexReport;
use super::Search;
use super::model::SearchDoc;
use crate::catalog::store as catalog_store;
use crate::core::error::AppResult;
use crate::core::journal::Journal;
use crate::core::runtime::{self, Cx, Done, Failed, Handler};
use crate::core::units;
use crate::metadata::MetadataProvider;
use crate::metadata::records::{RECORD_TTL, collection_path};
use crate::metadata::service::ProviderSet;
use crate::metadata::store as metadata_store;
use crate::music::model::server_music_id;
use crate::music::store as music_store;
use async_trait::async_trait;
use serde_json::json;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

/// The key of the index pass in the job runtime.
pub const JOB: &str = "singularr:index";

/// The first pass waits this long after a start.
pub const START_DELAY: Duration = Duration::from_secs(5);
/// After a change, the pass waits this long for the rest of a sync pass's writes.
const QUIET: Duration = Duration::from_secs(30);
/// Collection records fetched from TMDB per pass.
const WARM_PER_PASS: usize = 100;

/// The TMDB path of an address the managers store (`.../t/p/original/abc.jpg`), else the
/// address as it is.
fn poster(url: Option<&str>) -> Option<String> {
    let u = url?;
    match u.find("image.tmdb.org/t/p/") {
        Some(at) => {
            let rest = &u[at + "image.tmdb.org/t/p/".len()..];
            rest.find('/').map(|i| rest[i..].to_string())
        }
        None => Some(u.to_string()),
    }
}

/// Every document of the library: its titles (those with a TMDB id), the collections its
/// movies belong to, and the media servers' albums and artists. An album or artist has the id
/// the API gives it: `album-<key>` on the first media server (`first`), `album-<server>_<key>`
/// on another.
pub async fn documents(pool: &PgPool, first: Option<&str>) -> AppResult<Vec<SearchDoc>> {
    let titles = catalog_store::search_titles(pool).await?;
    let mut docs: Vec<SearchDoc> = Vec::with_capacity(titles.len());
    let mut collections: BTreeMap<i64, String> = BTreeMap::new();
    for t in &titles {
        let extra: Vec<String> = [&t.collection, &t.studio, &t.network]
            .into_iter()
            .flatten()
            .cloned()
            .collect();
        if let (Some(id), Some(name)) = (t.collection_tmdb_id, &t.collection) {
            collections.entry(id).or_insert_with(|| name.clone());
        }
        docs.push(SearchDoc {
            id: format!("{}-{}", t.kind, t.tmdb_id),
            kind: t.kind.clone(),
            title: t.title.clone(),
            original_title: t.original_title.clone(),
            year: t.year,
            extra,
            poster: poster(t.poster_url.as_deref()),
            rating: t.rating,
            parent: None,
            artist: None,
        });
    }
    // A collection's poster from its stored TMDB record, when there is one.
    let paths: Vec<String> = collections.keys().map(|id| collection_path(*id)).collect();
    let posters: std::collections::HashMap<String, String> =
        metadata_store::posters_of(pool, &paths)
            .await?
            .into_iter()
            .collect();
    for (id, name) in collections {
        docs.push(SearchDoc {
            id: format!("collection-{id}"),
            kind: "collection".to_string(),
            title: name,
            original_title: None,
            year: None,
            extra: Vec::new(),
            poster: posters
                .get(&collection_path(id))
                .filter(|p| p.starts_with('/'))
                .cloned(),
            rating: None,
            parent: None,
            artist: None,
        });
    }
    for a in music_store::server_albums(pool).await? {
        let extra: Vec<String> = [a.artist.clone(), a.label.clone()]
            .into_iter()
            .flatten()
            .collect();
        let id = server_music_id("album", first, &a.server, &a.rating_key);
        docs.push(SearchDoc {
            poster: a
                .thumb
                .as_ref()
                .map(|_| format!("/api/v1/music/albums/{id}/cover")),
            parent: a
                .artist_key
                .as_deref()
                .map(|k| server_music_id("artist", first, &a.server, k)),
            id,
            kind: "album".to_string(),
            title: a.title,
            original_title: None,
            year: a.year,
            extra,
            rating: None,
            artist: a.artist,
        });
    }
    for a in music_store::server_artists(pool).await? {
        let id = server_music_id("artist", first, &a.server, &a.rating_key);
        docs.push(SearchDoc {
            poster: a
                .thumb
                .as_ref()
                .map(|_| format!("/api/v1/music/artists/{id}/photo")),
            id,
            kind: "artist".to_string(),
            title: a.name,
            original_title: None,
            year: None,
            extra: a.genres,
            rating: None,
            parent: None,
            artist: None,
        });
    }
    Ok(docs)
}

/// One pass: the documents built and written, then collection records warmed.
pub async fn pass(
    pool: &PgPool,
    search: &Search,
    metadata: &ProviderSet,
) -> AppResult<IndexReport> {
    let docs = documents(pool, search.first_server()).await?;
    let report = search.index(&docs).await;
    if metadata.tmdb.info().configured {
        let paths: Vec<String> = docs
            .iter()
            .filter_map(|d| d.id.strip_prefix("collection-"))
            .filter_map(|n| n.parse::<i64>().ok())
            .map(collection_path)
            .collect();
        let asked = metadata
            .records(pool)
            .warm(&paths, RECORD_TTL, WARM_PER_PASS)
            .await;
        if asked > 0 {
            tracing::info!(asked, "collection records fetched for the collections list");
        }
    }
    Ok(report)
}

/// Events after which the index is written again.
pub fn relevant(kind: &str) -> bool {
    matches!(
        kind,
        "title_added"
            | "title_removed"
            | "copy_added"
            | "copy_removed"
            | "album_added"
            | "album_removed"
            | "integrations_pruned"
    )
}

/// The index's job: one pass. It runs whether or not the sync does: it reads Singularr's own
/// tables. Its writes replace the whole index: a run after one that lost its lease writes it
/// again in full.
pub struct IndexJob {
    pub pool: PgPool,
    pub search: Arc<Search>,
    pub metadata: Arc<ProviderSet>,
}

#[async_trait]
impl Handler for IndexJob {
    async fn run(&self, _cx: Cx) -> Result<Done, Failed> {
        let started = Instant::now();
        match pass(&self.pool, &self.search, &self.metadata).await {
            Ok(r) => {
                tracing::info!(
                    documents = r.documents,
                    builtin = ?r.builtin,
                    meilisearch = ?r.meilisearch,
                    ms = started.elapsed().as_millis() as u64,
                    "search index written"
                );
                Ok(Done::from(units::counted(
                    i64::try_from(r.documents).unwrap_or(i64::MAX),
                    "document",
                    "documents",
                )))
            }
            Err(e) => {
                tracing::warn!(error = %e, "the search index could not be written");
                Err(Failed {
                    error: e.to_string(),
                    class: Some(e.code().to_string()),
                    again: None,
                })
            }
        }
    }
}

/// The index's trigger: a pass after the catalog or the music changes. It waits a quiet half
/// minute after the event: a sync pass is indexed once. A pass going then is let end. The index
/// is written again after it. The pass after a start and the interval are its schedule's.
pub fn spawn_trigger(pool: PgPool, journal: &Journal) -> tokio::task::JoinHandle<()> {
    let mut events = journal.subscribe();
    tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(e) if relevant(&e.kind) => {
                    tokio::time::sleep(QUIET).await;
                    // Everything that arrived meanwhile is in this pass.
                    while matches!(events.try_recv(), Ok(_) | Err(TryRecvError::Lagged(_))) {}
                }
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
            if let Err(e) = runtime::run_after(&pool, JOB, json!({})).await {
                tracing::warn!(error = %e, "the search index could not be asked for a pass");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posters_become_tmdb_paths_and_changes_start_a_pass() {
        assert_eq!(
            poster(Some("https://image.tmdb.org/t/p/original/abc.jpg")).as_deref(),
            Some("/abc.jpg")
        );
        assert_eq!(
            poster(Some("https://artworks.thetvdb.com/banners/x.jpg")).as_deref(),
            Some("https://artworks.thetvdb.com/banners/x.jpg")
        );
        assert!(relevant("title_added") && relevant("album_removed"));
        assert!(!relevant("download_added") && !relevant("score_changed"));
    }
}
