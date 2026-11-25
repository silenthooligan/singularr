//! One manifest per integration type: every fact of a type in one place. The configuration,
//! the panels, the Test answer, the access book, the integration lines and the doctor read
//! them. A new type is its adapter, its manifest and one line in [`ALL`] and in
//! [`IntegrationType::manifest`].
//!
//! The types are those of the configuration (`core::config::IntegrationType`). The facts live
//! here, beside the adapters whose actions they name. The configuration reads them through
//! [`IntegrationType::manifest`].

use crate::core::config::IntegrationType;
use singularr_sdk::http::HttpProfile;
use singularr_sdk::manifest::{CountWords, Group, Kind, Manifest, Maturity, PassSpec, SecretSpec};

const fn words(key: &'static str, one: &'static str, many: &'static str) -> CountWords {
    CountWords { key, one, many }
}

const TITLES: CountWords = words("titles", "title", "titles");
const ITEMS: CountWords = words("items", "item", "items");
const ALBUMS: CountWords = words("albums", "album", "albums");
const TRACKS: CountWords = words("tracks", "track", "tracks");
const QUEUE: CountWords = words("queue", "in queue", "in queue");
const GRABS: CountWords = words("grabs_known", "grab", "grabs");
const TORRENTS: CountWords = words("torrents", "torrent", "torrents");
const DOWNLOADS: CountWords = words("downloads", "download", "downloads");
const REQUESTS: CountWords = words("requests", "request", "requests");
const PENDING: CountWords = words("pending_on_site", "pending", "pending");
const RUNS: CountWords = words("runs", "run", "runs");
const RUNS_OPEN: CountWords = words("runs_open", "open", "open");
const NODES: CountWords = words("nodes", "node", "nodes");
const BUSY: CountWords = words("workers_busy", "busy", "busy");
const PLAYS: CountWords = words("plays", "play", "plays");

const fn pass(field: &'static str, label: &'static str, setting: &'static str) -> PassSpec {
    PassSpec {
        field,
        label,
        setting,
    }
}

const fn secret(
    key: &'static str,
    label: &'static str,
    env_suffix: &'static str,
    optional: bool,
) -> SecretSpec {
    SecretSpec {
        key,
        label,
        env_suffix,
        optional,
        help: None,
    }
}

const API_KEY: SecretSpec = secret("apiKey", "API key", "API_KEY", false);
const PASSWORD: SecretSpec = secret("password", "Password", "PASS", false);
/// qBittorrent can trust its subnet, Transmission and NZBGet can run without a password.
const OPTIONAL_PASSWORD: SecretSpec = secret("password", "Password", "PASS", true);

const SERVER_READS_PLEX: &[&str] = &[
    "Libraries and folders",
    "Movies and shows with ratings, plays and editions",
    "Artists, albums and tracks",
    "Now playing",
    "Artwork",
];
const SERVER_READS: &[&str] = &[
    "Libraries and folders",
    "Movies and shows with ratings and plays",
    "Artists, albums and tracks",
    "Now playing",
    "Users",
    "Artwork",
];
const SERVER_PASSES: &[PassSpec] = &[pass("syncEvery", "Sync interval", "jobs.server")];
const SERVER_COUNTS: &[CountWords] = &[ITEMS, ALBUMS, TRACKS, PLAYS];
/// A media server's artwork is read through Singularr: a page of posters asks for many at once.
const SERVER_HTTP: HttpProfile = HttpProfile {
    in_flight: 16,
    ..HttpProfile::DEFAULT
};
const MANAGER_PASSES: &[PassSpec] = &[
    pass("syncEvery", "Sync interval", "jobs.titles"),
    pass("queueEvery", "Queue sync interval", "jobs.queue"),
    pass("historyEvery", "Grab history sync interval", "jobs.history"),
];
const MANAGER_COUNTS: &[CountWords] = &[TITLES, QUEUE, GRABS];
const TORRENT_READS: &[&str] = &[
    "Torrent state, progress and folder",
    "Speeds, seeds, peers and ratio",
    "Category and tracker",
];
const DOWNLOAD_PASSES: &[PassSpec] = &[pass("syncEvery", "Sync interval", "jobs.downloads")];
const PROCESSING_PASSES: &[PassSpec] = &[pass(
    "syncEvery",
    "Sync interval",
    "jobs.processing.syncEvery",
)];
const PROCESSING_COUNTS: &[CountWords] = &[RUNS, RUNS_OPEN, NODES, BUSY];

