//! What the API and the web app ask the catalog.

use super::model::{
    CopyQuery, CopyView, EditionHolding, EmptyLane, EpisodeName, FolderScanStatus, KindTotals,
    Library, NamedCount, SeasonFiles, TitleDetail, TitleEdition, TitleKind, TitleQuery,
    TitleSummary,
};
use super::store;
use crate::core::config::{ArrConfig, ArrKind};
use crate::core::error::{AppError, AppResult};
use crate::core::model::{Page, Paging};
use singularr_core::naming::parse_library_name;
use singularr_core::release::{Quality, Resolution};
use singularr_core::{EditionList, Policy};
use sqlx::PgPool;

/// Copies a title's detail carries before it points at the paged list.
pub const DETAIL_COPIES: u32 = 200;

/// A lane as it applies to one library: which kind of title it holds there, and whether it
/// takes 2160p. Built from the policy's `arr_profiles` and the configured arrs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneSlot {
    pub lane: String,
    pub label: String,
    pub library: String,
    pub kind: TitleKind,
    pub uhd: bool,
}

/// Every (lane, library) pair the policy and the arrs define.
pub fn lane_slots(policy: &Policy, arrs: &[ArrConfig]) -> Vec<LaneSlot> {
    let mut slots: Vec<LaneSlot> = Vec::new();
    for lane in &policy.lanes {
        let uhd = lane
            .ladder
            .iter()
            .flatten()
            .filter_map(|label| Quality::from_label(label))
            .any(|q| q.resolution == Resolution::R2160);
        for profile in &lane.arr_profiles {
            let Some((arr_id, _)) = profile.split_once(':') else {
                continue;
            };
            let Some(arr) = arrs.iter().find(|a| a.id.eq_ignore_ascii_case(arr_id)) else {
                continue;
            };
            let slot = LaneSlot {
                lane: lane.id.clone(),
                label: lane.label.clone(),
                library: arr.library.clone(),
                kind: match arr.kind {
                    ArrKind::Radarr => TitleKind::Movie,
                    ArrKind::Sonarr => TitleKind::Show,
                },
                uhd,
            };
            if !slots.contains(&slot) {
                slots.push(slot);
            }
        }
    }
    slots
}

pub async fn libraries(pool: &PgPool) -> AppResult<Vec<Library>> {
    Ok(store::libraries(pool).await?)
}

pub async fn titles(
    pool: &PgPool,
    slots: &[LaneSlot],
    q: &TitleQuery,
) -> AppResult<Page<TitleSummary>> {
    if let Some(kind) = q.kind.as_deref().filter(|k| !k.is_empty())
        && TitleKind::parse(kind).is_none()
    {
        return Err(AppError::bad_request(format!(
            "kind is `movie` or `show`, not `{kind}`"
        )));
    }
    if let Some(sort) = q.sort.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && store::title_sort(sort.strip_prefix('-').unwrap_or(sort)).is_none()
    {
        return Err(AppError::bad_request(format!(
            "sort is one of added, title, year, score, size, copies, next, not `{sort}`"
        )));
    }
    if let Some(order) = q.order.as_deref().filter(|o| !o.is_empty())
        && !["asc", "desc"].contains(&order.to_ascii_lowercase().as_str())
    {
        return Err(AppError::bad_request(format!(
            "order is `asc` or `desc`, not `{order}`"
        )));
    }
    let mut uhd_lanes: Vec<String> = slots
        .iter()
        .filter(|s| s.uhd)
        .map(|s| s.lane.clone())
        .collect();
    uhd_lanes.dedup();
    let edition_key = q.edition.as_deref().and_then(edition_filter_key);
    let paging = Paging::new(q.page, q.per_page);
    let (mut items, total) =
        store::list_titles(pool, q, &uhd_lanes, edition_key.as_deref(), paging).await?;
    let ids: Vec<i64> = items.iter().map(|t| t.id).collect();
    let mut leads = store::lead_copies(pool, &ids).await?;
    for item in &mut items {
        if let Some(at) = leads.iter().position(|l| l.title_id == item.id) {
            item.lead = Some(leads.swap_remove(at));
        }
    }
    Ok(Page::new(items, paging, total))
}

/// The `edition` filter as an edition key: a key, label or alias of the edition list, an
/// `other:` key, or text written like a key (`directors-cut`). `None` for other text, which is
/// then matched against the edition names of the copies.
pub fn edition_filter_key(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    EditionList::shipped().key_for(t).or_else(|| {
        t.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            .then(|| t.to_string())
    })
}

