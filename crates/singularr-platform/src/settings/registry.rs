//! The registry's types: a setting is one [`Field`] in a [`Group`] of a [`Section`]. The app
//! holds the list (its configuration's [`Described::sections`](super::Described::sections)).
//! Each entry says its type, its default, the variables and the instance file key that fix it,
//! how its value in force is read from the configuration, and whether it can be saved here.
//!
//! A field that `saves` can be changed in the interface (`PATCH /api/v1/settings`) while no
//! variable and no instance file key fixes it. Every other field is a read-only view of the
//! configuration with its source. With them the Settings page shows the whole configuration.
//! `C` is the configuration the fields read.

use super::Described;
use serde_json::Value;
use std::fmt;

/// One choice of a `select` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub value: &'static str,
    pub label: &'static str,
}

pub const fn choice(value: &'static str, label: &'static str) -> Choice {
    Choice { value, label }
}

/// A field's type, as the settings contract names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// Never sent: the answer says whether it is set.
    Secret,
    Number {
        min: i64,
        max: i64,
        unit: Option<&'static str>,
    },
    Bool,
    Select(&'static [Choice]),
    /// Seconds.
    Duration {
        min: u64,
    },
    /// An absolute `http://` or `https://` address; with `relative`, a path on this site too.
    Url {
        relative: bool,
    },
    Path,
    /// A list of strings.
    List,
    Timezone,
    Language,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::Secret => "secret",
            Kind::Number { .. } => "number",
            Kind::Bool => "bool",
            Kind::Select(_) => "select",
            Kind::Duration { .. } => "duration",
            Kind::Url { .. } => "url",
            Kind::Path => "path",
            Kind::List => "list",
            Kind::Timezone => "timezone",
            Kind::Language => "language",
        }
    }
}

/// A field's default when nothing fixes or saves its value.
pub enum Fallback<C> {
    None,
    Text(&'static str),
    Int(i64),
    Bool(bool),
    List(&'static [&'static str]),
    /// The process's time zone (`TZ`), else UTC.
    TimeZone,
    /// A default the configuration gives: the app's own, read from it.
    Of(fn(&C) -> Value),
}

impl<C> Clone for Fallback<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for Fallback<C> {}

impl<C> fmt::Debug for Fallback<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fallback::None => write!(f, "None"),
            Fallback::Text(t) => write!(f, "Text({t:?})"),
            Fallback::Int(n) => write!(f, "Int({n})"),
            Fallback::Bool(b) => write!(f, "Bool({b})"),
            Fallback::List(items) => write!(f, "List({items:?})"),
            Fallback::TimeZone => write!(f, "TimeZone"),
            Fallback::Of(_) => write!(f, "Of(..)"),
        }
    }
}

/// How the value in force is read from the configuration, after its defaults and floors.
pub type Read<C> = fn(&C) -> Value;

/// A check of a text value beyond its type: it returns the value as stored, or why not.
pub type Check = fn(&str) -> Result<String, String>;

pub struct Field<C> {
    /// `<section>.<name>`, camel case after the dot.
    pub key: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    pub default: Fallback<C>,
    /// The variables that fix the value; the first one set wins.
    pub env: &'static [&'static str],
    /// The instance file's key that fixes the value: `workspace.name`.
    pub file: Option<&'static str>,
    /// The value in force as the configuration has it. `None`: the value is the variable's own
    /// text, or what is saved, or the default.
    pub read: Option<Read<C>>,
    pub check: Option<Check>,
    /// A person's own value, with the installation's value as the default for everyone.
    pub user: bool,
    /// The value can be saved here when nothing fixes it.
    pub saves: bool,
    /// A change takes effect at the next start.
    pub restart: bool,
    /// Shown only to callers that hold the `settings` scope.
    pub private: bool,
}

impl<C> Clone for Field<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C> Copy for Field<C> {}

impl<C> Field<C> {
    pub const fn new(
        key: &'static str,
        label: &'static str,
        kind: Kind,
        help: &'static str,
    ) -> Field<C> {
        Field {
            key,
            label,
            help,
            kind,
            default: Fallback::None,
            env: &[],
            file: None,
            read: None,
            check: None,
            user: false,
            saves: false,
            restart: false,
            private: false,
        }
    }

    pub const fn by_default(mut self, default: Fallback<C>) -> Field<C> {
        self.default = default;
        self
    }

    pub const fn env(mut self, vars: &'static [&'static str]) -> Field<C> {
        self.env = vars;
        self
    }

    pub const fn file(mut self, key: &'static str) -> Field<C> {
        self.file = Some(key);
        self
    }

    pub const fn read(mut self, read: Read<C>) -> Field<C> {
        self.read = Some(read);
        self
    }

    pub const fn check(mut self, check: Check) -> Field<C> {
        self.check = Some(check);
        self
    }

    pub const fn saves(mut self) -> Field<C> {
        self.saves = true;
        self
    }

    pub const fn per_user(mut self) -> Field<C> {
        self.user = true;
        self
    }

    pub const fn restart(mut self) -> Field<C> {
        self.restart = true;
        self
    }

    pub const fn private(mut self) -> Field<C> {
        self.private = true;
        self
    }

    pub fn is_secret(&self) -> bool {
        self.kind == Kind::Secret
    }
}

pub struct Group<C: 'static> {
    pub id: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub fields: &'static [Field<C>],
}

impl<C: 'static> Clone for Group<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C: 'static> Copy for Group<C> {}

pub struct Section<C: 'static> {
    pub id: &'static str,
    pub label: &'static str,
    pub summary: &'static str,
    pub groups: &'static [Group<C>],
    /// Collections with routes of their own, drawn by the page: `server`, `integrations`,
    /// `libraries`, `people`, `roles`, `channels`, `keys`, `jobs`, `cache`, `services`, `about`.
    pub panels: &'static [&'static str],
}

impl<C: 'static> Clone for Section<C> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<C: 'static> Copy for Section<C> {}

/// Every field of a configuration, in the order of the sections and groups.
pub fn fields<C: Described>() -> impl Iterator<Item = &'static Field<C>> {
    C::sections()
        .iter()
        .flat_map(|s| s.groups.iter())
        .flat_map(|g| g.fields.iter())
}

pub fn field<C: Described>(key: &str) -> Option<&'static Field<C>> {
    fields::<C>().find(|f| f.key == key)
}