pub const PLEX: Manifest = Manifest {
    type_id: "plex",
    name: "Plex",
    label: "Plex Media Server",
    kinds: &[Kind::MediaServer, Kind::Player],
    group: Group::Server,
    role: "media server",
    description: "Libraries, ratings, plays, editions, music and sessions",
    reads: SERVER_READS_PLEX,
    env_prefixes: &["PLEX"],
    secret: secret("token", "Token", "TOKEN", false),
    takes_user: false,
    web_url: false,
    default_address: "http://plex:32400",
    max_instances: Some(1),
    actions: &[],
    passes: SERVER_PASSES,
    counts: SERVER_COUNTS,
    maturity: Maturity::Stable,
    http: SERVER_HTTP,
};

pub const JELLYFIN: Manifest = Manifest {
    type_id: "jellyfin",
    name: "Jellyfin",
    label: "Jellyfin",
    kinds: &[Kind::MediaServer, Kind::Player],
    group: Group::Server,
    role: "media server",
    description: "Libraries, ratings, plays, music and sessions",
    reads: SERVER_READS,
    env_prefixes: &["JELLYFIN"],
    secret: API_KEY,
    takes_user: false,
    web_url: true,
    default_address: "http://jellyfin:8096",
    max_instances: Some(1),
    actions: &[],
    passes: SERVER_PASSES,
    counts: SERVER_COUNTS,
    maturity: Maturity::Stable,
    http: SERVER_HTTP,
};

pub const EMBY: Manifest = Manifest {
    type_id: "emby",
    name: "Emby",
    label: "Emby",
    env_prefixes: &["EMBY"],
    default_address: "http://emby:8096",
    maturity: Maturity::Untested,
    ..JELLYFIN
};

pub const RADARR: Manifest = Manifest {
    type_id: "radarr",
    name: "Radarr",
    label: "Radarr",
    kinds: &[Kind::Manager],
    group: Group::Managers,
    role: "movies",
    description: "Movies, files, editions, queue and history",
    reads: &[
        "Movies, files and custom formats",
        "Quality profiles and tags",
        "Queue",
        "Grab history",
        "Release name parsing",
    ],
    env_prefixes: &["RADARR"],
    secret: API_KEY,
    takes_user: false,
    web_url: false,
    default_address: "http://radarr:7878",
    max_instances: None,
    actions: super::arr::ACTIONS,
    passes: MANAGER_PASSES,
    counts: MANAGER_COUNTS,
    maturity: Maturity::Stable,
    // A manager's whole list of titles is one answer: Radarr's of a large library passes 20 MB.
    http: HttpProfile {
        max_body: 256 << 20,
        ..HttpProfile::DEFAULT
    },
};

pub const SONARR: Manifest = Manifest {
    type_id: "sonarr",
    name: "Sonarr",
    label: "Sonarr",
    role: "shows",
    description: "Shows, episodes, files, queue and history",
    reads: &[
        "Series, seasons and episode files",
        "Quality profiles and tags",
        "Queue",
        "Grab history",
        "Release name parsing",
    ],
    env_prefixes: &["SONARR"],
    default_address: "http://sonarr:8989",
    ..RADARR
};

pub const QBITTORRENT: Manifest = Manifest {
    type_id: "qbittorrent",
    name: "qBittorrent",
    label: "qBittorrent",
    kinds: &[Kind::DownloadClient],
    group: Group::Downloads,
    role: "downloads",
    description: "Torrents, progress and unassigned downloads",
    reads: TORRENT_READS,
    env_prefixes: &["QBIT"],
    secret: OPTIONAL_PASSWORD,
    takes_user: true,
    web_url: false,
    default_address: "http://qbittorrent:8080",
    max_instances: None,
    actions: super::qbit::ACTIONS,
    passes: DOWNLOAD_PASSES,
    counts: &[TORRENTS],
    maturity: Maturity::Stable,
    http: HttpProfile::DEFAULT,
};

pub const TRANSMISSION: Manifest = Manifest {
    type_id: "transmission",
    name: "Transmission",
    label: "Transmission",
    env_prefixes: &["TRANSMISSION"],
    default_address: "http://transmission:9091",
    actions: super::transmission::ACTIONS,
    ..QBITTORRENT
};

pub const DELUGE: Manifest = Manifest {
    type_id: "deluge",
    name: "Deluge",
    label: "Deluge",
    env_prefixes: &["DELUGE"],
    secret: PASSWORD,
    takes_user: false,
    default_address: "http://deluge:8112",
    actions: super::deluge::ACTIONS,
    ..QBITTORRENT
};

pub const SABNZBD: Manifest = Manifest {
    type_id: "sabnzbd",
    name: "SABnzbd",
    label: "SABnzbd",
    kinds: &[Kind::DownloadClient],
    group: Group::Downloads,
    role: "downloads",
    description: "Usenet downloads, progress and unassigned downloads",
    reads: &[
        "Queue: state, progress, speed and time left",
        "History: completed, failed and in post-processing",
        "Category and folder",
    ],
    env_prefixes: &["SABNZBD"],
    secret: API_KEY,
    takes_user: false,
    web_url: false,
    default_address: "http://sabnzbd:8080",
    max_instances: None,
    actions: super::sabnzbd::ACTIONS,
    passes: DOWNLOAD_PASSES,
    counts: &[DOWNLOADS],
    maturity: Maturity::Stable,
    http: HttpProfile::DEFAULT,
};

