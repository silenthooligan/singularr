//! The `/api/v1/stats` routes: play statistics, read from the plays the plays module keeps (every
//! media server's sessions, a server's own history, and Tautulli, one row per play).
//!
//! - `GET /stats/overview`: the head figures. Totals, plays and hours per day split by
//!   delivery, plays by hour and weekday, the most played titles, the people, platforms and
//!   players, the shares of delivery and network, the most streams at one time.
//! - `GET /stats/plays`: the history. Every play with who, what, where (address and location),
//!   on which player and how it was delivered; newest first and paged with `before`, or in the
//!   order of any column and paged with `page`.
//! - `GET /stats/people`, `/stats/people/{id}`: the people, each with their last address,
//!   location and player; one person with their players, addresses and hours.
//! - `GET /stats/libraries`, `/stats/players`, `/stats/graphs`: the plays by library; by app,
//!   platform and player; and the graphs (days, hours, weekdays, delivery, resolutions,
//!   platforms, people, servers, the most streams at once each day, why the servers
//!   transcoded).
//! - `GET /stats/options`: what the history's filters choose from, and the location database
//!   in use with its credit.
//!
//! Every route takes the same filters: a range of days or dates, a kind, a library, a person, a
//! media server, a player, an app, a platform, a delivery. Days are the days of the
//! workspace's time zone.
//!
//! Who watched what, and from where, is the people's own business: every route here is the
//! owner's alone. A location is made on this machine from the address (`core::geoip`). The
//! title record carries a title's counts for every caller and its newest plays for the owner
//! ([`title_plays`]).

use super::pages::cards::Cards;
use super::pages::model::{EpisodeLine, TitleCard};
use super::routes::{ApiPath, ApiQuery};
use super::state::AppState;
use crate::catalog::store as catalog_store;
use crate::core::auth::Caller;
use crate::core::error::{AppError, AppResult};
use crate::core::geoip::{self, Place};
use crate::core::schema::ProblemDoc;
use crate::engines::Delivery;
use crate::metadata::record::{EpisodeRef, PlayPerson, TitlePlay, TitlePlays};
use crate::plays::model::{Filter, Play, SortKey, decision_of, kinds_of};
use crate::plays::store::{
    self, AddressCount, Day, LabelCount, NameCount, PersonCount, TitleCount, Totals,
};
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
/// Rows of the lists of the players page.
const MANY: i64 = 100;
/// Plays a page of the history holds unless asked otherwise, and at most.
const PAGE: i64 = 50;
const PAGE_MAX: i64 = 500;
/// The newest plays a person's record and a title record carry.
const RECENT: i64 = 20;
const TITLE_RECENT: i64 = 10;
/// The addresses a person's record lists.
const ADDRESSES: i64 = 50;
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

/// A text of a filter: trimmed, `None` when empty.
fn word(text: &Option<String>) -> Option<String> {
    text.as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

// Shapes ------------------------------------------------------------------------------------------

/// What every statistics route takes: the range and the filters.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatsQuery {
    /// `7`, `30`, `90`, `365` or `all`. Default 30; the history's default is all.
    pub days: Option<String>,
    /// The first and the last day, `YYYY-MM-DD` in the workspace's time zone, in place of
    /// `days`. Either may be left out.
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    /// `movie`, `show` or `music`.
    pub kind: Option<String>,
    /// A library by its id: `plex-1`, `<media server>-<section>`.
    pub library: Option<String>,
    /// A person by the media server's account id.
    pub person: Option<i64>,
    /// A media server by its integration id.
    pub server: Option<String>,
    /// A player by its name.
    pub player: Option<String>,
    /// A player app: `Plex for Android (TV)`.
    pub product: Option<String>,
    pub platform: Option<String>,
    /// `direct-play`, `direct-stream` or `transcode`.
    pub decision: Option<String>,
    /// A title by its product id: `movie-603`, `show-1396`.
    pub title: Option<String>,
    /// A client address.
    pub address: Option<String>,
    /// The history: plays a page holds (50, at most 500).
    pub limit: Option<i64>,
    /// The history, newest first: the `next` of the page before.
    pub before: Option<i64>,
    /// The history in the order of a column: `date`, `person`, `title`, `server`, `player`,
    /// `platform`, `product`, `delivery`, `quality`, `duration`, `paused`, `progress`,
    /// `address`. Newest first (`date`, `desc`) is paged with `before`, any other order with
    /// `page`.
    pub sort: Option<String>,
    /// `asc` or `desc`. Default `desc`.
    pub dir: Option<String>,
    /// The page of a sorted history, from 1.
    pub page: Option<i64>,
}

pub type OverviewQuery = StatsQuery;
pub type PlaysQuery = StatsQuery;
pub type PeopleQuery = StatsQuery;

/// What a page of statistics covers.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StatsRange {
    /// Null for every day and for a range of dates.
    pub days: Option<i64>,
    /// The start of the first day. Null for every day.
    pub since: Option<DateTime<Utc>>,
    /// The end of the last day of a range of dates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// The first and the last day of a range of dates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<NaiveDate>,
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

