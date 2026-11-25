//! The library tables: every movie or every show as one row of `{columns, rows}`, in three
//! queries (the titles with their lead copy, where their downloads are, the open requests).

use super::cards;
use super::labels::{self, HeldFacts, Motion};
use super::model::LibraryTable;
use crate::catalog::model::TitleKind;
use crate::catalog::store::{self as catalog_store, LibraryRow};
use crate::core::error::AppResult;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use singularr_core::Policy;
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};

/// The movie table's columns, in the order of `web/data/library-movies.json`.
pub const MOVIE_COLUMNS: [&str; 31] = [
    "id",
    "title",
    "year",
    "poster",
    "rating",
    "imdb",
    "certification",
    "runtime",
    "genres",
    "studio",
    "collection",
    "state",
    "monitored",
    "library",
    "profile",
    "resolution",
    "source",
    "codec",
    "hdr",
    "audio",
    "group",
    "editions",
    "files",
    "size",
    "added",
    "score",
    "cutoffMet",
    "released",
    "collectionId",
    "plays",
    "rt",
];

/// The show table's columns, in the order of `web/data/library-shows.json`.
pub const SHOW_COLUMNS: [&str; 20] = [
    "id",
    "title",
    "year",
    "poster",
    "rating",
    "certification",
    "network",
    "status",
    "genres",
    "seasons",
    "episodes",
    "have",
    "nextAir",
    "library",
    "profile",
    "size",
    "added",
    "monitored",
    "state",
    "plays",
];

fn day(at: Option<DateTime<Utc>>) -> Option<NaiveDate> {
    at.map(|a| a.date_naive())
}

/// One movie as a row of the table.
pub fn movie_row(r: &LibraryRow, motion: Motion, requested: bool, policy: &Policy) -> Vec<Value> {
    let held = HeldFacts::from_json(r.held.as_ref());
    // Held, or on its way; else not out yet, else asked for, else missing.
    let state = if r.files > 0 || motion.any() {
        labels::card_state("movie", r.files, 0, 0, motion, requested)
    } else if labels::unreleased(r.status.as_deref()) {
        "unreleased"
    } else if requested {
        "requested"
    } else {
        "missing"
    };
    let codec = r
        .probe_codec
        .as_deref()
        .and_then(labels::probe_codec)
        .or_else(|| held.video_codec.map(|c| labels::codec(c).to_string()));
    let hdr = r
        .probe_hdr
        .as_deref()
        .and_then(labels::probe_hdr)
        .or_else(|| labels::hdr(&held.hdr));
    let audio = r
        .probe_audio
        .as_deref()
        .and_then(|a| labels::probe_audio(a, r.probe_channels.as_deref()))
        .or_else(|| labels::release_audio(&held));
    let cutoff_met = r.score.and_then(|score| {
        let lane = policy.lane(r.lane.as_deref()?)?;
        Some(score >= lane.cutoff_score)
    });
    let added = r.server_added.or(r.copy_added).or(r.engine_added);
    vec![
        json!(labels::title_id("movie", r.tmdb_id)),
        json!(r.title),
        json!(r.year),
        json!(labels::poster(r.poster_url.as_deref())),
        json!(r.rating),
        json!(r.imdb_rating),
        json!(r.certification),
        json!(r.runtime_minutes),
        json!(r.genres),
        json!(r.studio),
        json!(r.collection),
        json!(state),
        json!(r.monitored),
        json!(r.library),
        json!(r.profile),
        json!(held.resolution.map(labels::lines)),
        json!(held.source.map(labels::source)),
        json!(codec),
        json!(hdr),
        json!(audio),
        json!(held.group),
        json!(r.editions),
        json!(r.files),
        json!(r.size_bytes),
        json!(added),
        json!(r.score),
        json!(cutoff_met),
        json!(day(r.in_cinemas.or(r.digital_release))),
        json!(r.collection_tmdb_id.map(|n| format!("collection-{n}"))),
        json!(r.plays),
        json!(r.critics),
    ]
}

/// One show as a row of the table.
pub fn show_row(r: &LibraryRow, motion: Motion, requested: bool, now: DateTime<Utc>) -> Vec<Value> {
    let episodes = r.episodes.unwrap_or(0);
    let have = r.have.unwrap_or(0);
    // Held (all or part), or on its way; else nothing aired yet, else asked for, else missing.
    let state = if have > 0 || motion.any() {
        labels::card_state("show", i64::from(have), have, episodes, motion, requested)
    } else if episodes == 0
        || r.status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("upcoming"))
    {
        "unaired"
    } else if requested {
        "requested"
    } else {
        "missing"
    };
    vec![
        json!(labels::title_id("show", r.tmdb_id)),
        json!(r.title),
        json!(r.year),
        json!(labels::poster(r.poster_url.as_deref())),
        json!(r.rating),
        json!(r.certification),
        json!(r.network),
        json!(r.status.as_deref().map(str::to_ascii_lowercase)),
        json!(r.genres),
        json!(r.seasons),
        json!(r.episodes),
        json!(r.have),
        json!(day(r.next_airing.filter(|n| *n > now))),
        json!(r.library),
        json!(r.profile),
        json!(r.size_bytes),
        json!(r.server_added.or(r.engine_added)),
        json!(r.monitored),
        json!(state),
        json!(r.plays),
    ]
}

/// The whole table of one kind.
pub async fn table(pool: &PgPool, kind: TitleKind, policy: &Policy) -> AppResult<LibraryTable> {
    let titles = catalog_store::library_rows(pool, kind.as_str()).await?;
    let motions: HashMap<i64, Motion> = cards::motions(pool).await?;
    let tmdb: Vec<i64> = titles.iter().filter_map(|r| r.tmdb_id).collect();
    let requested: HashSet<(String, i64)> = cards::requested(pool, &tmdb).await?;
    let now = Utc::now();
    let is_requested = |r: &LibraryRow| {
        r.tmdb_id
            .is_some_and(|t| requested.contains(&(kind.as_str().to_string(), t)))
    };
    let motion = |r: &LibraryRow| motions.get(&r.id).copied().unwrap_or_default();
    let columns: &[&str] = match kind {
        TitleKind::Movie => MOVIE_COLUMNS.as_slice(),
        TitleKind::Show => SHOW_COLUMNS.as_slice(),
    };
    let rows = titles
        .iter()
        .map(|r| match kind {
            TitleKind::Movie => movie_row(r, motion(r), is_requested(r), policy),
            TitleKind::Show => show_row(r, motion(r), is_requested(r), now),
        })
        .collect();
    Ok(LibraryTable {
        columns: columns.iter().map(|c| c.to_string()).collect(),
        rows,
    })
}
