//! The index pass: every movie, show, collection, album and artist the library has, as search
//! documents, written to every provider. In full at start, again after the catalog or the
//! music changes (a journal event, then a quiet half minute so a sync pass is indexed once),
//! and every `SINGULARR_SEARCH_INDEX_SECS`. Each pass also fetches the TMDB records of
//! collections not stored yet, a few at a time, so the collections list fills up.

use super::IndexReport;
use super::Search;
use super::model::SearchDoc;
use crate::catalog::store as catalog_store;
use crate::core::error::AppResult;
use crate::core::journal::Journal;
use crate::metadata::MetadataProvider;
use crate::metadata::records::{RECORD_TTL, collection_path};
use crate::metadata::service::ProviderSet;
use crate::metadata::store as metadata_store;
use crate::music::store as music_store;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::broadcast::error::{RecvError, TryRecvError};

/// The first pass waits this long after the start.
const START_DELAY: Duration = Duration::from_secs(5);
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
/// movies belong to, and the Plex server's albums and artists.
pub async fn documents(pool: &PgPool) -> AppResult<Vec<SearchDoc>> {
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
        docs.push(SearchDoc {
            poster: a
                .thumb
                .as_ref()
                .map(|_| format!("/api/v1/music/albums/album-{}/cover", a.rating_key)),
            id: format!("album-{}", a.rating_key),
            kind: "album".to_string(),
            title: a.title,
            original_title: None,
            year: a.year,
            extra,
            rating: None,
            parent: a.artist_key.map(|k| format!("artist-{k}")),
            artist: a.artist,
        });
    }
    for a in music_store::server_artists(pool).await? {
        docs.push(SearchDoc {
            poster: a
                .thumb
                .as_ref()
                .map(|_| format!("/api/v1/music/artists/artist-{}/photo", a.rating_key)),
            id: format!("artist-{}", a.rating_key),
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
    let docs = documents(pool).await?;
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

pub struct IndexDeps {
    pub pool: PgPool,
    pub journal: Journal,
    pub search: Arc<Search>,
    pub metadata: Arc<ProviderSet>,
    pub interval: Duration,
}

/// The index loop. It runs whether or not the sync does: it reads Singularr's own tables.
pub fn spawn(deps: IndexDeps) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run(deps))
}

async fn run(deps: IndexDeps) {
    let mut events = deps.journal.subscribe();
    let mut open = true;
    tokio::time::sleep(START_DELAY).await;
    loop {
        let started = Instant::now();
        match pass(&deps.pool, &deps.search, &deps.metadata).await {
            Ok(r) => tracing::info!(
                documents = r.documents,
                builtin = ?r.builtin,
                meilisearch = ?r.meilisearch,
                ms = started.elapsed().as_millis() as u64,
                "search index written"
            ),
            Err(e) => tracing::warn!(error = %e, "the search index could not be written"),
        }
        let next = tokio::time::sleep(deps.interval);
        tokio::pin!(next);
        loop {
            if !open {
                (&mut next).await;
                break;
            }
            tokio::select! {
                () = &mut next => break,
                got = events.recv() => match got {
                    Ok(e) if relevant(&e.kind) => {
                        tokio::time::sleep(QUIET).await;
                        // Everything that arrived meanwhile is in this pass.
                        while matches!(
                            events.try_recv(),
                            Ok(_) | Err(TryRecvError::Lagged(_))
                        ) {}
                        break;
                    }
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => break,
                    Err(RecvError::Closed) => open = false,
                },
            }
        }
    }
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
