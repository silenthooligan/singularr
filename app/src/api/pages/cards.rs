//! Title cards with their library state, for many titles at once: a few queries, never one per
//! title.

use super::labels::{self, Motion};
use super::model::TitleCard;
use crate::catalog::store::{self as catalog_store, CardRow};
use crate::core::error::AppResult;
use crate::lifecycle::store::{self as downloads_store, TitleStage};
use crate::requests::store as requests_store;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

/// Where every title's downloads are, by title id.
pub async fn motions(pool: &PgPool) -> AppResult<HashMap<i64, Motion>> {
    Ok(downloads_store::title_stages(pool)
        .await?
        .into_iter()
        .map(|s: TitleStage| {
            (
                s.title_id,
                Motion {
                    stuck: s.stuck,
                    downloading: s.downloading,
                    importing: s.importing,
                },
            )
        })
        .collect())
}

/// The titles an open request names, by kind and TMDB id.
pub async fn requested(pool: &PgPool, tmdb_ids: &[i64]) -> AppResult<HashSet<(String, i64)>> {
    let mut ids = tmdb_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();
    Ok(requests_store::open_requests(pool, &ids)
        .await?
        .into_iter()
        .map(|(kind, tmdb, _)| (kind, tmdb))
        .collect())
}

/// Catalog titles as cards, by title id, and the engine ids that lead to them.
#[derive(Debug, Default)]
pub struct Cards {
    rows: HashMap<i64, CardRow>,
    by_engine: HashMap<(String, i64), i64>,
    motions: HashMap<i64, Motion>,
    requested: HashSet<(String, i64)>,
}

impl Cards {
    async fn from_rows(pool: &PgPool, rows: Vec<CardRow>) -> AppResult<Cards> {
        let tmdb: Vec<i64> = rows.iter().filter_map(|r| r.tmdb_id).collect();
        let requested = requested(pool, &tmdb).await?;
        let motions = motions(pool).await?;
        let mut by_engine = HashMap::new();
        for r in &rows {
            for l in &r.links {
                by_engine.insert((l.engine.clone(), l.engine_title_id), r.id);
            }
        }
        Ok(Cards {
            rows: rows.into_iter().map(|r| (r.id, r)).collect(),
            by_engine,
            motions,
            requested,
        })
    }

    /// The cards of these catalog titles.
    pub async fn of_titles(pool: &PgPool, ids: &[i64]) -> AppResult<Cards> {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let rows = catalog_store::title_cards(pool, &ids).await?;
        Cards::from_rows(pool, rows).await
    }

    /// The cards of the titles engines know by these ids of their own, per engine.
    pub async fn of_engine_titles(
        pool: &PgPool,
        wanted: &[(String, Vec<i64>)],
    ) -> AppResult<Cards> {
        let mut rows: Vec<CardRow> = Vec::new();
        for (engine, ids) in wanted {
            let mut ids = ids.clone();
            ids.sort_unstable();
            ids.dedup();
            for r in catalog_store::engine_title_cards(pool, engine, &ids).await? {
                if !rows.iter().any(|x| x.id == r.id) {
                    rows.push(r);
                }
            }
        }
        Cards::from_rows(pool, rows).await
    }

    pub fn row(&self, title_id: i64) -> Option<&CardRow> {
        self.rows.get(&title_id)
    }

    /// The catalog title an engine knows by its own id.
    pub fn title_of(&self, engine: &str, engine_title_id: i64) -> Option<i64> {
        self.by_engine
            .get(&(engine.to_string(), engine_title_id))
            .copied()
    }

    pub fn motion(&self, title_id: i64) -> Motion {
        self.motions.get(&title_id).copied().unwrap_or_default()
    }

    /// The card of a catalog title, with its library state.
    pub fn card(&self, title_id: i64) -> Option<TitleCard> {
        self.rows.get(&title_id).map(|r| self.card_of(r))
    }

    pub fn card_of(&self, r: &CardRow) -> TitleCard {
        let requested = r
            .tmdb_id
            .is_some_and(|t| self.requested.contains(&(r.kind.clone(), t)));
        TitleCard {
            id: labels::title_id(&r.kind, r.tmdb_id),
            kind: r.kind.clone(),
            title: r.title.clone(),
            year: r.year,
            poster: labels::poster(r.poster_url.as_deref()),
            rating: r.rating,
            state: Some(
                labels::card_state(
                    &r.kind,
                    r.copies,
                    r.have.unwrap_or(0),
                    r.episodes.unwrap_or(0),
                    self.motion(r.id),
                    requested,
                )
                .to_string(),
            ),
        }
    }

    /// The card of an engine's title, or a bare card with what the caller knows when the
    /// catalog does not hold it.
    pub fn card_for(
        &self,
        engine: &str,
        engine_title_id: i64,
        kind: &str,
        fallback: &str,
    ) -> TitleCard {
        match self
            .title_of(engine, engine_title_id)
            .and_then(|id| self.card(id))
        {
            Some(c) => c,
            None => TitleCard {
                id: None,
                kind: kind.to_string(),
                title: fallback.to_string(),
                year: None,
                poster: None,
                rating: None,
                state: None,
            },
        }
    }

    /// The engine's profile of a title, when an engine of it names one.
    pub fn profile(&self, title_id: i64, engine: Option<&str>) -> Option<String> {
        let r = self.rows.get(&title_id)?;
        r.links
            .iter()
            .find(|l| engine.is_none_or(|e| l.engine == e))
            .or_else(|| r.links.first())
            .and_then(|l| l.profile.clone())
    }

    /// The lane a title is scored in, by the engine that owns a file of it.
    pub fn lane(&self, title_id: i64, engine: Option<&str>) -> Option<String> {
        let r = self.rows.get(&title_id)?;
        r.links
            .iter()
            .find(|l| engine.is_none_or(|e| l.engine == e))
            .or_else(|| r.links.first())
            .and_then(|l| l.lane.clone())
    }
}
