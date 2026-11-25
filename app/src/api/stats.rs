//! The `/api/v1/stats` routes: play statistics, read from the plays the plays module keeps (the
//! media server's sessions and history, and Tautulli, one row per play).
//!
//! `GET /stats/overview` is the Statistics page's head: totals, plays and hours per day split
//! by delivery, plays by hour and weekday, the most played titles, the people, platforms and
//! players, the shares of delivery and network, and the most streams at one time.
//! `GET /stats/plays` is the history, newest first, paged with `before`; `GET /stats/people`
//! and `/stats/people/{id}` are the people. Days are the days of the workspace's time zone.
//!
//! Who watched what is the people's own business: every route here is the owner's alone. The
//! title record carries a title's counts for every caller and its newest plays for the owner
//! ([`title_plays`]).

use super::pages::cards::Cards;
use super::pages::model::{EpisodeLine, TitleCard};
use super::routes::{ApiPath, ApiQuery};
use super::state::AppState;
use crate::catalog::store as catalog_store;
use crate::core::auth::Caller;
use crate::core::error::{AppError, AppResult};
use crate::core::schema::ProblemDoc;
use crate::metadata::record::{EpisodeRef, PlayPerson, TitlePlay, TitlePlays};
use crate::plays::model::{Filter, Play, decision_of, kinds_of};
use crate::plays::store::{self, Day, NameCount, PersonCount, TitleCount, Totals};
use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::{IntoParams, OpenApi, ToSchema};

type ApiResult<T> = Result<Json<T>, AppError>;

/// Rows of each ranked list.
const TOP: i64 = 10;
/// Plays a page of the history holds unless asked otherwise, and at most.
const PAGE: i64 = 50;
const PAGE_MAX: i64 = 500;
/// The newest plays a person's record and a title record carry.
const RECENT: i64 = 20;
const TITLE_RECENT: i64 = 10;
/// A range of every day longer than this is drawn by month.
const DAYS_BY_DAY: i64 = 400;

/// Who may read play statistics: the owner.
fn owner_only(caller: &Caller) -> AppResult<()> {
    match caller {
        Caller::Owner { .. } => Ok(()),
        Caller::Key { .. } => Err(AppError::Forbidden(
            "Play statistics are the owner's: a key cannot read them.".to_string(),
        )),
    }
}

/// The days of a range: `7`, `30`, `90`, `365` or `all`; any whole number of days up to ten
/// years is taken too. Default 30. `None`: every day.
fn days_of(text: Option<&str>) -> AppResult<Option<i64>> {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(Some(30)),
        Some("all") => Ok(None),
        Some(t) => t
            .parse::<i64>()
            .ok()
            .filter(|d| (1..=3650).contains(d))
            .map(Some)
            .ok_or_else(|| {
                AppError::bad_request(format!("`days` is 7, 30, 90, 365 or all, not `{t}`"))
            }),
    }
}

fn kinds(text: Option<&str>) -> AppResult<Vec<String>> {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(Vec::new()),
        Some(t) => kinds_of(t).ok_or_else(|| {
            AppError::bad_request(format!("`kind` is movie, show or music, not `{t}`"))
        }),
    }
}

/// The workspace's time zone, by whose days plays are counted.
fn zone(state: &AppState) -> String {
    state
        .settings
        .text("general.timeZone")
        .unwrap_or_else(|| "UTC".to_string())
}

fn hours(seconds: i64) -> f64 {
    (seconds as f64 / 360.0).round() / 10.0
}

// Shapes ------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OverviewQuery {
    /// `7`, `30`, `90`, `365` or `all`. Default 30.
    pub days: Option<String>,
    /// `movie`, `show` or `music`.
    pub kind: Option<String>,
    /// A library by its id: `plex-1`, `<media server>-<section>`.
    pub library: Option<String>,
    /// A person by the media server's account id.
    pub person: Option<i64>,
}