/// A media server, as a play names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlayServer {
    /// The integration id.
    pub id: String,
    /// `plex`, `jellyfin`, `emby`.
    pub kind: Option<String>,
    pub name: String,
}

/// Where an address is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaceView {
    /// `Local network`, `Paris, Île-de-France, France`. Null when the database does not know
    /// the address, or there is no database.
    pub label: Option<String>,
    /// On a private, loopback or link-local network: no lookup is made.
    pub local: bool,
    pub city: Option<String>,
    pub region: Option<String>,
    pub country: Option<String>,
    /// ISO 3166-1 alpha-2.
    pub country_code: Option<String>,
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
    /// Addresses they played from.
    pub addresses: i64,
    /// Their newest play's address and its location, player, app, platform, media server and
    /// title.
    pub last_address: Option<String>,
    pub last_location: Option<PlaceView>,
    pub last_player: Option<String>,
    pub last_product: Option<String>,
    pub last_platform: Option<String>,
    pub last_server: Option<PlayServer>,
    pub last_title: Option<String>,
}

/// A platform, a player, an app or a media server by its plays.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct TopName {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    /// An app's newest version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub plays: i64,
    pub hours: f64,
    pub people: i64,
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

/// A file, or what was sent of it: resolution, bitrate (kbit/s), codecs, audio channels,
/// container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Format {
    pub resolution: Option<String>,
    pub bitrate: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_codec: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_channels: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
}

impl Format {
    fn of(
        resolution: &Option<String>,
        bitrate: Option<i64>,
        video_codec: &Option<String>,
        audio_codec: &Option<String>,
        audio_channels: Option<i32>,
        container: &Option<String>,
    ) -> Option<Format> {
        let any = resolution.is_some()
            || bitrate.is_some()
            || video_codec.is_some()
            || audio_codec.is_some()
            || audio_channels.is_some()
            || container.is_some();
        any.then(|| Format {
            resolution: resolution.clone(),
            bitrate,
            video_codec: video_codec.clone(),
            audio_codec: audio_codec.clone(),
            audio_channels,
            container: container.clone(),
        })
    }
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
    /// Why the server transcoded, in its words, where it says (Jellyfin and Emby do).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// How a session of a media server is delivered, as the sessions route and Home show it.
pub fn session_delivery(d: &Delivery, decision: &str) -> PlayDelivery {
    PlayDelivery {
        decision: Some(decision.to_string()).filter(|d| !d.is_empty()),
        video: d.video.clone(),
        audio: d.audio.clone(),
        source: Format::of(
            &d.source_resolution,
            d.source_bitrate,
            &d.source_video_codec,
            &d.source_audio_codec,
            d.source_audio_channels,
            &d.source_container,
        ),
        stream: Format::of(
            &d.stream_resolution,
            d.stream_bitrate,
            &d.stream_video_codec,
            &d.stream_audio_codec,
            d.stream_audio_channels,
            &d.stream_container,
        ),
        reasons: d.transcode_reasons.clone(),
    }
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
    /// The media server it played on.
    pub server: PlayServer,
    pub player: Option<String>,
    pub device: Option<String>,
    pub platform: Option<String>,
    pub platform_version: Option<String>,
    pub product: Option<String>,
    pub product_version: Option<String>,
    /// The device's own id on the media server.
    pub device_id: Option<String>,
    /// `home` or `outside`.
    pub network: Option<String>,
    /// The client's address, while the setting keeps addresses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// Where the address is, made on this machine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<PlaceView>,
    /// The connection was encrypted.
    pub secure: Option<bool>,
    /// The server's vendor relayed the connection (Plex).
    pub relayed: Option<bool>,
    /// What the stream took, kbit/s.
    pub bandwidth: Option<i64>,
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
    /// What to send as `before` for the next page, newest first. Null on the last page, and
    /// for a sorted history.
    pub next: Option<i64>,
    /// Plays the filters match, over every page.
    pub total: i64,
    pub limit: i64,
    /// The page of a sorted history.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<i64>,
}

/// `GET /api/v1/stats/people`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PeoplePage {
    pub range: StatsRange,
    pub people: Vec<StatsPerson>,
}

/// An address a person played from.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AddressView {
    pub address: String,
    pub location: Option<PlaceView>,
    pub plays: i64,
    /// The player last seen there.
    pub player: Option<String>,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
}

/// Plays and hours of one hour of the day or one weekday.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Slot {
    pub plays: i64,
    pub hours: f64,
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
    /// Their platforms and apps.
    pub platforms: Vec<TopName>,
    pub products: Vec<TopName>,
    /// The addresses they played from, the newest first, each with its location.
    pub addresses: Vec<AddressView>,
    /// Plays by hour of the day (0 to 23) and by weekday (Monday first).
    pub hours: Vec<Slot>,
    pub weekdays: Vec<Slot>,
    pub recent: Vec<PlayView>,
}