/// A title's editions, each with the files that hold it: its copies and the files in its
/// folders no engine tracks.
pub async fn title_editions(pool: &PgPool, id: i64) -> AppResult<Vec<TitleEdition>> {
    let rows = store::title_editions(pool, id).await?;
    let holdings = store::edition_holdings(pool, id).await?;
    Ok(rows
        .into_iter()
        .map(|row| TitleEdition {
            held: holdings
                .iter()
                .filter(|h| h.edition_key == row.key)
                .map(|h| {
                    let file = h
                        .file_name
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(&h.file_name);
                    EditionHolding {
                        library: h.library_id.clone(),
                        engine: h.engine.clone(),
                        copy_id: h.copy_id,
                        tracked: h.copy_id.is_some(),
                        lane: h.lane.clone(),
                        quality: h.quality.clone(),
                        resolution: h.resolution.clone().or_else(|| {
                            h.quality
                                .as_deref()
                                .and_then(Quality::from_label)
                                .map(|q| q.resolution.label().to_string())
                        }),
                        file_name: h.file_name.clone(),
                        size_bytes: h.size_bytes,
                        plays_as: parse_library_name(file).edition,
                    }
                })
                .collect(),
            key: row.key,
            label: row.label,
            wanted: row.wanted,
            is_default: row.is_default,
            set_by: row.set_by,
        })
        .collect())
}

/// The last scan of the movie folders, if one ran.
pub async fn folder_scan_status(pool: &PgPool) -> AppResult<Option<FolderScanStatus>> {
    Ok(store::folder_scan_status(pool).await?)
}

/// Totals over the titles of one kind, for the head of the Movies and Shows pages.
pub async fn kind_totals(pool: &PgPool, kind: TitleKind) -> AppResult<KindTotals> {
    Ok(store::kind_totals(pool, kind.as_str()).await?)
}

/// What the files of each season of a show are: qualities and groups with their counts,
/// the size and the newest file.
pub async fn season_files(pool: &PgPool, title_id: i64) -> AppResult<Vec<SeasonFiles>> {
    let rows = store::season_files(pool, title_id).await?;
    let mut out: Vec<SeasonFiles> = Vec::new();
    for row in rows {
        let at = match out
            .iter()
            .position(|s| s.season_number == row.season_number)
        {
            Some(at) => at,
            None => {
                out.push(SeasonFiles {
                    season_number: row.season_number,
                    qualities: Vec::new(),
                    groups: Vec::new(),
                    size_bytes: 0,
                    last_added: None,
                });
                out.len() - 1
            }
        };
        let season = &mut out[at];
        season.size_bytes += row.size_bytes;
        season.last_added = season.last_added.max(row.last_added);
        for (list, name) in [
            (&mut season.qualities, row.quality),
            (&mut season.groups, row.release_group),
        ] {
            let name = name.unwrap_or_else(|| "unknown".to_string());
            match list.iter_mut().find(|c| c.name == name) {
                Some(c) => c.count += row.files,
                None => list.push(NamedCount {
                    name,
                    count: row.files,
                }),
            }
        }
    }
    for season in &mut out {
        season
            .qualities
            .sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
        season
            .groups
            .sort_by(|a, b| b.count.cmp(&a.count).then(a.name.cmp(&b.name)));
    }
    Ok(out)
}

/// The names of a show's episodes, read from the library file names.
pub async fn episode_names(
    pool: &PgPool,
    title_id: i64,
    season: Option<i32>,
) -> AppResult<Vec<EpisodeName>> {
    Ok(store::episode_names(pool, title_id, season).await?)
}

/// Copies held per lane, by lane id.
pub async fn lane_copy_counts(pool: &PgPool) -> AppResult<Vec<(String, i64)>> {
    Ok(store::lane_copy_counts(pool).await?)
}

/// Titles guarded by each protection, by protection id.
pub async fn protection_title_counts(pool: &PgPool) -> AppResult<Vec<(String, i64)>> {
    Ok(store::protection_title_counts(pool).await?)
}

/// Whether a title is a movie or a show, for the addresses of the web app.
pub async fn title_kind(pool: &PgPool, id: i64) -> AppResult<String> {
    store::title(pool, id)
        .await?
        .map(|t| t.kind)
        .ok_or_else(|| AppError::not_found(format!("There is no title {id}.")))
}