/// What a page of statistics covers.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatsRange {
    /// Null for every day.
    pub days: Option<i64>,
    /// The start of the first day. Null for every day.
    pub since: Option<DateTime<Utc>>,
    pub today: NaiveDate,
    /// The time zone the days are counted in.
    pub time_zone: String,
    /// When the first play of the range started.
    pub first: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct StatsTotals {
    pub plays: i64,
    pub hours: f64,
    pub people: i64,
    pub titles: i64,
}

/// One bar of the plays per day: a day, or the first day of its week or month.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeriesPoint {
    pub at: NaiveDate,
    pub plays: i64,
    pub hours: f64,
    pub direct_play: i64,
    pub direct_stream: i64,
    pub transcode: i64,
    /// Plays whose delivery no source told.
    pub other: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Series {
    /// `day` or `month`.
    pub bucket: String,
    pub points: Vec<SeriesPoint>,
}

/// A title by its plays.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TopTitle {
    pub title: TitleCard,
    pub plays: i64,
    pub hours: f64,
    pub people: i64,
    pub last: DateTime<Utc>,
}

/// A person by their plays.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatsPerson {
    /// The media server's account id.
    pub id: Option<i64>,
    pub name: Option<String>,
    pub plays: i64,
    pub hours: f64,
    pub first: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Players they played on.
    pub devices: i64,
}

/// A platform or a player by its plays.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TopName {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    pub plays: i64,
    pub hours: f64,
    pub last: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Top {
    pub movies: Vec<TopTitle>,
    pub shows: Vec<TopTitle>,
    pub music: Vec<TopTitle>,
    pub people: Vec<StatsPerson>,
    pub platforms: Vec<TopName>,
    pub players: Vec<TopName>,
}

/// Plays by delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryShare {
    pub direct_play: i64,
    pub direct_stream: i64,
    pub transcode: i64,
    pub other: i64,
}

/// Plays by where the player was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct NetworkShare {
    pub home: i64,
    pub outside: i64,
    pub other: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Concurrent {
    pub streams: i64,
    /// When that many first played at once.
    pub at: DateTime<Utc>,
}

/// A source plays are read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct StatsSource {
    /// The integration id.
    pub id: String,
    /// `server` (sessions and history) or `history` (Tautulli).
    pub kind: String,
    pub name: String,
}

/// `GET /api/v1/stats/overview`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Overview {
    pub range: StatsRange,
    pub sources: Vec<StatsSource>,
    pub totals: StatsTotals,
    pub series: Series,
    /// Plays by hour of the day, 0 to 23.
    pub hours: Vec<i64>,
    /// Plays by weekday, Monday first.
    pub weekdays: Vec<i64>,
    /// Plays by weekday (Monday first) and hour.
    pub grid: Vec<Vec<i64>>,
    pub top: Top,
    pub delivery: DeliveryShare,
    pub network: NetworkShare,
    /// The most streams at one time. Null when no play's stop is known.
    pub concurrent: Option<Concurrent>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PlaysQuery {
    /// Plays a page holds: 50, at most 500.
    pub limit: Option<i64>,
    /// The `next` of the page before: plays older than that one.
    pub before: Option<i64>,
    pub person: Option<i64>,
    /// `movie`, `show` or `music`.
    pub kind: Option<String>,
    /// A title by its product id: `movie-603`, `show-1396`.
    pub title: Option<String>,
    /// `direct-play`, `direct-stream` or `transcode`.
    pub decision: Option<String>,
    pub platform: Option<String>,
    /// `7`, `30`, `90`, `365` or `all`. Default all.
    pub days: Option<String>,
    pub library: Option<String>,
}

/// A resolution and a bitrate (kbit/s).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct Format {
    pub resolution: Option<String>,
    pub bitrate: Option<i64>,
}