/// A library by its plays.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryStats {
    /// `<media server>-<section>`.
    pub id: String,
    pub name: String,
    /// `movie`, `show`, `music`, `homevideo`.
    pub kind: Option<String>,
    pub server: Option<PlayServer>,
    pub plays: i64,
    pub people: i64,
    pub hours: f64,
    pub last: Option<DateTime<Utc>>,
    pub last_title: Option<String>,
}

/// `GET /api/v1/stats/libraries`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LibrariesPage {
    pub range: StatsRange,
    pub libraries: Vec<LibraryStats>,
}

/// `GET /api/v1/stats/players`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct PlayersPage {
    pub range: StatsRange,
    /// The player apps, each with its newest version.
    pub products: Vec<TopName>,
    pub platforms: Vec<TopName>,
    /// The players, by the names people gave their devices.
    pub players: Vec<TopName>,
}

/// A label by its plays: a resolution, a reason.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct LabelStat {
    pub label: String,
    pub plays: i64,
    pub hours: f64,
}

/// Plays by the file's resolution and by the resolution sent.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Resolutions {
    pub source: Vec<LabelStat>,
    pub stream: Vec<LabelStat>,
}

/// The most streams at one time on a day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct DayStreams {
    pub at: NaiveDate,
    pub streams: i64,
}

/// A media server by its plays.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ServerStats {
    pub server: PlayServer,
    pub plays: i64,
    pub hours: f64,
    pub people: i64,
}

/// `GET /api/v1/stats/graphs`.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Graphs {
    pub range: StatsRange,
    /// Plays and hours per day (or month), by delivery.
    pub series: Series,
    /// Plays and hours by hour of the day (0 to 23) and by weekday (Monday first).
    pub hours: Vec<Slot>,
    pub weekdays: Vec<Slot>,
    pub delivery: DeliveryShare,
    pub resolutions: Resolutions,
    pub platforms: Vec<TopName>,
    pub people: Vec<StatsPerson>,
    pub servers: Vec<ServerStats>,
    /// The most streams at one time, day by day.
    pub streams: Vec<DayStreams>,
    /// Why the servers transcoded, where they say.
    pub reasons: Vec<LabelStat>,
}

/// The location database in use, with the credit its license asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct LocationSource {
    /// The database's file, null when there is none: addresses are then shown alone.
    pub database: Option<String>,
    /// When the database was built.
    pub built: Option<DateTime<Utc>>,
    pub credit: String,
    pub url: String,
}

/// A library a filter names, by its id and its name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct LibraryChoice {
    /// `<media server>-<section>`.
    pub id: String,
    pub name: String,
}

/// `GET /api/v1/stats/options`: what the history's filters choose from.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct StatsOptions {
    pub servers: Vec<PlayServer>,
    /// The media servers' libraries, for the name of the library a view is narrowed to.
    pub libraries: Vec<LibraryChoice>,
    pub people: Vec<PlayPerson>,
    pub players: Vec<String>,
    pub products: Vec<String>,
    pub platforms: Vec<String>,
    pub locations: LocationSource,
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

fn place_view(p: Place) -> PlaceView {
    PlaceView {
        label: p.label(),
        local: p.local,
        city: p.city,
        region: p.region,
        country: p.country,
        country_code: p.country_code,
    }
}

/// Whether the players' addresses are kept. Off, no answer carries an address or a location,
/// whatever a play still holds.
fn addresses_kept(state: &AppState) -> bool {
    crate::plays::service::keep_addresses(&state.settings)
}

/// An address an answer may carry: none while the setting keeps no address.
fn shown(state: &AppState, address: Option<&str>) -> Option<String> {
    address
        .filter(|_| addresses_kept(state))
        .map(str::to_string)
}

/// Where an address is, from the location database in use. None while the setting keeps no
/// address.
fn locate(state: &AppState, address: Option<&str>) -> Option<PlaceView> {
    address
        .filter(|_| addresses_kept(state))
        .and_then(|a| state.engines.geoip.locate(a))
        .map(place_view)
}

/// A media server by its integration id: its name and kind from the configuration, the kind a
/// play recorded where the configuration no longer lists it.
fn server_of(state: &AppState, id: &str, kind: Option<&str>) -> PlayServer {
    let integration = state.config.integration(id);
    PlayServer {
        id: id.to_string(),
        kind: kind
            .map(str::to_string)
            .or_else(|| integration.map(|i| i.kind.as_str().to_string())),
        name: integration
            .map(|i| i.name.clone())
            .unwrap_or_else(|| id.to_string()),
    }
}

fn person(state: &AppState, p: &PersonCount) -> StatsPerson {
    StatsPerson {
        id: p.account_id,
        name: p.name.clone(),
        plays: p.plays,
        hours: hours(p.watched),
        first: p.first,
        last_seen: p.last,
        devices: p.devices,
        addresses: if addresses_kept(state) {
            p.addresses
        } else {
            0
        },
        last_location: locate(state, p.last_address.as_deref()),
        last_address: shown(state, p.last_address.as_deref()),
        last_player: p.last_player.clone(),
        last_product: p.last_product.clone(),
        last_platform: p.last_platform.clone(),
        last_server: p.last_server.as_deref().map(|s| server_of(state, s, None)),
        last_title: p.last_title.clone(),
    }
}

