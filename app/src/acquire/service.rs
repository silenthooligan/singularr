//! What is asked of a manager: an interactive search (a read, but it asks every indexer), a
//! grab, a search command, an import of a refused download as it is, a removal from the queue.
//! Every one but the interactive search is an act: refused here before anything is sent while
//! the manager's access is `read` (and by `core::outbound` on the way out), and journaled once
//! the manager took it. A second search of a title is refused while the first runs.

use super::model::{
    ActAnswer, ManagerScore, ProfileRef, ReleaseList, SearchManager, SearchManyAnswer, SearchScope,
    SearchTitle,
};
use super::verdict::{Held, Judge, counts};
use crate::catalog::model::{CopyQuery, TitleKind};
use crate::catalog::store::{self as catalog_store, CopyRow};
use crate::core::config::Config;
use crate::core::journal::{Journal, event};
use crate::core::model::Paging;
use crate::core::outbound::OutboundError;
use crate::engines::access::read_only_words;
use crate::engines::arr::{self, SEARCH_MAX};
use crate::engines::service::EngineSet;
use crate::engines::{
    EngineError, ImportFile, LibraryEngine, ManagerAct, ManagerActed, ReleaseScope,
};
use crate::lifecycle::model::Download;
use crate::lifecycle::store as downloads_store;
use crate::policy::{ActivePolicy, first_lane_of_arr, held_context};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

/// The most titles and episodes one search from the wanted list names.
pub const SEARCH_MANY_MAX: usize = 50;

/// Why an ask was not made, or did not go through.
#[derive(Debug, thiserror::Error)]
pub enum AcquireError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    /// The manager's access is `read`: nothing was sent.
    #[error("{0}")]
    NotAllowed(String),
    /// Another search of the title runs now.
    #[error("{0}")]
    Busy(String),
    #[error("{0}")]
    NotOffered(String),
    /// The manager said no, in its words.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Upstream(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

use AcquireError::{BadRequest, Busy, NotFound, NotOffered, Upstream};

/// The title an ask is for, with the manager that holds it and what its releases are judged
/// with.
#[derive(Debug, Clone)]
pub struct Target {
    pub title_id: i64,
    pub card: SearchTitle,
    pub tmdb_id: Option<i64>,
    /// The manager's integration id, and its own id of the title.
    pub manager: String,
    pub engine_title_id: i64,
    pub library: String,
    pub lane: Option<String>,
    pub tags: Vec<String>,
    pub original_language: Option<String>,
}

impl Target {
    fn is_movie(&self) -> bool {
        self.card.kind == TitleKind::Movie.as_str()
    }
}

/// One entry of a search from the wanted list: a movie, or one episode of a show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedItem {
    /// The title's id: `movie-603`, `show-1396`, or its catalog number.
    pub title: String,
    pub season: Option<i32>,
    pub episode: Option<i32>,
}