/// How a play was delivered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlayDelivery {
    /// `direct play`, `direct stream` or `transcode`.
    pub decision: Option<String>,
    pub video: Option<String>,
    pub audio: Option<String>,
    /// The file, and what was sent.
    pub source: Option<Format>,
    pub stream: Option<Format>,
}

/// One play of the history.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlayView {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub stopped: Option<DateTime<Utc>>,
    pub watched_seconds: Option<i32>,
    pub paused_seconds: Option<i32>,
    /// 0 to 100.
    pub progress: Option<f64>,
    pub person: PlayPerson,
    /// The movie, or an episode's show.
    pub title: TitleCard,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<EpisodeLine>,
    pub player: Option<String>,
    pub device: Option<String>,
    pub platform: Option<String>,
    pub product: Option<String>,
    /// `home` or `outside`.
    pub network: Option<String>,
    /// Only while the setting keeps addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    pub delivery: PlayDelivery,
    /// Who told of it: `live`, `server`, `tautulli`.
    pub sources: Vec<String>,
    /// It plays now.
    pub playing: bool,
}

/// `GET /api/v1/stats/plays`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PlaysPage {
    pub plays: Vec<PlayView>,
    /// What to send as `before` for the next page. Null on the last page.
    pub next: Option<i64>,
    /// Plays the filters match, over every page.
    pub total: i64,
    pub limit: i64,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PeopleQuery {
    /// `7`, `30`, `90`, `365` or `all`. Default 30.
    pub days: Option<String>,
}

/// `GET /api/v1/stats/people`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PeoplePage {
    pub range: StatsRange,
    pub people: Vec<StatsPerson>,
}

/// `GET /api/v1/stats/people/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PersonStats {
    pub person: StatsPerson,
    pub range: StatsRange,
    pub series: Series,
    pub delivery: DeliveryShare,
    pub network: NetworkShare,
    /// What they played most, movies and shows together.
    pub top: Vec<TopTitle>,
    /// The players they played on.
    pub devices: Vec<TopName>,
    pub recent: Vec<PlayView>,
}

// Building ------------------------------------------------------------------------------------------

fn card(
    cards: &Cards,
    title_id: Option<i64>,
    kind: &str,
    title: &str,
    year: Option<i32>,
) -> TitleCard {
    title_id
        .and_then(|id| cards.card(id))
        .unwrap_or_else(|| TitleCard {
            id: None,
            kind: match kind {
                "episode" => "show".to_string(),
                other => other.to_string(),
            },
            title: title.to_string(),
            year,
            poster: None,
            rating: None,
            state: None,
        })
}

fn top_title(cards: &Cards, t: &TitleCount) -> TopTitle {
    TopTitle {
        title: card(cards, t.title_id, &t.kind, &t.title, t.year),
        plays: t.plays,
        hours: hours(t.watched),
        people: t.people,
        last: t.last,
    }
}

fn person(p: &PersonCount) -> StatsPerson {
    StatsPerson {
        id: p.account_id,
        name: p.name.clone(),
        plays: p.plays,
        hours: hours(p.watched),
        first: p.first,
        last_seen: p.last,
        devices: p.devices,
    }
}

fn top_name(n: &NameCount) -> TopName {
    TopName {
        name: n.name.clone(),
        platform: n.platform.clone(),
        product: n.product.clone(),
        plays: n.plays,
        hours: hours(n.watched),
        last: n.last,
    }
}

fn shares(t: &Totals) -> (DeliveryShare, NetworkShare) {
    (
        DeliveryShare {
            direct_play: t.direct_play,
            direct_stream: t.direct_stream,
            transcode: t.transcode,
            other: (t.plays - t.direct_play - t.direct_stream - t.transcode).max(0),
        },
        NetworkShare {
            home: t.home,
            outside: t.outside,
            other: (t.plays - t.home - t.outside).max(0),
        },
    )
}

