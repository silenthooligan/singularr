//! One typed configuration, read once at startup.
//!
//! Every variable the program reads is listed here and in the README. A value that is empty or
//! still the placeholder `CHANGEME` counts as unset, so a fresh `.env` starts without calling
//! an engine with a wrong key.
//!
//! The connections are **integrations**: typed (`plex`, `jellyfin`, `emby`, `radarr`, `sonarr`,
//! the download clients `qbittorrent`, `transmission`, `deluge`, `sabnzbd` and `nzbget`,
//! `prowlarr`, `tdarr`, `hush`, `seerr`, `tautulli`), any number per type, each with an id, a display name,
//! an address and a secret. The code names no instance. A simple install lists them in the
//! environment, one per type (`RADARR_URL` and `RADARR_API_KEY`); a larger one lists them in
//! the instance file named by `SINGULARR_INSTANCE` (`singularr.toml`), which names each secret's
//! variable and never holds a secret itself. The workspace names and public addresses are configuration too.
//!
//! What Singularr may do with each action of an integration can be fixed here too:
//! `<TYPE>_ACCESS` in the environment form, `access` on the integration in the instance file.
//! Each takes `read`, `act` (every action of the type) or a comma list of action ids. Left out,
//! each action has what the owner saved in the interface, else `read`.
//!
//! The bootstrap is the platform's (`singularr_platform::config`): the database, the instance
//! file's path, the owners and the edge, the time limit of a call and the face. Its names keep
//! their paths here. The parsing knows no manifest of its own: it reads each type's facts from
//! the table it is handed ([`Manifests`]). The engines hold the table and the constructors that
//! pass it (`Config::from`, `Config::with_instance`).

use super::outbound::Grant;
pub use singularr_platform::config::{
    Bootstrap, ConfigError, Env, Face, PLACEHOLDER, ProcessEnv, redact_url,
};
use singularr_platform::config::{seconds, set};
use singularr_sdk::manifest::Manifest;
use std::fmt;
use std::time::Duration;

/// The kinds of integration this version knows. Each has a manifest (`engines::manifests`). The
/// parsing reads it from the table it is handed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IntegrationType {
    Plex,
    /// A Jellyfin server, read through its HTTP API with an API key.
    Jellyfin,
    /// An Emby server, read through its HTTP API with an API key.
    Emby,
    Radarr,
    Sonarr,
    Qbittorrent,
    Transmission,
    Deluge,
    Sabnzbd,
    Nzbget,
    Prowlarr,
    Tdarr,
    /// The HUSH bridge: it stages files for a worker and imports what the worker made.
    Hush,
    Seerr,
    /// Tautulli: the media server's plays, kept for years, with durations and delivery.
    Tautulli,
}

impl IntegrationType {
    pub const ALL: [IntegrationType; 15] = [
        IntegrationType::Plex,
        IntegrationType::Jellyfin,
        IntegrationType::Emby,
        IntegrationType::Radarr,
        IntegrationType::Sonarr,
        IntegrationType::Qbittorrent,
        IntegrationType::Transmission,
        IntegrationType::Deluge,
        IntegrationType::Sabnzbd,
        IntegrationType::Nzbget,
        IntegrationType::Prowlarr,
        IntegrationType::Tdarr,
        IntegrationType::Hush,
        IntegrationType::Seerr,
        IntegrationType::Tautulli,
    ];

    /// The type's id, as the instance file and the API name it. Its manifest names it too
    /// (`engines::manifests` holds the two to one id).
    pub fn as_str(self) -> &'static str {
        match self {
            IntegrationType::Plex => "plex",
            IntegrationType::Jellyfin => "jellyfin",
            IntegrationType::Emby => "emby",
            IntegrationType::Radarr => "radarr",
            IntegrationType::Sonarr => "sonarr",
            IntegrationType::Qbittorrent => "qbittorrent",
            IntegrationType::Transmission => "transmission",
            IntegrationType::Deluge => "deluge",
            IntegrationType::Sabnzbd => "sabnzbd",
            IntegrationType::Nzbget => "nzbget",
            IntegrationType::Prowlarr => "prowlarr",
            IntegrationType::Tdarr => "tdarr",
            IntegrationType::Hush => "hush",
            IntegrationType::Seerr => "seerr",
            IntegrationType::Tautulli => "tautulli",
        }
    }

    pub fn parse(text: &str) -> Option<IntegrationType> {
        let t = text.trim().to_ascii_lowercase();
        IntegrationType::ALL
            .into_iter()
            .find(|k| k.as_str() == t)
            .or_else(|| (t == "qbit").then_some(IntegrationType::Qbittorrent))
    }

    /// The download client of this type, for a type that is one.
    pub fn client(self) -> Option<ClientKind> {
        ClientKind::ALL
            .into_iter()
            .find(|c| c.integration() == self)
    }

    /// The media server types: the servers that hold the libraries and play them.
    pub fn serves_media(self) -> bool {
        MediaServerKind::of(self).is_some()
    }
}

/// Where the parsing finds each type's manifest: the engines' table
/// (`engines::manifests::MANIFESTS`).
#[derive(Clone, Copy)]
pub struct Manifests(pub fn(IntegrationType) -> &'static Manifest);

impl Manifests {
    /// The manifest of a type.
    pub fn of(self, kind: IntegrationType) -> &'static Manifest {
        (self.0)(kind)
    }
}

/// One configured connection. The secret is never printed: the printout says whether it is
/// set and from which variable.
#[derive(Clone, PartialEq, Eq)]
pub struct Integration {
    /// Lower case letters, digits and `-`; unique. The environment form uses the type name.
    pub id: String,
    pub kind: IntegrationType,
    pub name: String,
    pub role: String,
    /// The base address, without a trailing `/`.
    pub url: Option<String>,
    /// Where people open the server's web app, for the links to its items (Jellyfin, Emby):
    /// `<PREFIX>_WEB_URL` or the instance file's `web_url`. `None`: at `url`.
    pub web_url: Option<String>,
    /// The sign-in name (qBittorrent, Transmission, NZBGet).
    pub user: Option<String>,
    pub secret: Option<String>,
    /// The variable the secret is read from.
    pub secret_var: String,
    /// The variable of the address, for messages: `RADARR_URL`, or the instance file.
    pub url_var: String,
    /// What the environment or the instance file says Singularr may do with it. `None`: neither
    /// says, and the access is the one saved in the interface, else `read`.
    pub access: Option<Grant>,
    /// What says so, for messages: the variable of the environment form (`HUSH_ACCESS`), or
    /// the integration's `access` in the instance file.
    pub access_var: String,
    /// `environment` or `instance file`.
    pub source: &'static str,
    /// Its type runs without a secret (its manifest's `secret.optional`): an address alone is a
    /// usable integration.
    pub secret_optional: bool,
}

impl Integration {
    /// Address and (where the type needs one) secret are set, so the integration is asked.
    pub fn configured(&self) -> bool {
        self.url.is_some() && (self.secret.is_some() || self.secret_optional)
    }

    /// Why an integration is not configured, in words. `None` when it is.
    pub fn missing(&self) -> Option<String> {
        if self.url.is_none() {
            Some(format!("{} is not set", self.url_var))
        } else if self.secret.is_none() && !self.secret_optional {
            Some(format!("{} is not set", self.secret_var))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrKind {
    Radarr,
    Sonarr,
}

impl ArrKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArrKind::Radarr => "radarr",
            ArrKind::Sonarr => "sonarr",
        }
    }
}

/// One Radarr or Sonarr integration, as the arr adapter takes it. `url` and `api_key` are both
/// set or the engine is not configured.
#[derive(Clone, PartialEq, Eq)]
pub struct ArrConfig {
    /// The integration id, which is the engine id everywhere.
    pub id: String,
    pub kind: ArrKind,
    /// The library this integration manages. Until libraries come from the media server it is
    /// the integration's own library, with the integration's id.
    pub library: String,
    pub url: Option<String>,
    pub api_key: Option<String>,
}

impl ArrConfig {
    pub fn configured(&self) -> bool {
        self.url.is_some() && self.api_key.is_some()
    }
}

/// The kinds of media server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaServerKind {
    Plex,
    Jellyfin,
    Emby,
}

impl MediaServerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MediaServerKind::Plex => "plex",
            MediaServerKind::Jellyfin => "jellyfin",
            MediaServerKind::Emby => "emby",
        }
    }

    /// The media server kind of an integration type. `None` for a type that is no media server.
    pub fn of(kind: IntegrationType) -> Option<MediaServerKind> {
        match kind {
            IntegrationType::Plex => Some(MediaServerKind::Plex),
            IntegrationType::Jellyfin => Some(MediaServerKind::Jellyfin),
            IntegrationType::Emby => Some(MediaServerKind::Emby),
            _ => None,
        }
    }
}

/// One media server integration (a Plex, Jellyfin or Emby server), as its adapter takes it:
/// its address and its secret (Plex's token, the API key of Jellyfin and Emby).
#[derive(Clone, PartialEq, Eq)]
pub struct MediaServerConfig {
    pub id: String,
    pub kind: MediaServerKind,
    pub url: Option<String>,
    pub secret: Option<String>,
    /// Where people open a Jellyfin or Emby server's web app, when that is not `url`: the
    /// address of the links to its items.
    pub web_url: Option<String>,
}

impl MediaServerConfig {
    pub fn configured(&self) -> bool {
        self.url.is_some() && self.secret.is_some()
    }
}

/// The kinds of download client: three for torrents, two for usenet downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    Qbittorrent,
    Transmission,
    Deluge,
    Sabnzbd,
    Nzbget,
}

impl ClientKind {
    pub const ALL: [ClientKind; 5] = [
        ClientKind::Qbittorrent,
        ClientKind::Transmission,
        ClientKind::Deluge,
        ClientKind::Sabnzbd,
        ClientKind::Nzbget,
    ];

    pub fn integration(self) -> IntegrationType {
        match self {
            ClientKind::Qbittorrent => IntegrationType::Qbittorrent,
            ClientKind::Transmission => IntegrationType::Transmission,
            ClientKind::Deluge => IntegrationType::Deluge,
            ClientKind::Sabnzbd => IntegrationType::Sabnzbd,
            ClientKind::Nzbget => IntegrationType::Nzbget,
        }
    }

    pub fn as_str(self) -> &'static str {
        self.integration().as_str()
    }

    /// It downloads from usenet servers (SABnzbd, NZBGet).
    pub fn usenet(self) -> bool {
        matches!(self, ClientKind::Sabnzbd | ClientKind::Nzbget)
    }
}

/// One download client integration, as its adapter takes it. Whether it is asked is the
/// engines' (`engines::clients`), as its type's manifest says.
#[derive(Clone, PartialEq, Eq)]
pub struct DownloadClientConfig {
    /// The integration id.
    pub id: String,
    pub kind: ClientKind,
    pub url: Option<String>,
    /// The sign-in name, for a client that takes one.
    pub user: Option<String>,
    /// Its password, or SABnzbd's API key.
    pub secret: Option<String>,
}

/// The Seerr integration the requests are read from.
#[derive(Clone, PartialEq, Eq)]
pub struct SeerrConfig {
    pub id: String,
    pub url: String,
    pub api_key: Option<String>,
}

impl SeerrConfig {
    pub fn configured(&self) -> bool {
        self.api_key.is_some()
    }
}

/// The kinds of processing integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessorKind {
    Tdarr,
    Hush,
}

impl ProcessorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProcessorKind::Tdarr => "tdarr",
            ProcessorKind::Hush => "hush",
        }
    }
}

/// One processing integration (a Tdarr, a HUSH bridge), as its adapter takes it.
#[derive(Clone, PartialEq, Eq)]
pub struct ProcessorConfig {
    /// The integration id.
    pub id: String,
    pub kind: ProcessorKind,
    pub url: Option<String>,
    /// The service's key, when it asks for one: Tdarr's for every call, the bridge's for the
    /// calls that change something.
    pub api_key: Option<String>,
}

impl ProcessorConfig {
    pub fn configured(&self) -> bool {
        self.url.is_some()
    }
}

/// The Tautulli integration plays are imported from: its address and its key. The key travels
/// in the query string of every call, so it is never printed and never put in an error.
#[derive(Clone, PartialEq, Eq)]
pub struct TautulliConfig {
    pub id: String,
    pub url: Option<String>,
    pub api_key: Option<String>,
}