fn top_name(n: &NameCount) -> TopName {
    TopName {
        name: n.name.clone(),
        platform: n.platform.clone(),
        product: n.product.clone(),
        version: None,
        plays: n.plays,
        hours: hours(n.watched),
        people: n.people,
        last: n.last,
    }
}

/// An app with its newest version: the store names the version in the app's place.
fn top_app(n: &NameCount) -> TopName {
    TopName {
        product: None,
        version: n.product.clone(),
        ..top_name(n)
    }
}

fn label_stat(l: &LabelCount) -> LabelStat {
    LabelStat {
        label: l.label.clone(),
        plays: l.plays,
        hours: hours(l.watched),
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

/// The bars of a range: one per day from its first day to its last, days without plays at
/// zero; a range longer than [`DAYS_BY_DAY`] days is drawn by month.
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
    PlayDelivery {
        decision: r.decision.clone(),
        video: r.video_decision.clone(),
        audio: r.audio_decision.clone(),
        source: Format::of(
            &r.source_resolution,
            r.source_bitrate,
            &r.source_video_codec,
            &r.source_audio_codec,
            r.source_audio_channels,
            &r.source_container,
        ),
        stream: Format::of(
            &r.stream_resolution,
            r.stream_bitrate,
            &r.stream_video_codec,
            &r.stream_audio_codec,
            r.stream_audio_channels,
            &r.stream_container,
        ),
        reasons: r.transcode_reasons.clone(),
    }
}

fn network(local: Option<bool>) -> Option<String> {
    local.map(|l| if l { "home" } else { "outside" }.to_string())
}

fn play_view(state: &AppState, cards: &Cards, p: &Play) -> PlayView {
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
        server: server_of(state, &r.server, r.server_kind.as_deref()),
        player: r.player.clone(),
        device: r.device.clone(),
        platform: r.platform.clone(),
        platform_version: r.platform_version.clone(),
        product: r.product.clone(),
        product_version: r.product_version.clone(),
        device_id: r.device_id.clone(),
        network: network(r.local),
        location: locate(state, r.address.as_deref()),
        address: shown(state, r.address.as_deref()),
        secure: r.secure,
        relayed: r.relayed,
        bandwidth: r.bandwidth,
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
    Ok(plays.iter().map(|p| play_view(state, &cards, p)).collect())
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

/// The range and the filter a query asks for. `days` defaults to `default_days` (`None`: every
/// day); a range of dates takes its place.
async fn filter_of(
    state: &AppState,
    q: &StatsQuery,
    default_days: Option<i64>,
) -> AppResult<(StatsRange, Filter)> {
    let zone = zone(state);
    let dated = q.from.is_some() || q.to.is_some();
    let days = match q.days.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        _ if dated => None,
        None => default_days,
        some => days_of(some)?,
    };
    let (mut since, today) = store::local_range(&state.pool, &zone, days).await?;
    let mut until = None;
    if dated {
        if let (Some(from), Some(to)) = (q.from, q.to)
            && from > to
        {
            return Err(AppError::bad_request(format!(
                "`from` ({from}) is after `to` ({to})"
            )));
        }
        (since, until) = store::local_days(&state.pool, &zone, q.from, q.to).await?;
    }
    let f = Filter {
        since,
        until,
        kinds: kinds(q.kind.as_deref())?,
        library: word(&q.library),
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
        platform: word(&q.platform),
        server: word(&q.server),
        player: word(&q.player),
        product: word(&q.product),
        address: word(&q.address),
    };
    Ok((
        StatsRange {
            days,
            since,
            until,
            from: q.from,
            to: q.to,
            today,
            time_zone: zone,
            first: None,
        },
        f,
    ))
}

/// The first day of a range's bars: its own first day, else the day of its first play.
fn first_day(range: &StatsRange, rows: &[Day]) -> Option<NaiveDate> {
    match range.days {
        Some(d) => Some(range.today - chrono::Duration::days(d - 1)),
        None => range.from.or_else(|| rows.first().map(|r| r.day)),
    }
}

/// The last day of a range's bars: the last day of a range of dates, else today.
fn last_day(range: &StatsRange) -> NaiveDate {
    range.to.unwrap_or(range.today).min(range.today)
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

/// Plays by weekday (Monday first) and hour, with the hours watched in each.
async fn week(state: &AppState, f: &Filter, zone: &str) -> AppResult<Vec<Vec<(i64, i64)>>> {
    let cells = store::grid(&state.pool, f, zone).await?;
    let mut grid = vec![vec![(0i64, 0i64); 24]; 7];
    for c in &cells {
        if let (Ok(w), Ok(h)) = (usize::try_from(c.weekday - 1), usize::try_from(c.hour))
            && w < 7
            && h < 24
        {
            grid[w][h].0 += c.plays;
            grid[w][h].1 += c.watched;
        }
    }
    Ok(grid)
}

/// Plays and hours by hour of the day and by weekday, of a week's grid.
fn slots(grid: &[Vec<(i64, i64)>]) -> (Vec<Slot>, Vec<Slot>) {
    let slot = |(plays, watched): (i64, i64)| Slot {
        plays,
        hours: hours(watched),
    };
    let by_hour = (0..24)
        .map(|h| {
            slot(
                grid.iter()
                    .map(|row| row[h])
                    .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1)),
            )
        })
        .collect();
    let by_day = grid
        .iter()
        .map(|row| slot(row.iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))))
        .collect();
    (by_hour, by_day)
}