pub async fn title(pool: &PgPool, slots: &[LaneSlot], id: i64) -> AppResult<TitleDetail> {
    let row = store::title(pool, id)
        .await?
        .ok_or_else(|| AppError::not_found(format!("There is no title {id}.")))?;
    let engines = store::title_engines(pool, id).await?;
    let seasons = store::title_seasons(pool, id).await?;
    let paging = Paging::new(Some(1), Some(DETAIL_COPIES));
    let (copies, copies_total) =
        store::title_copies(pool, id, &CopyQuery::default(), paging).await?;
    let copies: Vec<CopyView> = copies.into_iter().map(|c| c.view()).collect();
    let mut protections: Vec<String> = copies
        .iter()
        .flat_map(|c| c.protections.iter().cloned())
        .collect();
    protections.sort();
    protections.dedup();
    // The lanes for this kind of title, per library, in which the title holds nothing.
    let held = store::title_lanes(pool, id).await?;
    let empty_lanes = slots
        .iter()
        .filter(|s| s.kind.as_str() == row.kind)
        .filter(|s| {
            !held
                .iter()
                .any(|(lane, library)| *lane == s.lane && *library == s.library)
        })
        .map(|s| EmptyLane {
            lane: s.lane.clone(),
            label: s.label.clone(),
            library: s.library.clone(),
            uhd: s.uhd,
        })
        .collect();
    let editions = title_editions(pool, id).await?;
    Ok(TitleDetail {
        id: row.id,
        kind: row.kind,
        title: row.title,
        year: row.year,
        original_language: row.original_language,
        poster_url: row.poster_url,
        status: row.status,
        tmdb_id: row.tmdb_id,
        tvdb_id: row.tvdb_id,
        imdb_id: row.imdb_id,
        about: row.about,
        engines,
        seasons,
        copies,
        copies_total,
        protections,
        empty_lanes,
        editions,
    })
}

pub async fn title_copies(pool: &PgPool, id: i64, q: &CopyQuery) -> AppResult<Page<CopyView>> {
    if store::title(pool, id).await?.is_none() {
        return Err(AppError::not_found(format!("There is no title {id}.")));
    }
    let paging = Paging::new(q.page, q.per_page);
    let (rows, total) = store::title_copies(pool, id, q, paging).await?;
    Ok(Page::new(
        rows.into_iter().map(|c| c.view()).collect(),
        paging,
        total,
    ))
}

pub async fn copy(pool: &PgPool, id: i64) -> AppResult<CopyView> {
    store::copy(pool, id)
        .await?
        .map(|c| c.view())
        .ok_or_else(|| AppError::not_found(format!("There is no copy {id}.")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;
    use crate::policy::ActivePolicy;
    use std::collections::BTreeMap;

    #[test]
    fn the_edition_filter_takes_keys_labels_and_aliases() {
        assert_eq!(
            edition_filter_key("Open Matte").as_deref(),
            Some("open-matte")
        );
        assert_eq!(edition_filter_key("DC").as_deref(), Some("directors-cut"));
        assert_eq!(
            edition_filter_key("4K Remaster").as_deref(),
            Some("remastered")
        );
        assert_eq!(
            edition_filter_key("theatrical").as_deref(),
            Some("theatrical")
        );
        assert_eq!(
            edition_filter_key("other:taco-bell-cut").as_deref(),
            Some("other:taco-bell-cut")
        );
        assert_eq!(edition_filter_key("Some Words"), None);
        assert_eq!(edition_filter_key("  "), None);
    }

    #[test]
    fn lanes_are_placed_in_libraries_by_the_arrs_they_stand_for() {
        let config = Config::from(&BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("RADARR_URL", "http://10.0.0.1:7878"),
            ("SONARR_URL", "http://10.0.0.1:8989"),
        ]))
        .unwrap();
        let slots = lane_slots(&ActivePolicy::builtin().policy, &config.arrs);
        let find = |lane: &str| slots.iter().find(|s| s.lane == lane).unwrap();
        assert_eq!(slots.len(), 4);
        let uhd = find("movies-uhd");
        assert_eq!(
            (uhd.library.as_str(), uhd.kind, uhd.uhd),
            ("radarr", TitleKind::Movie, true)
        );
        let hd = find("movies-hd");
        assert!(!hd.uhd);
        let shows = find("shows-hd");
        assert_eq!(
            (shows.library.as_str(), shows.kind, shows.uhd),
            ("sonarr", TitleKind::Show, false)
        );
        assert!(find("shows-uhd").uhd);
        // Without arr integrations no lane has a place.
        let bare = Config::from(&BTreeMap::from([(
            "DATABASE_URL",
            "postgres://x@127.0.0.1:1/x",
        )]))
        .unwrap();
        assert!(lane_slots(&ActivePolicy::builtin().policy, &bare.arrs).is_empty());
    }
}
