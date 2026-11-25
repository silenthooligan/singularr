//! What a connector type is, stated once: its name, its kinds and group, what it reads, the
//! variables it is configured by, its secret, its actions, the passes that read it, the words
//! of what they count and how it is called. Every list of types in the app reads from these.

/// A role a connector type fills. Plex fills two: a media server and a player.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    MediaServer,
    Player,
    DownloadClient,
    Indexer,
    Manager,
    Processor,
    RequestSource,
    PlayHistory,
    Metadata,
    Subtitles,
    Sink,
}

/// Where Settings lists a type, in the page's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Server,
    Managers,
    Downloads,
    Indexers,
    Processing,
    Requests,
    Metadata,
}

impl Group {
    /// Every group, in the order Settings lists them.
    pub const ALL: [Group; 7] = [
        Group::Server,
        Group::Managers,
        Group::Downloads,
        Group::Indexers,
        Group::Processing,
        Group::Requests,
        Group::Metadata,
    ];

    /// The group's id: `server`, `downloads`.
    pub fn id(self) -> &'static str {
        match self {
            Group::Server => "server",
            Group::Managers => "managers",
            Group::Downloads => "downloads",
            Group::Indexers => "indexers",
            Group::Processing => "processing",
            Group::Requests => "requests",
            Group::Metadata => "metadata",
        }
    }

    /// The group's heading.
    pub fn label(self) -> &'static str {
        match self {
            Group::Server => "Media server",
            Group::Managers => "Managers",
            Group::Downloads => "Download clients",
            Group::Indexers => "Indexers",
            Group::Processing => "Processing",
            Group::Requests => "Request sources",
            Group::Metadata => "Metadata providers",
        }
    }

    /// The line under the heading.
    pub fn description(self) -> &'static str {
        match self {
            Group::Server => "Libraries, sessions and music",
            Group::Managers => "Movie and show managers",
            Group::Downloads => "Release downloads",
            Group::Indexers => "Release search",
            Group::Processing => "Flows for imported files",
            Group::Requests => "Requests made outside the portal",
            Group::Metadata => "Titles, people, artwork, ratings and episode order",
        }
    }
}

/// How well a type is known to work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Maturity {
    Stable,
    /// Written from the service's API reference and never run against the service itself.
    Untested,
}

/// A type's secret: its field, its label and the end of its variable's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SecretSpec {
    /// The field key: `apiKey`, `token`, `password`.
    pub key: &'static str,
    /// The field label: `API key`, `Token`, `Password`.
    pub label: &'static str,
    /// The end of the variable's name: `API_KEY` gives `RADARR_API_KEY`.
    pub env_suffix: &'static str,
    /// An address alone is a usable integration: the service can run without a secret.
    pub optional: bool,
    /// The field's help in place of the default one.
    pub help: Option<&'static str>,
}

/// One thing Singularr can do to a service beyond reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
}

/// How often one pass of a type runs: the field Settings shows and the setting that holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassSpec {
    /// The field key: `syncEvery`, `queueEvery`.
    pub field: &'static str,
    pub label: &'static str,
    /// The setting's key: `jobs.titles`.
    pub setting: &'static str,
}

/// A number a type's passes record, with its words for one and for many: `titles`, `title`,
/// `titles`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountWords {
    pub key: &'static str,
    pub one: &'static str,
    pub many: &'static str,
}

/// A connector type's facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Manifest {
    /// The type's id: `qbittorrent`.
    pub type_id: &'static str,
    /// The product's name, an integration's name when it is given none: `Plex`.
    pub name: &'static str,
    /// The type as Settings lists it: `Plex Media Server`.
    pub label: &'static str,
    /// The roles it fills, the main one first.
    pub kinds: &'static [Kind],
    pub group: Group,
    /// What an integration of the type does unless its entry says otherwise: `movies`.
    pub role: &'static str,
    pub description: &'static str,
    /// What is read from it, in the panel's words. Empty: the type is listed and not read.
    pub reads: &'static [&'static str],
    /// The variable prefixes of the environment form, the product's own name first.
    pub env_prefixes: &'static [&'static str],
    pub secret: SecretSpec,
    /// It signs in with a user name beside its secret.
    pub takes_user: bool,
    /// Its items open in its own web app, at an address of their own when one is set.
    pub web_url: bool,
    /// The address it answers at unless it is told otherwise: `http://qbittorrent:8080`.
    pub default_address: &'static str,
    /// How many integrations of the type are read. `None`: any number.
    pub max_instances: Option<u8>,
    pub actions: &'static [Action],
    /// Its passes, the main one first.
    pub passes: &'static [PassSpec],
    /// What its passes count, in the order its line names them.
    pub counts: &'static [CountWords],
    pub maturity: Maturity,
    /// How it is called: answer size, calls at once and per period, reads sent again, pauses.
    pub http: crate::http::HttpProfile,
}

impl Manifest {
    /// The type is read: it lists what it reads.
    pub fn read(&self) -> bool {
        !self.reads.is_empty()
    }

    /// It fills this role.
    pub fn fills(&self, kind: Kind) -> bool {
        self.kinds.contains(&kind)
    }
}