pub async fn overview_of(state: &AppState, q: &StatsQuery) -> AppResult<Overview> {
    let (mut range, f) = filter_of(state, q, Some(30)).await?;
    let pool = &state.pool;
    let zone = range.time_zone.clone();
    let totals = store::totals(pool, &f).await?;
    range.first = totals.first;
    let day_rows = store::days(pool, &f, &zone).await?;
    let week = week(state, &f, &zone).await?;
    let mut lists: Vec<Vec<TitleCount>> = Vec::new();
    for kind in ["movie", "episode", "track"] {
        lists.push(match narrowed(&f, kind) {
            Some(k) => store::top_titles(pool, &k, TOP).await?,
            None => Vec::new(),
        });
    }
    let ids: Vec<i64> = lists.iter().flatten().filter_map(|t| t.title_id).collect();
    let cards = Cards::of_titles(pool, &ids).await?;
    let grid: Vec<Vec<i64>> = week
        .iter()
        .map(|row| row.iter().map(|c| c.0).collect())
        .collect();
    let (delivery, network) = shares(&totals);
    let first = first_day(&range, &day_rows);
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
        series: series(&day_rows, first, last_day(&range)),
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
                .map(|p| person(state, p))
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

pub async fn plays_of(state: &AppState, q: &StatsQuery) -> AppResult<PlaysPage> {
    let (_, f) = filter_of(state, q, None).await?;
    let limit = q.limit.unwrap_or(PAGE).clamp(1, PAGE_MAX);
    let sort = match q.sort.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(word) => Some(SortKey::of(word).ok_or_else(|| {
            AppError::bad_request(format!(
                "`sort` is a column of the history (date, person, title, server, player, platform, product, delivery, quality, duration, paused, progress, address), not `{word}`"
            ))
        })?),
        None => None,
    };
    let descending = match q.dir.as_deref().map(str::trim) {
        None | Some("") | Some("desc") => true,
        Some("asc") => false,
        Some(other) => {
            return Err(AppError::bad_request(format!(
                "`dir` is asc or desc, not `{other}`"
            )));
        }
    };
    // Newest first, the history is paged by the last play shown: a play that starts meanwhile
    // moves no row from one page to the next. Any other order is paged by number.
    let newest_first = sort.is_none_or(|s| s == SortKey::Date) && descending;
    if sort == Some(SortKey::Address) {
        return by_place(state, &f, q.page.unwrap_or(1).max(1), limit, descending).await;
    }
    if q.page.is_some() || !newest_first {
        let page = q.page.unwrap_or(1).max(1);
        let total = store::count(&state.pool, &f).await?;
        let rows = store::sorted(
            &state.pool,
            &f,
            sort.unwrap_or_default(),
            descending,
            (page - 1).saturating_mul(limit),
            limit,
        )
        .await?;
        return Ok(PlaysPage {
            plays: plays_views(state, &rows).await?,
            next: None,
            total,
            limit,
            page: Some(page),
        });
    }
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
        page: None,
    })
}

/// One play in the order of the location column.
struct Placed {
    /// The place's label, lower case.
    place: Option<String>,
    address: Option<String>,
    at: DateTime<Utc>,
    id: i64,
}

/// The history in the order of the location column: by the place an address is in, then by the
/// address. Places are made on this machine and are no column: the plays' addresses are
/// placed, ordered here, and the page's plays read by their ids. Plays without a place come
/// after those with one. Plays without an address come last, in either direction.
async fn by_place(
    state: &AppState,
    f: &Filter,
    page: i64,
    limit: i64,
    descending: bool,
) -> AppResult<PlaysPage> {
    let mut placed: Vec<Placed> = store::addresses_of(&state.pool, f)
        .await?
        .into_iter()
        .map(|(id, address, at)| Placed {
            place: locate(state, address.as_deref())
                .and_then(|p| p.label)
                .map(|l| l.to_lowercase()),
            address: shown(state, address.as_deref()),
            at,
            id,
        })
        .collect();
    let order = |x: &Option<String>, y: &Option<String>| match (x, y) {
        (Some(x), Some(y)) if descending => y.cmp(x),
        (Some(x), Some(y)) => x.cmp(y),
        (x, y) => y.is_some().cmp(&x.is_some()),
    };
    placed.sort_by(|a, b| {
        order(&a.place, &b.place)
            .then_with(|| order(&a.address, &b.address))
            .then_with(|| b.at.cmp(&a.at))
            .then_with(|| b.id.cmp(&a.id))
    });
    let total = placed.len() as i64;
    let skip = usize::try_from((page - 1).saturating_mul(limit)).unwrap_or(usize::MAX);
    let ids: Vec<i64> = placed
        .iter()
        .skip(skip)
        .take(usize::try_from(limit).unwrap_or(0))
        .map(|p| p.id)
        .collect();
    let mut rows = store::by_ids(&state.pool, &ids).await?;
    rows.sort_by_key(|p| ids.iter().position(|id| *id == p.id));
    Ok(PlaysPage {
        plays: plays_views(state, &rows).await?,
        next: None,
        total,
        limit,
        page: Some(page),
    })
}