/// The bars of a range: one per day from its first day to today, days without plays at zero;
/// a range of every day longer than [`DAYS_BY_DAY`] days is drawn by month.
pub fn series(rows: &[Day], first: Option<NaiveDate>, today: NaiveDate) -> Series {
    let point = |at: NaiveDate| SeriesPoint {
        at,
        plays: 0,
        hours: 0.0,
        direct_play: 0,
        direct_stream: 0,
        transcode: 0,
        other: 0,
    };
    let start = first
        .or_else(|| rows.first().map(|r| r.day))
        .unwrap_or(today)
        .min(today);
    let by_month = (today - start).num_days() > DAYS_BY_DAY;
    let bucket = |d: NaiveDate| {
        if by_month {
            d.with_day(1).unwrap_or(d)
        } else {
            d
        }
    };
    let mut points: BTreeMap<NaiveDate, (SeriesPoint, i64)> = BTreeMap::new();
    let mut d = start;
    while d <= today {
        points
            .entry(bucket(d))
            .or_insert_with(|| (point(bucket(d)), 0));
        d = d.succ_opt().unwrap_or(today + chrono::Duration::days(1));
    }
    for r in rows {
        let (p, watched) = points
            .entry(bucket(r.day))
            .or_insert_with(|| (point(bucket(r.day)), 0));
        p.plays += r.plays;
        p.direct_play += r.direct_play;
        p.direct_stream += r.direct_stream;
        p.transcode += r.transcode;
        p.other += (r.plays - r.direct_play - r.direct_stream - r.transcode).max(0);
        *watched += r.watched;
    }
    Series {
        bucket: if by_month { "month" } else { "day" }.to_string(),
        points: points
            .into_values()
            .map(|(mut p, watched)| {
                p.hours = hours(watched);
                p
            })
            .collect(),
    }
}

fn delivery(p: &Play) -> PlayDelivery {
    let r = &p.record;
    let format = |resolution: &Option<String>, bitrate: Option<i64>| {
        (resolution.is_some() || bitrate.is_some()).then(|| Format {
            resolution: resolution.clone(),
            bitrate,
        })
    };
    PlayDelivery {
        decision: r.decision.clone(),
        video: r.video_decision.clone(),
        audio: r.audio_decision.clone(),
        source: format(&r.source_resolution, r.source_bitrate),
        stream: format(&r.stream_resolution, r.stream_bitrate),
    }
}

fn network(local: Option<bool>) -> Option<String> {
    local.map(|l| if l { "home" } else { "outside" }.to_string())
}

fn play_view(cards: &Cards, p: &Play) -> PlayView {
    let r = &p.record;
    let episode = r.kind == "episode";
    PlayView {
        id: p.id,
        at: r.started_at,
        stopped: r.stopped_at,
        watched_seconds: r.watched_secs,
        paused_seconds: r.paused_secs,
        progress: r.progress,
        person: PlayPerson {
            id: r.account_id,
            name: r.account_name.clone(),
        },
        title: card(
            cards,
            r.title_id,
            &r.kind,
            if episode {
                r.show_title.as_deref().unwrap_or(&r.title)
            } else {
                &r.title
            },
            r.year,
        ),
        episode: match (episode, r.season, r.episode) {
            (true, Some(season), Some(number)) => Some(EpisodeLine {
                season,
                number,
                title: Some(r.title.clone()),
            }),
            _ => None,
        },
        player: r.player.clone(),
        device: r.device.clone(),
        platform: r.platform.clone(),
        product: r.product.clone(),
        network: network(r.local),
        address: r.address.clone(),
        delivery: delivery(p),
        sources: r
            .sources()
            .words()
            .into_iter()
            .map(str::to_string)
            .collect(),
        playing: r.session_id.is_some() && r.stopped_at.is_none(),
    }
}

async fn plays_views(state: &AppState, plays: &[Play]) -> AppResult<Vec<PlayView>> {
    let ids: Vec<i64> = plays.iter().filter_map(|p| p.record.title_id).collect();
    let cards = Cards::of_titles(&state.pool, &ids).await?;
    Ok(plays.iter().map(|p| play_view(&cards, p)).collect())
}

