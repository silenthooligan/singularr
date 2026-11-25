//! Engines behind ports. An engine is another program Singularr talks to: the Radarr and Sonarr
//! integrations (legacy, read in stage 1), the download clients (qBittorrent, Transmission,
//! Deluge, SABnzbd, NZBGet), Seerr, the media servers (Plex, Jellyfin, Emby, each behind
//! [`MediaServer`]), the processing services (a worker such as
//! Tdarr, a bridge such as HUSH) and a play history (Tautulli). An adapter is created per
//! configured integration; nothing here names an instance.
//!
//! Singularr reads every engine and acts on one only by permission. What it may do with an
//! integration is its access (`access`): `read` by default, `act` where the owner allows it
//! and the type's adapter declares actions. An adapter sends GET, the calls it declares as
//! reads (a sign-in, a query a service takes with POST), and the calls it declares as acts;
//! `core::outbound` refuses everything else for an engine's host, and the acts while the
//! access is `read`.

pub mod model;

#[cfg(feature = "ssr")]
pub mod access;
#[cfg(feature = "ssr")]
pub mod arr;
#[cfg(feature = "ssr")]
pub mod clients;
#[cfg(feature = "ssr")]
pub mod deluge;
#[cfg(feature = "ssr")]
pub mod hush;
#[cfg(feature = "ssr")]
pub mod jellyfin;
#[cfg(feature = "ssr")]
pub mod nzbget;
#[cfg(feature = "ssr")]
pub mod plex;
#[cfg(feature = "ssr")]
pub mod qbit;
#[cfg(feature = "ssr")]
pub mod sabnzbd;
#[cfg(feature = "ssr")]
pub mod seerr;
#[cfg(feature = "ssr")]
pub mod service;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod tautulli;
#[cfg(feature = "ssr")]
pub mod tdarr;
#[cfg(feature = "ssr")]
pub mod transmission;

#[cfg(feature = "ssr")]
pub use ports::*;

#[cfg(feature = "ssr")]
mod ports {
    use super::model::{ArrParse, DiscoverList};
    use crate::catalog::ingest::{EngineRef, FileFacts, TitleFacts};
    use crate::catalog::model::TitleKind;
    use crate::core::outbound::OutboundError;
    use async_trait::async_trait;
    use chrono::{DateTime, NaiveDate, Utc};
    use serde_json::Value;
    use std::sync::Arc;

    #[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
    pub enum EngineError {
        #[error(transparent)]
        Outbound(#[from] OutboundError),
        #[error("{engine} sent an answer that could not be read ({what}): {message}")]
        Decode {
            engine: String,
            what: &'static str,
            message: String,
        },
        #[error("{engine} refused the sign-in: {message}")]
        Login { engine: String, message: String },
    }

    /// What `/system/status` says.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct EngineInfo {
        pub app: Option<String>,
        pub version: Option<String>,
    }