pub async fn people_of(state: &AppState, q: &StatsQuery) -> AppResult<PeoplePage> {
    let (range, f) = filter_of(state, q, Some(30)).await?;
    Ok(PeoplePage {
        people: store::people(&state.pool, &f, 1000)
            .await?
            .iter()
            .map(|p| person(state, p))
            .collect(),
        range,
    })
}

fn address_view(state: &AppState, a: &AddressCount) -> AddressView {
    AddressView {
        location: locate(state, Some(&a.address)),
        address: a.address.clone(),
        plays: a.plays,
        player: a.player.clone(),
        first: a.first,
        last: a.last,
    }
}

pub async fn person_of(state: &AppState, id: i64, q: &StatsQuery) -> AppResult<PersonStats> {
    let pool = &state.pool;
    let ever = Filter {
        account: Some(id),
        ..Filter::default()
    };
    let Some(whole) = store::people(pool, &ever, 1)
        .await?
        .first()
        .map(|p| person(state, p))
    else {
        return Err(AppError::not_found(format!(
            "Nobody with the account {id} played anything."
        )));
    };
    let (mut range, mut f) = filter_of(state, q, Some(30)).await?;
    f.account = Some(id);
    let totals = store::totals(pool, &f).await?;
    range.first = totals.first;
    let day_rows = store::days(pool, &f, &range.time_zone).await?;
    let top = store::top_titles(pool, &f, TOP).await?;
    let ids: Vec<i64> = top.iter().filter_map(|t| t.title_id).collect();
    let cards = Cards::of_titles(pool, &ids).await?;
    let (recent, _, _) = store::page(pool, &f, None, RECENT).await?;
    let (delivery, network) = shares(&totals);
    let week = week(state, &f, &range.time_zone).await?;
    let (by_hour, by_day) = slots(&week);
    let first = first_day(&range, &day_rows);
    Ok(PersonStats {
        person: StatsPerson {
            plays: totals.plays,
            hours: hours(totals.watched),
            ..whole
        },
        series: series(&day_rows, first, last_day(&range)),
        delivery,
        network,
        top: top.iter().map(|t| top_title(&cards, t)).collect(),
        devices: store::players(pool, &f, TOP)
            .await?
            .iter()
            .map(top_name)
            .collect(),
        platforms: store::platforms(pool, &f, TOP)
            .await?
            .iter()
            .map(top_name)
            .collect(),
        products: store::products(pool, &f, TOP)
            .await?
            .iter()
            .map(top_app)
            .collect(),
        addresses: if addresses_kept(state) {
            store::addresses(pool, &f, ADDRESSES)
                .await?
                .iter()
                .map(|a| address_view(state, a))
                .collect()
        } else {
            Vec::new()
        },
        hours: by_hour,
        weekdays: by_day,
        recent: plays_views(state, &recent).await?,
        range,
    })
}

pub async fn libraries_of(state: &AppState, q: &StatsQuery) -> AppResult<LibrariesPage> {
    let (range, f) = filter_of(state, q, Some(30)).await?;
    let counted = store::libraries(&state.pool, &f).await?;
    let known = crate::catalog::service::libraries(&state.pool).await?;
    let server_of_library = |integration: Option<&str>| {
        integration
            .filter(|i| state.engines.media_server_of(i).is_some())
            .map(|i| server_of(state, i, None))
    };
    let mut libraries: Vec<LibraryStats> = counted
        .iter()
        .map(|c| {
            let row = known.iter().find(|l| l.id == c.library);
            LibraryStats {
                id: c.library.clone(),
                name: row
                    .map(|l| l.name.clone())
                    .unwrap_or_else(|| c.library.clone()),
                kind: row.and_then(|l| l.kind.clone()),
                server: server_of_library(row.and_then(|l| l.integration.as_deref())),
                plays: c.plays,
                people: c.people,
                hours: hours(c.watched),
                last: Some(c.last),
                last_title: c.last_title.clone(),
            }
        })
        .collect();
    // The media servers' libraries nobody played in the range are listed too.
    for l in &known {
        let on_server = l
            .integration
            .as_deref()
            .is_some_and(|i| state.engines.media_server_of(i).is_some());
        if on_server && !libraries.iter().any(|x| x.id == l.id) {
            libraries.push(LibraryStats {
                id: l.id.clone(),
                name: l.name.clone(),
                kind: l.kind.clone(),
                server: server_of_library(l.integration.as_deref()),
                plays: 0,
                people: 0,
                hours: 0.0,
                last: None,
                last_title: None,
            });
        }
    }
    Ok(LibrariesPage { range, libraries })
}