/// The range of `days`: its start and today, in the workspace's time zone.
async fn range(
    state: &AppState,
    days: Option<i64>,
) -> AppResult<(StatsRange, Option<DateTime<Utc>>)> {
    let zone = zone(state);
    let (since, today) = store::local_range(&state.pool, &zone, days).await?;
    Ok((
        StatsRange {
            days,
            since,
            today,
            time_zone: zone,
            first: None,
        },
        since,
    ))
}

/// The sources the installation reads plays from: each media server, then the play history.
fn sources(state: &AppState) -> Vec<StatsSource> {
    let mut out = Vec::new();
    for server in &state.engines.media_servers {
        if let Some(i) = state.config.integration(server.id()) {
            out.push(StatsSource {
                id: i.id.clone(),
                kind: "server".to_string(),
                name: i.name.clone(),
            });
        }
    }
    if let Some(history) = &state.engines.play_history
        && let Some(i) = state.config.integration(history.id())
    {
        out.push(StatsSource {
            id: i.id.clone(),
            kind: "history".to_string(),
            name: i.name.clone(),
        });
    }
    out
}

/// The kinds of a filter narrowed to one stored kind: `None` when the filter leaves it out.
fn narrowed(f: &Filter, kind: &str) -> Option<Filter> {
    (f.kinds.is_empty() || f.kinds.iter().any(|k| k == kind)).then(|| Filter {
        kinds: vec![kind.to_string()],
        ..f.clone()
    })
}

pub async fn overview_of(state: &AppState, q: &OverviewQuery) -> AppResult<Overview> {
    let days = days_of(q.days.as_deref())?;
    let (mut range, since) = range(state, days).await?;
    let f = Filter {
        since,
        kinds: kinds(q.kind.as_deref())?,
        library: q.library.clone().filter(|l| !l.trim().is_empty()),
        account: q.person,
        ..Filter::default()
    };
    let pool = &state.pool;
    let zone = range.time_zone.clone();
    let totals = store::totals(pool, &f).await?;
    range.first = totals.first;
    let day_rows = store::days(pool, &f, &zone).await?;
    let cells = store::grid(pool, &f, &zone).await?;
    let mut lists: Vec<Vec<TitleCount>> = Vec::new();
    for kind in ["movie", "episode", "track"] {
        lists.push(match narrowed(&f, kind) {
            Some(k) => store::top_titles(pool, &k, TOP).await?,
            None => Vec::new(),
        });
    }
    let ids: Vec<i64> = lists.iter().flatten().filter_map(|t| t.title_id).collect();
    let cards = Cards::of_titles(pool, &ids).await?;
    let mut grid = vec![vec![0i64; 24]; 7];
    for c in &cells {
        if let (Ok(w), Ok(h)) = (usize::try_from(c.weekday - 1), usize::try_from(c.hour))
            && w < 7
            && h < 24
        {
            grid[w][h] += c.plays;
        }
    }
    let (delivery, network) = shares(&totals);
    let first_day = match days {
        Some(_) => Some(range.today - chrono::Duration::days(days.unwrap_or(1) - 1)),
        None => day_rows.first().map(|r| r.day),
    };
    let [movies, shows, music]: [Vec<TitleCount>; 3] = lists
        .try_into()
        .unwrap_or_else(|_| [Vec::new(), Vec::new(), Vec::new()]);
    Ok(Overview {
        sources: sources(state),
        totals: StatsTotals {
            plays: totals.plays,
            hours: hours(totals.watched),
            people: totals.people,
            titles: totals.titles,
        },
        series: series(&day_rows, first_day, range.today),
        hours: (0..24)
            .map(|h| grid.iter().map(|row| row[h]).sum())
            .collect(),
        weekdays: grid.iter().map(|row| row.iter().sum()).collect(),
        grid,
        top: Top {
            movies: movies.iter().map(|t| top_title(&cards, t)).collect(),
            shows: shows.iter().map(|t| top_title(&cards, t)).collect(),
            music: music.iter().map(|t| top_title(&cards, t)).collect(),
            people: store::people(pool, &f, TOP)
                .await?
                .iter()
                .map(person)
                .collect(),
            platforms: store::platforms(pool, &f, TOP)
                .await?
                .iter()
                .map(top_name)
                .collect(),
            players: store::players(pool, &f, TOP)
                .await?
                .iter()
                .map(top_name)
                .collect(),
        },
        delivery,
        network,
        concurrent: store::peak(pool, &f).await?.map(|p| Concurrent {
            streams: p.streams,
            at: p.at,
        }),
        range,
    })
}