impl TautulliConfig {
    pub fn configured(&self) -> bool {
        self.url.is_some() && self.api_key.is_some()
    }
}

/// The request rules Singularr shows on each request. They mirror seerr-bridge (the season
/// gate) and Maintainerr (the friend-request cleanup); Singularr applies neither in stage 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRulesConfig {
    /// The owner's Seerr accounts, by Seerr user id, from `SINGULARR_REQUEST_OWNERS` (`1`).
    /// They are exempt from both rules. Ids, not names, as seerr-bridge's
    /// `SEERR_EXEMPT_USER_IDS`: a user can change their own display name.
    pub owners: Vec<i64>,
    /// Seasons of one show a friend may hold at a time (seerr-bridge `SEERR_MAX_SEASONS`).
    pub max_seasons: usize,
    /// Whether the season gate and the cleanup act (`SINGULARR_REQUEST_SEASON_GATE`).
    pub season_gate: bool,
}

/// The names and public addresses of this installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The workspace's name (`SINGULARR_WORKSPACE_NAME`). Default `Singularr`.
    pub name: String,
    /// The portal's name (`SINGULARR_PORTAL_NAME`). Default `Singularr Requests`.
    pub portal: String,
    /// The workspace's public address (`SINGULARR_OWNER_URL`), linked for the owner on the
    /// portal.
    pub url: Option<String>,
    /// The portal's public address (`SINGULARR_REQUESTS_PUBLIC_URL`).
    pub portal_url: Option<String>,
    /// Where the workspace's Sign out leads (`SINGULARR_SIGN_OUT_URL`): the sign-out address of
    /// the proxy that signs people in. Unset: the workspace offers no Sign out.
    pub sign_out_url: Option<String>,
}

/// A folder Singularr reads (the music library, the home movies): where it is mounted and how
/// often it is scanned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderConfig {
    pub root: String,
    pub interval: Duration,
}

/// The movie folders the catalog's read-only scan looks in for files no engine tracks (other
/// editions already on disk): the roots, as the engines name them and as they are mounted,
/// and how often. No roots: the scan is off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovieFoldersConfig {
    pub roots: Vec<String>,
    pub interval: Duration,
}

/// How often each engine is asked. The engines are shared production services, so the
/// defaults are slow on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub enabled: bool,
    pub titles: Duration,
    pub files_full: Duration,
    pub files_concurrency: usize,
    pub queue: Duration,
    /// Every download client: its downloads.
    pub downloads: Duration,
    pub history: Duration,
    pub history_max_pages: u32,
    pub jitter: Duration,
    /// Every request on the Seerr integration.
    pub requests: Duration,
    /// Each media server: sections, items, music.
    pub server: Duration,
    /// The processing services: their jobs and their state.
    pub processing: Duration,
}

/// Search: built in (PostgreSQL) always; Meilisearch in front of it when an address and a
/// key are both set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchConfig {
    /// `SINGULARR_SEARCH_URL`: the Meilisearch address, without a trailing `/`.
    pub url: Option<String>,
    /// `SINGULARR_SEARCH_KEY`: a Meilisearch key that may write documents and settings.
    pub key: Option<String>,
    /// `SINGULARR_SEARCH_PREFIX`, default `singularr_`: every index name starts with it.
    pub prefix: String,
    /// `SINGULARR_SEARCH_INDEX_SECS`, default 900, floor 60: the full index pass.
    pub interval: Duration,
}

impl SearchConfig {
    /// The Meilisearch address and key, when both are set.
    pub fn meilisearch(&self) -> Option<(&str, &str)> {
        Some((self.url.as_deref()?, self.key.as_deref()?))
    }
}

/// Locations (`core::geoip`): a database file of the operator's, or the monthly download of
/// DB-IP's City Lite into the data directory. The download is on unless turned off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeoIpConfig {
    /// `SINGULARR_GEOIP_PATH`: a MaxMind DB file to place addresses with. Set, nothing is
    /// downloaded.
    pub path: Option<String>,
    /// `SINGULARR_GEOIP_DOWNLOAD`: `on` downloads the database, `off` does not. Unset, the
    /// setting in Settings decides, on by default.
    pub download: Option<bool>,
    /// `SINGULARR_DATA_DIR`, default `/data`: where Singularr keeps the files it downloads.
    pub data_dir: String,
}

impl GeoIpConfig {
    /// The database may be downloaded: no file of the operator's is named and the environment
    /// does not turn the download off. Whether it is, the download setting says
    /// (`plays::locations`), on unless the owner turns it off.
    pub fn may_download(&self) -> bool {
        self.path.is_none() && self.download != Some(false)
    }
}

/// The portal's database role, kept by the full process at each start (`requests::role`).
#[derive(Clone, PartialEq, Eq)]
pub struct PortalRoleConfig {
    /// `SINGULARR_REQUESTS_DB_ROLE`: the role's name. Unset, the role `deploy/requests-role.sql`
    /// names.
    pub role: Option<String>,
    /// `SINGULARR_REQUESTS_DB_PASSWORD`: the role's password, the one in the portal's
    /// `SINGULARR_REQUESTS_DATABASE_URL`. Unset, the role is left as it is.
    pub password: Option<String>,
}

#[derive(Clone)]
pub struct Config {
    /// The platform's part: the database, the instance file, the owners and the edge, the time
    /// limit of a call.
    pub bootstrap: Bootstrap,
    pub policy_path: String,
    /// `SINGULARR_POLICY` is set: the installation pins its policy in a file, so the policy's
    /// source is the file until it is imported (`system.policySource`).
    pub policy_pinned: bool,
    /// `rules/editions.toml` in the image: the closed list of editions.
    pub editions_path: String,
    pub workspace: Workspace,
    /// Every integration, configured or not, in the order of the instance file and then the
    /// environment.
    pub integrations: Vec<Integration>,
    /// The Radarr and Sonarr integrations, as the arr adapter takes them.
    pub arrs: Vec<ArrConfig>,
    /// The media servers, in the configuration's order: the first integration of each media
    /// server type (this version reads one Plex, one Jellyfin and one Emby server).
    pub media_servers: Vec<MediaServerConfig>,
    /// The download client integrations (qBittorrent, Transmission, Deluge, SABnzbd, NZBGet),
    /// every one of them.
    pub downloads: Vec<DownloadClientConfig>,
    /// The first Seerr integration (this version reads requests from one), when there is one.
    pub seerr: Vec<SeerrConfig>,
    /// The processing integrations (Tdarr, HUSH), every one of them.
    pub processors: Vec<ProcessorConfig>,
    /// The first Tautulli integration (plays are imported from one), when there is one.
    pub tautulli: Option<TautulliConfig>,
    pub request_rules: RequestRulesConfig,
    pub music: FolderConfig,
    pub home_movies: FolderConfig,
    pub movie_folders: MovieFoldersConfig,
    pub tmdb_token: Option<String>,
    pub tvdb_key: Option<String>,
    /// The two-letter region for release dates, ratings and where to watch
    /// (`SINGULARR_WATCH_REGION`, default `US`).
    pub watch_region: String,
    /// Plex's web app, where a title's Plex link opens (`SINGULARR_PLEX_WEB_URL`, default
    /// `https://app.plex.tv/desktop`). The link adds the server's machine identifier and the
    /// item's key.
    pub plex_web_url: String,
    pub search: SearchConfig,
    pub geoip: GeoIpConfig,
    pub portal_role: PortalRoleConfig,
    pub sync: SyncConfig,
    /// Names of variables whose value was unusable, with the reason. Printed at startup.
    pub warnings: Vec<String>,
}

/// The variables of the full process that Singularr Requests refuses to start with: its
/// database login, the instance file, every integration secret of the environment form, the
/// edge token and the owner list. It refuses any other variable named like a secret too
/// ([`named_like_a_secret`]).
pub const NOT_FOR_THE_FACE: &[&str] = &[
    "DATABASE_URL",
    "SINGULARR_INSTANCE",
    "PLEX_TOKEN",
    "JELLYFIN_API_KEY",
    "EMBY_API_KEY",
    "RADARR_API_KEY",
    "SONARR_API_KEY",
    "QBIT_USER",
    "QBIT_PASS",
    "TRANSMISSION_USER",
    "TRANSMISSION_PASS",
    "DELUGE_PASS",
    "SABNZBD_API_KEY",
    "NZBGET_USER",
    "NZBGET_PASS",
    "PROWLARR_API_KEY",
    "TDARR_API_KEY",
    "HUSH_API_KEY",
    "SEERR_API_KEY",
    "PLEXSEERR_API_KEY",
    "TAUTULLI_API_KEY",
    "SINGULARR_TVDB_KEY",
    "SINGULARR_SEARCH_KEY",
    "SINGULARR_EDGE_TOKEN",
    "SINGULARR_OWNER_EMAILS",
];

/// The one variable named like a secret that Singularr Requests reads: the TMDB read token.
const FACE_TOKEN: &str = "SINGULARR_TMDB_TOKEN";

/// A variable named like a secret: an API key, a token or a password, the endings every
/// integration's secret has (`RADARR_4K_API_KEY`, `PLEX_TOKEN`, `QBIT_PASS`). Singularr Requests
/// refuses to start with one set, the TMDB read token alone excepted. A secret of the full
/// process reaches it under no such name, a file integration's included.
pub fn named_like_a_secret(name: &str) -> bool {
    name != FACE_TOKEN
        && ["_API_KEY", "_TOKEN", "_PASS"]
            .iter()
            .any(|end| name.ends_with(end))
}

/// The whole environment of Singularr Requests: its database login (the role of `deploy/requests-role.sql`),
/// its session secret, the TMDB read token and its public address. Nothing else.
#[derive(Clone)]
pub struct RequestsFaceConfig {
    pub database_url: String,
    pub session_secret: String,
    pub tmdb_token: Option<String>,
    /// The portal's public origin (`SINGULARR_REQUESTS_PUBLIC_URL`): the Plex sign-in comes
    /// back here and every absolute link starts with it. No path.
    pub public_url: String,
    pub db_max_connections: u32,
    pub http_timeout: Duration,
}

impl RequestsFaceConfig {
    pub fn from_env() -> Result<RequestsFaceConfig, ConfigError> {
        RequestsFaceConfig::from(&ProcessEnv)
    }

    pub fn from(env: &dyn Env) -> Result<RequestsFaceConfig, ConfigError> {
        let mut present: Vec<String> = NOT_FOR_THE_FACE
            .iter()
            .copied()
            .filter(|name| set(env, name).is_some())
            .map(str::to_string)
            .collect();
        let mut named: Vec<String> = env
            .names()
            .into_iter()
            .filter(|name| {
                named_like_a_secret(name) && !present.contains(name) && set(env, name).is_some()
            })
            .collect();
        named.sort();
        present.extend(named);
        if !present.is_empty() {
            return Err(ConfigError::NotForTheFace(present.join(", ")));
        }
        let database_url = set(env, "SINGULARR_REQUESTS_DATABASE_URL")
            .ok_or(ConfigError::Missing("SINGULARR_REQUESTS_DATABASE_URL"))?;
        let session_secret = set(env, "SINGULARR_REQUESTS_SESSION_SECRET")
            .ok_or(ConfigError::Missing("SINGULARR_REQUESTS_SESSION_SECRET"))?;
        if session_secret.chars().count() < 32 {
            return Err(ConfigError::Invalid(
                "SINGULARR_REQUESTS_SESSION_SECRET",
                "it must be at least 32 characters (openssl rand -hex 32)".to_string(),
            ));
        }
        let public_url = set(env, "SINGULARR_REQUESTS_PUBLIC_URL")
            .ok_or(ConfigError::Missing("SINGULARR_REQUESTS_PUBLIC_URL"))?
            .trim_end_matches('/')
            .to_string();
        let local = ["http://localhost", "http://127.0.0.1"]
            .iter()
            .any(|p| public_url == *p || public_url.starts_with(&format!("{p}:")));
        let host = public_url
            .strip_prefix("https://")
            .filter(|h| !h.is_empty() && !h.contains(['/', '?', '#', '@', ' ']));
        if host.is_none() && !local {
            return Err(ConfigError::Invalid(
                "SINGULARR_REQUESTS_PUBLIC_URL",
                format!("`{public_url}` is not an https:// origin without a path"),
            ));
        }
        let mut warnings = Vec::new();
        Ok(RequestsFaceConfig {
            database_url,
            session_secret,
            tmdb_token: set(env, "SINGULARR_TMDB_TOKEN"),
            public_url,
            db_max_connections: set(env, "SINGULARR_REQUESTS_DB_MAX_CONNECTIONS")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(5)
                .clamp(2, 10),
            http_timeout: seconds(env, "SINGULARR_HTTP_TIMEOUT_SECS", 15, 5, &mut warnings),
        })
    }
}

