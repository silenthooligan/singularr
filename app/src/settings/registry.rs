//! The registry: every setting the server describes, as data. A setting is one [`Field`] entry
//! in [`SECTIONS`]; adding a setting is adding an entry. The types are the platform's
//! (`singularr_platform::settings::registry`). Each entry says its type, its default,
//! the variables and the instance file key that fix it, how its value in force is read from the
//! configuration, and whether it can be saved here.
//!
//! A field that `saves` can be changed in the interface (`PATCH /api/v1/settings`) as long as no
//! variable and no instance file key fixes it. Every other field is a read-only view of the
//! configuration, shown with its source, so the Settings page shows the whole configuration.

use crate::core::config::{Config, redact_url};
use serde_json::{Value, json};
use singularr_platform::settings::registry as kit;
pub use singularr_platform::settings::registry::{Check, Choice, Kind, choice};

/// The registry's types over the app's configuration.
pub type Field = kit::Field<Config>;
pub type Group = kit::Group<Config>;
pub type Section = kit::Section<Config>;
pub type Fallback = kit::Fallback<Config>;
pub type Read = kit::Read<Config>;

/// Every field, in the order of the sections and groups.
pub fn fields() -> impl Iterator<Item = &'static Field> {
    kit::fields::<Config>()
}

pub fn field(key: &str) -> Option<&'static Field> {
    kit::field::<Config>(key)
}

/// `file` when `SINGULARR_POLICY` names a policy file, `saved` otherwise.
fn policy_source(c: &Config) -> Value {
    json!(if c.policy_pinned { "file" } else { "saved" })
}

// Reading the configuration --------------------------------------------------------------------

fn secs(d: std::time::Duration) -> Value {
    json!(d.as_secs())
}

fn opt(v: &Option<String>) -> Value {
    v.as_ref().map(|s| json!(s)).unwrap_or(Value::Null)
}

/// Two ASCII letters, upper case: `US`, `DE`.
fn region(text: &str) -> Result<String, String> {
    let t = text.trim().to_ascii_uppercase();
    if t.len() == 2 && t.chars().all(|c| c.is_ascii_uppercase()) {
        Ok(t)
    } else {
        Err("Two-letter country code, such as US, GB or DE".to_string())
    }
}

const DATE_FORMATS: &[Choice] = &[
    choice("D MMM YYYY", "23 Nov 2026"),
    choice("MMM D, YYYY", "Nov 23, 2026"),
    choice("YYYY-MM-DD", "2026-11-23"),
    choice("DD/MM/YYYY", "23/11/2026"),
    choice("MM/DD/YYYY", "11/23/2026"),
];

const TIME_FORMATS: &[Choice] = &[
    choice("24h", "24-hour (14:05)"),
    choice("12h", "12-hour (2:05 PM)"),
];

const THEMES: &[Choice] = &[
    choice("horizon", "Event Horizon"),
    choice("mocha", "Mocha"),
    choice("graphite", "Graphite"),
];

const POLICY_SOURCES: &[Choice] = &[choice("saved", "Saved"), choice("file", "Policy file")];

const DENSITIES: &[Choice] = &[
    choice("comfortable", "Comfortable"),
    choice("compact", "Compact"),
];

/// The pages a person can start on, by the client's page names.
const START_PAGES: &[Choice] = &[
    choice("index", "Home"),
    choice("discover", "Discover"),
    choice("movies", "Movies"),
    choice("shows", "Shows"),
    choice("music", "Music"),
    choice("requests", "Requests"),
    choice("activity", "Activity"),
    choice("calendar", "Calendar"),
];

const SIDEBARS: &[Choice] = &[
    choice("full", "Names and icons"),
    choice("rail", "Icons only"),
];

const PORTAL_QUALITY: &[Choice] = &[choice("HD", "HD")];

/// How long plays are kept, in days.
const PLAYS_KEPT: &[Choice] = &[
    choice("forever", "Forever"),
    choice("90", "90 days"),
    choice("365", "1 year"),
    choice("730", "2 years"),
    choice("1825", "5 years"),
];