    /// One item of an arr's download queue.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct QueueItem {
        pub queue_id: i64,
        /// The arr's id of the movie or series. `None`: the arr does not know what this
        /// download belongs to.
        pub engine_title_id: Option<i64>,
        pub title: String,
        /// The download client's id of the download, upper case: a torrent's hash, a usenet
        /// client's job id ([`ClientDownload::download_id`]).
        pub download_id: Option<String>,
        pub status: Option<String>,
        /// `downloading`, `importPending`, `importBlocked`, `failedPending`, ...
        pub tracked_state: Option<String>,
        /// `ok`, `warning` or `error`.
        pub tracked_status: Option<String>,
        pub messages: Vec<String>,
        /// The indexer the release came from, as the arr names it.
        pub indexer: Option<String>,
    }

    /// One page of an arr's grab history, newest first.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct GrabPage {
        /// Download ids (torrent hashes, usenet job ids), upper case.
        pub download_ids: Vec<String>,
        pub records: usize,
        pub total_records: i64,
    }

    /// An episode as a manager names it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct EpisodeRef {
        pub season: i32,
        pub number: i32,
        pub title: Option<String>,
    }

    /// One entry of a manager's calendar: an episode that airs, or one release of a movie.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct CalendarEntry {
        /// The manager's id of the movie or the series.
        pub engine_title_id: i64,
        pub episode: Option<EpisodeRef>,
        /// Movies: `theaters`, `digital` or `physical`.
        pub release: Option<&'static str>,
        /// The day, as the manager gives it.
        pub date: NaiveDate,
        /// When an episode airs.
        pub airs_at: Option<DateTime<Utc>>,
        pub has_file: bool,
        pub monitored: bool,
    }

    /// One record of a manager's history.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct HistoryEntry {
        /// The manager's id of the record.
        pub id: i64,
        /// The manager's id of the movie or the series.
        pub engine_title_id: Option<i64>,
        pub episode: Option<EpisodeRef>,
        /// The event as the manager names it: `grabbed`, `downloadFolderImported`,
        /// `downloadFailed`, `movieFileDeleted`, `episodeFileDeleted`, ...
        pub event: String,
        pub at: DateTime<Utc>,
        /// The release name.
        pub source_title: Option<String>,
        /// The manager's quality name: `WEBDL-1080p`.
        pub quality: Option<String>,
        pub group: Option<String>,
        pub size: Option<i64>,
        pub indexer: Option<String>,
        /// Why: the reason a file was deleted (`Upgrade`), the message of a failed download.
        pub reason: Option<String>,
        /// Where an import put the file.
        pub imported_path: Option<String>,
    }

    /// One entry of a manager's wanted list: a monitored movie or episode it is missing, or
    /// one it holds below the cutoff of its profile.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WantedEntry {
        /// The manager's id of the movie or the episode.
        pub id: i64,
        /// The manager's id of the movie or the series.
        pub engine_title_id: i64,
        pub episode: Option<EpisodeRef>,
        /// When the movie came out for a home library, or the episode aired.
        pub released: Option<DateTime<Utc>>,
        pub monitored: bool,
        /// The manager's quality name of the file held (below the cutoff).
        pub have: Option<String>,
        pub last_search: Option<DateTime<Utc>>,
    }

    /// The first entries of a wanted list, and how many there are in all.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct WantedPage {
        pub entries: Vec<WantedEntry>,
        pub total: i64,
    }

    /// How a manager names files: its naming settings (`/config/naming`), and the custom
    /// formats it writes into names (`/customformat`, those included when renaming).
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ArrNaming {
        /// Whether it renames files at all.
        pub rename: bool,
        pub movie_folder: Option<String>,
        pub movie_file: Option<String>,
        pub series_folder: Option<String>,
        pub season_folder: Option<String>,
        pub episode_file: Option<String>,
        /// Its colon replacement as it says it: `smart` (Radarr), `4` (Sonarr).
        pub colon: Option<String>,
        pub named_formats: Vec<String>,
    }

    /// What an interactive search is for: a movie, one episode, or one season of a series.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ReleaseScope {
        Movie { movie: i64 },
        Episode { episode: i64 },
        Season { series: i64, season: i32 },
    }

    /// One release a search found, with what the manager made of it: its reading, its custom
    /// formats and score, and whether it would take it and why not.
    #[derive(Debug, Clone, PartialEq)]
    pub struct FoundRelease {
        /// The indexer's id of the release: with `indexer_id`, what a grab names.
        pub guid: String,
        pub indexer_id: i64,
        pub indexer: Option<String>,
        /// The release name.
        pub name: String,
        /// `torrent` or `usenet`.
        pub protocol: Option<String>,
        /// The manager's quality name: `Bluray-1080p`.
        pub quality: Option<String>,
        pub size: Option<i64>,
        /// Days since it was posted.
        pub age_days: Option<i64>,
        pub published: Option<DateTime<Utc>>,
        pub seeders: Option<i64>,
        pub leechers: Option<i64>,
        pub languages: Vec<String>,
        pub group: Option<String>,
        pub edition: Option<String>,
        /// A show's season and episodes as the manager mapped them.
        pub season: Option<i32>,
        pub episodes: Vec<i32>,
        pub full_season: bool,
        /// The manager's custom formats and their score.
        pub formats: Vec<String>,
        pub score: Option<i32>,
        /// The manager would take it.
        pub approved: bool,
        /// Why it would not, in the manager's words.
        pub rejections: Vec<String>,
        /// Only for now: a delay, a download client that is down.
        pub temporarily_rejected: bool,
    }

    /// An episode as the manager numbers it, with its id.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ManagerEpisode {
        pub id: i64,
        pub season: i32,
        pub number: i32,
        pub has_file: bool,
        pub monitored: bool,
    }

    /// The manager's queue entry of one download, as it answers now.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct QueueEntry {
        pub queue_id: i64,
        pub engine_title_id: Option<i64>,
        /// Upper case.
        pub download_id: String,
        /// The download id as the manager wrote it. The manager's own lookups (the manual
        /// import, the download a command names) match it exactly. Usenet ids are lower case.
        pub download_id_as_written: String,
        pub title: String,
        /// `torrent` or `usenet`.
        pub protocol: Option<String>,
        pub download_client: Option<String>,
        /// The download client moves a download to another category once it is imported, so
        /// its files stay where they are.
        pub post_import_category: bool,
        pub tracked_state: Option<String>,
        pub messages: Vec<String>,
    }

    /// One file a manual import offers for a download, with what the manager read of it. The
    /// quality and the languages are kept as the manager wrote them and sent back unchanged.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ImportFile {
        pub path: String,
        pub size: Option<i64>,
        /// The movie or the series the manager matched it to.
        pub title_id: Option<i64>,
        /// A show's episodes, by the manager's ids, and their season.
        pub episode_ids: Vec<i64>,
        pub season: Option<i32>,
        pub quality: Option<Value>,
        pub languages: Value,
        pub release_group: Option<String>,
        /// As the manager wrote it.
        pub download_id: Option<String>,
        pub indexer_flags: i64,
        /// Sonarr's word for what the release holds: `singleEpisode`, `seasonPack`.
        pub release_type: Option<String>,
        /// Why the manager would not import it, in its words.
        pub rejections: Vec<String>,
    }

    /// How a manual import puts files in the library.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ImportMode {
        /// The manager decides by what its download client allows.
        Auto,
        Move,
        /// A hardlink where the file system allows one, else a copy: the download keeps its
        /// files.
        Copy,
    }

    impl ImportMode {
        pub const ALL: [ImportMode; 3] = [ImportMode::Auto, ImportMode::Move, ImportMode::Copy];

        /// The managers' word.
        pub fn as_str(self) -> &'static str {
            match self {
                ImportMode::Auto => "auto",
                ImportMode::Move => "move",
                ImportMode::Copy => "copy",
            }
        }
    }

    /// A change asked of a manager (Radarr, Sonarr). Each is an act: `core::outbound` lets it
    /// through only while the integration's access is `act`, and only in the one form its
    /// adapter declares.
    #[derive(Debug, Clone, PartialEq)]
    pub enum ManagerAct {
        /// Grab a release a search found, by its guid and its indexer.
        Grab { guid: String, indexer_id: i64 },
        /// Search for these movies.
        SearchMovies { movies: Vec<i64> },
        /// Search for these episodes, by the manager's ids.
        SearchEpisodes { episodes: Vec<i64> },
        /// Search for one season of a series.
        SearchSeason { series: i64, season: i32 },
        /// Search for a whole series.
        SearchSeries { series: i64 },
        /// Import files of a download the manager refused, as they are.
        Import {
            files: Vec<ImportFile>,
            mode: ImportMode,
        },
        /// Take a download out of the queue and the download client, and with `blocklist` keep
        /// its release from being grabbed again.
        Remove { queue_id: i64, blocklist: bool },
    }

    impl ManagerAct {
        /// What it is, in the word of the adapter's actions.
        pub fn word(&self) -> &'static str {
            match self {
                ManagerAct::Grab { .. } => "grab",
                ManagerAct::SearchMovies { .. }
                | ManagerAct::SearchEpisodes { .. }
                | ManagerAct::SearchSeason { .. }
                | ManagerAct::SearchSeries { .. } => "search",
                ManagerAct::Import { .. } => "import",
                ManagerAct::Remove { .. } => "remove",
            }
        }
    }

    /// How an act on a manager ended.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ManagerActed {
        /// The manager took it; `command` is the id of the command it queued, when it queued
        /// one.
        Done { command: Option<i64> },
        /// The manager offers no such act.
        NotOffered,
        /// The manager said no, in its own words.
        Refused(String),
    }

    /// A manager (Radarr, Sonarr): titles, files, queue, grab history, its own parser for the
    /// shadow comparison; its interactive search, its manual import offer; and the acts its
    /// adapter declares ([`LibraryEngine::act`]), which pass only with write access.
    #[async_trait]
    pub trait LibraryEngine: Send + Sync {
        fn engine(&self) -> &EngineRef;
        async fn status(&self) -> Result<EngineInfo, EngineError>;
        /// Every title, with tags and profiles resolved to names and the lane mapped.
        /// Movies carry their file; shows do not (see [`LibraryEngine::files`]).
        async fn titles(&self) -> Result<Vec<TitleFacts>, EngineError>;
        /// The files of one title. Used for shows.
        async fn files(&self, engine_title_id: i64) -> Result<Vec<FileFacts>, EngineError>;
        async fn queue(&self) -> Result<Vec<QueueItem>, EngineError>;
        async fn grabs(&self, page: u32, page_size: u32) -> Result<GrabPage, EngineError>;
        /// Ask the arr to parse a release name.
        async fn parse(&self, name: &str) -> Result<ArrParse, EngineError>;
        /// The manager's calendar from `start` to `end`, both days included: its monitored
        /// titles' episodes and releases.
        async fn calendar(
            &self,
            start: NaiveDate,
            end: NaiveDate,
        ) -> Result<Vec<CalendarEntry>, EngineError>;
        /// The newest `limit` records of the manager's history, every kind of event, newest
        /// first.
        async fn history(&self, limit: u32) -> Result<Vec<HistoryEntry>, EngineError>;
        /// The manager's wanted list: monitored titles it is missing, or with `cutoff` the
        /// ones it holds below the cutoff of their profile. At most `limit` entries.
        async fn wanted(&self, cutoff: bool, limit: u32) -> Result<WantedPage, EngineError>;
        /// How the manager names files. None from an engine that has nothing to say.
        async fn naming(&self) -> Result<Option<ArrNaming>, EngineError> {
            Ok(None)
        }
        /// An interactive search: every release the manager's indexers offer for the scope,
        /// with the manager's word on each. It asks every indexer and can take a minute, so it
        /// is asked on a person's click alone, never by a job. `None`: not offered.
        async fn releases(
            &self,
            _scope: ReleaseScope,
        ) -> Result<Option<Vec<FoundRelease>>, EngineError> {
            Ok(None)
        }
        /// A series' episodes with the manager's ids: one season's, or all.
        async fn episodes(
            &self,
            _series: i64,
            _season: Option<i32>,
        ) -> Result<Vec<ManagerEpisode>, EngineError> {
            Ok(Vec::new())
        }
        /// The queue entries of one download (a pack can be one per episode), read now. The
        /// id matches in any case.
        async fn queue_entries(&self, _download_id: &str) -> Result<Vec<QueueEntry>, EngineError> {
            Ok(Vec::new())
        }
        /// What a manual import offers for one download: its files, each with the manager's
        /// reading and why it would not import it. The id goes as the manager wrote it.
        async fn import_offer(&self, _download_id: &str) -> Result<Vec<ImportFile>, EngineError> {
            Ok(Vec::new())
        }
        /// Change something in the manager. This is an act: refused on the way out unless the
        /// integration's access is `act`.
        async fn act(&self, _act: &ManagerAct) -> Result<ManagerActed, EngineError> {
            Ok(ManagerActed::NotOffered)
        }
    }

    /// How a download travels: a torrent from its peers, or a usenet download from news
    /// servers.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Protocol {
        Torrent,
        Usenet,
    }

    impl Protocol {
        /// The managers' word. A release and a queue entry carry it too.
        pub fn as_str(self) -> &'static str {
            match self {
                Protocol::Torrent => "torrent",
                Protocol::Usenet => "usenet",
            }
        }
    }

    /// Where a download stands in its client, in the product's words. The client's own word
    /// stays beside it ([`ClientDownload::state`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum DownloadStatus {
        /// It waits for its turn.
        Queued,
        Downloading,
        /// It wants data and none arrives.
        Stalled,
        Paused,
        /// The client checks the data it holds: a hash check, a par2 verify or repair.
        Checking,
        /// The client works on what it downloaded: unpacks, renames, moves it, or runs a
        /// script on it.
        Unpacking,
        /// Complete, and shared with peers.
        Seeding,
        /// Complete, with nothing left to do in the client.
        Completed,
        /// The client gave up on it, or reports an error with its files.
        Failed,
    }

    impl DownloadStatus {
        pub const ALL: [DownloadStatus; 9] = [
            DownloadStatus::Queued,
            DownloadStatus::Downloading,
            DownloadStatus::Stalled,
            DownloadStatus::Paused,
            DownloadStatus::Checking,
            DownloadStatus::Unpacking,
            DownloadStatus::Seeding,
            DownloadStatus::Completed,
            DownloadStatus::Failed,
        ];

        pub fn as_str(self) -> &'static str {
            match self {
                DownloadStatus::Queued => "queued",
                DownloadStatus::Downloading => "downloading",
                DownloadStatus::Stalled => "stalled",
                DownloadStatus::Paused => "paused",
                DownloadStatus::Checking => "checking",
                DownloadStatus::Unpacking => "unpacking",
                DownloadStatus::Seeding => "seeding",
                DownloadStatus::Completed => "completed",
                DownloadStatus::Failed => "failed",
            }
        }
    }

    /// One download as its client lists it: a torrent, or a usenet download. What only a
    /// torrent has (seeds, peers, a ratio, time seeding) is absent, or 0, for a usenet one.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ClientDownload {
        /// The client's own id of it, as the client writes it: a torrent's hash, SABnzbd's
        /// `nzo_id`, NZBGet's `NZBID`.
        pub id: String,
        /// The id a manager knows it by, upper case: what the manager's queue and grab history
        /// name as `downloadId`. A torrent's hash, SABnzbd's `nzo_id`, and for NZBGet the
        /// `drone` parameter a manager gives the downloads it grabs, else the `NZBID`.
        pub download_id: String,
        pub protocol: Protocol,
        pub name: String,
        /// The client's category or label.
        pub category: Option<String>,
        pub status: DownloadStatus,
        /// The client's own word for where it stands: `stalledDL`, `seed`, `Paused`,
        /// `PP_QUEUED`.
        pub state: String,
        /// 0 to 1.
        pub progress: f64,
        pub size_bytes: i64,
        /// The bytes of it the client holds, when it says.
        pub downloaded_bytes: Option<i64>,
        pub added_at: Option<DateTime<Utc>>,
        pub completed_at: Option<DateTime<Utc>>,
        pub save_path: Option<String>,
        /// Bytes per second down and up, and seconds left; absent when the client cannot tell.
        pub speed: Option<i32>,
        pub upload_speed: Option<i32>,
        pub eta: Option<i32>,
        /// Where it comes from as the client knows it: a torrent's tracker, by host.
        pub tracker: Option<String>,
        /// The seeds and peers a torrent is connected to, its share ratio, and how long it
        /// seeded.
        pub seeds: Option<i32>,
        pub peers: Option<i32>,
        pub ratio: Option<f64>,
        pub seeding_seconds: i64,
    }

    /// What a download client lists in one pass: its downloads, and the edge of the history it
    /// is read over. A finished download that completed before `finished_since` is past that
    /// edge. It is not listed. Its absence is no removal. A torrent client lists every
    /// torrent it holds and has no edge.
    #[derive(Debug, Clone, Default)]
    pub struct ClientListing {
        pub downloads: Vec<ClientDownload>,
        pub finished_since: Option<DateTime<Utc>>,
    }

    impl From<Vec<ClientDownload>> for ClientListing {
        fn from(downloads: Vec<ClientDownload>) -> Self {
            ClientListing {
                downloads,
                finished_since: None,
            }
        }
    }

    /// A download client: torrents (qBittorrent, Transmission, Deluge) or usenet downloads
    /// (SABnzbd, NZBGet). Read-only: it lists its downloads. Each adapter declares the calls it
    /// sends ([`crate::core::outbound::Calls`]), every one a read, and no action.
    #[async_trait]
    pub trait DownloadClient: Send + Sync {
        /// The integration id.
        fn id(&self) -> &str;
        /// The integration type: `qbittorrent`, `transmission`, `deluge`, `sabnzbd`, `nzbget`.
        fn kind(&self) -> &'static str;
        fn protocol(&self) -> Protocol;
        /// Every download it lists now: a torrent client's torrents; a usenet client's queue
        /// and the jobs of its history that finished in the last
        /// [`clients::HISTORY_DAYS`](crate::engines::clients::HISTORY_DAYS) days. The managers
        /// import from that history.
        async fn downloads(&self) -> Result<Vec<ClientDownload>, EngineError>;
        /// The same downloads with the edge of the history read. A torrent client has none.
        async fn listing(&self) -> Result<ClientListing, EngineError> {
            Ok(self.downloads().await?.into())
        }
    }

    /// One request on a Seerr site. The requester is a display name: the adapter never
    /// reads an email address.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SiteRequest {
        pub seerr_id: i64,
        pub kind: TitleKind,
        pub is_4k: bool,
        /// Seerr's request status: 1 pending, 2 approved, 3 declined, 4 failed, 5 completed.
        pub request_status: i32,
        /// Seerr's media status for the request's tier (the 4K status for a 4K request):
        /// 1 unknown, 2 pending, 3 processing, 4 partially available, 5 available,
        /// 6 blocklisted, 7 deleted.
        pub media_status: i32,
        pub tmdb_id: Option<i64>,
        pub tvdb_id: Option<i64>,
        pub requested_by: String,
        /// Seerr's id of the requesting account.
        pub requested_by_id: Option<i64>,
        pub created_at: DateTime<Utc>,
        pub updated_at: Option<DateTime<Utc>>,
        /// Seasons asked for, sorted. Empty for a movie.
        pub seasons: Vec<i32>,
    }

    /// One page of a site's request list, newest first.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SiteRequestPage {
        pub requests: Vec<SiteRequest>,
        /// Results on the page that could not be read (no id).
        pub unreadable: usize,
        /// Requests on the site, over all pages.
        pub total: i64,
    }

    /// One account on a Seerr site, read only to know who may sign in to Singularr Requests and
    /// which Seerr account a request goes out as. No address is read: the display name is the
    /// first name without an `@`, as for requesters.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SiteUser {
        pub seerr_user_id: i64,
        pub display_name: String,
        /// The plex.tv account id of a Plex user.
        pub plex_id: Option<i64>,
        pub plex_username: Option<String>,
        /// Seerr's account type: 1 Plex, 2 local, 3 Jellyfin, 4 Emby.
        pub user_type: Option<i32>,
    }

    /// One page of a site's accounts.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SiteUserPage {
        pub users: Vec<SiteUser>,
        /// Results on the page that could not be read (no id).
        pub unreadable: usize,
        /// Accounts on the site, over all pages.
        pub total: i64,
    }

    /// What a site's title lookup says a title is.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SiteMedia {
        pub title: String,
        pub year: Option<i32>,
        /// TMDB image paths, `/abc.jpg`.
        pub poster_path: Option<String>,
        pub backdrop_path: Option<String>,
        pub tvdb_id: Option<i64>,
    }

    /// Seerr's own request counts (`/request/count`).
    #[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
    pub struct SiteRequestCount {
        pub total: i64,
        pub movie: Option<i64>,
        pub tv: Option<i64>,
        pub pending: Option<i64>,
        pub approved: Option<i64>,
        pub declined: Option<i64>,
        pub processing: Option<i64>,
        pub available: Option<i64>,
    }

    /// One title of a discover list.
    #[derive(Debug, Clone, PartialEq)]
    pub struct DiscoverEntry {
        pub kind: TitleKind,
        pub tmdb_id: i64,
        pub title: String,
        pub year: Option<i32>,
        pub overview: Option<String>,
        /// TMDB's vote average, 0 to 10.
        pub rating: Option<f64>,
        pub poster_path: Option<String>,
        pub backdrop_path: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq)]
    pub struct DiscoverPage {
        pub page: u32,
        pub total_pages: u32,
        pub total_results: i64,
        pub entries: Vec<DiscoverEntry>,
    }

    /// What the media server says it is.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerIdentity {
        pub name: String,
        /// The server's own identifier: the address of an item in the server's web app names
        /// it (Plex's machine identifier, the server id of Jellyfin and Emby).
        pub machine_id: Option<String>,
        pub version: Option<String>,
        pub platform: Option<String>,
        /// The server's owner has Plex Pass (editions in one item need it). False for every
        /// other server.
        pub plex_pass: bool,
    }

    /// One library section of the media server.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerSection {
        pub key: String,
        /// `movie`, `show`, `music`, `homevideo` or `photo`; Plex says `artist` for music and
        /// marks personal media by its agent (`lifecycle::server::section_kind` reads both).
        pub kind: String,
        pub title: String,
        pub agent: Option<String>,
        /// The folders, as the server names them.
        pub locations: Vec<String>,
    }

    /// One movie or show of a section, with its provider ids and what the server knows of it.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ServerItem {
        pub rating_key: String,
        pub kind: String,
        pub title: String,
        pub year: Option<i32>,
        pub imdb: Option<String>,
        pub tmdb: Option<i64>,
        pub tvdb: Option<i64>,
        /// The critic rating, 0 to 10, and its source's image key
        /// (`rottentomatoes://image.rating.ripe`).
        pub critic_rating: Option<f64>,
        pub critic_image: Option<String>,
        pub audience_rating: Option<f64>,
        pub audience_image: Option<String>,
        pub content_rating: Option<String>,
        pub plays: i64,
        pub last_played: Option<DateTime<Utc>>,
        /// The edition title of a movie item (`Director's Cut`).
        pub edition: Option<String>,
        pub added: Option<DateTime<Utc>>,
        /// A show's episodes, and those watched.
        pub episodes: Option<i64>,
        pub watched_episodes: Option<i64>,
        pub thumb: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerArtist {
        pub rating_key: String,
        pub name: String,
        pub genres: Vec<String>,
        pub countries: Vec<String>,
        pub summary: Option<String>,
        /// The server's image path; the photo is served through Singularr.
        pub thumb: Option<String>,
        pub added: Option<DateTime<Utc>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerAlbum {
        pub rating_key: String,
        pub title: String,
        pub artist: Option<String>,
        pub artist_key: Option<String>,
        pub year: Option<i32>,
        pub released: Option<String>,
        pub genres: Vec<String>,
        /// The record label.
        pub label: Option<String>,
        pub added: Option<DateTime<Utc>>,
        pub thumb: Option<String>,
        pub plays: i64,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerTrack {
        pub rating_key: String,
        pub album_key: String,
        pub number: Option<i32>,
        pub disc: Option<i32>,
        pub title: String,
        pub artist: Option<String>,
        pub duration_ms: Option<i64>,
        pub codec: Option<String>,
        /// kbit/s.
        pub bitrate: Option<i64>,
        pub channels: Option<i64>,
        pub container: Option<String>,
        pub size: Option<i64>,
        /// The file, as the server names it.
        pub file: Option<String>,
    }

    /// How a stream reaches its player: for the video and the audio, `direct play`,
    /// `direct stream` (the container changes, nothing is encoded again) or `transcode`, and the
    /// file's resolution and bitrate next to what is sent. Absent where the server does not say.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Delivery {
        pub video: Option<String>,
        pub audio: Option<String>,
        /// `4K`, `1080p`, `720p`, `SD`.
        pub source_resolution: Option<String>,
        /// kbit/s.
        pub source_bitrate: Option<i64>,
        pub stream_resolution: Option<String>,
        pub stream_bitrate: Option<i64>,
    }

    /// Something playing now.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ServerSession {
        /// The media server it plays on, by its integration id: its keys are that server's.
        /// The adapter sets it; a session read from an answer alone has none.
        pub server: String,
        /// `movie`, `episode`, `track`.
        pub kind: String,
        pub title: String,
        /// The show of an episode, and its season and episode numbers.
        pub show: Option<String>,
        pub season: Option<i32>,
        pub episode: Option<i32>,
        /// The person watching, by their name on the server.
        pub person: Option<String>,
        pub player: Option<String>,
        /// `playing`, `paused`, `buffering`.
        pub state: Option<String>,
        /// 0 to 100.
        pub progress: Option<f64>,
        /// `direct play`, `direct stream` or `transcode`.
        pub decision: String,
        pub rating_key: Option<String>,
        /// An episode's show, by the server's key of the show.
        pub show_key: Option<String>,
        /// The server's id of the session, unique while it plays.
        pub session_id: Option<String>,
        /// The person's account on the server, by the server's id of it.
        pub account_id: Option<i64>,
        /// The section the item is in, by the server's key.
        pub library_key: Option<String>,
        pub year: Option<i32>,
        /// Where the player is in the item, and how long the item is, in milliseconds.
        pub offset_ms: Option<i64>,
        pub duration_ms: Option<i64>,
        /// The player's device, platform and app.
        pub device: Option<String>,
        pub platform: Option<String>,
        pub product: Option<String>,
        /// The player is on the server's own network.
        pub local: Option<bool>,
        /// The player's address as the server sees it.
        pub address: Option<String>,
        pub delivery: Delivery,
        /// What the stream takes, kbit/s.
        pub bandwidth: Option<i64>,
        /// The whole entry as the server gave it.
        pub raw: Value,
    }

    /// One entry of the media server's own history: an item it marked as played, with who and
    /// on which device. It has no durations and nothing of the delivery.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ServerPlay {
        /// The server's id of the entry.
        pub id: i64,
        pub rating_key: Option<String>,
        /// An episode's show, by the server's key of the show.
        pub show_key: Option<String>,
        /// `movie`, `episode`, `track`.
        pub kind: String,
        pub title: String,
        pub show: Option<String>,
        pub season: Option<i32>,
        pub episode: Option<i32>,
        pub year: Option<i32>,
        pub library_key: Option<String>,
        /// When the server marked it as played.
        pub viewed_at: DateTime<Utc>,
        pub account_id: Option<i64>,
        pub device_id: Option<i64>,
        /// The entry as the server gave it.
        pub raw: Value,
    }

    /// One page of the media server's history, newest first.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ServerPlayPage {
        pub plays: Vec<ServerPlay>,
        /// Entries in the whole history.
        pub total: i64,
    }

    /// An account of the media server: the owner and the people it is shared with.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerAccount {
        pub id: i64,
        pub name: Option<String>,
    }

    /// A device that has played from the media server.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerDevice {
        pub id: i64,
        pub name: Option<String>,
        pub platform: Option<String>,
    }

    /// The resolution a picture of this height reads as: `4K`, `1080p`, `720p`, `SD`.
    pub fn height_word(height: i64) -> Option<String> {
        let word = match height {
            h if h >= 1500 => "4K",
            h if h >= 900 => "1080p",
            h if h >= 650 => "720p",
            h if h > 0 => "SD",
            _ => return None,
        };
        Some(word.to_string())
    }

    /// The resolution and bitrate of an item's own file.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct ItemMedia {
        pub resolution: Option<String>,
        /// kbit/s.
        pub bitrate: Option<i64>,
    }

    /// One item of a plain video library (a movie section without a metadata agent).
    #[derive(Debug, Clone, PartialEq)]
    pub struct ServerVideo {
        pub rating_key: String,
        pub title: String,
        pub year: Option<i32>,
        pub duration_ms: Option<i64>,
        /// `h264`, `hevc`, as the server names it.
        pub codec: Option<String>,
        pub height: Option<i64>,
        /// The file as the server names it.
        pub file: Option<String>,
        pub size: Option<i64>,
        pub added: Option<DateTime<Utc>>,
        pub thumb: Option<String>,
        pub plays: i64,
    }

    /// An image the server made.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ServerImage {
        pub content_type: String,
        pub bytes: Vec<u8>,
    }

    /// Where an item of the media server opens in the server's own web app.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ItemLink {
        /// The item as the server's web app names it: Plex's `/library/metadata/109882`, the
        /// item id of Jellyfin and Emby.
        pub key: String,
        pub url: String,
    }

    /// A media server (Plex, Jellyfin, Emby), read-only: what it is, its libraries and their
    /// items with their provider ids and files, music, what is playing, its accounts, and
    /// images resized by the server. Each server has an adapter of its own behind this port;
    /// the passes, the pages and the plays read every server the same way. What only some
    /// servers keep (a history of plays, the devices behind it) has a default of none.
    #[async_trait]
    pub trait MediaServer: Send + Sync {
        /// The integration id.
        fn id(&self) -> &str;
        /// The integration type: `plex`, `jellyfin`, `emby`.
        fn kind(&self) -> &'static str;
        async fn identity(&self) -> Result<ServerIdentity, EngineError>;
        async fn sections(&self) -> Result<Vec<ServerSection>, EngineError>;
        /// The movies or shows of a movie or show section.
        async fn items(&self, section: &ServerSection) -> Result<Vec<ServerItem>, EngineError>;
        async fn artists(&self, section: &str) -> Result<Vec<ServerArtist>, EngineError>;
        async fn albums(&self, section: &str) -> Result<Vec<ServerAlbum>, EngineError>;
        async fn tracks(&self, section: &str) -> Result<Vec<ServerTrack>, EngineError>;
        async fn sessions(&self) -> Result<Vec<ServerSession>, EngineError>;
        /// The server keeps a history of plays that can be read ([`MediaServer::history`],
        /// with [`MediaServer::devices`] behind it). Plays are recorded from the sessions of
        /// every server, whether or not it keeps one.
        fn keeps_history(&self) -> bool {
            false
        }
        /// One page of the server's history of plays, newest first. Empty for a server that
        /// keeps none.
        async fn history(&self, _start: u32, _size: u32) -> Result<ServerPlayPage, EngineError> {
            Ok(ServerPlayPage {
                plays: Vec::new(),
                total: 0,
            })
        }
        /// The server's accounts, by id and name.
        async fn accounts(&self) -> Result<Vec<ServerAccount>, EngineError>;
        /// The devices that have played from the server. Empty for a server whose history
        /// names none.
        async fn devices(&self) -> Result<Vec<ServerDevice>, EngineError> {
            Ok(Vec::new())
        }
        /// What an item's own file is: the source of a stream that is transcoded. `None` when
        /// the server has no such item.
        async fn item_media(&self, rating_key: &str) -> Result<Option<ItemMedia>, EngineError>;
        /// The items of a plain video library, with their files.
        async fn videos(&self, section: &str) -> Result<Vec<ServerVideo>, EngineError>;
        /// An image of the server (`thumb` as an item names it), resized to `size` pixels.
        async fn photo(&self, thumb: &str, size: u32) -> Result<ServerImage, EngineError>;
        /// The image of an item by its key, as [`MediaServer::photo`] takes it. `None` for a
        /// key the server cannot have.
        fn item_thumb(&self, rating_key: &str) -> Option<String>;
        /// Where an item opens in the server's own web app; `machine_id` is the server's own
        /// identifier, when its identity named one. `None` when no address can be made.
        fn item_link(&self, machine_id: Option<&str>, rating_key: &str) -> Option<ItemLink>;
    }

    /// A person as a play history names them. `admin`: the media server's owner.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct HistoryUser {
        pub id: i64,
        pub name: Option<String>,
        pub admin: bool,
    }

    /// One play a play history kept: one sitting, from its start to its stop.
    #[derive(Debug, Clone, PartialEq)]
    pub struct HistoryPlay {
        /// The history's id of the play.
        pub id: i64,
        /// The first play of the viewing this one continues, by the history's id.
        pub reference_id: Option<i64>,
        pub user_id: Option<i64>,
        pub user: Option<String>,
        pub rating_key: Option<String>,
        pub show_key: Option<String>,
        /// `movie`, `episode`, `track`.
        pub kind: String,
        pub title: String,
        pub show: Option<String>,
        pub season: Option<i32>,
        pub episode: Option<i32>,
        pub year: Option<i32>,
        pub started: DateTime<Utc>,
        pub stopped: Option<DateTime<Utc>>,
        /// Seconds it played, pauses left out, and seconds it was paused.
        pub played_secs: Option<i64>,
        pub paused_secs: Option<i64>,
        /// 0 to 100.
        pub percent: Option<f64>,
        pub player: Option<String>,
        pub platform: Option<String>,
        pub product: Option<String>,
        pub address: Option<String>,
        /// On the server's own network.
        pub local: Option<bool>,
        /// `direct play`, `direct stream` or `transcode`.
        pub decision: Option<String>,
        /// The play as the history gave it.
        pub raw: Value,
    }

    /// One page of a play history, newest first.
    #[derive(Debug, Clone, PartialEq)]
    pub struct HistoryPage {
        pub plays: Vec<HistoryPlay>,
        /// Plays the question matches, over every page.
        pub total: i64,
    }

    /// A history of the media server's plays that keeps more than the server does (Tautulli):
    /// read-only, GET alone.
    #[async_trait]
    pub trait PlayHistory: Send + Sync {
        /// The integration id.
        fn id(&self) -> &str;
        async fn status(&self) -> Result<EngineInfo, EngineError>;
        /// The people it knows, with the owner marked.
        async fn users(&self) -> Result<Vec<HistoryUser>, EngineError>;
        /// One page of plays, newest first; with `after`, the plays that started on that day
        /// or later.
        async fn plays(
            &self,
            start: u32,
            length: u32,
            after: Option<NaiveDate>,
        ) -> Result<HistoryPage, EngineError>;
    }

    /// A Seerr site, read-only in stage 1: its requests, its title lookups, its discover
    /// lists. Approving, declining and requesting are stage 2.
    #[async_trait]
    pub trait RequestSite: Send + Sync {
        /// The integration id.
        fn id(&self) -> &str;
        /// The media server the requests are for (`plex`): a request's `site`.
        fn site(&self) -> &str;
        async fn status(&self) -> Result<EngineInfo, EngineError>;
        async fn count(&self) -> Result<SiteRequestCount, EngineError>;
        /// One page of every request, newest first.
        async fn requests(&self, take: u32, skip: u32) -> Result<SiteRequestPage, EngineError>;
        /// One page of the site's accounts, oldest first.
        async fn users(&self, take: u32, skip: u32) -> Result<SiteUserPage, EngineError>;
        /// `None` when the site does not know the title.
        async fn media(
            &self,
            kind: TitleKind,
            tmdb_id: i64,
        ) -> Result<Option<SiteMedia>, EngineError>;
        async fn discover(
            &self,
            list: DiscoverList,
            page: u32,
        ) -> Result<DiscoverPage, EngineError>;
    }

    /// How many workers of each kind: a node's limits, or the lengths of its queues.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct WorkerCounts {
        pub transcode_gpu: i64,
        pub transcode_cpu: i64,
        pub health_gpu: i64,
        pub health_cpu: i64,
    }

    /// One node of a worker service: a machine that runs its workers.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ProcessorNode {
        pub id: String,
        pub name: String,
        pub paused: bool,
        /// Workers busy now.
        pub busy: i64,
        pub limits: WorkerCounts,
        pub queue: WorkerCounts,
    }

    /// What a processing service says about itself.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ProcessorStatus {
        pub version: Option<String>,
        /// One line about its state, in its adapter's words.
        pub detail: String,
        /// A bridge's own facts, as a small JSON object.
        pub facts: Option<serde_json::Value>,
        /// A worker service's nodes.
        pub nodes: Option<Vec<ProcessorNode>>,
    }

    /// A file a job names: where it is and, when the service says, its size in bytes.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
    pub struct ProcessorFile {
        pub path: String,
        pub size: Option<i64>,
    }

    /// A file an imported result replaced.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub struct ProcessorReplaced {
        pub path: String,
        #[serde(default)]
        pub size: Option<i64>,
        /// Whether the replaced file was still on disk when the service looked.
        #[serde(default)]
        pub still_there: Option<bool>,
    }

    /// One event of a job, in the service's words.
    #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    pub struct ProcessorEvent {
        pub at: DateTime<Utc>,
        pub event: String,
        #[serde(default)]
        pub actor: Option<String>,
        /// The first of the service's note, reason, error or detail that is set.
        #[serde(default)]
        pub note: Option<String>,
    }

    /// One job of a processing service's own history, with its state in the product's words:
    /// `staged` (waiting for a worker), `verifying` (the output is being checked), `imported`,
    /// `failed`, `refused`, `pruned`, `superseded`.
    #[derive(Debug, Clone, PartialEq, serde::Serialize)]
    pub struct ProcessorRun {
        /// The service's own id of the job.
        pub job_id: String,
        /// The flow, by the service's id of it.
        pub flow: String,
        /// What started it, as the service words it.
        pub trigger: String,
        pub state: String,
        pub created: DateTime<Utc>,
        pub updated: Option<DateTime<Utc>>,
        /// When it closed. `None` while it is open.
        pub finished: Option<DateTime<Utc>>,
        /// Where the file waits for a worker.
        pub queue_path: Option<String>,
        pub source: Option<ProcessorFile>,
        /// What the worker made, before it is imported.
        pub output: Option<ProcessorFile>,
        /// The imported file.
        pub result: Option<ProcessorFile>,
        /// The manager's id of the imported file, when the service names it.
        pub manager_file_id: Option<i64>,
        pub replaced: Vec<ProcessorReplaced>,
        /// Why it was refused or failed, in the service's words.
        pub detail: Option<String>,
        pub events: Vec<ProcessorEvent>,
        /// The job exactly as the service gave it.
        pub raw: serde_json::Value,
    }

    /// Something a worker is on now. Every field is as the service reports it and absent when
    /// it does not.
    #[derive(Debug, Clone, PartialEq)]
    pub struct ProcessorWorker {
        /// The worker's own id, by which the service is asked to cancel what it is on.
        pub id: Option<String>,
        /// The node it runs on, by the service's id of the node.
        pub node_id: Option<String>,
        /// The node it runs on, by name.
        pub node: Option<String>,
        /// The path it works on.
        pub file: Option<String>,
        /// 0 to 100.
        pub percent: Option<f64>,
        pub fps: Option<f64>,
        /// The time left, as the service writes it.
        pub eta: Option<String>,
        pub status: Option<String>,
        /// The kind of worker, as the service names it.
        pub kind: Option<String>,
        pub started_at: Option<DateTime<Utc>>,
        /// The service's own mark of the file it is on, when it names one. A worker that runs
        /// a flow works on a copy of the file, so `file` is not where the file waits: the
        /// mark is what its record of the waiting file carries too ([`WorkerFile::footprint`]).
        pub footprint: Option<String>,
        /// The whole entry as the service gave it.
        pub raw: serde_json::Value,
    }

    /// What a file must be for a flow to make sense. The interface greys a flow out with it;
    /// nothing is enforced with it.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct FlowNeeds {
        /// The fewest lines the file must have: 2160.
        pub min_resolution: Option<i32>,
        /// Video codecs the file must not already be in, in the product's words: `HEVC`.
        pub not_codec: Vec<String>,
    }

    /// One flow a person would choose: what a processing service can do to a file. A service
    /// may run one flow under several ids of its own (`modes`).
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ProcessorFlow {
        /// The first of its modes.
        pub id: String,
        pub label: String,
        /// One line about what it changes.
        pub about: Option<String>,
        /// The manager tag that starts it, where there is one.
        pub tag: Option<String>,
        /// Every id the service runs it under.
        pub modes: Vec<String>,
        pub needs: Option<FlowNeeds>,
    }

    /// What started a job, in the product's words.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct TriggerWords {
        /// `rule`, `backlog`, `tag`, `manual`, `api` (sent on demand) or `other`.
        pub kind: &'static str,
        pub label: String,
    }

    /// The name Singularr gives a service for itself when it hands a file over, so the
    /// service's own record of the job says who asked.
    pub const SENDER: &str = "singularr";

    /// The names this product sent under before, which a service's records still hold.
    pub const EARLIER_SENDERS: &[&str] = &["marquee"];

    /// What a service says it would do with one file for one of its flows.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StageOption {
        /// The flow, by the id a caller sends it under.
        pub flow: String,
        /// What the service would run for this file under that flow, by its own id of it.
        pub mode: Option<String>,
        /// It would take the file.
        pub would_stage: bool,
        /// The queue folder the file would go under, and the path it would get there.
        pub queue: Option<String>,
        pub queue_path: Option<String>,
        /// Why it would not, in the service's word: `already_staged`, `outside_roots`,
        /// `no_mode`, `not_found`.
        pub reason: Option<String>,
        /// The service's id of the job that has the file: an open one, or one that is over
        /// and whose file still lies in a queue.
        pub job: Option<String>,
    }

    /// What a service says it would do with one file, flow by flow. Asking changes nothing.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StageOptions {
        /// The service finds the file.
        pub exists: bool,
        pub flows: Vec<StageOption>,
    }

    /// Why a service did not take a file.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Refusal {
        /// It does not find the file.
        NotFound,
        /// The file is in a queue already, or an open job has it.
        AlreadyStaged,
        /// It will not take this file for this flow: outside what it serves, or its own
        /// guards said no.
        Refused,
        /// It did not accept the key it was sent.
        Unauthorized,
        /// It answered something else.
        Failed,
    }

    /// How handing a file to a flow ended.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Sent {
        /// The service takes no file on demand.
        NotOffered,
        /// The file is staged.
        Staged {
            /// What the service runs, by its own id of it.
            mode: Option<String>,
            /// Its id of the job it opened, when it opened one.
            job: Option<String>,
            /// Where the file waits now.
            queue_path: Option<String>,
        },
        /// The service did not take it.
        Refused {
            why: Refusal,
            /// Its reason, in its own word.
            reason: Option<String>,
            /// Its own words for it.
            detail: Option<String>,
            /// Its id of the job that has the file: an open one, or one that is over and
            /// whose file still lies in a queue.
            job: Option<String>,
        },
        /// It did not answer in time, or gave up waiting on its own work. That is not a no:
        /// it may still stage the file. The call is never made a second time.
        Unanswered,
    }

    /// The jobs a service was asked for by id.
    #[derive(Debug, Clone, PartialEq)]
    pub struct JobsById {
        pub jobs: Vec<ProcessorRun>,
        /// The ids it has no job for.
        pub missing: Vec<String>,
    }

    /// One page of a service's jobs, newest first.
    #[derive(Debug, Clone, PartialEq)]
    pub struct JobsPage {
        pub jobs: Vec<ProcessorRun>,
        /// What to ask the next, older page with. `None`: there is no older job.
        pub next: Option<String>,
    }

    /// One library of a worker service: a folder it watches, and the flow it runs on the files
    /// that appear there.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WorkerLibrary {
        pub id: String,
        pub name: String,
        /// The folder it watches, without a trailing separator.
        pub folder: String,
        /// The id of the flow it runs.
        pub flow_id: Option<String>,
        pub priority: Option<i64>,
        /// New files are held back for a while before a worker takes them.
        pub held: bool,
        /// It watches its folder for new files.
        pub watching: bool,
        /// Files it transcoded and checked, and the bytes its transcodes saved. A counter the
        /// worker has not started is 0, as the worker itself shows it.
        pub transcodes: i64,
        pub health_checks: i64,
        pub saved_bytes: i64,
    }

    /// One flow of a worker service: the steps a file goes through.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WorkerFlow {
        pub id: String,
        pub name: String,
        /// What it does, in its author's words.
        pub about: Option<String>,
        /// The names of its steps, in the order the worker lists them.
        pub steps: Vec<String>,
    }

    /// A worker service's own configuration: its libraries and its flows.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct WorkerSetup {
        pub libraries: Vec<WorkerLibrary>,
        pub flows: Vec<WorkerFlow>,
    }

    impl WorkerSetup {
        /// The library whose folder holds this path: the one with the longest folder that
        /// the path lies below.
        pub fn library_of(&self, path: &str) -> Option<&WorkerLibrary> {
            let path = path.trim();
            self.libraries
                .iter()
                .filter(|l| {
                    !l.folder.is_empty()
                        && path
                            .strip_prefix(l.folder.as_str())
                            .is_some_and(|rest| rest.starts_with(['/', '\\']))
                })
                .max_by_key(|l| l.folder.len())
        }

        pub fn flow(&self, id: &str) -> Option<&WorkerFlow> {
            self.flows.iter().find(|f| f.id == id)
        }
    }

    /// How many files a worker service has in each of its tables.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct WorkerTables {
        /// Waiting to be transcoded.
        pub queue: i64,
        /// Transcoded, or not in need of it.
        pub done: i64,
        /// The transcode failed.
        pub failed: i64,
        /// Waiting for a health check.
        pub health_queue: i64,
        pub healthy: i64,
        /// The health check failed.
        pub health_failed: i64,
    }

    /// A worker service in numbers, over everything it ever did.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct WorkerTotals {
        /// Files it holds a record of now.
        pub files: i64,
        pub transcodes: i64,
        pub health_checks: i64,
        /// Bytes its transcodes saved.
        pub saved_bytes: i64,
        pub tables: WorkerTables,
    }

    /// What a worker service's own records say of one file.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WorkerFile {
        /// The library it is in, by the worker's id of it.
        pub library: Option<String>,
        /// Where its transcode stands, in the product's words: `queued`, `done`, `failed` or
        /// `notRequired`. `None` for words the adapter does not know.
        pub state: Option<&'static str>,
        /// The same in the worker's own words.
        pub words: Option<String>,
        /// Its health check, in the worker's own words.
        pub health: Option<String>,
        /// Its size before and after the transcode, in bytes.
        pub old_size: Option<i64>,
        pub new_size: Option<i64>,
        pub transcoded_at: Option<DateTime<Utc>>,
        /// The worker holds it back until then.
        pub hold_until: Option<DateTime<Utc>>,
        /// The service's own mark of the file. A busy worker that names the same mark is on
        /// this file ([`ProcessorWorker::footprint`]).
        pub footprint: Option<String>,
    }

    /// The answer to a question about one file.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FileAnswer {
        /// The service keeps no records of files that can be asked for.
        NotOffered,
        /// It has no record of that file.
        Absent,
        Known(WorkerFile),
    }

    /// One report a worker service keeps of one job it ran on a file.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WorkerReport {
        /// The report's own name: what its text is asked for by.
        pub id: String,
        /// The job it reports, by the service's id of it.
        pub job: Option<String>,
        /// What the job was, in the service's word: `transcode`, `healthcheck`.
        pub kind: Option<String>,
        /// When the job started.
        pub at: Option<DateTime<Utc>>,
        /// The version of the service that ran it.
        pub version: Option<String>,
    }

    /// The text of one report.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ReportText {
        pub text: String,
        /// The job is still running, so the text still grows.
        pub running: bool,
        /// False: the service has no such report.
        pub exists: bool,
    }

    /// One file in a worker service's queue.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct QueuedFile {
        pub path: String,
        /// The library it is in, by the worker's id of it.
        pub library: Option<String>,
        pub size: Option<i64>,
        /// The video codec and resolution, in the worker's own words: `h264`, `1080p`.
        pub codec: Option<String>,
        pub resolution: Option<String>,
        /// Moved to the front of the queue.
        pub bumped: bool,
        /// The worker holds it back until then.
        pub hold_until: Option<DateTime<Utc>>,
        /// When the worker first recorded it.
        pub added: Option<DateTime<Utc>>,
    }

    /// The first files of a worker service's queue in the order it gives them, and how many it
    /// holds.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct WorkerQueue {
        pub total: i64,
        pub files: Vec<QueuedFile>,
        /// When it was read.
        pub read_at: DateTime<Utc>,
    }

    /// What the server of a worker service uses now. Absent when it does not say.
    #[derive(Debug, Clone, Default, PartialEq)]
    pub struct WorkerResources {
        /// The machine's processor, 0 to 100.
        pub cpu_percent: Option<f64>,
        /// The machine's memory in use and in all, in bytes.
        pub memory_used: Option<i64>,
        pub memory_total: Option<i64>,
        /// The server process: its memory now and at its peak, in bytes, and how long it runs.
        pub process_memory: Option<i64>,
        pub process_memory_peak: Option<i64>,
        pub uptime_seconds: Option<i64>,
    }

    /// One period of a worker service's history of space saved.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct SavedBucket {
        /// The period, in the service's own words.
        pub bucket: String,
        pub saved_bytes: i64,
    }

    /// One period of a worker service's history of verdicts: how its jobs ended.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct VerdictBucket {
        pub bucket: String,
        pub transcoded: i64,
        pub transcode_failed: i64,
        pub not_required: i64,
        pub healthy: i64,
        pub health_failed: i64,
    }

    /// A worker service's history, as it keeps it.
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct WorkerHistory {
        pub saved: Vec<SavedBucket>,
        pub verdicts: Vec<VerdictBucket>,
    }

    /// The four kinds of worker a node runs.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum WorkerKind {
        TranscodeGpu,
        TranscodeCpu,
        HealthGpu,
        HealthCpu,
    }

    impl WorkerKind {
        pub const ALL: [WorkerKind; 4] = [
            WorkerKind::TranscodeGpu,
            WorkerKind::TranscodeCpu,
            WorkerKind::HealthGpu,
            WorkerKind::HealthCpu,
        ];

        /// The API's word: `transcodeGpu`, as the limits are named in every answer.
        pub fn as_str(self) -> &'static str {
            match self {
                WorkerKind::TranscodeGpu => "transcodeGpu",
                WorkerKind::TranscodeCpu => "transcodeCpu",
                WorkerKind::HealthGpu => "healthGpu",
                WorkerKind::HealthCpu => "healthCpu",
            }
        }

        pub fn parse(word: &str) -> Option<WorkerKind> {
            WorkerKind::ALL
                .into_iter()
                .find(|k| k.as_str().eq_ignore_ascii_case(word.trim()))
        }

        /// This kind's number of a node's limits or queue lengths.
        pub fn of(self, counts: &WorkerCounts) -> i64 {
            match self {
                WorkerKind::TranscodeGpu => counts.transcode_gpu,
                WorkerKind::TranscodeCpu => counts.transcode_cpu,
                WorkerKind::HealthGpu => counts.health_gpu,
                WorkerKind::HealthCpu => counts.health_cpu,
            }
        }
    }

    /// A change to a worker service. Each is an act: `core::outbound` lets it through only
    /// while the integration's access is `act`, and only in the form its adapter declares.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum WorkerAct {
        /// One worker more, or one fewer, of one kind on a node.
        Limit {
            node: String,
            kind: WorkerKind,
            up: bool,
        },
        /// Pause a node, or resume it.
        Pause { node: String, paused: bool },
        /// Cancel what one worker is on.
        Cancel { node: String, worker: String },
        /// Put one file back in the transcode queue.
        Requeue { path: String },
        /// Look for new files in a library's folder.
        Scan { library: String, folder: String },
        /// Watch a library's folder for new files, or stop.
        Watch {
            library: String,
            folder: String,
            on: bool,
        },
        /// Run this flow in a library.
        Flow { library: String, flow: String },
    }

    impl WorkerAct {
        /// What it is, in the word the journal and the adapter's actions use.
        pub fn word(&self) -> &'static str {
            match self {
                WorkerAct::Limit { .. } => "limits",
                WorkerAct::Pause { .. } => "pause",
                WorkerAct::Cancel { .. } => "cancel",
                WorkerAct::Requeue { .. } => "requeue",
                WorkerAct::Scan { .. } => "scan",
                WorkerAct::Watch { .. } => "watch",
                WorkerAct::Flow { .. } => "flow",
            }
        }
    }

    /// How an act on a worker service ended.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Acted {
        /// The service took it.
        Done,
        /// The service offers no such act.
        NotOffered,
        /// The service said no, in its own words.
        Refused(String),
    }

    /// A processing service: a worker that rewrites files, or a bridge that hands files to
    /// one and imports the results. The words for its flows and triggers are its adapter's.
    /// What only some services offer (a worker's own records; a bridge's jobs by id, its word
    /// on what it would do with a file, and taking one) has a default of "not offered", so a
    /// service without it is unaffected. One method changes something in the service:
    /// [`Processor::send`], which `core::outbound` lets through only while the integration's
    /// access is `act`.
    #[async_trait]
    pub trait Processor: Send + Sync {
        /// The integration id.
        fn id(&self) -> &str;
        /// The integration type: `hush`, `tdarr`.
        fn kind(&self) -> &'static str;
        /// What it is in the pipeline: `bridge` or `worker`.
        fn role(&self) -> &'static str;
        /// Whether it answers, with its version when it has one, a line about its state and
        /// its facts.
        async fn status(&self) -> Result<ProcessorStatus, EngineError>;
        /// The newest `limit` jobs of its own history, newest first. Empty for a service
        /// whose history cannot be read with GET.
        async fn runs(&self, limit: u32) -> Result<Vec<ProcessorRun>, EngineError>;
        /// What it works on now. Empty for a service that does no work itself.
        async fn workers(&self) -> Result<Vec<ProcessorWorker>, EngineError>;
        /// The flows it offers, in the order a person chooses from.
        fn flows(&self) -> Vec<ProcessorFlow>;
        /// The label of a flow by one of its ids; the id itself for one it does not know.
        fn flow_label(&self, flow: &str) -> String {
            self.flows()
                .into_iter()
                .find(|f| f.modes.iter().any(|m| m == flow))
                .map(|f| f.label)
                .unwrap_or_else(|| flow.to_string())
        }
        /// What started a job, from the service's own text.
        fn trigger(&self, raw: &str) -> TriggerWords {
            TriggerWords {
                kind: "other",
                label: raw.to_string(),
            }
        }
        /// The tag on a title in its manager with which the owner keeps that title away from
        /// this service. A file of a title that carries it is never handed over, whatever the
        /// service itself would do with it. `None`: the service knows no such tag.
        fn protecting_tag(&self) -> Option<&'static str> {
            None
        }
        /// Read the service's own configuration (a worker's libraries and flows) and keep it
        /// in memory, for [`Processor::setup`]. It is configuration: read at start and with
        /// every whole read, never for a page. Nothing to do for a service that has none.
        async fn read_setup(&self) -> Result<(), EngineError> {
            Ok(())
        }
        /// The configuration last read. `None`: the service has none, or it was not read yet.
        fn setup(&self) -> Option<Arc<WorkerSetup>> {
            None
        }
        /// Read the service's totals and keep them in memory, for [`Processor::totals`].
        async fn read_totals(&self) -> Result<(), EngineError> {
            Ok(())
        }
        /// The totals last read. `None`: the service has none, or they were not read yet.
        fn totals(&self) -> Option<WorkerTotals> {
            None
        }
        /// What the service's own records say of one file, by the path it knows the file
        /// under.
        async fn file(&self, _path: &str) -> Result<FileAnswer, EngineError> {
            Ok(FileAnswer::NotOffered)
        }
        /// What the service would do with one file, for each of its flows. Asking changes
        /// nothing. `None`: it does not say.
        async fn options(&self, _path: &str) -> Result<Option<StageOptions>, EngineError> {
            Ok(None)
        }
        /// Hand one file to one flow. This is an act: it is refused on the way out unless the
        /// integration's access is `act`. `requested_by` names the caller in the service's
        /// own record of the job.
        async fn send(
            &self,
            _path: &str,
            _flow: &str,
            _requested_by: &str,
        ) -> Result<Sent, EngineError> {
            Ok(Sent::NotOffered)
        }
        /// The jobs with these ids of the service's own. `None`: it cannot be asked by id.
        async fn jobs_by_id(&self, _ids: &[String]) -> Result<Option<JobsById>, EngineError> {
            Ok(None)
        }
        /// At most `limit` of its jobs that are older than the job `before`, newest first.
        /// `None`: it lists no more than [`Processor::runs`] gives.
        async fn jobs_before(
            &self,
            _before: &str,
            _limit: u32,
        ) -> Result<Option<JobsPage>, EngineError> {
            Ok(None)
        }
        /// The reports a worker keeps of the jobs it ran on the file that carries this
        /// footprint, newest first. `None`: it keeps none that can be asked for.
        async fn reports(
            &self,
            _footprint: &str,
        ) -> Result<Option<Vec<WorkerReport>>, EngineError> {
            Ok(None)
        }
        /// The text of one of those reports. `None`: it keeps none that can be asked for.
        async fn report(
            &self,
            _footprint: &str,
            _report: &WorkerReport,
        ) -> Result<Option<ReportText>, EngineError> {
            Ok(None)
        }
        /// Read the first files of its queue and keep them, for [`Processor::queue`].
        async fn read_queue(&self) -> Result<(), EngineError> {
            Ok(())
        }
        /// Its queue as last read. `None`: it has none, or it was not read yet.
        fn queue(&self) -> Option<WorkerQueue> {
            None
        }
        /// What its server uses now. `None`: it does not say.
        async fn resources(&self) -> Result<Option<WorkerResources>, EngineError> {
            Ok(None)
        }
        /// Its history of space saved and of verdicts. `None`: it keeps none.
        async fn history(&self) -> Result<Option<WorkerHistory>, EngineError> {
            Ok(None)
        }
        /// Change something in a worker service. This is an act: refused on the way out unless
        /// the integration's access is `act`.
        async fn act(&self, _act: &WorkerAct) -> Result<Acted, EngineError> {
            Ok(Acted::NotOffered)
        }
    }
}