/// A title by its product id (`movie-603`) as the catalog's id. A title the catalog does not
/// hold matches no play.
async fn title_filter(state: &AppState, text: &str) -> AppResult<i64> {
    let (kind, tmdb) = text
        .trim()
        .split_once('-')
        .and_then(|(k, n)| Some((k, n.parse::<i64>().ok()?)))
        .filter(|(k, _)| matches!(*k, "movie" | "show"))
        .ok_or_else(|| {
            AppError::bad_request(format!(
                "`title` is a title id such as movie-603 or show-1396, not `{text}`"
            ))
        })?;
    let links = catalog_store::title_links(&state.pool, &[tmdb], &[]).await?;
    Ok(links
        .iter()
        .find(|l| l.kind == kind && l.tmdb_id == Some(tmdb))
        .map(|l| l.id)
        .unwrap_or(-1))
}

pub async fn plays_of(state: &AppState, q: &PlaysQuery) -> AppResult<PlaysPage> {
    let days = match q.days.as_deref() {
        None => None,
        some => days_of(some)?,
    };
    let (_, since) = range(state, days).await?;
    let limit = q.limit.unwrap_or(PAGE).clamp(1, PAGE_MAX);
    let f = Filter {
        since,
        kinds: kinds(q.kind.as_deref())?,
        library: q.library.clone().filter(|l| !l.trim().is_empty()),
        account: q.person,
        title_id: match q.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(t) => Some(title_filter(state, t).await?),
            None => None,
        },
        decision: match q
            .decision
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            Some(d) => Some(
                decision_of(d)
                    .ok_or_else(|| {
                        AppError::bad_request(format!(
                            "`decision` is direct-play, direct-stream or transcode, not `{d}`"
                        ))
                    })?
                    .to_string(),
            ),
            None => None,
        },
        platform: q.platform.clone().filter(|p| !p.trim().is_empty()),
    };
    let (rows, more, total) = store::page(&state.pool, &f, q.before, limit).await?;
    Ok(PlaysPage {
        next: if more {
            rows.last().map(|p| p.id)
        } else {
            None
        },
        plays: plays_views(state, &rows).await?,
        total,
        limit,
    })
}

pub async fn people_of(state: &AppState, q: &PeopleQuery) -> AppResult<PeoplePage> {
    let days = days_of(q.days.as_deref())?;
    let (range, since) = range(state, days).await?;
    let f = Filter {
        since,
        ..Filter::default()
    };
    Ok(PeoplePage {
        people: store::people(&state.pool, &f, 1000)
            .await?
            .iter()
            .map(person)
            .collect(),
        range,
    })
}