pub async fn players_of(state: &AppState, q: &StatsQuery) -> AppResult<PlayersPage> {
    let (range, f) = filter_of(state, q, Some(30)).await?;
    let pool = &state.pool;
    Ok(PlayersPage {
        products: store::products(pool, &f, MANY)
            .await?
            .iter()
            .map(top_app)
            .collect(),
        platforms: store::platforms(pool, &f, MANY)
            .await?
            .iter()
            .map(top_name)
            .collect(),
        players: store::players(pool, &f, MANY)
            .await?
            .iter()
            .map(top_name)
            .collect(),
        range,
    })
}

pub async fn graphs_of(state: &AppState, q: &StatsQuery) -> AppResult<Graphs> {
    let (mut range, f) = filter_of(state, q, Some(30)).await?;
    let pool = &state.pool;
    let zone = range.time_zone.clone();
    let totals = store::totals(pool, &f).await?;
    range.first = totals.first;
    let day_rows = store::days(pool, &f, &zone).await?;
    let (by_hour, by_day) = slots(&week(state, &f, &zone).await?);
    let (delivery, _) = shares(&totals);
    let first = first_day(&range, &day_rows);
    Ok(Graphs {
        series: series(&day_rows, first, last_day(&range)),
        hours: by_hour,
        weekdays: by_day,
        delivery,
        resolutions: Resolutions {
            source: store::resolutions(pool, &f, false)
                .await?
                .iter()
                .map(label_stat)
                .collect(),
            stream: store::resolutions(pool, &f, true)
                .await?
                .iter()
                .map(label_stat)
                .collect(),
        },
        platforms: store::platforms(pool, &f, TOP)
            .await?
            .iter()
            .map(top_name)
            .collect(),
        people: store::people(pool, &f, TOP)
            .await?
            .iter()
            .map(|p| person(state, p))
            .collect(),
        servers: store::servers(pool, &f)
            .await?
            .iter()
            .map(|n| ServerStats {
                server: server_of(state, &n.name, n.platform.as_deref()),
                plays: n.plays,
                hours: hours(n.watched),
                people: n.people,
            })
            .collect(),
        streams: store::peaks(pool, &f, &zone)
            .await?
            .into_iter()
            .map(|p| DayStreams {
                at: p.day,
                streams: p.streams,
            })
            .collect(),
        reasons: store::reasons(pool, &f)
            .await?
            .iter()
            .map(label_stat)
            .collect(),
        range,
    })
}

/// The location database in use, with the credit DB-IP's license asks for.
pub fn location_source(state: &AppState) -> LocationSource {
    let source = state.engines.geoip.source();
    LocationSource {
        built: source.as_ref().and_then(|s| s.built),
        database: source.map(|s| s.file),
        credit: geoip::CREDIT.to_string(),
        url: geoip::CREDIT_URL.to_string(),
    }
}