pub const NZBGET: Manifest = Manifest {
    type_id: "nzbget",
    name: "NZBGet",
    label: "NZBGet",
    reads: &[
        "Queue: state, progress, speed and time left",
        "History: completed, failed and deleted",
        "Category and folder",
    ],
    env_prefixes: &["NZBGET"],
    secret: OPTIONAL_PASSWORD,
    takes_user: true,
    default_address: "http://nzbget:6789",
    actions: super::nzbget::ACTIONS,
    ..SABNZBD
};

/// Listed with its configuration and not read.
pub const PROWLARR: Manifest = Manifest {
    type_id: "prowlarr",
    name: "Prowlarr",
    label: "Prowlarr",
    kinds: &[Kind::Indexer],
    group: Group::Indexers,
    role: "indexers",
    description: "Indexers",
    reads: &[],
    env_prefixes: &["PROWLARR"],
    secret: API_KEY,
    takes_user: false,
    web_url: false,
    default_address: "http://prowlarr:9696",
    max_instances: None,
    actions: &[],
    passes: &[],
    counts: &[],
    maturity: Maturity::Stable,
    http: HttpProfile::DEFAULT,
};

/// Tdarr can run without a key.
pub const TDARR: Manifest = Manifest {
    type_id: "tdarr",
    name: "Tdarr",
    label: "Tdarr",
    kinds: &[Kind::Processor],
    group: Group::Processing,
    role: "processing",
    description: "Worker nodes, queues, active workers, libraries and flows",
    reads: &[
        "Version",
        "Nodes, worker limits and queue lengths",
        "Active workers",
        "Libraries and flows",
        "Statistics",
        "File status of open runs",
    ],
    env_prefixes: &["TDARR"],
    secret: secret("apiKey", "API key", "API_KEY", true),
    takes_user: false,
    web_url: false,
    default_address: "http://tdarr:8265",
    max_instances: None,
    actions: super::tdarr::ACTIONS,
    passes: PROCESSING_PASSES,
    counts: PROCESSING_COUNTS,
    maturity: Maturity::Stable,
    http: HttpProfile::DEFAULT,
};

/// The HUSH bridge: it stages files for a worker and imports what the worker made. It has no
/// port of its own to name. It needs a key only when it is set up to ask for one.
pub const HUSH: Manifest = Manifest {
    type_id: "hush",
    name: "HUSH",
    label: "HUSH",
    description: "Processing runs, their results and imports",
    reads: &[
        "Runs: flow, trigger, state, source, output and result",
        "Run events",
        "Ledger and watcher status",
        "Flow options for a file",
    ],
    env_prefixes: &["HUSH"],
    secret: SecretSpec {
        help: Some("Needed only when the service requires a key"),
        ..secret("apiKey", "API key", "API_KEY", true)
    },
    default_address: "http://hush",
    actions: super::hush::ACTIONS,
    ..TDARR
};

/// `PLEXSEERR` is the older name of `SEERR` and keeps working.
pub const SEERR: Manifest = Manifest {
    type_id: "seerr",
    name: "Seerr",
    label: "Seerr",
    kinds: &[Kind::RequestSource],
    group: Group::Requests,
    role: "requests",
    description: "Requests made in Seerr",
    reads: &[
        "Requests and status",
        "Requester display names",
        "Title lookups",
        "Discover lists",
    ],
    env_prefixes: &["SEERR", "PLEXSEERR"],
    secret: API_KEY,
    takes_user: false,
    web_url: false,
    default_address: "http://seerr:5055",
    max_instances: Some(1),
    actions: &[],
    passes: &[pass("syncEvery", "Sync interval", "jobs.requests")],
    counts: &[REQUESTS, PENDING],
    maturity: Maturity::Stable,
    http: HttpProfile::DEFAULT,
};

/// Tautulli: the media server's plays, kept for years, with durations and delivery.
pub const TAUTULLI: Manifest = Manifest {
    type_id: "tautulli",
    name: "Tautulli",
    label: "Tautulli",
    kinds: &[Kind::PlayHistory],
    group: Group::Server,
    role: "plays",
    description: "Play history with durations, devices and delivery",
    reads: &[
        "Play history with durations, devices and delivery",
        "Users",
        "Version",
    ],
    env_prefixes: &["TAUTULLI"],
    secret: API_KEY,
    takes_user: false,
    web_url: false,
    default_address: "http://tautulli:8181",
    max_instances: Some(1),
    actions: &[],
    passes: &[pass(
        "syncEvery",
        "Read it every",
        "jobs.plays.historyEvery",
    )],
    counts: &[PLAYS],
    maturity: Maturity::Stable,
    http: HttpProfile::DEFAULT,
};