pub async fn person_of(state: &AppState, id: i64, q: &PeopleQuery) -> AppResult<PersonStats> {
    let days = days_of(q.days.as_deref())?;
    let (mut range, since) = range(state, days).await?;
    let pool = &state.pool;
    let ever = Filter {
        account: Some(id),
        ..Filter::default()
    };
    let Some(whole) = store::people(pool, &ever, 1).await?.first().map(person) else {
        return Err(AppError::not_found(format!(
            "Nobody with the account {id} played anything."
        )));
    };
    let f = Filter {
        since,
        account: Some(id),
        ..Filter::default()
    };
    let totals = store::totals(pool, &f).await?;
    range.first = totals.first;
    let day_rows = store::days(pool, &f, &range.time_zone).await?;
    let top = store::top_titles(pool, &f, TOP).await?;
    let ids: Vec<i64> = top.iter().filter_map(|t| t.title_id).collect();
    let cards = Cards::of_titles(pool, &ids).await?;
    let (recent, _, _) = store::page(pool, &f, None, RECENT).await?;
    let (delivery, network) = shares(&totals);
    let first_day = match days {
        Some(d) => Some(range.today - chrono::Duration::days(d - 1)),
        None => day_rows.first().map(|r| r.day),
    };
    Ok(PersonStats {
        person: StatsPerson {
            plays: totals.plays,
            hours: hours(totals.watched),
            ..whole
        },
        series: series(&day_rows, first_day, range.today),
        delivery,
        network,
        top: top.iter().map(|t| top_title(&cards, t)).collect(),
        devices: store::players(pool, &f, TOP)
            .await?
            .iter()
            .map(top_name)
            .collect(),
        recent: plays_views(state, &recent).await?,
        range,
    })
}

/// A title's plays for its record: null when the installation reads no media server and no
/// play history; the newest plays for the owner alone.
pub async fn title_plays(
    state: &AppState,
    caller: &Caller,
    title_id: Option<i64>,
) -> AppResult<Option<TitlePlays>> {
    if state.engines.media_servers.is_empty() && state.engines.play_history.is_none() {
        return Ok(None);
    }
    let Some(id) = title_id else {
        return Ok(Some(TitlePlays {
            count: 0,
            people: 0,
            hours: 0.0,
            last: None,
            recent: Vec::new(),
        }));
    };
    let totals = store::title_totals(&state.pool, id).await?;
    let recent = if matches!(caller, Caller::Owner { .. }) && totals.plays > 0 {
        let f = Filter {
            title_id: Some(id),
            ..Filter::default()
        };
        store::page(&state.pool, &f, None, TITLE_RECENT).await?.0
    } else {
        Vec::new()
    };
    Ok(Some(TitlePlays {
        count: totals.plays,
        people: totals.people,
        hours: hours(totals.watched),
        last: totals.last,
        recent: recent
            .iter()
            .map(|p| {
                let r = &p.record;
                TitlePlay {
                    id: p.id,
                    at: r.started_at,
                    person: PlayPerson {
                        id: r.account_id,
                        name: r.account_name.clone(),
                    },
                    episode: match (r.season, r.episode) {
                        (Some(season), Some(number)) if r.kind == "episode" => {
                            Some(EpisodeRef { season, number })
                        }
                        _ => None,
                    },
                    watched_seconds: r.watched_secs,
                    progress: r.progress,
                    player: r.player.clone(),
                    platform: r.platform.clone(),
                    decision: r.decision.clone(),
                }
            })
            .collect(),
    }))
}

// Handlers ------------------------------------------------------------------------------------------