/// The startup printout of the portal. No secret appears in it.
impl fmt::Display for RequestsFaceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Singularr Requests configuration")?;
        writeln!(
            f,
            "  SINGULARR_REQUESTS_DATABASE_URL   {}",
            redact_url(&self.database_url)
        )?;
        writeln!(
            f,
            "  SINGULARR_REQUESTS_SESSION_SECRET {}",
            secret(&Some(self.session_secret.clone()))
        )?;
        writeln!(
            f,
            "  SINGULARR_TMDB_TOKEN              {}",
            secret(&self.tmdb_token)
        )?;
        write!(f, "  SINGULARR_REQUESTS_PUBLIC_URL     {}", self.public_url)
    }
}

/// An address without its trailing `/`.
fn base_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// The instance file as written. Unknown keys are refused, so a typo is an error and not a
/// silently missing integration.
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct InstanceFile {
    #[serde(default)]
    workspace: FileWorkspace,
    #[serde(default, rename = "integration")]
    integrations: Vec<FileIntegration>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileWorkspace {
    name: Option<String>,
    portal: Option<String>,
    url: Option<String>,
    portal_url: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileIntegration {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    name: Option<String>,
    role: Option<String>,
    url: Option<String>,
    /// Where people open a Jellyfin or Emby server's web app.
    web_url: Option<String>,
    user: Option<String>,
    /// The name of the environment variable that holds the secret.
    secret: Option<String>,
    /// What Singularr may do with it: `read` or `act`.
    access: Option<String>,
}

/// A valid integration id: 1 to 32 of `a-z`, `0-9` and `-`, starting with a letter or digit.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A valid variable name: upper case letters, digits and `_`, starting with a letter.
fn valid_var(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The action ids a grant names that the type does not declare.
fn unknown_actions(m: &Manifest, grant: &Grant) -> Vec<String> {
    match grant {
        Grant::Actions(ids) => ids
            .iter()
            .filter(|id| !m.actions.iter().any(|a| a.id == id.as_str()))
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

/// The variable a file integration's secret is read from by default: `RADARR_4K_API_KEY` for
/// the id `radarr-4k`.
fn default_secret_var(id: &str, m: &Manifest) -> String {
    format!(
        "{}_{}",
        id.to_ascii_uppercase().replace('-', "_"),
        m.secret.env_suffix
    )
}

/// Read the instance file's text into the workspace names and the integrations it lists.
fn parse_instance(
    env: &dyn Env,
    path: &str,
    text: &str,
    manifests: Manifests,
) -> Result<(FileWorkspace, Vec<Integration>), ConfigError> {
    let bad = |message: String| ConfigError::Instance {
        path: path.to_string(),
        message,
    };
    let file: InstanceFile = toml::from_str(text).map_err(|e| bad(e.to_string()))?;
    let mut out: Vec<Integration> = Vec::new();
    for entry in file.integrations {
        let id = entry.id.trim().to_string();
        if !valid_id(&id) {
            return Err(bad(format!(
                "integration id `{id}` is not 1 to 32 of a-z, 0-9 and -"
            )));
        }
        if out.iter().any(|i| i.id == id) {
            return Err(bad(format!("integration id `{id}` is listed twice")));
        }
        let kind = IntegrationType::parse(&entry.kind).ok_or_else(|| {
            bad(format!(
                "integration `{id}` has type `{}`; the types are {}",
                entry.kind,
                IntegrationType::ALL
                    .iter()
                    .map(|k| k.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
        let m = manifests.of(kind);
        let url = entry.url.as_deref().map(base_url).filter(|u| !u.is_empty());
        if let Some(u) = &url
            && !(u.starts_with("http://") || u.starts_with("https://"))
        {
            return Err(bad(format!(
                "integration `{id}` has the address `{u}`, which is not http:// or https://"
            )));
        }
        let web_url = entry
            .web_url
            .as_deref()
            .map(base_url)
            .filter(|u| !u.is_empty());
        if let Some(u) = &web_url {
            if !m.web_url {
                return Err(bad(format!(
                    "integration `{id}`: `web_url` is read for jellyfin and emby alone"
                )));
            }
            if !(u.starts_with("http://") || u.starts_with("https://")) {
                return Err(bad(format!(
                    "integration `{id}`: the web address `{u}` is not http:// or https://"
                )));
            }
        }
        let secret_var = match entry.secret.map(|s| s.trim().to_string()) {
            Some(var) if valid_var(&var) => var,
            Some(_) => {
                return Err(bad(format!(
                    "integration `{id}`: `secret` names the environment variable that holds the secret (upper case, digits and _), never the secret itself"
                )));
            }
            None => default_secret_var(&id, m),
        };
        // A word that is none of them is an error, as a typo anywhere in the file is: a
        // permission is never guessed. So is an action the type does not declare.
        let access = match entry.access.as_deref().map(str::trim) {
            None => None,
            Some(word) => {
                let ids: Vec<&str> = m.actions.iter().map(|a| a.id).collect();
                let takes = if ids.is_empty() {
                    format!("`read` or `act` ({} has no action)", m.name)
                } else {
                    format!(
                        "`read`, `act` or a list of {}'s actions ({})",
                        m.name,
                        ids.join(", ")
                    )
                };
                let grant = Grant::parse(word)
                    .filter(|grant| unknown_actions(m, grant).is_empty())
                    .ok_or_else(|| {
                        bad(format!(
                            "integration `{id}`: `access` takes {takes}. `{word}` is none of them."
                        ))
                    })?;
                Some(grant)
            }
        };
        out.push(Integration {
            name: entry
                .name
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| m.name.to_string()),
            role: entry
                .role
                .map(|r| r.trim().to_string())
                .filter(|r| !r.is_empty())
                .unwrap_or_else(|| m.role.to_string()),
            url,
            web_url,
            user: entry.user.filter(|u| !u.trim().is_empty()),
            secret: set(env, &secret_var),
            url_var: format!("the `url` of integration {id}"),
            secret_var,
            access,
            access_var: format!("the `access` of integration {id}"),
            id,
            kind,
            source: "instance file",
            secret_optional: m.secret.optional,
        });
    }
    Ok((file.workspace, out))
}

/// An address as two integrations are compared by: without a trailing `/`, in lower case.
fn same_address(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// The environment form: at most one integration per type, present when its address or its
/// secret is set. The id is the type's name.
fn env_integrations(
    env: &dyn Env,
    taken: &[Integration],
    warnings: &mut Vec<String>,
    manifests: Manifests,
) -> Vec<Integration> {
    let mut out = Vec::new();
    for kind in IntegrationType::ALL {
        let m = manifests.of(kind);
        let suffix = m.secret.env_suffix;
        let Some(prefix) = m.env_prefixes.iter().copied().find(|p| {
            set(env, &format!("{p}_URL")).is_some()
                || set(env, &format!("{p}_{suffix}")).is_some()
                || (m.takes_user && set(env, &format!("{p}_USER")).is_some())
        }) else {
            continue;
        };
        let id = kind.as_str().to_string();
        if taken.iter().any(|i| i.id == id) {
            warnings.push(format!(
                "{prefix}_URL is ignored: the instance file has the id `{id}`"
            ));
            continue;
        }
        let secret_var = format!("{prefix}_{suffix}");
        // A word that is not `read`, `act` or a list of the type's actions gives no more than
        // reading, whatever was saved in the interface: a permission is never guessed. An id the
        // type does not declare is left out.
        let access_var = format!("{prefix}_ACCESS");
        let access = set(env, &access_var).map(|word| {
            let neither = |warnings: &mut Vec<String>| {
                warnings.push(format!(
                    "{access_var}={word:?} is neither read nor act nor a list of {}'s actions; read-only applies",
                    m.name
                ));
                Grant::Read
            };
            match Grant::parse(&word) {
                Some(Grant::Actions(ids)) => {
                    let known: Vec<String> = ids
                        .iter()
                        .filter(|id| m.actions.iter().any(|a| a.id == id.as_str()))
                        .cloned()
                        .collect();
                    if known.is_empty() {
                        neither(warnings)
                    } else {
                        if known.len() < ids.len() {
                            let unknown: Vec<&str> = ids
                                .iter()
                                .filter(|id| !known.contains(id))
                                .map(String::as_str)
                                .collect();
                            warnings.push(format!(
                                "{access_var}={word:?} names actions {} does not have ({}); they are left out",
                                m.name,
                                unknown.join(", ")
                            ));
                        }
                        Grant::Actions(known)
                    }
                }
                Some(grant) => grant,
                None => neither(warnings),
            }
        });
        // Where people open a Jellyfin or Emby server's web app, when that is not the address
        // Singularr reads it at.
        let web_var = format!("{prefix}_WEB_URL");
        let web_url = match set(env, &web_var).map(|u| base_url(&u)) {
            Some(_) if !m.web_url => None,
            Some(u) if u.starts_with("http://") || u.starts_with("https://") => Some(u),
            Some(u) => {
                warnings.push(format!(
                    "{web_var}={u:?} is not an http:// or https:// address; item links open at {prefix}_URL"
                ));
                None
            }
            None => None,
        };
        out.push(Integration {
            name: set(env, &format!("{prefix}_NAME")).unwrap_or_else(|| m.name.to_string()),
            role: m.role.to_string(),
            url: set(env, &format!("{prefix}_URL")).map(|u| base_url(&u)),
            web_url,
            user: if m.takes_user {
                set(env, &format!("{prefix}_USER"))
            } else {
                None
            },
            secret: set(env, &secret_var),
            url_var: format!("{prefix}_URL"),
            secret_var,
            access,
            access_var,
            id,
            kind,
            source: "environment",
            secret_optional: m.secret.optional,
        });
    }
    out
}

impl Config {
    /// The configuration of this environment, with the instance file it names, read with these
    /// manifests. The program and its tests call `Config::from` (`engines::manifests`).
    pub fn read(env: &dyn Env, manifests: Manifests) -> Result<Config, ConfigError> {
        match set(env, "SINGULARR_INSTANCE") {
            None => Config::parse(env, None, manifests),
            Some(path) => {
                let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Instance {
                    path: path.clone(),
                    message: e.to_string(),
                })?;
                Config::parse(env, Some((&path, &text)), manifests)
            }
        }
    }

    /// The configuration of this environment and this instance file text (path, text), read
    /// with these manifests. The bootstrap comes first: its warnings lead the list.
    pub fn parse(
        env: &dyn Env,
        instance: Option<(&str, &str)>,
        manifests: Manifests,
    ) -> Result<Config, ConfigError> {
        let mut warnings = Vec::new();
        let bootstrap = Bootstrap::from(env, instance.map(|(path, _)| path), &mut warnings)?;

        let (file_workspace, mut integrations) = match instance {
            Some((path, text)) => parse_instance(env, path, text, manifests)?,
            None => (FileWorkspace::default(), Vec::new()),
        };
        let from_env = env_integrations(env, &integrations, &mut warnings, manifests);
        integrations.extend(from_env);
        for i in &integrations {
            if let Some(why) = i.missing() {
                warnings.push(format!(
                    "Integration {} ({}) not configured: {why}",
                    i.id,
                    i.kind.as_str()
                ));
            }
        }

        let arrs: Vec<ArrConfig> = integrations
            .iter()
            .filter_map(|i| {
                let kind = match i.kind {
                    IntegrationType::Radarr => ArrKind::Radarr,
                    IntegrationType::Sonarr => ArrKind::Sonarr,
                    _ => return None,
                };
                Some(ArrConfig {
                    id: i.id.clone(),
                    kind,
                    library: i.id.clone(),
                    url: i.url.clone(),
                    api_key: i.secret.clone(),
                })
            })
            .collect();
        let of = |kind: IntegrationType| integrations.iter().filter(move |i| i.kind == kind);
        // One server of each media server type is read: the first of its type, in the
        // configuration's order.
        for kind in IntegrationType::ALL
            .into_iter()
            .filter(|k| k.serves_media())
        {
            if of(kind).count() > 1 {
                warnings.push(format!(
                    "More than one {} integration; the first is used",
                    manifests.of(kind).name
                ));
            }
        }
        let mut media_servers: Vec<MediaServerConfig> = Vec::new();
        for i in &integrations {
            let Some(kind) = MediaServerKind::of(i.kind) else {
                continue;
            };
            if media_servers.iter().any(|m| m.kind == kind) {
                continue;
            }
            media_servers.push(MediaServerConfig {
                id: i.id.clone(),
                kind,
                url: i.url.clone(),
                secret: i.secret.clone(),
                web_url: i.web_url.clone(),
            });
        }
        // Every download client is read, of every type: torrents and usenet downloads side by
        // side. Two integrations at one address would read one client twice and store each of
        // its downloads twice. The first is read.
        let mut downloads: Vec<DownloadClientConfig> = Vec::new();
        for i in &integrations {
            let Some(kind) = i.kind.client() else {
                continue;
            };
            let address = i.url.as_deref().map(same_address);
            let first = address.as_ref().and_then(|a| {
                downloads
                    .iter()
                    .find(|d| d.url.as_deref().map(same_address).as_ref() == Some(a))
            });
            if let Some(first) = first {
                let name = |id: &str| {
                    integrations
                        .iter()
                        .find(|o| o.id == id)
                        .map_or_else(|| id.to_string(), |o| o.name.clone())
                };
                warnings.push(format!(
                    "{} has the address of {}: it is not read",
                    i.name,
                    name(&first.id)
                ));
                continue;
            }
            downloads.push(DownloadClientConfig {
                id: i.id.clone(),
                kind,
                url: i.url.clone(),
                user: i.user.clone().filter(|_| manifests.of(i.kind).takes_user),
                secret: i.secret.clone(),
            });
        }
        if downloads.is_empty() {
            warnings.push("No download client".to_string());
        }
        if of(IntegrationType::Seerr).count() > 1 {
            warnings
                .push("More than one Seerr integration; requests come from the first".to_string());
        }
        let seerr: Vec<SeerrConfig> = of(IntegrationType::Seerr)
            .filter_map(|i| {
                Some(SeerrConfig {
                    id: i.id.clone(),
                    url: i.url.clone()?,
                    api_key: i.secret.clone(),
                })
            })
            .take(1)
            .collect();
        let seerr_ids: Vec<String> = of(IntegrationType::Seerr).map(|i| i.id.clone()).collect();
        let processors: Vec<ProcessorConfig> = integrations
            .iter()
            .filter_map(|i| {
                let kind = match i.kind {
                    IntegrationType::Tdarr => ProcessorKind::Tdarr,
                    IntegrationType::Hush => ProcessorKind::Hush,
                    _ => return None,
                };
                Some(ProcessorConfig {
                    id: i.id.clone(),
                    kind,
                    url: i.url.clone(),
                    api_key: i.secret.clone(),
                })
            })
            .collect();
        if of(IntegrationType::Tautulli).count() > 1 {
            warnings
                .push("More than one Tautulli integration; plays come from the first".to_string());
        }
        let tautulli = of(IntegrationType::Tautulli)
            .next()
            .map(|i| TautulliConfig {
                id: i.id.clone(),
                url: i.url.clone(),
                api_key: i.secret.clone(),
            });

        let names = |name: &str, default: &str| -> Vec<String> {
            let mut list: Vec<String> = set(env, name)
                .unwrap_or_else(|| default.to_string())
                .split(',')
                .map(|n| n.trim().to_lowercase())
                .filter(|n| !n.is_empty())
                .collect();
            list.sort();
            list.dedup();
            list
        };
        // Seerr user ids. The older form `site:id` is read too: `plex:` and the id of a Seerr
        // integration name the one request site; `jellyfin:` named a second Seerr site, which
        // this version does not read.
        let mut owners: Vec<i64> = Vec::new();
        for entry in names("SINGULARR_REQUEST_OWNERS", "1") {
            let (prefix, id) = match entry.split_once(':') {
                Some((p, id)) => (Some(p.trim()), id.trim()),
                None => (None, entry.as_str()),
            };
            let known = |p: &str| p == "plex" || seerr_ids.iter().any(|s| s == p);
            match (prefix, id.parse::<i64>().ok().filter(|n| *n > 0)) {
                (Some("jellyfin"), _) => warnings.push(format!(
                    "SINGULARR_REQUEST_OWNERS entry `{entry}` ignored: no second request site is read"
                )),
                (None, Some(n)) => owners.push(n),
                (Some(p), Some(n)) if known(p) => owners.push(n),
                _ => warnings.push(format!(
                    "SINGULARR_REQUEST_OWNERS entry `{entry}` ignored: not a Seerr user ID"
                )),
            }
        }
        owners.sort_unstable();
        owners.dedup();
        let season_gate = match set(env, "SINGULARR_REQUEST_SEASON_GATE") {
            Some(v) => !matches!(
                v.to_ascii_lowercase().as_str(),
                "false" | "0" | "no" | "off"
            ),
            // The older form named the request sites the gate acted on.
            None => match set(env, "SINGULARR_REQUEST_GATED_SITES") {
                None => true,
                Some(_) => names("SINGULARR_REQUEST_GATED_SITES", "")
                    .iter()
                    .any(|s| s == "plex" || seerr_ids.contains(s)),
            },
        };
        let request_rules = RequestRulesConfig {
            owners,
            max_seasons: set(env, "SINGULARR_REQUEST_MAX_SEASONS")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(2)
                .clamp(1, 100),
            season_gate,
        };
        let folder =
            |root: &str, default: &str, secs: &str, warnings: &mut Vec<String>| FolderConfig {
                root: set(env, root).unwrap_or_else(|| default.to_string()),
                interval: seconds(env, secs, 21_600, 600, warnings),
            };
        let music = folder(
            "SINGULARR_MUSIC_ROOT",
            "/media/music",
            "SINGULARR_MUSIC_SCAN_SECS",
            &mut warnings,
        );
        let home_movies = folder(
            "SINGULARR_HOME_MOVIES_ROOT",
            "/media/homemovies",
            "SINGULARR_HOME_MOVIES_SCAN_SECS",
            &mut warnings,
        );
        // The roots the Radarr integrations keep movies under, mounted read-only at the same
        // paths, so an engine's folder is a path in the container as it stands.
        let movie_folders = MovieFoldersConfig {
            roots: set(env, "SINGULARR_MOVIE_ROOTS")
                .unwrap_or_default()
                .split(',')
                .map(|r| r.trim().trim_end_matches('/').to_string())
                .filter(|r| r.starts_with('/') && r.len() > 1)
                .collect(),
            interval: seconds(env, "SINGULARR_MOVIE_SCAN_SECS", 21_600, 600, &mut warnings),
        };

        let address = |var: &str, file: Option<String>| {
            set(env, var)
                .or(file.map(|u| u.trim().to_string()).filter(|u| !u.is_empty()))
                .map(|u| base_url(&u))
        };
        let text = |var: &str, file: Option<String>, default: &str| {
            set(env, var)
                .or(file.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()))
                .unwrap_or_else(|| default.to_string())
        };
        let workspace = Workspace {
            name: text("SINGULARR_WORKSPACE_NAME", file_workspace.name, "Singularr"),
            portal: text(
                "SINGULARR_PORTAL_NAME",
                file_workspace.portal,
                "Singularr Requests",
            ),
            url: address("SINGULARR_OWNER_URL", file_workspace.url),
            portal_url: address("SINGULARR_REQUESTS_PUBLIC_URL", file_workspace.portal_url),
            sign_out_url: set(env, "SINGULARR_SIGN_OUT_URL"),
        };

        let search = SearchConfig {
            url: set(env, "SINGULARR_SEARCH_URL").and_then(|u| {
                if u.starts_with("http://") || u.starts_with("https://") {
                    Some(base_url(&u))
                } else {
                    warnings.push(format!(
                        "SINGULARR_SEARCH_URL={u:?} is not an http:// or https:// address; built-in search in use"
                    ));
                    None
                }
            }),
            key: set(env, "SINGULARR_SEARCH_KEY"),
            prefix: match set(env, "SINGULARR_SEARCH_PREFIX") {
                None => "singularr_".to_string(),
                Some(p)
                    if p.len() <= 64
                        && p
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') =>
                {
                    p
                }
                Some(p) => {
                    warnings.push(format!(
                        "SINGULARR_SEARCH_PREFIX={p:?} may hold only letters, digits, - and _; using singularr_"
                    ));
                    "singularr_".to_string()
                }
            },
            interval: seconds(env, "SINGULARR_SEARCH_INDEX_SECS", 900, 60, &mut warnings),
        };
        let geoip = GeoIpConfig {
            path: set(env, "SINGULARR_GEOIP_PATH"),
            download: set(env, "SINGULARR_GEOIP_DOWNLOAD").map(|v| {
                match v.to_ascii_lowercase().as_str() {
                    "on" | "true" | "yes" | "1" => true,
                    "off" | "false" | "no" | "0" => false,
                    other => {
                        warnings.push(format!(
                            "SINGULARR_GEOIP_DOWNLOAD={other:?} is neither on nor off; the location database is not downloaded"
                        ));
                        false
                    }
                }
            }),
            data_dir: set(env, "SINGULARR_DATA_DIR").unwrap_or_else(|| "/data".to_string()),
        };
        let portal_role = PortalRoleConfig {
            role: match set(env, "SINGULARR_REQUESTS_DB_ROLE") {
                Some(name) if !plain_role_name(&name) => {
                    return Err(ConfigError::Invalid(
                        "SINGULARR_REQUESTS_DB_ROLE",
                        format!(
                            "`{name}` is not a role name of lowercase letters, digits and _ (at most 63, the first no digit)"
                        ),
                    ));
                }
                name => name,
            },
            password: set(env, "SINGULARR_REQUESTS_DB_PASSWORD"),
        };
        let sync = SyncConfig {
            enabled: set(env, "SINGULARR_SYNC_ENABLED")
                .map(|v| {
                    !matches!(
                        v.to_ascii_lowercase().as_str(),
                        "false" | "0" | "no" | "off"
                    )
                })
                .unwrap_or(true),
            titles: seconds(env, "SINGULARR_SYNC_TITLES_SECS", 900, 60, &mut warnings),
            files_full: seconds(
                env,
                "SINGULARR_SYNC_FILES_FULL_SECS",
                21_600,
                600,
                &mut warnings,
            ),
            files_concurrency: set(env, "SINGULARR_SYNC_FILES_CONCURRENCY")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(2)
                .clamp(1, 4),
            queue: seconds(env, "SINGULARR_SYNC_QUEUE_SECS", 60, 15, &mut warnings),
            // `SINGULARR_SYNC_QBIT_SECS` is the older name and keeps working.
            downloads: seconds(
                env,
                if set(env, "SINGULARR_SYNC_DOWNLOADS_SECS").is_some() {
                    "SINGULARR_SYNC_DOWNLOADS_SECS"
                } else {
                    "SINGULARR_SYNC_QBIT_SECS"
                },
                30,
                10,
                &mut warnings,
            ),
            history: seconds(env, "SINGULARR_SYNC_HISTORY_SECS", 900, 60, &mut warnings),
            history_max_pages: set(env, "SINGULARR_SYNC_HISTORY_MAX_PAGES")
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(40)
                .clamp(1, 400),
            jitter: seconds(env, "SINGULARR_SYNC_JITTER_SECS", 20, 0, &mut warnings),
            requests: seconds(env, "SINGULARR_SYNC_REQUESTS_SECS", 300, 60, &mut warnings),
            server: seconds(env, "SINGULARR_SYNC_SERVER_SECS", 3600, 300, &mut warnings),
            processing: seconds(env, "SINGULARR_SYNC_PROCESSING_SECS", 60, 15, &mut warnings),
        };

        Ok(Config {
            bootstrap,
            policy_path: set(env, "SINGULARR_POLICY")
                .unwrap_or_else(|| "/app/rules/policy.toml".to_string()),
            policy_pinned: set(env, "SINGULARR_POLICY").is_some(),
            editions_path: set(env, "SINGULARR_EDITIONS")
                .unwrap_or_else(|| "/app/rules/editions.toml".to_string()),
            workspace,
            integrations,
            arrs,
            media_servers,
            downloads,
            seerr,
            processors,
            tautulli,
            request_rules,
            music,
            home_movies,
            movie_folders,
            tmdb_token: set(env, "SINGULARR_TMDB_TOKEN"),
            tvdb_key: set(env, "SINGULARR_TVDB_KEY"),
            watch_region: set(env, "SINGULARR_WATCH_REGION")
                .map(|r| r.to_ascii_uppercase())
                .filter(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_uppercase()))
                .unwrap_or_else(|| "US".to_string()),
            plex_web_url: set(env, "SINGULARR_PLEX_WEB_URL")
                .filter(|u| u.starts_with("https://") || u.starts_with("http://"))
                .map(|u| base_url(&u))
                .unwrap_or_else(|| "https://app.plex.tv/desktop".to_string()),
            search,
            geoip,
            portal_role,
            sync,
            warnings,
        })
    }

    pub fn arr(&self, id: &str) -> Option<&ArrConfig> {
        self.arrs.iter().find(|a| a.id.eq_ignore_ascii_case(id))
    }

    pub fn integration(&self, id: &str) -> Option<&Integration> {
        self.integrations.iter().find(|i| i.id == id)
    }

    /// The first configured media server, by its id: the first the engines read. Its albums and
    /// artists have the short ids (`album-<key>`) a server's had before more than one was read.
    pub fn first_media_server(&self) -> Option<&str> {
        self.media_servers
            .iter()
            .find(|m| m.configured())
            .map(|m| m.id.as_str())
    }

    /// The ids of every listed integration, configured or not.
    pub fn integration_ids(&self) -> Vec<String> {
        self.integrations.iter().map(|i| i.id.clone()).collect()
    }
}

/// A role name that needs no quotes in SQL as far as its characters go: lowercase letters,
/// digits and `_`, at most 63, the first no digit (`SINGULARR_REQUESTS_DB_ROLE`).
pub fn plain_role_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn secret(v: &Option<String>) -> String {
    match v {
        Some(s) => format!("set ({} characters)", s.chars().count()),
        None => "unset".to_string(),
    }
}

/// The startup printout. No secret appears in it: keys, passwords and tokens are shown as
/// set or unset, and the database password is replaced.
impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Singularr configuration")?;
        writeln!(
            f,
            "  DATABASE_URL                   {}",
            redact_url(&self.bootstrap.database_url)
        )?;
        writeln!(
            f,
            "  SINGULARR_DB_MAX_CONNECTIONS   {}",
            self.bootstrap.db_max_connections
        )?;
        writeln!(
            f,
            "  SINGULARR_REQUESTS_DB_ROLE     {}",
            self.portal_role
                .role
                .as_deref()
                .unwrap_or("unset (the role of deploy/requests-role.sql)")
        )?;
        writeln!(
            f,
            "  SINGULARR_REQUESTS_DB_PASSWORD {}",
            secret(&self.portal_role.password)
        )?;
        writeln!(f, "  SINGULARR_POLICY               {}", self.policy_path)?;
        writeln!(f, "  SINGULARR_EDITIONS             {}", self.editions_path)?;
        writeln!(
            f,
            "  SINGULARR_INSTANCE             {}",
            self.bootstrap.instance_path.as_deref().unwrap_or("unset")
        )?;
        let w = &self.workspace;
        writeln!(
            f,
            "  workspace                      {} at {}",
            w.name,
            w.url.as_deref().unwrap_or("no public address")
        )?;
        writeln!(
            f,
            "  portal                         {} at {}",
            w.portal,
            w.portal_url.as_deref().unwrap_or("no public address")
        )?;
        writeln!(
            f,
            "  SINGULARR_SIGN_OUT_URL         {}",
            w.sign_out_url.as_deref().unwrap_or("unset")
        )?;
        if self.integrations.is_empty() {
            writeln!(f, "  integrations                   none")?;
        }
        for i in &self.integrations {
            let key = format!("{} {}", i.secret_var, secret(&i.secret));
            // What the configuration says Singularr may do with it, when it says.
            let access = match &i.access {
                Some(grant) => format!(", access {}", grant.as_text()),
                None => String::new(),
            };
            let web = match &i.web_url {
                Some(web) => format!(", web app at {web}"),
                None => String::new(),
            };
            writeln!(
                f,
                "  integration {:<16} {} \"{}\" at {} ({}{}, {key}{access}{web}; from the {})",
                i.id,
                i.kind.as_str(),
                i.name,
                i.url.as_deref().unwrap_or("unset"),
                if i.user.is_some() { "user set, " } else { "" },
                i.role,
                i.source
            )?;
        }
        let r = &self.request_rules;
        writeln!(
            f,
            "  SINGULARR_REQUEST_OWNERS       {}",
            r.owners
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        )?;
        writeln!(f, "  SINGULARR_REQUEST_MAX_SEASONS  {}", r.max_seasons)?;
        writeln!(
            f,
            "  SINGULARR_REQUEST_SEASON_GATE  {}",
            if r.season_gate { "on" } else { "off" }
        )?;
        writeln!(
            f,
            "  SINGULARR_MUSIC_ROOT           {} (scanned every {} s)",
            self.music.root,
            self.music.interval.as_secs()
        )?;
        writeln!(
            f,
            "  SINGULARR_HOME_MOVIES_ROOT     {} (scanned every {} s)",
            self.home_movies.root,
            self.home_movies.interval.as_secs()
        )?;
        writeln!(
            f,
            "  SINGULARR_MOVIE_ROOTS          {} (scanned every {} s)",
            if self.movie_folders.roots.is_empty() {
                "none, the scan is off".to_string()
            } else {
                self.movie_folders.roots.join(",")
            },
            self.movie_folders.interval.as_secs()
        )?;
        writeln!(
            f,
            "  SINGULARR_TMDB_TOKEN           {}",
            secret(&self.tmdb_token)
        )?;
        writeln!(
            f,
            "  SINGULARR_TVDB_KEY             {}",
            secret(&self.tvdb_key)
        )?;
        writeln!(f, "  SINGULARR_WATCH_REGION         {}", self.watch_region)?;
        writeln!(f, "  SINGULARR_PLEX_WEB_URL         {}", self.plex_web_url)?;
        writeln!(
            f,
            "  SINGULARR_SEARCH_URL           {}",
            self.search
                .url
                .as_deref()
                .map(redact_url)
                .unwrap_or_else(|| "unset (built-in search)".to_string())
        )?;
        writeln!(
            f,
            "  SINGULARR_SEARCH_KEY           {}",
            secret(&self.search.key)
        )?;
        writeln!(f, "  SINGULARR_SEARCH_PREFIX        {}", self.search.prefix)?;
        writeln!(
            f,
            "  SINGULARR_SEARCH_INDEX_SECS    {}",
            self.search.interval.as_secs()
        )?;
        writeln!(
            f,
            "  SINGULARR_GEOIP_PATH           {}",
            self.geoip.path.as_deref().unwrap_or("unset")
        )?;
        writeln!(
            f,
            "  SINGULARR_GEOIP_DOWNLOAD       {}",
            match self.geoip.download {
                Some(true) => "on",
                Some(false) => "off",
                None => "unset (Settings, on by default)",
            }
        )?;
        writeln!(
            f,
            "  SINGULARR_DATA_DIR             {}",
            self.geoip.data_dir
        )?;
        writeln!(
            f,
            "  SINGULARR_OWNER_EMAILS         {} address(es)",
            self.bootstrap
                .owner_emails
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .count()
        )?;
        writeln!(
            f,
            "  SINGULARR_EDGE_TOKEN           {}",
            match (
                self.bootstrap.edge_token_set,
                self.bootstrap.edge_token_placeholder
            ) {
                (true, _) => "set",
                (false, true) => "still the placeholder",
                (false, false) => "unset",
            }
        )?;
        writeln!(
            f,
            "  SINGULARR_TRUSTED_PROXY_IPS    {}",
            self.bootstrap.trusted_proxies.as_deref().unwrap_or("unset")
        )?;
        let s = &self.sync;
        writeln!(f, "  SINGULARR_SYNC_ENABLED         {}", s.enabled)?;
        writeln!(f, "  SINGULARR_SYNC_TITLES_SECS     {}", s.titles.as_secs())?;
        writeln!(
            f,
            "  SINGULARR_SYNC_FILES_FULL_SECS {}",
            s.files_full.as_secs()
        )?;
        writeln!(
            f,
            "  SINGULARR_SYNC_FILES_CONCURRENCY {}",
            s.files_concurrency
        )?;
        writeln!(f, "  SINGULARR_SYNC_QUEUE_SECS      {}", s.queue.as_secs())?;
        writeln!(
            f,
            "  SINGULARR_SYNC_DOWNLOADS_SECS  {}",
            s.downloads.as_secs()
        )?;
        writeln!(
            f,
            "  SINGULARR_SYNC_HISTORY_SECS    {}",
            s.history.as_secs()
        )?;
        writeln!(
            f,
            "  SINGULARR_SYNC_HISTORY_MAX_PAGES {}",
            s.history_max_pages
        )?;
        writeln!(f, "  SINGULARR_SYNC_JITTER_SECS     {}", s.jitter.as_secs())?;
        writeln!(
            f,
            "  SINGULARR_SYNC_REQUESTS_SECS   {}",
            s.requests.as_secs()
        )?;
        writeln!(f, "  SINGULARR_SYNC_SERVER_SECS     {}", s.server.as_secs())?;
        writeln!(
            f,
            "  SINGULARR_SYNC_PROCESSING_SECS {}",
            s.processing.as_secs()
        )?;
        write!(
            f,
            "  SINGULARR_HTTP_TIMEOUT_SECS    {}",
            self.bootstrap.http_timeout.as_secs()
        )?;
        for w in &self.warnings {
            write!(f, "\n  warning: {w}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    /// `core` knows no engine: the parsing reads every fact of a type from the table it is
    /// handed. The code above the tests, its comments left out, names no engine and calls no
    /// manifest, as a method or by its path.
    #[test]
    fn the_parsing_reads_the_manifests_from_the_table_it_is_handed() {
        let code: Vec<&str> = include_str!("config.rs")
            .split("#[cfg(test)]")
            .next()
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        let code = code.join("\n");
        let mut refused = vec!["engines::".to_string(), "manifest(".to_string()];
        for method in [
            "label",
            "default_role",
            "env_prefixes",
            "secret_suffix",
            "secret_optional",
            "takes_user",
            "processes",
            "has_web_app",
        ] {
            refused.push(format!(".{method}("));
            refused.push(format!("::{method}("));
        }
        for word in &refused {
            assert!(!code.contains(word.as_str()), "core::config calls `{word}`");
        }
    }

    fn env() -> BTreeMap<&'static str, &'static str> {
        BTreeMap::from([
            (
                "DATABASE_URL",
                "postgres://singularr:hunter2secret@10.0.0.83:5432/singularr",
            ),
            ("RADARR_URL", "http://10.0.0.61:7979/"),
            ("RADARR_API_KEY", "0123456789abcdef0123456789abcdef"),
            ("SONARR_URL", "http://10.0.0.61:8989"),
            ("SONARR_API_KEY", "CHANGEME"),
            ("QBIT_URL", "http://10.0.0.62:8080"),
            ("QBIT_USER", "admin"),
            ("QBIT_PASS", "torrent-password"),
            ("SINGULARR_TMDB_TOKEN", "tmdb-secret-token"),
            ("SINGULARR_OWNER_EMAILS", "owner@example.com"),
            ("SINGULARR_SYNC_QUEUE_SECS", "5"),
            ("PLEXSEERR_URL", "http://10.0.0.62:5055/"),
            ("PLEXSEERR_API_KEY", "seerr-secret-key"),
            ("SINGULARR_REQUESTS_DB_PASSWORD", "portal-db-password"),
        ])
    }

    #[test]
    fn the_environment_form_lists_one_integration_per_type() {
        let c = Config::from(&env()).unwrap();
        let ids: Vec<&str> = c.integrations.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["radarr", "sonarr", "qbittorrent", "seerr"]);
        let radarr = c.integration("radarr").unwrap();
        assert_eq!(
            (radarr.kind, radarr.name.as_str(), radarr.role.as_str()),
            (IntegrationType::Radarr, "Radarr", "movies")
        );
        assert!(radarr.configured());
        let sonarr = c.integration("sonarr").unwrap();
        assert!(!sonarr.configured(), "CHANGEME is not a key");
        assert_eq!(
            sonarr.missing().as_deref(),
            Some("SONARR_API_KEY is not set")
        );
        // PLEXSEERR_* is the older name of SEERR_* and keeps working.
        let seerr = c.integration("seerr").unwrap();
        assert_eq!(seerr.secret_var, "PLEXSEERR_API_KEY");
        assert_eq!(c.seerr.len(), 1);
        assert_eq!(c.seerr[0].url, "http://10.0.0.62:5055");
        assert!(c.seerr[0].configured());
        let qbit = c.integration("qbittorrent").unwrap();
        assert_eq!(qbit.user.as_deref(), Some("admin"));
        assert_eq!(c.downloads.len(), 1);
        assert_eq!(
            (c.downloads[0].id.as_str(), c.downloads[0].kind),
            ("qbittorrent", ClientKind::Qbittorrent)
        );
        // No Plex, no Prowlarr, no Tdarr: nothing set, nothing listed.
        assert!(c.integration("plex").is_none());
        let mut e = env();
        e.insert("SEERR_URL", "http://10.0.0.9:5055");
        e.insert("SEERR_API_KEY", "other-key");
        e.insert("RADARR_NAME", "Radarr HD");
        let c = Config::from(&e).unwrap();
        assert_eq!(c.integration("seerr").unwrap().secret_var, "SEERR_API_KEY");
        assert_eq!(c.seerr[0].url, "http://10.0.0.9:5055");
        assert_eq!(c.integration("radarr").unwrap().name, "Radarr HD");
    }

    #[test]
    fn an_instance_file_lists_more_integrations_and_names_their_secrets() {
        let file = r#"
            [workspace]
            name = "Home"
            portal = "Home Requests"
            url = "https://media.example.org/"

            [[integration]]
            id = "radarr-4k"
            type = "radarr"
            name = "Radarr 4K"
            url = "http://10.0.0.5:7879"

            [[integration]]
            id = "downloads"
            type = "qbittorrent"
            url = "http://10.0.0.6:8080"
            user = "admin"
            secret = "DOWNLOADS_PASSWORD"
        "#;
        let mut e = env();
        e.insert("RADARR_4K_API_KEY", "4k-key");
        e.insert("DOWNLOADS_PASSWORD", "pw");
        let c = Config::with_instance(&e, Some(("/x/instance.toml", file))).unwrap();
        let ids: Vec<&str> = c.integrations.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "radarr-4k",
                "downloads",
                "radarr",
                "sonarr",
                "qbittorrent",
                "seerr"
            ]
        );
        let uhd = c.arr("radarr-4k").unwrap();
        assert!(uhd.configured());
        assert_eq!(uhd.library, "radarr-4k");
        assert_eq!(
            c.integration("radarr-4k").unwrap().secret_var,
            "RADARR_4K_API_KEY"
        );
        // Every download client is read, the file's first.
        let clients: Vec<&str> = c.downloads.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(clients, vec!["downloads", "qbittorrent"]);
        assert_eq!(c.downloads[0].secret.as_deref(), Some("pw"));
        assert_eq!(c.downloads[0].user.as_deref(), Some("admin"));
        assert_eq!(c.workspace.name, "Home");
        assert_eq!(c.workspace.portal, "Home Requests");
        assert_eq!(
            c.workspace.url.as_deref(),
            Some("https://media.example.org")
        );
        let text = c.to_string();
        for secret in ["4k-key", "torrent-password"] {
            assert!(!text.contains(secret), "the printout leaks {secret}");
        }
        // A secret written into the file is refused, and so are unknown keys and types.
        for bad in [
            "[[integration]]\nid = \"r\"\ntype = \"radarr\"\nsecret = \"0123abcd\"\n",
            "[[integration]]\nid = \"r\"\ntype = \"kodi\"\n",
            "[[integration]]\nid = \"R\"\ntype = \"radarr\"\n",
            "[[integration]]\nid = \"r\"\ntype = \"radarr\"\napi_key = \"x\"\n",
            "[[integration]]\nid = \"r\"\ntype = \"radarr\"\n[[integration]]\nid = \"r\"\ntype = \"sonarr\"\n",
            "[[integration]]\nid = \"r\"\ntype = \"radarr\"\nurl = \"10.0.0.5:7878\"\n",
        ] {
            assert!(
                matches!(
                    Config::with_instance(&env(), Some(("/x/i.toml", bad))),
                    Err(ConfigError::Instance { .. })
                ),
                "{bad}"
            );
        }
        // An environment variable for an id the file defines is ignored, with a warning.
        let file =
            "[[integration]]\nid = \"radarr\"\ntype = \"radarr\"\nurl = \"http://10.0.0.7:7878\"\n";
        let c = Config::with_instance(&env(), Some(("/x/i.toml", file))).unwrap();
        assert_eq!(
            c.arr("radarr").unwrap().url.as_deref(),
            Some("http://10.0.0.7:7878")
        );
        assert!(
            c.warnings
                .iter()
                .any(|w| w.starts_with("RADARR_URL is ignored"))
        );
    }

    #[test]
    fn processing_integrations_come_from_their_addresses() {
        let mut e = env();
        e.insert("TDARR_URL", "http://10.0.0.60:8265/");
        e.insert("HUSH_URL", "http://10.0.0.60:8787");
        let c = Config::from(&e).unwrap();
        let ids: Vec<&str> = c.integrations.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["radarr", "sonarr", "qbittorrent", "tdarr", "hush", "seerr"]
        );
        let hush = c.integration("hush").unwrap();
        assert_eq!(
            (hush.kind, hush.name.as_str(), hush.role.as_str()),
            (IntegrationType::Hush, "HUSH", "processing")
        );
        assert!(hush.configured() && hush.missing().is_none());
        assert!(hush.kind.secret_optional() && hush.kind.processes());
        // Tdarr and the bridge run without a key; the address alone configures them.
        let tdarr = c.integration("tdarr").unwrap();
        assert!(tdarr.configured() && tdarr.kind.secret_optional() && tdarr.kind.processes());
        assert_eq!(c.processors.len(), 2);
        assert_eq!(
            (c.processors[0].kind, c.processors[0].url.as_deref()),
            (ProcessorKind::Tdarr, Some("http://10.0.0.60:8265"))
        );
        assert_eq!(
            (c.processors[1].kind, c.processors[1].api_key.as_deref()),
            (ProcessorKind::Hush, None)
        );
        assert_eq!(c.sync.processing, Duration::from_secs(60));
        let text = c.to_string();
        assert!(
            text.contains("HUSH_API_KEY unset; from the environment"),
            "{text}"
        );
        // A bridge that wants a key for the calls that change something is given one. The key
        // alone lists the integration, as every secret does, and is never printed.
        e.insert("HUSH_API_KEY", "bridge-key-value");
        let c = Config::from(&e).unwrap();
        assert_eq!(c.processors[1].api_key.as_deref(), Some("bridge-key-value"));
        let text = c.to_string();
        assert!(text.contains("HUSH_API_KEY set (16 characters)"), "{text}");
        assert!(!text.contains("bridge-key-value"));
        // Listed without an address, a processing integration is not asked.
        let mut e = env();
        e.insert("TDARR_API_KEY", "tdarr-key");
        e.insert("SINGULARR_SYNC_PROCESSING_SECS", "5");
        let c = Config::from(&e).unwrap();
        assert!(!c.processors[0].configured());
        assert_eq!(
            c.integration("tdarr").unwrap().missing().as_deref(),
            Some("TDARR_URL is not set")
        );
        assert_eq!(c.sync.processing, Duration::from_secs(15));
        assert!(!IntegrationType::Radarr.processes());
    }

    #[test]
    fn media_servers_of_every_type_come_from_their_addresses_and_keys() {
        let mut e = env();
        e.insert("PLEX_URL", "http://10.0.0.30:32400");
        e.insert("PLEX_TOKEN", "plex-token");
        e.insert("JELLYFIN_URL", "http://10.0.0.31:8096/");
        e.insert("JELLYFIN_API_KEY", "0123456789abcdef0123456789abcdef");
        e.insert("JELLYFIN_WEB_URL", "https://watch.example.org/");
        e.insert("EMBY_URL", "http://10.0.0.32:8096");
        e.insert("EMBY_WEB_URL", "watch.example.org");
        e.insert("EMBY_ACCESS", "read");
        e.insert("RADARR_WEB_URL", "https://radarr.example.org");
        let c = Config::from(&e).unwrap();
        let ids: Vec<&str> = c.integrations.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "plex",
                "jellyfin",
                "emby",
                "radarr",
                "sonarr",
                "qbittorrent",
                "seerr"
            ]
        );
        let jellyfin = c.integration("jellyfin").unwrap();
        assert_eq!(
            (
                jellyfin.kind,
                jellyfin.name.as_str(),
                jellyfin.role.as_str(),
                jellyfin.secret_var.as_str()
            ),
            (
                IntegrationType::Jellyfin,
                "Jellyfin",
                "media server",
                "JELLYFIN_API_KEY"
            )
        );
        assert!(jellyfin.configured());
        // Emby without its key is listed and not asked; its access variable is read like any.
        let emby = c.integration("emby").unwrap();
        assert_eq!(emby.missing().as_deref(), Some("EMBY_API_KEY is not set"));
        assert_eq!(
            (emby.access.clone(), emby.access_var.as_str()),
            (Some(Grant::Read), "EMBY_ACCESS")
        );
        let servers: Vec<(&str, MediaServerKind, bool)> = c
            .media_servers
            .iter()
            .map(|m| (m.id.as_str(), m.kind, m.configured()))
            .collect();
        assert_eq!(
            servers,
            vec![
                ("plex", MediaServerKind::Plex, true),
                ("jellyfin", MediaServerKind::Jellyfin, true),
                ("emby", MediaServerKind::Emby, false)
            ]
        );
        assert_eq!(
            c.media_servers[1].url.as_deref(),
            Some("http://10.0.0.31:8096")
        );
        // Jellyfin's items open at its web address. Emby's is no http(s) address: its items open
        // at its address, with a warning. A manager has no web app to name.
        assert_eq!(
            c.media_servers[1].web_url.as_deref(),
            Some("https://watch.example.org")
        );
        assert_eq!(c.media_servers[2].web_url, None);
        assert!(
            c.warnings.iter().any(|w| w.starts_with("EMBY_WEB_URL=")),
            "{:?}",
            c.warnings
        );
        assert_eq!(c.integration("radarr").unwrap().web_url, None);
        assert_eq!(c.media_servers[0].web_url, None);
        let text = c.to_string();
        assert!(
            text.contains("JELLYFIN_API_KEY set (32 characters)"),
            "{text}"
        );
        assert!(
            text.contains("web app at https://watch.example.org"),
            "{text}"
        );
        assert!(!text.contains("0123456789abcdef0123456789abcdef"));
        assert!(NOT_FOR_THE_FACE.contains(&"JELLYFIN_API_KEY"));
        assert!(NOT_FOR_THE_FACE.contains(&"EMBY_API_KEY"));
        assert!(IntegrationType::Emby.serves_media() && !IntegrationType::Tautulli.serves_media());

        // The instance file names the types `jellyfin` and `emby`; the first of each type is
        // read. A second one is named in a warning.
        let file = r#"
            [[integration]]
            id = "living-room"
            type = "jellyfin"
            url = "http://10.0.0.41:8096/jellyfin"

            [[integration]]
            id = "den"
            type = "jellyfin"
            url = "http://10.0.0.42:8096"

            [[integration]]
            id = "cabin"
            type = "emby"
            name = "Cabin"
            url = "http://10.0.0.43:8096"
            web_url = "https://cabin.example.org/"
            secret = "CABIN_KEY"
        "#;
        let mut e = env();
        e.insert("LIVING_ROOM_API_KEY", "k1");
        e.insert("CABIN_KEY", "k2");
        let c = Config::with_instance(&e, Some(("/x/instance.toml", file))).unwrap();
        assert_eq!(
            (
                c.media_servers[0].web_url.as_deref(),
                c.media_servers[1].web_url.as_deref()
            ),
            (None, Some("https://cabin.example.org"))
        );
        // A web address is read for Jellyfin and Emby alone: an http(s) one.
        for (kind, web) in [
            ("radarr", "https://radarr.example.org"),
            ("jellyfin", "ftp://watch.example.org"),
        ] {
            let bad = format!(
                "[[integration]]\nid = \"x\"\ntype = \"{kind}\"\nurl = \"http://10.0.0.44:7878\"\nweb_url = \"{web}\"\n"
            );
            assert!(
                matches!(
                    Config::with_instance(&e, Some(("/x/instance.toml", bad.as_str()))),
                    Err(ConfigError::Instance { .. })
                ),
                "{kind} {web}"
            );
        }
        let servers: Vec<(&str, MediaServerKind, bool)> = c
            .media_servers
            .iter()
            .map(|m| (m.id.as_str(), m.kind, m.configured()))
            .collect();
        assert_eq!(
            servers,
            vec![
                ("living-room", MediaServerKind::Jellyfin, true),
                ("cabin", MediaServerKind::Emby, true)
            ]
        );
        assert_eq!(c.integration("cabin").unwrap().name, "Cabin");
        assert!(
            c.warnings
                .iter()
                .any(|w| w == "More than one Jellyfin integration; the first is used"),
            "{:?}",
            c.warnings
        );
    }

    #[test]
    fn tautulli_comes_from_its_address_and_its_key() {
        let mut e = env();
        e.insert("TAUTULLI_URL", "http://10.0.0.50:8181/");
        let c = Config::from(&e).unwrap();
        let t = c.tautulli.clone().unwrap();
        assert_eq!(
            (t.id.as_str(), t.url.as_deref(), t.configured()),
            ("tautulli", Some("http://10.0.0.50:8181"), false)
        );
        // Without its key it is listed and not asked.
        assert_eq!(
            c.integration("tautulli").unwrap().missing().as_deref(),
            Some("TAUTULLI_API_KEY is not set")
        );
        e.insert("TAUTULLI_API_KEY", "0123456789abcdef0123456789abcdef");
        let c = Config::from(&e).unwrap();
        assert!(c.tautulli.as_ref().unwrap().configured());
        let i = c.integration("tautulli").unwrap();
        assert_eq!(
            (i.kind, i.name.as_str(), i.role.as_str()),
            (IntegrationType::Tautulli, "Tautulli", "plays")
        );
        // The printout says the key is set, never what it is.
        let text = c.to_string();
        assert!(!text.contains("0123456789abcdef"), "{text}");
        assert!(
            text.contains("TAUTULLI_API_KEY set (32 characters)"),
            "{text}"
        );
        // The portal refuses to start with it.
        assert!(NOT_FOR_THE_FACE.contains(&"TAUTULLI_API_KEY"));
    }

    #[test]
    fn every_download_client_comes_from_its_variables_torrents_and_usenet_alike() {
        let mut e = env();
        e.insert("TRANSMISSION_URL", "http://10.0.0.62:9091/");
        e.insert("TRANSMISSION_USER", "fx");
        e.insert("DELUGE_URL", "http://10.0.0.62:8112");
        e.insert("SABNZBD_URL", "http://10.0.0.63:8080");
        e.insert("SABNZBD_API_KEY", "sab-secret-key");
        e.insert("NZBGET_URL", "http://10.0.0.63:6789");
        e.insert("NZBGET_USER", "nzbget");
        e.insert("NZBGET_PASS", "nzbget-secret");
        e.insert("SINGULARR_SYNC_DOWNLOADS_SECS", "45");
        let c = Config::from(&e).unwrap();
        let clients: Vec<(&str, ClientKind, bool)> = c
            .downloads
            .iter()
            .map(|d| (d.id.as_str(), d.kind, d.configured()))
            .collect();
        assert_eq!(
            clients,
            vec![
                ("qbittorrent", ClientKind::Qbittorrent, true),
                ("transmission", ClientKind::Transmission, true),
                ("deluge", ClientKind::Deluge, false),
                ("sabnzbd", ClientKind::Sabnzbd, true),
                ("nzbget", ClientKind::Nzbget, true),
            ]
        );
        // Deluge signs in with a password alone. It is not asked without one.
        let deluge = c.integration("deluge").unwrap();
        assert_eq!(
            (deluge.missing().as_deref(), deluge.name.as_str()),
            (Some("DELUGE_PASS is not set"), "Deluge")
        );
        assert_eq!(c.downloads[2].user, None);
        // Transmission and NZBGet take a user name; SABnzbd its key.
        assert_eq!(c.downloads[1].user.as_deref(), Some("fx"));
        assert_eq!(c.downloads[1].secret, None);
        assert_eq!(
            c.integration("sabnzbd").unwrap().secret_var,
            "SABNZBD_API_KEY"
        );
        assert_eq!(c.integration("nzbget").unwrap().secret_var, "NZBGET_PASS");
        assert!(ClientKind::Sabnzbd.usenet() && !ClientKind::Deluge.usenet());
        for kind in ClientKind::ALL {
            assert_eq!(kind.integration().client(), Some(kind));
            assert_eq!(kind.integration().default_role(), "downloads");
        }
        assert_eq!(IntegrationType::Radarr.client(), None);
        assert_eq!(c.sync.downloads, Duration::from_secs(45));
        let text = c.to_string();
        for secret in ["sab-secret-key", "nzbget-secret"] {
            assert!(!text.contains(secret), "the printout leaks {secret}");
        }
        assert!(text.contains("SINGULARR_SYNC_DOWNLOADS_SECS  45"), "{text}");
        // The older name of the interval keeps working; the product's name wins.
        let mut older = env();
        older.insert("SINGULARR_SYNC_QBIT_SECS", "40");
        assert_eq!(
            Config::from(&older).unwrap().sync.downloads,
            Duration::from_secs(40)
        );
        older.insert("SINGULARR_SYNC_DOWNLOADS_SECS", "50");
        assert_eq!(
            Config::from(&older).unwrap().sync.downloads,
            Duration::from_secs(50)
        );
        // The instance file names the new types. A user only counts where the type takes one.
        let file = "[[integration]]\nid = \"usenet\"\ntype = \"sabnzbd\"\nurl = \"http://10.0.0.63:8080\"\nuser = \"ignored\"\n";
        let mut e = env();
        e.insert("USENET_API_KEY", "k");
        let c = Config::with_instance(&e, Some(("/x/i.toml", file))).unwrap();
        assert_eq!(
            (
                c.downloads[0].id.as_str(),
                c.downloads[0].kind,
                c.downloads[0].user.as_deref()
            ),
            ("usenet", ClientKind::Sabnzbd, None)
        );
        assert!(c.downloads[0].configured());
        // Two integrations at one address are one client: the first is read. The configuration
        // warns.
        let file = "[[integration]]\nid = \"seedbox\"\ntype = \"qbittorrent\"\nname = \"Seedbox\"\nurl = \"http://seedbox.lan:8080\"\n\n[[integration]]\nid = \"seedbox-two\"\ntype = \"qbittorrent\"\nname = \"Seedbox two\"\nurl = \"http://Seedbox.LAN:8080/\"\n";
        let c = Config::with_instance(&env(), Some(("/x/i.toml", file))).unwrap();
        let ids: Vec<&str> = c.downloads.iter().map(|d| d.id.as_str()).collect();
        assert!(
            ids.contains(&"seedbox") && !ids.contains(&"seedbox-two"),
            "{ids:?}"
        );
        assert!(
            c.warnings
                .iter()
                .any(|w| w == "Seedbox two has the address of Seedbox: it is not read"),
            "{:?}",
            c.warnings
        );
        assert!(c.integration("seedbox-two").is_some());
        // Without any, the configuration warns.
        let mut none = env();
        for var in ["QBIT_URL", "QBIT_USER", "QBIT_PASS"] {
            none.remove(var);
        }
        let c = Config::from(&none).unwrap();
        assert!(c.downloads.is_empty());
        assert!(c.warnings.iter().any(|w| w == "No download client"));
        // The portal refuses to start with any of their secrets: each is named like one
        // (`the_portal_refuses_every_variable_named_like_a_secret`). Their user names are listed.
        for var in [
            "TRANSMISSION_PASS",
            "DELUGE_PASS",
            "SABNZBD_API_KEY",
            "NZBGET_PASS",
        ] {
            assert!(named_like_a_secret(var), "{var}");
        }
        for kind in ClientKind::ALL.map(ClientKind::integration) {
            if kind.takes_user() {
                let user = format!("{}_USER", kind.env_prefixes()[0]);
                assert!(NOT_FOR_THE_FACE.contains(&user.as_str()), "{user}");
            }
        }
    }

    #[test]
    fn what_singularr_may_do_with_an_integration_can_be_fixed_in_the_configuration() {
        // Left out, the configuration says nothing: the access is the saved one, else `read`.
        let mut e = env();
        e.insert("HUSH_URL", "http://10.0.0.60:8787");
        let c = Config::from(&e).unwrap();
        let hush = c.integration("hush").unwrap();
        assert_eq!(
            (hush.access.clone(), hush.access_var.as_str()),
            (None, "HUSH_ACCESS")
        );
        assert_eq!(c.integration("radarr").unwrap().access, None);
        // The environment form: one variable per type, in any case.
        e.insert("HUSH_ACCESS", " Act ");
        e.insert("RADARR_ACCESS", "read");
        let c = Config::from(&e).unwrap();
        assert_eq!(c.integration("hush").unwrap().access, Some(Grant::Act));
        assert_eq!(c.integration("radarr").unwrap().access, Some(Grant::Read));
        assert_eq!(c.integration("sonarr").unwrap().access, None);
        // A list of the type's actions grants those alone. An id it does not declare is left
        // out and named.
        let mut listed = e.clone();
        listed.insert("RADARR_ACCESS", "grab, search, scan");
        let c2 = Config::from(&listed).unwrap();
        assert_eq!(
            c2.integration("radarr").unwrap().access,
            Some(Grant::Actions(vec!["grab".into(), "search".into()]))
        );
        assert!(
            c2.warnings.iter().any(|w| w
                .starts_with("RADARR_ACCESS=\"grab, search, scan\" names actions")
                && w.contains("(scan)")),
            "{:?}",
            c2.warnings
        );
        let text = c.to_string();
        assert!(text.contains("HUSH_API_KEY unset, access act;"), "{text}");
        // A word that is neither gives no more than reading, and says so.
        e.insert("HUSH_ACCESS", "yes");
        let c = Config::from(&e).unwrap();
        assert_eq!(c.integration("hush").unwrap().access, Some(Grant::Read));
        assert!(
            c.warnings
                .iter()
                .any(|w| w.starts_with("HUSH_ACCESS=\"yes\" is neither read nor act")),
            "{:?}",
            c.warnings
        );
        // The variable alone lists no integration.
        let mut alone = env();
        alone.insert("HUSH_ACCESS", "act");
        assert!(Config::from(&alone).unwrap().integration("hush").is_none());

        // The instance file: `access` on the integration. The variable of the environment
        // form says nothing about an integration of the file.
        let file = r#"
            [[integration]]
            id = "bridge"
            type = "hush"
            url = "http://10.0.0.60:8787"
            access = "act"

            [[integration]]
            id = "worker"
            type = "tdarr"
            url = "http://10.0.0.60:8265"
        "#;
        let mut e = env();
        e.insert("TDARR_ACCESS", "act");
        e.insert("BRIDGE_API_KEY", "bridge-key-value");
        let c = Config::with_instance(&e, Some(("/x/instance.toml", file))).unwrap();
        let bridge = c.integration("bridge").unwrap();
        assert_eq!(bridge.access, Some(Grant::Act));
        assert_eq!(bridge.access_var, "the `access` of integration bridge");
        assert_eq!(bridge.secret_var, "BRIDGE_API_KEY");
        assert_eq!(c.integration("worker").unwrap().access, None);
        // A word that is neither is an error there, as every typo in the file is.
        let bad = "[[integration]]\nid = \"b\"\ntype = \"hush\"\naccess = \"write\"\n";
        match Config::with_instance(&env(), Some(("/x/i.toml", bad))) {
            Err(ConfigError::Instance { message, .. }) => {
                assert!(
                    message.contains(
                        "`access` takes `read`, `act` or a list of HUSH's actions (send). `write` is"
                    ),
                    "{message}"
                )
            }
            other => panic!("{:?}", other.map(|_| ())),
        }
    }

    #[test]
    fn request_rules_take_user_ids_and_the_older_site_form() {
        let c = Config::from(&env()).unwrap();
        assert_eq!(c.request_rules.owners, vec![1]);
        assert_eq!(c.request_rules.max_seasons, 2);
        assert!(c.request_rules.season_gate);
        let mut e = env();
        e.insert(
            "SINGULARR_REQUEST_OWNERS",
            " Plex:3 , someone, emby:2, plex:x, 7, jellyfin:1, seerr:9",
        );
        e.insert("SINGULARR_REQUEST_GATED_SITES", "jellyfin");
        e.insert("SINGULARR_SYNC_REQUESTS_SECS", "10");
        let c = Config::from(&e).unwrap();
        assert_eq!(c.request_rules.owners, vec![3, 7, 9]);
        assert!(
            !c.request_rules.season_gate,
            "the gate acted on no site served here"
        );
        assert_eq!(
            c.warnings
                .iter()
                .filter(|w| w.starts_with("SINGULARR_REQUEST_OWNERS entry"))
                .count(),
            4,
            "a name, an unknown site, a bad id and a Jellyfin entry are each refused: {:?}",
            c.warnings
        );
        assert_eq!(c.sync.requests, Duration::from_secs(60));
        let mut e = env();
        e.insert("SINGULARR_REQUEST_GATED_SITES", "plex");
        assert!(Config::from(&e).unwrap().request_rules.season_gate);
        e.insert("SINGULARR_REQUEST_SEASON_GATE", "off");
        assert!(!Config::from(&e).unwrap().request_rules.season_gate);
    }

    #[test]
    fn folders_and_workspace_have_neutral_defaults() {
        let c = Config::from(&env()).unwrap();
        assert_eq!(c.music.root, "/media/music");
        assert!(
            c.movie_folders.roots.is_empty(),
            "no roots: the scan is off"
        );
        assert_eq!(c.editions_path, "/app/rules/editions.toml");
        assert_eq!(c.home_movies.root, "/media/homemovies");
        assert_eq!(c.music.interval, Duration::from_secs(21_600));
        assert_eq!(c.sync.requests, Duration::from_secs(300));
        assert_eq!(c.workspace.name, "Singularr");
        assert_eq!(c.workspace.portal, "Singularr Requests");
        assert_eq!(c.workspace.url, None);
        assert_eq!(c.bootstrap.trusted_proxies, None);
        // A fresh install trusts no proxy, and says so.
        assert!(
            c.warnings
                .iter()
                .any(|w| w.starts_with("SINGULARR_TRUSTED_PROXY_IPS is not set"))
        );
        let mut e = env();
        e.insert(
            "SINGULARR_MOVIE_ROOTS",
            "/media/movies/, relative, /media/4k",
        );
        e.insert("SINGULARR_OWNER_URL", "https://singularr.example.org/");
        let c = Config::from(&e).unwrap();
        assert_eq!(c.movie_folders.roots, vec!["/media/movies", "/media/4k"]);
        assert_eq!(
            c.workspace.url.as_deref(),
            Some("https://singularr.example.org")
        );
    }

    #[test]
    fn arrs_come_from_the_integrations_with_a_library_of_their_own() {
        let c = Config::from(&env()).unwrap();
        let radarr = c.arr("radarr").unwrap();
        assert!(radarr.configured());
        assert_eq!(radarr.url.as_deref(), Some("http://10.0.0.61:7979"));
        assert_eq!(radarr.library, "radarr");
        assert!(
            !c.arr("sonarr").unwrap().configured(),
            "CHANGEME is not a key"
        );
        assert_eq!(c.arrs.len(), 2);
        assert!(
            c.warnings
                .iter()
                .any(|w| w.contains("Integration sonarr (sonarr) not configured"))
        );
    }

    #[test]
    fn a_placeholder_edge_token_is_told_apart_from_none() {
        let mut e = env();
        let c = Config::from(&e).unwrap();
        assert!(!c.bootstrap.edge_token_set && !c.bootstrap.edge_token_placeholder);
        e.insert("SINGULARR_EDGE_TOKEN", "CHANGEME");
        let c = Config::from(&e).unwrap();
        assert!(!c.bootstrap.edge_token_set && c.bootstrap.edge_token_placeholder);
        assert!(c.to_string().contains("still the placeholder"));
        e.insert("SINGULARR_EDGE_TOKEN", "a-real-token");
        let c = Config::from(&e).unwrap();
        assert!(c.bootstrap.edge_token_set && !c.bootstrap.edge_token_placeholder);
        assert!(!c.to_string().contains("a-real-token"));
    }

    fn face_env() -> BTreeMap<&'static str, &'static str> {
        BTreeMap::from([
            (
                "SINGULARR_REQUESTS_DATABASE_URL",
                "postgres://singularr_requests:face-db-secret@10.0.0.83:5432/singularr",
            ),
            (
                "SINGULARR_REQUESTS_SESSION_SECRET",
                "0123456789abcdef0123456789abcdef-session",
            ),
            ("SINGULARR_TMDB_TOKEN", "tmdb-secret-token"),
            (
                "SINGULARR_REQUESTS_PUBLIC_URL",
                "https://requests.example.org/",
            ),
        ])
    }

    #[test]
    fn the_face_is_chosen_by_singularr_face() {
        assert_eq!(Face::from_env(&BTreeMap::new()).unwrap(), Face::Owner);
        let e = BTreeMap::from([("SINGULARR_FACE", "Requests")]);
        assert_eq!(Face::from_env(&e).unwrap(), Face::Requests);
        let e = BTreeMap::from([("SINGULARR_FACE", "public")]);
        assert!(matches!(Face::from_env(&e), Err(ConfigError::Invalid(..))));
    }

    #[test]
    fn the_face_starts_with_its_own_variables_and_nothing_else() {
        let c = RequestsFaceConfig::from(&face_env()).unwrap();
        assert_eq!(c.public_url, "https://requests.example.org");
        assert!(c.tmdb_token.is_some());
        let text = c.to_string();
        for secret in [
            "face-db-secret",
            "0123456789abcdef0123456789abcdef",
            "tmdb-secret-token",
        ] {
            assert!(!text.contains(secret), "the printout leaks {secret}");
        }
        // It refuses an integration secret, the full database login, the instance file or
        // the edge token.
        for var in [
            "PLEXSEERR_API_KEY",
            "SEERR_API_KEY",
            "PLEX_TOKEN",
            "DATABASE_URL",
            "SINGULARR_INSTANCE",
            "SINGULARR_EDGE_TOKEN",
            "QBIT_PASS",
        ] {
            let mut e = face_env();
            e.insert(var, "something");
            match RequestsFaceConfig::from(&e) {
                Err(ConfigError::NotForTheFace(list)) => assert!(list.contains(var)),
                other => panic!("{var}: {:?}", other.map(|_| ())),
            }
        }
        // A placeholder is no value, so the shared .env defaults do not stop it.
        let mut e = face_env();
        e.insert("RADARR_API_KEY", "CHANGEME");
        assert!(RequestsFaceConfig::from(&e).is_ok());
        // Its own variables are required and checked.
        let mut e = face_env();
        e.insert("SINGULARR_REQUESTS_SESSION_SECRET", "short");
        assert!(matches!(
            RequestsFaceConfig::from(&e),
            Err(ConfigError::Invalid(..))
        ));
        for var in [
            "SINGULARR_REQUESTS_DATABASE_URL",
            "SINGULARR_REQUESTS_PUBLIC_URL",
        ] {
            let mut e = face_env();
            e.remove(var);
            assert!(matches!(
                RequestsFaceConfig::from(&e),
                Err(ConfigError::Missing(_))
            ));
        }
        for (url, ok) in [
            ("https://requests.example.org/", true),
            ("https://requests.example.org/path", false),
            ("http://requests.example.org", false),
            ("http://localhost:7302", true),
        ] {
            let mut e = face_env();
            e.insert("SINGULARR_REQUESTS_PUBLIC_URL", url);
            assert_eq!(RequestsFaceConfig::from(&e).is_ok(), ok, "{url}");
        }
    }

    /// The portal refuses every variable named like a secret: each type's secret in the
    /// environment form under each of its prefixes, the default name of a file integration's,
    /// and any other key, token or password. The TMDB read token is its own. Its own settings
    /// pass. A placeholder is no value.
    #[test]
    fn the_portal_refuses_every_variable_named_like_a_secret() {
        let mut names: Vec<String> = Vec::new();
        for kind in IntegrationType::ALL {
            for prefix in kind.env_prefixes() {
                names.push(format!("{prefix}_{}", kind.secret_suffix()));
            }
            names.push(default_secret_var(
                &format!("{}-2", kind.as_str()),
                kind.manifest(),
            ));
        }
        names.extend(
            [
                "DEN_API_KEY",
                "SOME_TOKEN",
                "SMTP_PASS",
                "SINGULARR_EDGE_TOKEN",
            ]
            .map(String::from),
        );
        assert!(names.iter().any(|n| n == "QBITTORRENT_2_PASS"));
        for name in &names {
            let mut e: BTreeMap<&str, &str> = face_env();
            e.insert(name.as_str(), "something");
            match RequestsFaceConfig::from(&e) {
                Err(ConfigError::NotForTheFace(list)) => assert!(
                    list.split(", ").any(|n| n == name.as_str()),
                    "{name}: {list}"
                ),
                other => panic!("{name}: {:?}", other.map(|_| ())),
            }
            e.insert(name.as_str(), PLACEHOLDER);
            assert!(RequestsFaceConfig::from(&e).is_ok(), "{name}");
        }
        let mut e = face_env();
        for (name, value) in [
            ("SINGULARR_FACE", "requests"),
            ("SINGULARR_REQUESTS_DB_MAX_CONNECTIONS", "4"),
            ("SINGULARR_HTTP_TIMEOUT_SECS", "20"),
            ("RUST_LOG", "info"),
            ("TZ", "UTC"),
        ] {
            e.insert(name, value);
        }
        let c = RequestsFaceConfig::from(&e).unwrap();
        assert!(c.tmdb_token.is_some());
        assert!(!named_like_a_secret("SINGULARR_TMDB_TOKEN"));
        assert!(!named_like_a_secret("SINGULARR_REQUESTS_SESSION_SECRET"));
    }

    #[test]
    fn a_missing_database_url_is_an_error() {
        let mut e = env();
        e.remove("DATABASE_URL");
        assert!(matches!(
            Config::from(&e),
            Err(ConfigError::Missing("DATABASE_URL"))
        ));
    }

    #[test]
    fn the_printout_holds_no_secret() {
        let c = Config::from(&env()).unwrap();
        let text = c.to_string();
        for secret in [
            "hunter2secret",
            "0123456789abcdef0123456789abcdef",
            "torrent-password",
            "tmdb-secret-token",
            "owner@example.com",
            "seerr-secret-key",
            "portal-db-password",
        ] {
            assert!(!text.contains(secret), "the printout leaks {secret}");
        }
        assert!(text.contains("postgres://singularr:***@10.0.0.83:5432/singularr"));
        assert!(text.contains("http://10.0.0.61:7979"));
        assert!(text.contains("RADARR_API_KEY set (32 characters)"));
        assert!(text.contains("SINGULARR_REQUESTS_DB_PASSWORD set (18 characters)"));
    }

    #[test]
    fn the_portal_role_is_a_plain_name() {
        let c = Config::from(&env()).unwrap();
        assert!(c.portal_role.role.is_none(), "unset: the script's role");
        assert_eq!(
            c.portal_role.password.as_deref(),
            Some("portal-db-password")
        );
        let mut e = env();
        e.insert("SINGULARR_REQUESTS_DB_ROLE", "portal_2");
        assert_eq!(
            Config::from(&e).unwrap().portal_role.role.as_deref(),
            Some("portal_2")
        );
        let long: &'static str = Box::leak("p".repeat(64).into_boxed_str());
        for bad in [
            "Portal",
            "2portal",
            "portal-role",
            "portal role",
            "x; DROP ROLE y",
            long,
        ] {
            e.insert("SINGULARR_REQUESTS_DB_ROLE", bad);
            assert!(
                matches!(
                    Config::from(&e),
                    Err(ConfigError::Invalid("SINGULARR_REQUESTS_DB_ROLE", _))
                ),
                "{bad}"
            );
        }
        assert!(plain_role_name(&"p".repeat(63)));
    }

    #[test]
    fn intervals_have_floors() {
        let c = Config::from(&env()).unwrap();
        assert_eq!(c.sync.queue, Duration::from_secs(15));
        assert_eq!(c.sync.titles, Duration::from_secs(900));
        assert_eq!(c.sync.files_concurrency, 2);
        assert!(
            c.warnings
                .iter()
                .any(|w| w.contains("SINGULARR_SYNC_QUEUE_SECS"))
        );
    }
}