const PORTAL_SIGN_IN: &[Choice] = &[choice("plex", "Plex")];

/// The sections, in the order of the Settings page's sub-pages. The words follow
/// `docs/copy.md`: a label is a noun phrase, a help text is one line of twelve words at most
/// or empty, and neither ends with a period.
pub static SECTIONS: &[Section] = &[
    Section {
        id: "general",
        label: "General",
        summary: "Names, language, dates and times",
        panels: &["server"],
        groups: &[
            Group {
                id: "names",
                label: "Names",
                help: "",
                fields: &[
                    Field::new(
                        "general.name",
                        "Workspace name",
                        Kind::Text,
                        "Shown in the sidebar and the browser tab",
                    )
                    .by_default(Fallback::Text("Singularr"))
                    .env(&["SINGULARR_WORKSPACE_NAME"])
                    .file("workspace.name")
                    .read(|c| json!(c.workspace.name))
                    .saves(),
                    Field::new(
                        "general.url",
                        "Workspace address",
                        Kind::Url { relative: false },
                        "Public address, linked from the portal",
                    )
                    .env(&["SINGULARR_OWNER_URL"])
                    .file("workspace.url")
                    .read(|c| opt(&c.workspace.url))
                    .saves(),
                    Field::new(
                        "general.signOutUrl",
                        "Sign-out address",
                        Kind::Url { relative: true },
                        "Where Sign out leads, for example /oauth2/sign_out",
                    )
                    .env(&["SINGULARR_SIGN_OUT_URL"])
                    .read(|c| opt(&c.workspace.sign_out_url))
                    .saves(),
                ],
            },
            Group {
                id: "locale",
                label: "Language and region",
                help: "",
                fields: &[
                    Field::new(
                        "general.language",
                        "Metadata language",
                        Kind::Language,
                        "Language of titles, overviews and taglines",
                    )
                    .by_default(Fallback::Text("en-US"))
                    .saves(),
                    Field::new(
                        "general.region",
                        "Region",
                        Kind::Text,
                        "Country for release dates, certifications and where to watch",
                    )
                    .by_default(Fallback::Text("US"))
                    .env(&["SINGULARR_WATCH_REGION"])
                    .read(|c| json!(c.watch_region))
                    .check(region)
                    .saves(),
                ],
            },
            Group {
                id: "time",
                label: "Dates and times",
                help: "",
                fields: &[
                    Field::new(
                        "general.timeZone",
                        "Time zone",
                        Kind::Timezone,
                        "Defaults to the server's time zone",
                    )
                    .by_default(Fallback::TimeZone)
                    .saves(),
                    Field::new(
                        "general.dateFormat",
                        "Date format",
                        Kind::Select(DATE_FORMATS),
                        "",
                    )
                    .by_default(Fallback::Text("D MMM YYYY"))
                    .saves(),
                    Field::new(
                        "general.timeFormat",
                        "Time format",
                        Kind::Select(TIME_FORMATS),
                        "",
                    )
                    .by_default(Fallback::Text("24h"))
                    .saves(),
                ],
            },
        ],
    },
    Section {
        id: "integrations",
        label: "Integrations",
        summary: "Services and metadata providers",
        panels: &["integrations"],
        groups: &[
            Group {
                id: "metadata",
                label: "Metadata providers",
                help: "",
                fields: &[
                    Field::new(
                        "integrations.tmdbToken",
                        "TMDB read access token",
                        Kind::Secret,
                        "",
                    )
                    .env(&["SINGULARR_TMDB_TOKEN"])
                    .restart(),
                    Field::new(
                        "integrations.tvdbKey",
                        "TVDB API key",
                        Kind::Secret,
                        "TVDB v4 key, for comparing episode order",
                    )
                    .env(&["SINGULARR_TVDB_KEY"])
                    .restart(),
                ],
            },
            Group {
                id: "connections",
                label: "Connections",
                help: "",
                fields: &[
                    Field::new(
                        "integrations.instanceFile",
                        "Instance file",
                        Kind::Path,
                        "Lists integrations; secrets stay in the environment",
                    )
                    .env(&["SINGULARR_INSTANCE"])
                    .read(|c| opt(&c.bootstrap.instance_path))
                    .restart(),
                    Field::new(
                        "integrations.timeout",
                        "Request timeout",
                        Kind::Duration { min: 5 },
                        "",
                    )
                    .by_default(Fallback::Int(30))
                    .env(&["SINGULARR_HTTP_TIMEOUT_SECS"])
                    .read(|c| secs(c.bootstrap.http_timeout))
                    .restart(),
                    Field::new(
                        "integrations.plexWebUrl",
                        "Plex web app address",
                        Kind::Url { relative: false },
                        "Base address for links that open a title in Plex",
                    )
                    .by_default(Fallback::Text("https://app.plex.tv/desktop"))
                    .env(&["SINGULARR_PLEX_WEB_URL"])
                    .read(|c| json!(c.plex_web_url))
                    .restart(),
                ],
            },
        ],
    },
    Section {
        id: "libraries",
        label: "Libraries",
        summary: "Media server libraries and folders",
        panels: &["libraries"],
        groups: &[
            Group {
                id: "folders",
                label: "Folders",
                help: "",
                fields: &[
                    Field::new(
                        "libraries.musicRoot",
                        "Music folder",
                        Kind::Path,
                        "Artist and album folders",
                    )
                    .by_default(Fallback::Text("/media/music"))
                    .env(&["SINGULARR_MUSIC_ROOT"])
                    .read(|c| json!(c.music.root))
                    .restart(),
                    Field::new(
                        "libraries.homeMoviesRoot",
                        "Home movies folder",
                        Kind::Path,
                        "One video file per recording",
                    )
                    .by_default(Fallback::Text("/media/homemovies"))
                    .env(&["SINGULARR_HOME_MOVIES_ROOT"])
                    .read(|c| json!(c.home_movies.root))
                    .restart(),
                    Field::new(
                        "libraries.movieRoots",
                        "Movie root folders",
                        Kind::List,
                        "Scanned for untracked files; empty turns the scan off",
                    )
                    .by_default(Fallback::List(&[]))
                    .env(&["SINGULARR_MOVIE_ROOTS"])
                    .read(|c| json!(c.movie_folders.roots))
                    .restart(),
                ],
            },
            Group {
                id: "editions",
                label: "Editions",
                help: "",
                fields: &[Field::new(
                    "libraries.editionsFile",
                    "Edition list",
                    Kind::Path,
                    "Falls back to the built-in list when missing",
                )
                .by_default(Fallback::Text("/app/rules/editions.toml"))
                .env(&["SINGULARR_EDITIONS"])
                .read(|c| json!(c.editions_path))
                .restart()],
            },
        ],
    },
    Section {
        id: "people",
        label: "People and access",
        summary: "Sign-in, roles and request rules",
        panels: &["people", "roles"],
        groups: &[
            Group {
                id: "signIn",
                label: "Sign-in",
                help: "",
                fields: &[
                    Field::new(
                        "people.ownerEmails",
                        "Owner email addresses",
                        Kind::List,
                        "Accounts that sign in as the owner",
                    )
                    .by_default(Fallback::List(&[]))
                    .env(&["SINGULARR_OWNER_EMAILS"])
                    .read(|c| {
                        json!(
                            c.bootstrap
                                .owner_emails
                                .split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                        )
                    })
                    .private()
                    .restart(),
                    Field::new(
                        "people.trustedProxies",
                        "Trusted proxies",
                        Kind::List,
                        "Proxy addresses whose identity headers are accepted",
                    )
                    .by_default(Fallback::List(&[]))
                    .env(&["SINGULARR_TRUSTED_PROXY_IPS"])
                    .read(|c| {
                        json!(
                            c.bootstrap
                                .trusted_proxies
                                .as_deref()
                                .unwrap_or("")
                                .split(',')
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                        )
                    })
                    .restart(),
                    Field::new(
                        "people.edgeToken",
                        "Edge token",
                        Kind::Secret,
                        "Sent by the proxy as X-Edge-Token",
                    )
                    .env(&["SINGULARR_EDGE_TOKEN"])
                    .restart(),
                ],
            },
            Group {
                id: "requestRules",
                label: "Request rules",
                help: "",
                fields: &[
                    Field::new(
                        "people.requestOwners",
                        "Owner accounts on Seerr",
                        Kind::List,
                        "Seerr user IDs exempt from the season limit and retention",
                    )
                    .by_default(Fallback::List(&["1"]))
                    .env(&["SINGULARR_REQUEST_OWNERS"])
                    .read(|c| {
                        json!(
                            c.request_rules
                                .owners
                                .iter()
                                .map(|id| id.to_string())
                                .collect::<Vec<_>>()
                        )
                    })
                    .restart(),
                    Field::new(
                        "people.seasonGate",
                        "Season limit",
                        Kind::Bool,
                        "Limits the seasons of one show a member holds at once",
                    )
                    .by_default(Fallback::Bool(true))
                    .env(&[
                        "SINGULARR_REQUEST_SEASON_GATE",
                        "SINGULARR_REQUEST_GATED_SITES",
                    ])
                    .read(|c| json!(c.request_rules.season_gate))
                    .restart(),
                    Field::new(
                        "people.maxSeasons",
                        "Maximum seasons per show",
                        Kind::Number {
                            min: 1,
                            max: 100,
                            unit: Some("seasons"),
                        },
                        "",
                    )
                    .by_default(Fallback::Int(2))
                    .env(&["SINGULARR_REQUEST_MAX_SEASONS"])
                    .read(|c| json!(c.request_rules.max_seasons))
                    .restart(),
                ],
            },
        ],
    },
    Section {
        id: "portal",
        label: "Portal",
        summary: "Request portal name, address and requests",
        panels: &[],
        groups: &[
            Group {
                id: "portal",
                label: "Portal",
                help: "",
                fields: &[
                    Field::new(
                        "portal.name",
                        "Portal name",
                        Kind::Text,
                        "Shown to requesters",
                    )
                    .by_default(Fallback::Text("Singularr Requests"))
                    .env(&["SINGULARR_PORTAL_NAME"])
                    .file("workspace.portal")
                    .read(|c| json!(c.workspace.portal))
                    .saves(),
                    Field::new(
                        "portal.url",
                        "Portal address",
                        Kind::Url { relative: false },
                        "Public address, linked from the workspace",
                    )
                    .env(&["SINGULARR_REQUESTS_PUBLIC_URL"])
                    .file("workspace.portal_url")
                    .read(|c| opt(&c.workspace.portal_url))
                    .saves(),
                ],
            },
            Group {
                id: "requests",
                label: "Portal requests",
                help: "",
                fields: &[
                    Field::new(
                        "portal.quality",
                        "Quality",
                        Kind::Select(PORTAL_QUALITY),
                        "4K is requested from the workspace",
                    )
                    .by_default(Fallback::Text("HD")),
                    Field::new(
                        "portal.signIn",
                        "Sign-in",
                        Kind::Select(PORTAL_SIGN_IN),
                        "Plex accounts the media server is shared with",
                    )
                    .by_default(Fallback::Text("plex")),
                ],
            },
        ],
    },
    Section {
        id: "notifications",
        label: "Notifications",
        summary: "Channels and events",
        panels: &["channels"],
        groups: &[],
    },
    Section {
        id: "appearance",
        label: "Appearance",
        summary: "Theme, density and start page",
        panels: &[],
        groups: &[Group {
            id: "look",
            label: "Look",
            help: "",
            fields: &[
                Field::new("appearance.theme", "Theme", Kind::Select(THEMES), "")
                    .by_default(Fallback::Text("horizon"))
                    .per_user()
                    .saves(),
                Field::new(
                    "appearance.density",
                    "Table density",
                    Kind::Select(DENSITIES),
                    "",
                )
                .by_default(Fallback::Text("comfortable"))
                .per_user()
                .saves(),
                Field::new(
                    "appearance.startPage",
                    "Start page",
                    Kind::Select(START_PAGES),
                    "",
                )
                .by_default(Fallback::Text("index"))
                .per_user()
                .saves(),
                Field::new("appearance.sidebar", "Sidebar", Kind::Select(SIDEBARS), "")
                    .by_default(Fallback::Text("full"))
                    .per_user()
                    .saves(),
            ],
        }],
    },
    Section {
        id: "jobs",
        label: "Jobs",
        summary: "Schedules and cache",
        panels: &["jobs", "cache"],
        groups: &[
            Group {
                id: "sync",
                label: "Sync intervals",
                help: "",
                fields: &[
                    Field::new(
                        "jobs.syncEnabled",
                        "Integration sync",
                        Kind::Bool,
                        "Off serves stored data; folders are still scanned",
                    )
                    .by_default(Fallback::Bool(true))
                    .env(&["SINGULARR_SYNC_ENABLED"])
                    .read(|c| json!(c.sync.enabled))
                    .restart(),
                    Field::new(
                        "jobs.titles",
                        "Titles and files",
                        Kind::Duration { min: 60 },
                        "",
                    )
                    .by_default(Fallback::Int(900))
                    .env(&["SINGULARR_SYNC_TITLES_SECS"])
                    .read(|c| secs(c.sync.titles))
                    .restart(),
                    Field::new(
                        "jobs.filesFull",
                        "Full episode file scan",
                        Kind::Duration { min: 600 },
                        "Every show's files, changed or not",
                    )
                    .by_default(Fallback::Int(21_600))
                    .env(&["SINGULARR_SYNC_FILES_FULL_SECS"])
                    .read(|c| secs(c.sync.files_full))
                    .restart(),
                    Field::new(
                        "jobs.filesConcurrency",
                        "Concurrent show reads",
                        Kind::Number {
                            min: 1,
                            max: 4,
                            unit: Some("shows"),
                        },
                        "",
                    )
                    .by_default(Fallback::Int(2))
                    .env(&["SINGULARR_SYNC_FILES_CONCURRENCY"])
                    .read(|c| json!(c.sync.files_concurrency))
                    .restart(),
                    Field::new(
                        "jobs.queue",
                        "Download queues",
                        Kind::Duration { min: 15 },
                        "",
                    )
                    .by_default(Fallback::Int(60))
                    .env(&["SINGULARR_SYNC_QUEUE_SECS"])
                    .read(|c| secs(c.sync.queue))
                    .restart(),
                    Field::new(
                        "jobs.downloads",
                        "Download clients",
                        Kind::Duration { min: 10 },
                        "",
                    )
                    .by_default(Fallback::Int(30))
                    .env(&["SINGULARR_SYNC_DOWNLOADS_SECS", "SINGULARR_SYNC_QBIT_SECS"])
                    .read(|c| secs(c.sync.downloads))
                    .restart(),
                    Field::new(
                        "jobs.history",
                        "Grab history",
                        Kind::Duration { min: 60 },
                        "",
                    )
                    .by_default(Fallback::Int(900))
                    .env(&["SINGULARR_SYNC_HISTORY_SECS"])
                    .read(|c| secs(c.sync.history))
                    .restart(),
                    Field::new(
                        "jobs.historyPages",
                        "Grab history pages per read",
                        Kind::Number {
                            min: 1,
                            max: 400,
                            unit: Some("pages"),
                        },
                        "At most; 250 grabs per page",
                    )
                    .by_default(Fallback::Int(40))
                    .env(&["SINGULARR_SYNC_HISTORY_MAX_PAGES"])
                    .read(|c| json!(c.sync.history_max_pages))
                    .restart(),
                    Field::new(
                        "jobs.requests",
                        "Requests",
                        Kind::Duration { min: 60 },
                        "Requests and accounts",
                    )
                    .by_default(Fallback::Int(300))
                    .env(&["SINGULARR_SYNC_REQUESTS_SECS"])
                    .read(|c| secs(c.sync.requests))
                    .restart(),
                    Field::new(
                        "jobs.server",
                        "Media server",
                        Kind::Duration { min: 300 },
                        "Libraries, ratings, plays and music",
                    )
                    .by_default(Fallback::Int(3600))
                    .env(&["SINGULARR_SYNC_SERVER_SECS"])
                    .read(|c| secs(c.sync.server))
                    .restart(),
                    Field::new(
                        "jobs.processing.syncEvery",
                        "Processing services",
                        Kind::Duration { min: 15 },
                        "Status and newest runs; a full read every 30 min",
                    )
                    .by_default(Fallback::Int(60))
                    .env(&["SINGULARR_SYNC_PROCESSING_SECS"])
                    .read(|c| secs(c.sync.processing))
                    .restart(),
                    Field::new(
                        "jobs.jitter",
                        "Startup delay",
                        Kind::Duration { min: 0 },
                        "Random delay before each first sync, up to this long",
                    )
                    .by_default(Fallback::Int(20))
                    .env(&["SINGULARR_SYNC_JITTER_SECS"])
                    .read(|c| secs(c.sync.jitter))
                    .restart(),
                ],
            },
            Group {
                id: "scans",
                label: "Folder scans",
                help: "",
                fields: &[
                    Field::new(
                        "jobs.musicScan",
                        "Music folder",
                        Kind::Duration { min: 600 },
                        "",
                    )
                    .by_default(Fallback::Int(21_600))
                    .env(&["SINGULARR_MUSIC_SCAN_SECS"])
                    .read(|c| secs(c.music.interval))
                    .restart(),
                    Field::new(
                        "jobs.homeMoviesScan",
                        "Home movies folder",
                        Kind::Duration { min: 600 },
                        "",
                    )
                    .by_default(Fallback::Int(21_600))
                    .env(&["SINGULARR_HOME_MOVIES_SCAN_SECS"])
                    .read(|c| secs(c.home_movies.interval))
                    .restart(),
                    Field::new(
                        "jobs.movieScan",
                        "Movie root folders",
                        Kind::Duration { min: 600 },
                        "Files no manager tracks",
                    )
                    .by_default(Fallback::Int(21_600))
                    .env(&["SINGULARR_MOVIE_SCAN_SECS"])
                    .read(|c| secs(c.movie_folders.interval))
                    .restart(),
                ],
            },
            Group {
                id: "processing",
                label: "Processing",
                help: "",
                fields: &[Field::new(
                    "jobs.processing.stuckAfter",
                    "Stuck threshold",
                    Kind::Duration { min: 60 },
                    "An open run with no worker is Stuck after this long",
                )
                .by_default(Fallback::Int(21_600))
                .saves()],
            },
            Group {
                id: "plays",
                label: "Play history",
                help: "",
                fields: &[
                    Field::new(
                        "jobs.plays.historyEvery",
                        "Sync interval",
                        Kind::Duration { min: 300 },
                        "Media server history and Tautulli",
                    )
                    .by_default(Fallback::Int(900))
                    .saves(),
                    Field::new(
                        "jobs.plays.keepFor",
                        "Retention",
                        Kind::Select(PLAYS_KEPT),
                        "",
                    )
                    .by_default(Fallback::Text("forever"))
                    .saves(),
                    Field::new(
                        "jobs.plays.keepAddresses",
                        "Keep IP addresses",
                        Kind::Bool,
                        "Off: local or remote only",
                    )
                    .by_default(Fallback::Bool(true))
                    .saves(),
                    Field::new(
                        "jobs.plays.locations",
                        "Location database download",
                        Kind::Bool,
                        "DB-IP City Lite, monthly, into the data folder",
                    )
                    .by_default(Fallback::Bool(true))
                    .env(&["SINGULARR_GEOIP_DOWNLOAD"])
                    .read(|c| json!(c.geoip.download.unwrap_or(true)))
                    .saves(),
                    Field::new(
                        "jobs.plays.locationsFile",
                        "Location database file",
                        Kind::Path,
                        "Replaces the download",
                    )
                    .env(&["SINGULARR_GEOIP_PATH"])
                    .read(|c| opt(&c.geoip.path))
                    .restart(),
                ],
            },
        ],
    },
    Section {
        id: "api",
        label: "API",
        summary: "API keys and reference",
        panels: &["keys"],
        groups: &[Group {
            id: "reference",
            label: "Reference",
            help: "",
            fields: &[
                Field::new(
                    "api.reference",
                    "API reference",
                    Kind::Url { relative: true },
                    "",
                )
                .by_default(Fallback::Text("/api/docs")),
                Field::new(
                    "api.document",
                    "OpenAPI document",
                    Kind::Url { relative: true },
                    "OpenAPI 3.1",
                )
                .by_default(Fallback::Text("/api/v1/openapi.json")),
            ],
        }],
    },
    Section {
        id: "system",
        label: "System",
        summary: "Database, search, files and build",
        panels: &["services", "about"],
        groups: &[
            Group {
                id: "database",
                label: "Database",
                help: "",
                fields: &[
                    Field::new(
                        "system.database",
                        "Database address",
                        Kind::Text,
                        "Shown without the password",
                    )
                    .env(&["DATABASE_URL"])
                    .read(|c| json!(redact_url(&c.bootstrap.database_url)))
                    .private()
                    .restart(),
                    Field::new(
                        "system.databaseConnections",
                        "Connection pool size",
                        Kind::Number {
                            min: 2,
                            max: 50,
                            unit: Some("connections"),
                        },
                        "",
                    )
                    .by_default(Fallback::Int(10))
                    .env(&["SINGULARR_DB_MAX_CONNECTIONS"])
                    .read(|c| json!(c.bootstrap.db_max_connections))
                    .restart(),
                    Field::new(
                        "system.dataDir",
                        "Data folder",
                        Kind::Path,
                        "Downloaded files",
                    )
                    .by_default(Fallback::Text("/data"))
                    .env(&["SINGULARR_DATA_DIR"])
                    .read(|c| json!(c.geoip.data_dir))
                    .restart(),
                ],
            },
            Group {
                id: "search",
                label: "Search",
                help: "",
                fields: &[
                    Field::new(
                        "system.searchUrl",
                        "Meilisearch address",
                        Kind::Url { relative: false },
                        "Needs the API key; empty uses built-in search",
                    )
                    .env(&["SINGULARR_SEARCH_URL"])
                    .read(|c| opt(&c.search.url))
                    .restart(),
                    Field::new(
                        "system.searchKey",
                        "Meilisearch API key",
                        Kind::Secret,
                        "Needs read and write access to the prefixed indexes",
                    )
                    .env(&["SINGULARR_SEARCH_KEY"])
                    .restart(),
                    Field::new("system.searchPrefix", "Index prefix", Kind::Text, "")
                        .by_default(Fallback::Text("singularr_"))
                        .env(&["SINGULARR_SEARCH_PREFIX"])
                        .read(|c| json!(c.search.prefix))
                        .restart(),
                    Field::new(
                        "system.searchIndexEvery",
                        "Full index interval",
                        Kind::Duration { min: 60 },
                        "Changes are indexed shortly after each sync",
                    )
                    .by_default(Fallback::Int(900))
                    .env(&["SINGULARR_SEARCH_INDEX_SECS"])
                    .read(|c| secs(c.search.interval))
                    .restart(),
                ],
            },
            Group {
                id: "files",
                label: "Files",
                help: "",
                fields: &[
                    Field::new(
                        "system.policyFile",
                        "Policy file",
                        Kind::Path,
                        "Falls back to the built-in policy when missing",
                    )
                    .by_default(Fallback::Text("/app/rules/policy.toml"))
                    .env(&["SINGULARR_POLICY"])
                    .read(|c| json!(c.policy_path))
                    .restart(),
                    Field::new(
                        "system.policySource",
                        "Policy source",
                        Kind::Select(POLICY_SOURCES),
                        "Saved profiles and lists, or the policy file read-only",
                    )
                    .by_default(Fallback::Of(policy_source))
                    .env(&["SINGULARR_POLICY_SOURCE"])
                    .saves(),
                    Field::new("system.webDir", "Web client folder", Kind::Path, "")
                        .by_default(Fallback::Text("/app/web"))
                        .env(&["SINGULARR_WEB_DIR"])
                        .restart(),
                ],
            },
        ],
    },
];