/// Every type, in the order Settings lists them: the order of `IntegrationType::ALL`.
pub static ALL: [&Manifest; 15] = [
    &PLEX,
    &JELLYFIN,
    &EMBY,
    &RADARR,
    &SONARR,
    &QBITTORRENT,
    &TRANSMISSION,
    &DELUGE,
    &SABNZBD,
    &NZBGET,
    &PROWLARR,
    &TDARR,
    &HUSH,
    &SEERR,
    &TAUTULLI,
];

impl IntegrationType {
    /// The type's manifest.
    pub fn manifest(self) -> &'static Manifest {
        match self {
            IntegrationType::Plex => &PLEX,
            IntegrationType::Jellyfin => &JELLYFIN,
            IntegrationType::Emby => &EMBY,
            IntegrationType::Radarr => &RADARR,
            IntegrationType::Sonarr => &SONARR,
            IntegrationType::Qbittorrent => &QBITTORRENT,
            IntegrationType::Transmission => &TRANSMISSION,
            IntegrationType::Deluge => &DELUGE,
            IntegrationType::Sabnzbd => &SABNZBD,
            IntegrationType::Nzbget => &NZBGET,
            IntegrationType::Prowlarr => &PROWLARR,
            IntegrationType::Tdarr => &TDARR,
            IntegrationType::Hush => &HUSH,
            IntegrationType::Seerr => &SEERR,
            IntegrationType::Tautulli => &TAUTULLI,
        }
    }
}

/// The words of every number the passes count, each once, in the order of the types and of
/// their lines: for a pass's line, whatever its type.
pub fn count_words() -> &'static [CountWords] {
    static WORDS: std::sync::LazyLock<Vec<CountWords>> = std::sync::LazyLock::new(|| {
        let mut all: Vec<CountWords> = Vec::new();
        for words in ALL.iter().flat_map(|m| m.counts) {
            if !all.iter().any(|w| w.key == words.key) {
                all.push(*words);
            }
        }
        all
    });
    &WORDS
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use singularr_sdk::testkit::check_manifest;

    /// A type no build lists: the tests that show a new manifest reaches every reader use it.
    pub(crate) static SAMPLE: Manifest = Manifest {
        type_id: "sample",
        name: "Sample",
        label: "Sample Server",
        kinds: &[Kind::PlayHistory],
        group: Group::Server,
        role: "plays",
        description: "Plays and users",
        reads: &["Plays", "Users"],
        env_prefixes: &["SAMPLE"],
        secret: secret("token", "Token", "TOKEN", false),
        takes_user: true,
        web_url: false,
        default_address: "http://sample:8400",
        max_instances: Some(1),
        actions: &[],
        passes: &[pass(
            "syncEvery",
            "Sync interval",
            "jobs.plays.historyEvery",
        )],
        counts: &[PLAYS],
        maturity: Maturity::Untested,
        http: HttpProfile::DEFAULT,
    };

    #[test]
    fn every_manifest_passes_the_check_and_names_its_own_type() {
        for m in ALL.iter().copied().chain([&SAMPLE]) {
            check_manifest(m).unwrap_or_else(|e| panic!("{}: {e}", m.type_id));
        }
        let ids: Vec<&str> = ALL.iter().map(|m| m.type_id).collect();
        let types: Vec<&str> = IntegrationType::ALL
            .iter()
            .map(|k| k.manifest().type_id)
            .collect();
        assert_eq!(ids, types, "ALL is in the order of IntegrationType::ALL");
        for kind in IntegrationType::ALL {
            assert_eq!(IntegrationType::parse(kind.manifest().type_id), Some(kind));
        }
    }

    /// The words of a pass's line keep the order they always had.
    #[test]
    fn the_count_words_keep_their_order() {
        let keys: Vec<&str> = count_words().iter().map(|w| w.key).collect();
        let before = [
            "titles",
            "items",
            "albums",
            "tracks",
            "queue",
            "grabs_known",
            "torrents",
            "downloads",
            "requests",
            "pending_on_site",
            "runs",
            "runs_open",
            "nodes",
            "workers_busy",
            "plays",
        ];
        assert_eq!(keys.len(), before.len());
        for m in ALL {
            let mine: Vec<usize> = m
                .counts
                .iter()
                .map(|w| before.iter().position(|k| *k == w.key).unwrap())
                .collect();
            assert!(mine.is_sorted(), "{}: {:?}", m.type_id, m.counts);
        }
    }
}