/// The key a title is held and kept under while it is searched.
fn key(title_id: i64) -> String {
    format!("title:{title_id}")
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A file held now, from the catalog's row of it.
fn held_of(row: CopyRow, tags: &[String]) -> Held {
    let ctx = held_context(
        row.size_bytes,
        tags,
        row.original_language.as_deref(),
        &row.languages,
        row.media_info.as_ref(),
    );
    Held {
        copy_id: row.id,
        file: file_name(&row.relative_path),
        release: row.release_name.clone(),
        season: row.season_number,
        episodes: row.episode_numbers.clone(),
        size: row.size_bytes,
        edition: row.edition_label.clone(),
        parsed: row.held(),
        ctx,
        manager: ManagerScore {
            quality: row.arr_quality.clone(),
            score: row.arr_score,
            formats: row.arr_custom_formats.clone(),
        },
    }
}

/// The files of a download to import as they are: those the manager matched to a title (a
/// movie, or a show's episodes) and gave a quality, never a sample. A movie takes its largest
/// file alone. A file without the download's id gets it as the manager wrote it.
pub fn files_to_import(
    kind: TitleKind,
    download_id: &str,
    offer: Vec<ImportFile>,
) -> Vec<ImportFile> {
    let mut usable: Vec<ImportFile> = offer
        .into_iter()
        .filter(|f| f.title_id.is_some() && f.quality.is_some())
        .filter(|f| kind == TitleKind::Movie || !f.episode_ids.is_empty())
        .filter(|f| {
            !f.rejections
                .iter()
                .any(|r| r.to_ascii_lowercase().contains("sample"))
        })
        .map(|mut f| {
            if f.download_id.is_none() {
                f.download_id = Some(download_id.trim().to_string());
            }
            f
        })
        .collect();
    if kind == TitleKind::Movie {
        usable.sort_by_key(|f| Reverse(f.size.unwrap_or(0)));
        usable.truncate(1);
    }
    usable
}

/// What an act came to, as the asks answer it: the command the manager queued, or why not.
fn acted(
    name: &str,
    read_only: String,
    result: Result<ManagerActed, EngineError>,
) -> Result<Option<i64>, AcquireError> {
    match result {
        Ok(ManagerActed::Done { command }) => Ok(command),
        Ok(ManagerActed::NotOffered) => Err(NotOffered(format!("Not supported by {name}."))),
        Ok(ManagerActed::Refused(words)) if words.is_empty() => {
            Err(AcquireError::Refused(format!("{name} refused.")))
        }
        Ok(ManagerActed::Refused(words)) => {
            Err(AcquireError::Refused(format!("{name} refused: {words}")))
        }
        // The access was taken back while the ask was on its way.
        Err(EngineError::Outbound(OutboundError::NotAllowed { .. })) => {
            Err(AcquireError::NotAllowed(read_only))
        }
        Err(e) => Err(Upstream(format!("Could not reach {name}: {e}"))),
    }
}

/// What every ask is made with.
pub struct Asker<'a> {
    pub pool: &'a PgPool,
    pub journal: &'a Journal,
    pub engines: &'a EngineSet,
    pub config: &'a Config,
    pub policy: &'a ActivePolicy,
    /// Who asks, as the journal names actors: `user:<address>`.
    pub actor: &'a str,
}