/// The Statistics page's head: totals, plays and hours per day by delivery, plays by hour and
/// weekday, the most played titles, people, platforms and players, the shares of delivery and
/// network, and the most streams at one time. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/overview", tag = "stats", params(OverviewQuery),
    responses((status = 200, body = Overview), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn overview(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<OverviewQuery>,
) -> ApiResult<Overview> {
    owner_only(&caller)?;
    Ok(Json(overview_of(&state, &q).await?))
}

/// Every play, newest first, paged with `before`: who, what, when, where and how. The owner's
/// alone.
#[utoipa::path(get, path = "/api/v1/stats/plays", tag = "stats", params(PlaysQuery),
    responses((status = 200, body = PlaysPage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn plays(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<PlaysQuery>,
) -> ApiResult<PlaysPage> {
    owner_only(&caller)?;
    Ok(Json(plays_of(&state, &q).await?))
}

/// The people who played, the most active first. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/people", tag = "stats", params(PeopleQuery),
    responses((status = 200, body = PeoplePage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn people(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<PeopleQuery>,
) -> ApiResult<PeoplePage> {
    owner_only(&caller)?;
    Ok(Json(people_of(&state, &q).await?))
}

/// One person: their plays per day, what they play most, their players, how their plays were
/// delivered and from where, and their newest plays. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/people/{id}", tag = "stats",
    params(("id" = i64, Path, description = "The media server's account id"), PeopleQuery),
    responses((status = 200, body = PersonStats), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn person_detail(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<PeopleQuery>,
) -> ApiResult<PersonStats> {
    owner_only(&caller)?;
    Ok(Json(person_of(&state, id, &q).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/stats/overview", get(overview))
        .route("/stats/plays", get(plays))
        .route("/stats/people", get(people))
        .route("/stats/people/{id}", get(person_detail))
}

#[derive(OpenApi)]
#[openapi(
    paths(overview, plays, people, person_detail),
    tags((name = "stats", description = "Play statistics: who played what, when, where and how. The owner's alone"))
)]
struct StatsDoc;

pub fn openapi() -> utoipa::openapi::OpenApi {
    StatsDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(text: &str, plays: i64, transcode: i64) -> Day {
        Day {
            day: NaiveDate::parse_from_str(text, "%Y-%m-%d").unwrap(),
            plays,
            watched: plays * 1800,
            direct_play: plays - transcode,
            direct_stream: 0,
            transcode,
        }
    }

    #[test]
    fn the_days_of_a_range_are_all_there_and_a_long_one_is_by_month() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        let rows = [day("2026-11-20", 3, 1), day("2026-11-23", 2, 0)];
        let s = series(&rows, NaiveDate::from_ymd_opt(2026, 11, 17), today);
        assert_eq!((s.bucket.as_str(), s.points.len()), ("day", 7));
        let p = &s.points[3];
        assert_eq!(
            (p.at.day(), p.plays, p.transcode, p.direct_play, p.hours),
            (20, 3, 1, 2, 1.5)
        );
        assert_eq!(s.points[0].plays, 0);
        let long = series(
            &[day("2023-01-05", 1, 0), day("2023-01-20", 2, 2)],
            NaiveDate::from_ymd_opt(2023, 1, 5),
            today,
        );
        assert_eq!(long.bucket, "month");
        assert_eq!((long.points[0].at.day(), long.points[0].plays), (1, 3));
        assert_eq!(long.points[0].transcode, 2);
        assert_eq!(long.points.last().unwrap().at.month(), 11);
    }

    #[test]
    fn the_routes_are_in_the_openapi_document() {
        let doc = serde_json::to_value(crate::api::routes::openapi()).unwrap();
        for path in [
            "/api/v1/stats/overview",
            "/api/v1/stats/plays",
            "/api/v1/stats/people",
            "/api/v1/stats/people/{id}",
        ] {
            assert!(doc["paths"][path]["get"].is_object(), "{path}");
        }
    }

    #[test]
    fn ranges_and_words_are_read_strictly() {
        assert_eq!(days_of(None).unwrap(), Some(30));
        assert_eq!(days_of(Some("all")).unwrap(), None);
        assert_eq!(days_of(Some("365")).unwrap(), Some(365));
        assert!(days_of(Some("0")).is_err() && days_of(Some("a week")).is_err());
        assert!(kinds(Some("book")).is_err());
        assert_eq!(hours(5400), 1.5);
        let key = Caller::Key {
            id: 1,
            name: "k".into(),
            scopes: vec![crate::core::model::Scope::Admin],
        };
        assert!(matches!(owner_only(&key), Err(AppError::Forbidden(_))));
        assert!(
            owner_only(&Caller::Owner {
                email: "o@example.com".into()
            })
            .is_ok()
        );
    }
}