pub async fn options_of(state: &AppState) -> AppResult<StatsOptions> {
    let choices = store::choices(&state.pool).await?;
    let mut servers: Vec<PlayServer> = state
        .engines
        .media_servers
        .iter()
        .map(|s| server_of(state, s.id(), Some(s.kind())))
        .collect();
    for (id, kind) in &choices.servers {
        if !servers.iter().any(|s| s.id == *id) {
            servers.push(server_of(state, id, kind.as_deref()));
        }
    }
    let libraries = crate::catalog::service::libraries(&state.pool)
        .await?
        .into_iter()
        .filter(|l| {
            l.integration
                .as_deref()
                .is_some_and(|i| state.engines.media_server_of(i).is_some())
        })
        .map(|l| LibraryChoice {
            id: l.id,
            name: l.name,
        })
        .collect();
    Ok(StatsOptions {
        servers,
        libraries,
        people: choices
            .people
            .into_iter()
            .map(|(id, name)| PlayPerson { id: Some(id), name })
            .collect(),
        players: choices.players,
        products: choices.products,
        platforms: choices.platforms,
        locations: location_source(state),
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
#[utoipa::path(get, path = "/api/v1/stats/overview", tag = "stats", params(StatsQuery),
    responses((status = 200, body = Overview), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn overview(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<StatsQuery>,
) -> ApiResult<Overview> {
    owner_only(&caller)?;
    Ok(Json(overview_of(&state, &q).await?))
}

/// Every play: who, what, when, where (the address and its location) and how, on which media
/// server and player. Newest first and paged with `before`, or in the order of `sort` and
/// paged with `page`. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/plays", tag = "stats", params(StatsQuery),
    responses((status = 200, body = PlaysPage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn plays(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<StatsQuery>,
) -> ApiResult<PlaysPage> {
    owner_only(&caller)?;
    Ok(Json(plays_of(&state, &q).await?))
}

/// The people who played, the most active first, each with their newest address, location,
/// player and title. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/people", tag = "stats", params(StatsQuery),
    responses((status = 200, body = PeoplePage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn people(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<StatsQuery>,
) -> ApiResult<PeoplePage> {
    owner_only(&caller)?;
    Ok(Json(people_of(&state, &q).await?))
}

/// One person: their plays per day, hours and weekdays, what they play most, their players,
/// platforms and apps, the addresses they played from with their locations, how their plays
/// were delivered, and their newest plays. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/people/{id}", tag = "stats",
    params(("id" = i64, Path, description = "The media server's account id"), StatsQuery),
    responses((status = 200, body = PersonStats), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn person_detail(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiPath(id): ApiPath<i64>,
    ApiQuery(q): ApiQuery<StatsQuery>,
) -> ApiResult<PersonStats> {
    owner_only(&caller)?;
    Ok(Json(person_of(&state, id, &q).await?))
}

/// The plays of each library: plays, people, hours and the last title played. The media
/// servers' libraries without plays are listed at zero. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/libraries", tag = "stats", params(StatsQuery),
    responses((status = 200, body = LibrariesPage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn libraries(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<StatsQuery>,
) -> ApiResult<LibrariesPage> {
    owner_only(&caller)?;
    Ok(Json(libraries_of(&state, &q).await?))
}

/// The player apps (with their newest versions), platforms and players by their plays,
/// people and hours. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/players", tag = "stats", params(StatsQuery),
    responses((status = 200, body = PlayersPage), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn players(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<StatsQuery>,
) -> ApiResult<PlayersPage> {
    owner_only(&caller)?;
    Ok(Json(players_of(&state, &q).await?))
}

/// The graphs: plays and hours by day, hour and weekday; by delivery; by the file's and the
/// stream's resolution; by platform, person and media server; the most streams at once each
/// day; why the servers transcoded. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/graphs", tag = "stats", params(StatsQuery),
    responses((status = 200, body = Graphs), (status = 400, body = ProblemDoc), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn graphs(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiQuery(q): ApiQuery<StatsQuery>,
) -> ApiResult<Graphs> {
    owner_only(&caller)?;
    Ok(Json(graphs_of(&state, &q).await?))
}

/// What the history's filters choose from (media servers, people, players, apps, platforms),
/// and the location database in use with its credit. The owner's alone.
#[utoipa::path(get, path = "/api/v1/stats/options", tag = "stats",
    responses((status = 200, body = StatsOptions), (status = 401, body = ProblemDoc), (status = 403, body = ProblemDoc)))]
async fn options(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<StatsOptions> {
    owner_only(&caller)?;
    Ok(Json(options_of(&state).await?))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/stats/overview", get(overview))
        .route("/stats/plays", get(plays))
        .route("/stats/people", get(people))
        .route("/stats/people/{id}", get(person_detail))
        .route("/stats/libraries", get(libraries))
        .route("/stats/players", get(players))
        .route("/stats/graphs", get(graphs))
        .route("/stats/options", get(options))
}

#[derive(OpenApi)]
#[openapi(
    paths(overview, plays, people, person_detail, libraries, players, graphs, options),
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
            "/api/v1/stats/libraries",
            "/api/v1/stats/players",
            "/api/v1/stats/graphs",
            "/api/v1/stats/options",
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
        assert_eq!(SortKey::of("ip"), Some(SortKey::Address));
        assert_eq!(SortKey::of("sideways"), None);
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

    #[test]
    fn a_session_is_delivered_with_its_codecs_and_reasons() {
        let d = Delivery {
            video: Some("transcode".into()),
            audio: Some("direct stream".into()),
            source_resolution: Some("4K".into()),
            source_bitrate: Some(40000),
            source_video_codec: Some("hevc".into()),
            source_audio_codec: Some("truehd".into()),
            source_audio_channels: Some(8),
            source_container: Some("mkv".into()),
            stream_resolution: Some("1080p".into()),
            stream_video_codec: Some("h264".into()),
            transcode_reasons: vec!["VideoCodecNotSupported".into()],
            ..Delivery::default()
        };
        let view = serde_json::to_value(session_delivery(&d, "transcode")).unwrap();
        assert_eq!(view["source"]["videoCodec"], "hevc");
        assert_eq!(view["source"]["audioChannels"], 8);
        assert_eq!(view["stream"]["videoCodec"], "h264");
        assert!(view["stream"].get("container").is_none());
        assert_eq!(view["reasons"][0], "VideoCodecNotSupported");
        let plain = serde_json::to_value(session_delivery(&Delivery::default(), "")).unwrap();
        assert!(plain["decision"].is_null() && plain["source"].is_null());
        assert!(plain.get("reasons").is_none());
    }
}