impl Asker<'_> {
    fn name(&self, id: &str) -> String {
        self.config
            .integration(id)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| id.to_string())
    }

    fn manager(&self, id: &str) -> Result<&Arc<dyn LibraryEngine>, AcquireError> {
        self.engines
            .arr(id)
            .ok_or_else(|| NotFound(format!("{} is not configured.", self.name(id))))
    }

    /// Nothing is asked of a manager while the act's action is read.
    fn may_act(&self, id: &str, action: &str) -> Result<(), AcquireError> {
        if self.engines.access.allows(id, action) {
            Ok(())
        } else {
            Err(AcquireError::NotAllowed(self.read_only(id, action)))
        }
    }

    /// A refused act on a manager, in words: `Remove a download is read-only on Radarr.`
    fn read_only(&self, id: &str, action: &str) -> String {
        read_only_words(self.engines.access.of(id).label(action), &self.name(id))
    }

    fn upstream(&self, id: &str) -> impl Fn(EngineError) -> AcquireError {
        let name = self.name(id);
        move |e| Upstream(format!("Could not reach {name}: {e}"))
    }

    fn busy(&self, title: &str) -> AcquireError {
        Busy(format!("A search of {title} is running."))
    }

    /// Write one event. What the manager did stands whatever happens here: a journal that
    /// cannot be written is logged.
    async fn journal(&self, kind: &str, subject_type: &str, subject_id: &str, body: Value) {
        let written: Result<(), sqlx::Error> = async {
            let mut tx = self.journal.begin(self.pool).await?;
            tx.append_one(event(kind, subject_type, subject_id, self.actor, body))
                .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        if let Err(e) = written {
            tracing::error!(kind, error = %e, "a manager took an act and the journal could not record it");
        }
    }

    /// The title of an id (`movie-603`, `show-1396`, or a catalog number) and the first
    /// configured manager that holds it.
    pub async fn target(&self, id: &str) -> Result<Target, AcquireError> {
        let id = id.trim();
        let unknown = || NotFound(format!("There is no title `{id}` in the library."));
        let title_id = match id.parse::<i64>() {
            Ok(n) if n > 0 => n,
            _ => {
                let (kind, tmdb) = id
                    .split_once('-')
                    .and_then(|(k, n)| Some((k, n.parse::<i64>().ok().filter(|n| *n > 0)?)))
                    .filter(|(k, _)| matches!(*k, "movie" | "show"))
                    .ok_or_else(|| {
                        BadRequest(format!(
                            "`{id}` is not a title id such as movie-603 or show-1396."
                        ))
                    })?;
                catalog_store::title_links(self.pool, &[tmdb], &[])
                    .await?
                    .into_iter()
                    .find(|l| l.kind == kind && l.tmdb_id == Some(tmdb))
                    .map(|l| l.id)
                    .ok_or_else(unknown)?
            }
        };
        let row = catalog_store::title(self.pool, title_id)
            .await?
            .ok_or_else(unknown)?;
        let facts = catalog_store::title_engines(self.pool, title_id)
            .await?
            .into_iter()
            .find(|e| self.engines.arr(&e.engine).is_some())
            .ok_or_else(|| NotFound(format!("No manager holds {}.", row.title)))?;
        Ok(Target {
            title_id,
            card: SearchTitle {
                id: row.tmdb_id.map(|t| format!("{}-{t}", row.kind)),
                kind: row.kind.clone(),
                title: row.title.clone(),
                year: row.year,
            },
            tmdb_id: row.tmdb_id,
            manager: facts.engine,
            engine_title_id: facts.engine_title_id,
            library: facts.library,
            lane: facts.lane,
            tags: facts.tags,
            original_language: row.original_language,
        })
    }

    fn manager_view(&self, id: &str) -> SearchManager {
        SearchManager {
            id: id.to_string(),
            name: self.name(id),
            access: self.engines.access.of(id).access.as_str().to_string(),
        }
    }

    /// The manager's ids of a season's episodes, by their numbers.
    async fn episode_ids(
        &self,
        engine: &Arc<dyn LibraryEngine>,
        t: &Target,
        season: i32,
        numbers: &[i32],
    ) -> Result<Vec<i64>, AcquireError> {
        let name = self.name(&t.manager);
        let listed = engine
            .episodes(t.engine_title_id, Some(season))
            .await
            .map_err(self.upstream(&t.manager))?;
        numbers
            .iter()
            .map(|n| {
                listed
                    .iter()
                    .find(|e| e.season == season && e.number == *n)
                    .map(|e| e.id)
                    .ok_or_else(|| {
                        NotFound(format!(
                            "{name} has no episode S{season:02}E{n:02} of {}.",
                            t.card.title
                        ))
                    })
            })
            .collect()
    }

    /// The files held now for what is searched: the manager's files of the movie, of the
    /// season, or of the one episode.
    async fn held(
        &self,
        t: &Target,
        season: Option<i32>,
        episode: Option<i32>,
    ) -> Result<Vec<Held>, AcquireError> {
        let q = CopyQuery {
            library: Some(t.library.clone()),
            season,
            page: None,
            per_page: None,
        };
        let (rows, _) =
            catalog_store::title_copies(self.pool, t.title_id, &q, Paging::new(Some(1), Some(200)))
                .await?;
        Ok(rows
            .into_iter()
            .filter(|r| r.engine == t.manager)
            .filter(|r| episode.is_none_or(|e| r.episode_numbers.contains(&e)))
            .map(|r| held_of(r, &t.tags))
            .collect())
    }

    /// The interactive search: every release the manager's indexers offer for the movie, a
    /// season or one episode, each with Singularr's verdict beside the manager's, and the files
    /// held now. It is a read and passes under read access; what it found is kept for a grab.
    pub async fn releases(
        &self,
        t: &Target,
        season: Option<i32>,
        episode: Option<i32>,
    ) -> Result<ReleaseList, AcquireError> {
        let engine = self.manager(&t.manager)?;
        let name = self.name(&t.manager);
        if t.is_movie() && (season.is_some() || episode.is_some()) {
            return Err(BadRequest("A movie has no seasons.".to_string()));
        }
        if !t.is_movie() && season.is_none() {
            return Err(BadRequest("`season` is needed for a show.".to_string()));
        }
        let _hold = self
            .engines
            .searches
            .hold(&[key(t.title_id)])
            .ok_or_else(|| self.busy(&t.card.title))?;
        let scope = match (season, episode) {
            (Some(season), Some(number)) => ReleaseScope::Episode {
                episode: self.episode_ids(engine, t, season, &[number]).await?[0],
            },
            (Some(season), None) => ReleaseScope::Season {
                series: t.engine_title_id,
                season,
            },
            _ => ReleaseScope::Movie {
                movie: t.engine_title_id,
            },
        };
        let started = Instant::now();
        let found = match engine.releases(scope).await {
            Ok(Some(found)) => found,
            Ok(None) => {
                return Err(NotOffered(format!(
                    "Interactive search not supported by {name}."
                )));
            }
            Err(e) => return Err(Upstream(format!("Could not reach {name}: {e}"))),
        };
        let took = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        self.engines.searches.keep(&key(t.title_id), found.clone());
        let held = self.held(t, season, episode).await?;
        let policy = &self.policy.policy;
        let lane_id = t
            .lane
            .clone()
            .or_else(|| first_lane_of_arr(policy, &t.manager));
        let lane = lane_id
            .as_deref()
            .and_then(|l| policy.lane(l))
            .ok_or_else(|| {
                NotFound(format!("No quality profile in force for {}.", t.card.title))
            })?;
        let judge = Judge {
            policy,
            lane,
            tags: &t.tags,
            original_language: t.original_language.as_deref(),
            held: &held,
            season,
            episode,
        };
        let releases = judge.all(&found);
        Ok(ReleaseList {
            title: t.card.clone(),
            manager: self.manager_view(&t.manager),
            scope: SearchScope { season, episode },
            searched: Utc::now(),
            took,
            profile: Some(ProfileRef {
                id: lane.id.clone(),
                label: lane.label.clone(),
                min_score: lane.min_score,
                cutoff: (lane.cutoff_score != i32::MAX).then_some(lane.cutoff_score),
            }),
            held: held.iter().map(|h| judge.held_view(h)).collect(),
            counts: counts(&releases),
            releases,
        })
    }

    /// Grab a release the last search of the title found.
    pub async fn grab(
        &self,
        t: &Target,
        guid: &str,
        indexer_id: i64,
    ) -> Result<ActAnswer, AcquireError> {
        self.may_act(&t.manager, "grab")?;
        let engine = self.manager(&t.manager)?;
        let release = self
            .engines
            .searches
            .found(&key(t.title_id), guid, indexer_id)
            .ok_or_else(|| NotFound("Not in the last search of this title.".to_string()))?;
        let name = self.name(&t.manager);
        acted(
            &name,
            self.read_only(&t.manager, "grab"),
            engine
                .act(&ManagerAct::Grab {
                    guid: release.guid.clone(),
                    indexer_id: release.indexer_id,
                })
                .await,
        )?;
        self.journal(
            "release_grabbed",
            "title",
            &t.title_id.to_string(),
            json!({
                "title_id": t.title_id,
                "title": t.card.title,
                "kind": t.card.kind,
                "tmdb_id": t.tmdb_id,
                "manager": t.manager,
                "manager_name": name,
                "release": release.name,
                "indexer": release.indexer,
                "quality": release.quality,
                "size_bytes": release.size,
                "season": release.season,
                "episodes": release.episodes,
            }),
        )
        .await;
        Ok(ActAnswer {
            act: "grab".to_string(),
            manager: t.manager.clone(),
            title: Some(t.card.clone()),
            detail: release.name,
            command: None,
        })
    }

    /// Search for the title: a movie, a whole show, a season, or episodes of a season.
    pub async fn search(
        &self,
        t: &Target,
        season: Option<i32>,
        episodes: &[i32],
    ) -> Result<ActAnswer, AcquireError> {
        self.may_act(&t.manager, "search")?;
        let engine = self.manager(&t.manager)?;
        if episodes.len() > SEARCH_MAX {
            return Err(BadRequest(format!(
                "At most {SEARCH_MAX} episodes are searched at once."
            )));
        }
        let _hold = self
            .engines
            .searches
            .hold(&[key(t.title_id)])
            .ok_or_else(|| self.busy(&t.card.title))?;
        let act = if t.is_movie() {
            if season.is_some() || !episodes.is_empty() {
                return Err(BadRequest("A movie has no seasons.".to_string()));
            }
            ManagerAct::SearchMovies {
                movies: vec![t.engine_title_id],
            }
        } else {
            match (season, episodes.is_empty()) {
                (None, true) => ManagerAct::SearchSeries {
                    series: t.engine_title_id,
                },
                (None, false) => {
                    return Err(BadRequest("`episodes` needs their `season`.".to_string()));
                }
                (Some(season), true) => ManagerAct::SearchSeason {
                    series: t.engine_title_id,
                    season,
                },
                (Some(season), false) => ManagerAct::SearchEpisodes {
                    episodes: self.episode_ids(engine, t, season, episodes).await?,
                },
            }
        };
        let name = self.name(&t.manager);
        let command = arr::command_name(&act).unwrap_or("search");
        let id = acted(
            &name,
            self.read_only(&t.manager, "search"),
            engine.act(&act).await,
        )?;
        self.journal(
            "search_sent",
            "title",
            &t.title_id.to_string(),
            json!({
                "title_id": t.title_id,
                "title": t.card.title,
                "kind": t.card.kind,
                "tmdb_id": t.tmdb_id,
                "manager": t.manager,
                "manager_name": name,
                "command": command,
                "command_id": id,
                "season": season,
                "episodes": episodes,
            }),
        )
        .await;
        Ok(ActAnswer {
            act: "search".to_string(),
            manager: t.manager.clone(),
            title: Some(t.card.clone()),
            detail: command.to_string(),
            command: id,
        })
    }

    /// The download behind a queue row, and the manager that owns it, once the act's action has
    /// write access on that manager.
    async fn owned(&self, hash: &str, action: &str) -> Result<(Download, String), AcquireError> {
        let d = downloads_store::one(self.pool, hash)
            .await?
            .ok_or_else(|| NotFound("There is no such download in the queue.".to_string()))?;
        let owner = d
            .owner
            .clone()
            .filter(|o| self.engines.arr(o).is_some())
            .ok_or_else(|| NotFound("No manager owns this download.".to_string()))?;
        self.may_act(&owner, action)?;
        Ok((d, owner))
    }

    async fn card_of(&self, title_id: Option<i64>) -> Result<Option<SearchTitle>, AcquireError> {
        let Some(id) = title_id else {
            return Ok(None);
        };
        Ok(catalog_store::title(self.pool, id)
            .await?
            .map(|row| SearchTitle {
                id: row.tmdb_id.map(|t| format!("{}-{t}", row.kind)),
                kind: row.kind,
                title: row.title,
                year: row.year,
            }))
    }

    /// Import a download the manager refused, as it is: the files its manual import offers,
    /// in the import mode its download client allows. The manager is asked by the download id
    /// its queue entry carries, in the manager's own case: its lookups match the id exactly.
    pub async fn import(&self, hash: &str) -> Result<ActAnswer, AcquireError> {
        let (d, owner) = self.owned(hash, "import").await?;
        let engine = self.manager(&owner)?;
        let name = self.name(&owner);
        let entries = engine
            .queue_entries(&d.hash)
            .await
            .map_err(self.upstream(&owner))?;
        let entry = entries
            .first()
            .ok_or_else(|| NotFound(format!("Not in the queue of {name}.")))?;
        let written = &entry.download_id_as_written;
        let offer = engine
            .import_offer(written)
            .await
            .map_err(self.upstream(&owner))?;
        let files = files_to_import(engine.engine().kind, written, offer);
        if files.is_empty() {
            return Err(NotOffered(format!(
                "No file of this download for {name} to import."
            )));
        }
        let mode = arr::import_mode(entry);
        let names: Vec<String> = files.iter().map(|f| file_name(&f.path)).collect();
        let command = acted(
            &name,
            self.read_only(&owner, "import"),
            engine.act(&ManagerAct::Import { files, mode }).await,
        )?;
        let card = self.card_of(d.title_id).await?;
        self.journal(
            "download_import_forced",
            "download",
            &d.hash,
            json!({
                "hash": d.hash,
                "name": d.name,
                "title_id": d.title_id,
                "title": card.as_ref().map(|c| c.title.clone()),
                "manager": owner,
                "manager_name": name,
                "files": names,
                "mode": mode.as_str(),
                "command_id": command,
            }),
        )
        .await;
        Ok(ActAnswer {
            act: "import".to_string(),
            manager: owner,
            title: card,
            detail: d.name,
            command,
        })
    }

    /// Take a download out of its manager's queue and the download client; with `blocklist`
    /// its release is not grabbed again.
    pub async fn remove(&self, hash: &str, blocklist: bool) -> Result<ActAnswer, AcquireError> {
        let (d, owner) = self.owned(hash, "remove").await?;
        let engine = self.manager(&owner)?;
        let name = self.name(&owner);
        let entries = engine
            .queue_entries(&d.hash)
            .await
            .map_err(self.upstream(&owner))?;
        let entry = entries
            .first()
            .ok_or_else(|| NotFound(format!("Not in the queue of {name}.")))?;
        acted(
            &name,
            self.read_only(&owner, "remove"),
            engine
                .act(&ManagerAct::Remove {
                    queue_id: entry.queue_id,
                    blocklist,
                })
                .await,
        )?;
        let card = self.card_of(d.title_id).await?;
        self.journal(
            "download_queue_removed",
            "download",
            &d.hash,
            json!({
                "hash": d.hash,
                "name": d.name,
                "title_id": d.title_id,
                "title": card.as_ref().map(|c| c.title.clone()),
                "manager": owner,
                "manager_name": name,
                "blocklist": blocklist,
            }),
        )
        .await;
        Ok(ActAnswer {
            act: "remove".to_string(),
            manager: owner,
            title: card,
            detail: d.name,
            command: None,
        })
    }

    /// Search for titles of the wanted list: movies, and single episodes of shows. One command
    /// per manager; every manager may act before anything is sent.
    pub async fn search_many(
        &self,
        items: &[WantedItem],
    ) -> Result<SearchManyAnswer, AcquireError> {
        if items.is_empty() || items.len() > SEARCH_MANY_MAX {
            return Err(BadRequest(format!(
                "`items` holds 1 to {SEARCH_MANY_MAX} titles or episodes."
            )));
        }
        let mut wanted: Vec<(Target, Option<(i32, i32)>)> = Vec::new();
        for item in items {
            let t = self.target(&item.title).await?;
            let episode = match (t.is_movie(), item.season, item.episode) {
                (true, None, None) => None,
                (true, _, _) => return Err(BadRequest("A movie has no seasons.".to_string())),
                (false, Some(s), Some(e)) => Some((s, e)),
                (false, _, _) => {
                    return Err(BadRequest(format!(
                        "A show is searched by its episodes here: `season` and `episode` of {}.",
                        t.card.title
                    )));
                }
            };
            wanted.push((t, episode));
        }
        for (t, _) in &wanted {
            self.may_act(&t.manager, "search")?;
        }
        let mut keys: Vec<String> = wanted.iter().map(|(t, _)| key(t.title_id)).collect();
        keys.sort();
        keys.dedup();
        let _hold = self
            .engines
            .searches
            .hold(&keys)
            .ok_or_else(|| Busy("A search of one of these titles is running.".to_string()))?;
        // By manager, in the order each was first named.
        let mut managers: Vec<String> = Vec::new();
        for (t, _) in &wanted {
            if !managers.contains(&t.manager) {
                managers.push(t.manager.clone());
            }
        }
        let mut sent = Vec::new();
        for manager in managers {
            let engine = self.manager(&manager)?;
            let name = self.name(&manager);
            let mine: Vec<&(Target, Option<(i32, i32)>)> = wanted
                .iter()
                .filter(|(t, _)| t.manager == manager)
                .collect();
            let labels: Vec<String> = mine
                .iter()
                .map(|(t, e)| match e {
                    Some((s, n)) => format!("{} S{s:02}E{n:02}", t.card.title),
                    None => t.card.title.clone(),
                })
                .collect();
            let act = if engine.engine().kind == TitleKind::Movie {
                let mut movies: Vec<i64> = mine.iter().map(|(t, _)| t.engine_title_id).collect();
                movies.sort_unstable();
                movies.dedup();
                ManagerAct::SearchMovies { movies }
            } else {
                // One read of each season's episodes.
                let mut by_season: BTreeMap<(i64, i32), (&Target, Vec<i32>)> = BTreeMap::new();
                for (t, e) in &mine {
                    if let Some((s, n)) = e {
                        by_season
                            .entry((t.engine_title_id, *s))
                            .or_insert((t, Vec::new()))
                            .1
                            .push(*n);
                    }
                }
                let mut episodes = Vec::new();
                for ((_, season), (t, numbers)) in &by_season {
                    episodes.extend(self.episode_ids(engine, t, *season, numbers).await?);
                }
                episodes.sort_unstable();
                episodes.dedup();
                ManagerAct::SearchEpisodes { episodes }
            };
            let command = arr::command_name(&act).unwrap_or("search");
            let id = acted(
                &name,
                self.read_only(&manager, "search"),
                engine.act(&act).await,
            )?;
            self.journal(
                "search_sent",
                "engine",
                &manager,
                json!({
                    "manager": manager,
                    "manager_name": name,
                    "command": command,
                    "command_id": id,
                    "titles": labels,
                    "count": labels.len(),
                }),
            )
            .await;
            sent.push(ActAnswer {
                act: "search".to_string(),
                manager: manager.clone(),
                title: None,
                detail: command.to_string(),
                command: id,
            });
        }
        Ok(SearchManyAnswer {
            sent,
            count: items.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The import override builds its files from what the manual import offered for the
    /// refused download of 24 Nov: its one file, the movie Radarr matched, its quality as Radarr
    /// read it.
    #[test]
    fn the_import_takes_the_file_the_manual_import_offers() {
        let offer: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../tests/fixtures/radarr_manualimport.json"
        ))
        .unwrap();
        assert_eq!(offer.len(), 1);
        let hash = "147E6690379BCAC2718AAA6A0D4089BFED6737B2";
        let file = ImportFile {
            path: offer[0]["path"].as_str().unwrap().to_string(),
            size: offer[0]["size"].as_i64(),
            title_id: offer[0]["movie"]["id"].as_i64(),
            episode_ids: Vec::new(),
            season: None,
            quality: Some(offer[0]["quality"].clone()),
            languages: offer[0]["languages"].clone(),
            release_group: None,
            download_id: None,
            indexer_flags: 1,
            release_type: None,
            rejections: vec![
                offer[0]["rejections"][0]["reason"]
                    .as_str()
                    .unwrap()
                    .to_string(),
            ],
        };
        let sample = ImportFile {
            path: "/downloads/x/Sample/sample.mkv".to_string(),
            size: Some(9_000_000_000_000),
            rejections: vec!["Sample".to_string()],
            ..file.clone()
        };
        let small = ImportFile {
            path: "/downloads/x/extra.mkv".to_string(),
            size: Some(10),
            ..file.clone()
        };
        let unmatched = ImportFile {
            title_id: None,
            ..file.clone()
        };
        let picked = files_to_import(
            TitleKind::Movie,
            hash,
            vec![small, sample, file.clone(), unmatched],
        );
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].path, "/downloads/The Devil Wears Prada.mkv");
        assert_eq!(picked[0].download_id.as_deref(), Some(hash));
        assert_eq!(picked[0].title_id, Some(4806));
        // A usenet id keeps the case its manager wrote it in.
        let usenet = files_to_import(
            TitleKind::Movie,
            " SABnzbd_nzo_x7f3k2ab ",
            vec![file.clone()],
        );
        assert_eq!(
            usenet[0].download_id.as_deref(),
            Some("SABnzbd_nzo_x7f3k2ab")
        );
        // A show takes every file that names its episodes.
        let episode = ImportFile {
            episode_ids: vec![16846],
            ..file.clone()
        };
        let shows = files_to_import(TitleKind::Show, hash, vec![episode.clone(), file, episode]);
        assert_eq!(shows.len(), 2);
    }

    #[test]
    fn a_title_is_held_and_kept_by_its_catalog_number() {
        assert_eq!(key(12), "title:12");
        assert_eq!(file_name("/a/b/c.mkv"), "c.mkv");
        assert_eq!(file_name("c.mkv"), "c.mkv");
    }
}
