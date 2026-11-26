//! Settings: everything that configures Singularr, described as data ([`registry`] holds the
//! types of the fields; the app holds the fields), each value with where it comes from, and the
//! values saved in the interface ([`store`]). The configuration the fields read is the app's
//! ([`Described`]).
//!
//! A value in force comes from the environment, the instance file, a save in the interface, or
//! the default, in that order: a variable that is set wins, then the instance file's key, then a
//! saved value, then the default. A value the environment or the file fixes is read-only here and
//! names the variable or the file. Saved values are kept in memory and read from the database at
//! start ([`Settings::reload`]); a save takes effect at once wherever the value is read through
//! this module ([`Settings::value`], [`Settings::text`], the metadata [`Locale`]).

pub mod model;
pub mod registry;
pub mod store;

use crate::auth::Caller;
use crate::config::{Env, PLACEHOLDER};
use crate::journal::{Journal, event};
use crate::model::Scope;
use chrono::{DateTime, Utc};
use model::{
    FieldView, GroupView, InstanceValue, OptionView, SectionView, SettingsPatch, SettingsSaved,
    SettingsView,
};
use registry::{Fallback, Field, Kind};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, PoisonError, RwLock};

/// The scope of the installation's own values.
pub const INSTANCE: &str = "instance";

/// What the settings read of the configuration they describe: its sections of fields and the
/// instance file it was read with. The app implements it for its configuration.
///
/// The mechanism also reads four names of its own. `general.language` and `general.region`
/// give the locale the metadata providers are asked in: en-US and US without them. The
/// instance file's `[workspace]` table holds the file keys that fix fields (`workspace.name`).
/// `TZ` is the default of a time zone field: UTC without it.
pub trait Described: Clone + Send + Sync + 'static {
    /// Every section of fields, in the order the Settings page shows them.
    fn sections() -> &'static [registry::Section<Self>];
    /// The instance file, when the configuration was read with one.
    fn instance_path(&self) -> Option<&str>;
}

/// The language and the region the metadata providers are asked in. The metadata service holds
/// a clone, so a save of `general.language` or `general.region` reaches it at once.
#[derive(Clone)]
pub struct Locale(Arc<RwLock<LocaleValue>>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocaleValue {
    /// `en-US`.
    pub language: String,
    /// `US`.
    pub region: String,
}

impl Locale {
    pub fn new(language: &str, region: &str) -> Locale {
        Locale(Arc::new(RwLock::new(LocaleValue {
            language: language.to_string(),
            region: region.to_string(),
        })))
    }

    pub fn get(&self) -> LocaleValue {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn language(&self) -> String {
        self.get().language
    }

    pub fn region(&self) -> String {
        self.get().region
    }

    fn set(&self, value: LocaleValue) {
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = value;
    }
}

impl Default for Locale {
    fn default() -> Self {
        Locale::new("en-US", "US")
    }
}

/// The scope of one person's own values.
pub fn user_scope(person: &str) -> String {
    format!("user:{person}")
}

/// The person behind a caller, for their own values: the owner's address. A key is a program
/// and has no own values.
pub fn person(caller: &Caller) -> Option<String> {
    match caller {
        Caller::Owner { email } => Some(email.clone()),
        Caller::Key { .. } => None,
    }
}

/// A set variable: trimmed, not empty and not the placeholder, as the configuration reads it.
fn set(env: &dyn Env, name: &str) -> Option<String> {
    env.get(name)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != PLACEHOLDER)
}

/// Where one field's value in force comes from, for the installation.
#[derive(Debug, Clone, PartialEq)]
struct Resolved {
    value: Value,
    source: &'static str,
    env: Option<String>,
    file: Option<String>,
    /// The environment or the instance file fixes it.
    fixed: bool,
    /// A secret: whether it is set.
    set: Option<bool>,
}

/// Why a save was not made.
#[derive(Debug, thiserror::Error)]
pub enum PatchError {
    /// The caller may not save this.
    #[error("{0}")]
    Forbidden(String),
    /// Nothing was stored: why, by key.
    #[error("the save was refused")]
    Refused(BTreeMap<String, String>),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// One value that changes in a save.
struct Change<C: 'static> {
    field: &'static Field<C>,
    /// Where it is stored: `instance` or `user:<who>`.
    scope: String,
    /// The caller's own value.
    own: bool,
    new: Option<Value>,
    from: Value,
    to: Value,
}

/// The settings of this process: the configuration it started with, what the environment and
/// the instance file fix, and the values saved in the interface.
pub struct Settings<C: Described> {
    config: C,
    /// The registry's variables that are set, with their text. A secret's text is not kept.
    env: BTreeMap<String, Option<String>>,
    /// The instance file's keys that hold a value: `workspace.name`.
    file_keys: BTreeSet<String>,
    /// The process's time zone: the default of `general.timeZone`.
    zone: String,
    saved: RwLock<BTreeMap<(String, String), Value>>,
    /// One save at a time, so each change's `from` is the value it replaced.
    save_lock: tokio::sync::Mutex<()>,
    locale: Locale,
    started: DateTime<Utc>,
}

impl<C: Described> Settings<C> {
    /// The settings of this configuration, the environment it was read from and the instance
    /// file's text. Nothing saved is known until [`Settings::reload`].
    pub fn new(config: &C, env: &dyn Env, instance: Option<&str>) -> Settings<C> {
        let mut vars = BTreeMap::new();
        for f in registry::fields::<C>() {
            for var in f.env {
                if let Some(text) = set(env, var) {
                    vars.insert(var.to_string(), (!f.is_secret()).then_some(text));
                }
            }
        }
        let mut file_keys = BTreeSet::new();
        if let Some(Ok(table)) = instance.map(toml::from_str::<toml::Table>)
            && let Some(toml::Value::Table(workspace)) = table.get("workspace")
        {
            for (key, value) in workspace {
                if value.as_str().is_some_and(|s| !s.trim().is_empty()) {
                    file_keys.insert(format!("workspace.{key}"));
                }
            }
        }
        let zone = set(env, "TZ")
            .filter(|z| zone_name(z))
            .unwrap_or_else(|| "UTC".to_string());
        let settings = Settings {
            config: config.clone(),
            env: vars,
            file_keys,
            zone,
            saved: RwLock::new(BTreeMap::new()),
            save_lock: tokio::sync::Mutex::new(()),
            locale: Locale::default(),
            started: Utc::now(),
        };
        settings.apply_locale();
        settings
    }

    /// Read the saved values from the database. Values of keys the registry no longer holds,
    /// or can no longer save, stay stored and are not used.
    pub async fn reload(&self, pool: &PgPool) -> Result<usize, sqlx::Error> {
        let rows = store::all(pool).await?;
        let saved: BTreeMap<(String, String), Value> = rows
            .into_iter()
            .filter(|r| registry::field::<C>(&r.key).is_some_and(|f| f.saves))
            .map(|r| ((r.scope, r.key), r.value))
            .collect();
        let n = saved.len();
        *self.saved.write().unwrap_or_else(PoisonError::into_inner) = saved;
        self.apply_locale();
        Ok(n)
    }

    /// Hold a value in memory as [`Settings::reload`] holds a saved row: a key the registry
    /// cannot save is left out. A saved language or region is put in force. Nothing is checked,
    /// written or journaled. The app's tests of its fields use it.
    #[cfg(feature = "testkit")]
    pub fn remember(&self, scope: &str, key: &str, value: Value) {
        if !registry::field::<C>(key).is_some_and(|f| f.saves) {
            return;
        }
        self.saved
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((scope.to_string(), key.to_string()), value);
        self.apply_locale();
    }

    /// The configuration the process started with.
    pub fn config(&self) -> &C {
        &self.config
    }

    /// When this process started.
    pub fn started(&self) -> DateTime<Utc> {
        self.started
    }

    /// The language and region handle the metadata service reads.
    pub fn locale(&self) -> Locale {
        self.locale.clone()
    }

    /// The installation's value in force of a setting: `value("general.name")`. `null` for a
    /// secret, a key the registry does not hold, or a setting without a value.
    pub fn value(&self, key: &str) -> Value {
        registry::field::<C>(key)
            .map(|f| self.instance(f).value)
            .unwrap_or(Value::Null)
    }

    /// [`Settings::value`] as text, when it is text that is not empty.
    pub fn text(&self, key: &str) -> Option<String> {
        match self.value(key) {
            Value::String(s) if !s.trim().is_empty() => Some(s),
            _ => None,
        }
    }

    /// A caller's value of a setting: their own for a setting each person sets for themselves,
    /// else the installation's.
    pub fn value_for(&self, caller: &Caller, key: &str) -> Value {
        let Some(f) = registry::field::<C>(key) else {
            return Value::Null;
        };
        person(caller)
            .filter(|_| f.user)
            .and_then(|p| self.saved_value(&user_scope(&p), f.key))
            .unwrap_or_else(|| self.instance(f).value)
    }

    /// The variable that fixes a setting, when one does: `env_of("system.policyFile")`.
    pub fn env_of(&self, key: &str) -> Option<String> {
        registry::field::<C>(key).and_then(|f| self.instance(f).env)
    }

    /// Where the installation's value of a setting comes from: `environment`, `file`, `saved` or
    /// `default`. `None` for a key the registry does not hold.
    pub fn source_of(&self, key: &str) -> Option<&'static str> {
        registry::field::<C>(key).map(|f| self.instance(f).source)
    }

    fn saved_value(&self, scope: &str, key: &str) -> Option<Value> {
        self.saved
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(scope.to_string(), key.to_string()))
            .cloned()
    }

    /// A field's default when nothing fixes or saves its value.
    pub fn fallback(&self, default: Fallback<C>) -> Value {
        match default {
            Fallback::None => Value::Null,
            Fallback::Text(t) => json!(t),
            Fallback::Int(n) => json!(n),
            Fallback::Bool(b) => json!(b),
            Fallback::List(items) => json!(items),
            Fallback::TimeZone => json!(self.zone),
            Fallback::Of(read) => read(&self.config),
        }
    }

    /// Where the installation's value of a field comes from, and what it is.
    fn instance(&self, f: &Field<C>) -> Resolved {
        let set = f
            .is_secret()
            .then(|| f.env.iter().any(|v| self.env.contains_key(*v)));
        if let Some(var) = f.env.iter().find(|v| self.env.contains_key(**v)) {
            let value = if f.is_secret() {
                Value::Null
            } else {
                match f.read {
                    Some(read) => read(&self.config),
                    None => self
                        .env
                        .get(*var)
                        .cloned()
                        .flatten()
                        .map(Value::String)
                        .unwrap_or(Value::Null),
                }
            };
            return Resolved {
                value,
                source: "environment",
                env: Some(var.to_string()),
                file: None,
                fixed: true,
                set,
            };
        }
        if let Some(key) = f.file
            && self.file_keys.contains(key)
        {
            return Resolved {
                value: f.read.map(|read| read(&self.config)).unwrap_or(Value::Null),
                source: "file",
                env: None,
                file: self.config.instance_path().map(str::to_string),
                fixed: true,
                set,
            };
        }
        if f.saves
            && let Some(value) = self.saved_value(INSTANCE, f.key)
        {
            return Resolved {
                value,
                source: "saved",
                env: None,
                file: None,
                fixed: false,
                set,
            };
        }
        let value = if f.is_secret() {
            Value::Null
        } else if f.saves {
            self.fallback(f.default)
        } else {
            f.read
                .map(|read| read(&self.config))
                .unwrap_or_else(|| self.fallback(f.default))
        };
        Resolved {
            value,
            source: "default",
            env: None,
            file: None,
            fixed: false,
            set,
        }
    }

    fn apply_locale(&self) {
        self.locale.set(LocaleValue {
            language: self
                .text("general.language")
                .unwrap_or_else(|| "en-US".to_string()),
            region: self
                .text("general.region")
                .unwrap_or_else(|| "US".to_string()),
        });
    }

    /// The whole schema with the values a caller sees. The values of private fields are
    /// withheld from callers without the `settings` scope; secrets are never sent.
    pub fn view(&self, caller: &Caller) -> SettingsView {
        let who = person(caller);
        let may_save = caller.has(Scope::Settings);
        SettingsView {
            sections: C::sections()
                .iter()
                .map(|s| SectionView {
                    id: s.id.to_string(),
                    label: s.label.to_string(),
                    summary: s.summary.to_string(),
                    panels: s.panels.iter().map(|p| p.to_string()).collect(),
                    groups: s
                        .groups
                        .iter()
                        .map(|g| GroupView {
                            id: g.id.to_string(),
                            label: g.label.to_string(),
                            help: g.help.to_string(),
                            fields: g
                                .fields
                                .iter()
                                .map(|f| self.field_view(f, who.as_deref(), may_save))
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    fn field_view(&self, f: &Field<C>, who: Option<&str>, may_save: bool) -> FieldView {
        let base = self.instance(f);
        let instance_editable = f.saves && !base.fixed && may_save;
        let (value, source, editable, instance) = if f.user {
            let inst = InstanceValue {
                value: base.value.clone(),
                source: base.source.to_string(),
                editable: instance_editable,
            };
            let mine = f.saves && who.is_some();
            match who.and_then(|p| self.saved_value(&user_scope(p), f.key)) {
                Some(own) => (own, "saved", mine, Some(inst)),
                None => (base.value.clone(), base.source, mine, Some(inst)),
            }
        } else {
            (base.value.clone(), base.source, instance_editable, None)
        };
        let withheld = f.private && !may_save;
        let (min, max, step, unit) = match f.kind {
            Kind::Number { min, max, unit } => (Some(min), Some(max), Some(1), unit),
            Kind::Duration { min } => (Some(min as i64), None, Some(1), None),
            _ => (None, None, None, None),
        };
        FieldView {
            key: f.key.to_string(),
            label: f.label.to_string(),
            kind: f.kind.name().to_string(),
            value: (!f.is_secret()).then_some(if withheld { Value::Null } else { value }),
            default: (!f.is_secret()).then(|| self.fallback(f.default)),
            set: base.set,
            source: source.to_string(),
            env: base.env,
            file: base.file,
            editable,
            restart: f.restart,
            scope: if f.user { "user" } else { INSTANCE }.to_string(),
            help: f.help.to_string(),
            withheld,
            options: match f.kind {
                Kind::Select(choices) => Some(
                    choices
                        .iter()
                        .map(|c| OptionView {
                            value: c.value.to_string(),
                            label: c.label.to_string(),
                        })
                        .collect(),
                ),
                _ => None,
            },
            min,
            max,
            step,
            unit: unit.map(str::to_string),
            instance,
            part: None,
            placeholder: None,
        }
    }

    /// One field as a caller sees it, for a panel that shows it: `field("jobs.titles", &caller)`.
    pub fn field(&self, key: &str, caller: &Caller) -> Option<FieldView> {
        let f = registry::field::<C>(key)?;
        Some(self.field_view(f, person(caller).as_deref(), caller.has(Scope::Settings)))
    }

    /// Why a field cannot be saved by anyone.
    fn not_saved(&self, f: &Field<C>) -> String {
        let r = self.instance(f);
        match (r.source, &r.env, &r.file) {
            ("environment", Some(var), _) => format!("Set by {var} in the environment"),
            ("file", _, Some(path)) => format!("Set by the instance file {path}"),
            _ if !f.env.is_empty() => format!("Set with {} in the environment", f.env[0]),
            _ => "Read-only".to_string(),
        }
    }

    /// Validate and save values. All or nothing: one value that is refused stores none. Every
    /// change is a `setting_changed` event in the journal; a value sent as it already is
    /// changes nothing and is listed as unchanged.
    ///
    /// Each value is saved where it belongs: a field each person sets for themselves is the
    /// caller's own when the caller is a person, and the installation's otherwise; every other
    /// field is the installation's. `scope: user` asks for the caller's own values only,
    /// `scope: instance` for the installation's (a person's field then sets the default for
    /// everyone). The installation's values need the `settings` scope.
    pub async fn patch(
        &self,
        pool: &PgPool,
        journal: &Journal,
        caller: &Caller,
        patch: &SettingsPatch,
    ) -> Result<SettingsSaved, PatchError> {
        let asked = match patch.scope.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(INSTANCE) => Some(false),
            Some("user") => Some(true),
            Some(other) => {
                return Err(PatchError::Refused(BTreeMap::from([(
                    "scope".to_string(),
                    format!("Either instance or user, not `{other}`"),
                )])));
            }
        };
        let who = person(caller);
        if asked == Some(true) && who.is_none() {
            return Err(PatchError::Forbidden(
                "An API key has no personal settings.".to_string(),
            ));
        }
        if patch.values.is_empty() {
            return Err(PatchError::Refused(BTreeMap::from([(
                "values".to_string(),
                "At least one setting required".to_string(),
            )])));
        }
        let mut errors = BTreeMap::new();
        // Each value with whether it is the caller's own.
        let mut planned: Vec<(&'static Field<C>, bool, Option<Value>)> = Vec::new();
        for (key, raw) in &patch.values {
            let Some(f) = registry::field::<C>(key) else {
                errors.insert(key.clone(), "Unknown setting".to_string());
                continue;
            };
            if asked == Some(true) && !f.user {
                errors.insert(key.clone(), "Not a personal setting".to_string());
                continue;
            }
            let own = f.user && who.is_some() && asked != Some(false);
            if !f.saves || (!own && self.instance(f).fixed) {
                errors.insert(key.clone(), self.not_saved(f));
                continue;
            }
            let value = if raw.is_null() {
                None
            } else {
                match validate(f, raw) {
                    Ok(v) => Some(v),
                    Err(why) => {
                        errors.insert(key.clone(), why);
                        continue;
                    }
                }
            };
            planned.push((f, own, value));
        }
        if !errors.is_empty() {
            return Err(PatchError::Refused(errors));
        }
        if planned.iter().any(|(_, own, _)| !own) && !caller.has(Scope::Settings) {
            return Err(PatchError::Forbidden(
                "Saving these settings needs the `settings` scope.".to_string(),
            ));
        }

        let _one_at_a_time = self.save_lock.lock().await;
        let own_scope = who.as_deref().map(user_scope);
        let mut changes = Vec::new();
        let mut unchanged = Vec::new();
        for (f, own, new) in planned {
            let scope = match (own, &own_scope) {
                (true, Some(mine)) => mine.clone(),
                _ => INSTANCE.to_string(),
            };
            if self.saved_value(&scope, f.key) == new {
                unchanged.push(f.key.to_string());
                continue;
            }
            let from = if own {
                self.value_for(caller, f.key)
            } else {
                self.instance(f).value
            };
            // Without a saved value a person's own falls back to the installation's, and the
            // installation's (nothing fixes it here) to the default.
            let to = match &new {
                Some(v) => v.clone(),
                None if own => self.instance(f).value,
                None => self.fallback(f.default),
            };
            changes.push(Change {
                field: f,
                scope,
                own,
                new,
                from,
                to,
            });
        }
        if !changes.is_empty() {
            let actor = caller.actor();
            let mut tx = journal.begin(pool).await?;
            let mut events = Vec::new();
            for c in &changes {
                match &c.new {
                    Some(v) => store::put(tx.conn(), &c.scope, c.field.key, v, &actor).await?,
                    None => store::remove(tx.conn(), &c.scope, c.field.key).await?,
                }
                events.push(event(
                    "setting_changed",
                    "setting",
                    c.field.key,
                    &actor,
                    json!({
                        "key": c.field.key,
                        "label": c.field.label,
                        "scope": if c.own { "user" } else { INSTANCE },
                        "from": c.from,
                        "to": c.to,
                        "reset": c.new.is_none(),
                        "restart": c.field.restart,
                    }),
                ));
            }
            tx.append(events).await?;
            tx.commit().await?;
            {
                let mut saved = self.saved.write().unwrap_or_else(PoisonError::into_inner);
                for c in &changes {
                    let key = (c.scope.clone(), c.field.key.to_string());
                    match &c.new {
                        Some(v) => {
                            saved.insert(key, v.clone());
                        }
                        None => {
                            saved.remove(&key);
                        }
                    }
                }
            }
            self.apply_locale();
        }
        Ok(SettingsSaved {
            restart: changes.iter().any(|c| c.field.restart),
            saved: changes.iter().map(|c| c.field.key.to_string()).collect(),
            unchanged,
        })
    }
}

/// Text that is not empty, at most `max` characters and without control characters.
fn plain(text: &str, max: usize) -> Result<(), String> {
    if text.is_empty() {
        return Err("Value required".to_string());
    }
    if text.chars().count() > max {
        return Err(format!("At most {max} characters"));
    }
    if text.chars().any(char::is_control) {
        return Err("No line breaks or control characters".to_string());
    }
    Ok(())
}

/// An absolute `http://` or `https://` address without its trailing `/`, or with `relative` a
/// path on this site (`/oauth2/sign_out`).
fn address(text: &str, relative: bool) -> Result<String, String> {
    plain(text, 500)?;
    if text.chars().any(char::is_whitespace) {
        return Err("No spaces in an address".to_string());
    }
    if relative && text.starts_with('/') && !text.starts_with("//") {
        return Ok(text.to_string());
    }
    let rest = text
        .strip_prefix("https://")
        .or_else(|| text.strip_prefix("http://"));
    match rest {
        Some(r) if !r.is_empty() && !r.starts_with('/') && !r.contains('@') => {
            Ok(text.trim_end_matches('/').to_string())
        }
        _ if relative => {
            Err("Address starting with https:// or http://, or a path starting with /".to_string())
        }
        _ => Err("Address starting with https:// or http://".to_string()),
    }
}

/// A time zone name: `UTC`, or an area and a place such as `Europe/Berlin` or
/// `America/Argentina/Buenos_Aires`.
fn zone_name(text: &str) -> bool {
    if text == "UTC" || text == "GMT" {
        return true;
    }
    let parts: Vec<&str> = text.split('/').collect();
    text.len() <= 64
        && (2..=3).contains(&parts.len())
        && parts[0].starts_with(|c: char| c.is_ascii_uppercase())
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'))
        })
}

/// A language tag as the metadata providers take it: `en`, `en-US`, `pt-BR`, `es-419`.
fn language(text: &str) -> Result<String, String> {
    let bad = || "Language tag such as en-US, de-DE or pt-BR".to_string();
    let (lang, area) = match text.split_once('-') {
        Some((l, a)) => (l, Some(a)),
        None => (text, None),
    };
    if !(2..=3).contains(&lang.len()) || !lang.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(bad());
    }
    let lang = lang.to_ascii_lowercase();
    match area {
        None => Ok(lang),
        Some(a) if a.len() == 2 && a.chars().all(|c| c.is_ascii_alphabetic()) => {
            Ok(format!("{lang}-{}", a.to_ascii_uppercase()))
        }
        Some(a) if a.len() == 3 && a.chars().all(|c| c.is_ascii_digit()) => {
            Ok(format!("{lang}-{a}"))
        }
        Some(_) => Err(bad()),
    }
}

/// A value checked against its field's type, as it is stored.
pub fn validate<C>(f: &Field<C>, raw: &Value) -> Result<Value, String> {
    let text = || {
        raw.as_str()
            .map(str::trim)
            .ok_or_else(|| "Text expected".to_string())
    };
    let value = match f.kind {
        Kind::Text => {
            let t = text()?;
            plain(t, 120)?;
            json!(t)
        }
        Kind::Url { relative } => json!(address(text()?, relative)?),
        Kind::Path => {
            let t = text()?;
            plain(t, 500)?;
            if !t.starts_with('/') {
                return Err("Absolute path starting with /".to_string());
            }
            json!(t)
        }
        Kind::Select(choices) => {
            let t = text()?;
            match choices.iter().find(|c| c.value == t) {
                Some(c) => json!(c.value),
                None => {
                    return Err(format!(
                        "One of: {}",
                        choices
                            .iter()
                            .map(|c| c.value)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
            }
        }
        Kind::Bool => match raw.as_bool() {
            Some(b) => json!(b),
            None => return Err("true or false expected".to_string()),
        },
        Kind::Number { min, max, .. } => match raw.as_i64() {
            Some(n) if (min..=max).contains(&n) => json!(n),
            _ => return Err(format!("Whole number from {min} to {max}")),
        },
        Kind::Duration { min } => match raw.as_u64() {
            Some(n) if n >= min => json!(n),
            _ => return Err(format!("Whole seconds, at least {min}")),
        },
        Kind::List => {
            let Some(items) = raw.as_array() else {
                return Err("List of text expected".to_string());
            };
            let mut out = Vec::new();
            for item in items {
                let Some(t) = item.as_str().map(str::trim) else {
                    return Err("List of text expected".to_string());
                };
                plain(t, 500)?;
                out.push(t.to_string());
            }
            json!(out)
        }
        Kind::Timezone => {
            let t = text()?;
            if !zone_name(t) {
                return Err("Time zone name such as Europe/Berlin or UTC".to_string());
            }
            json!(t)
        }
        Kind::Language => json!(language(text()?)?),
        Kind::Secret => {
            return Err("Set in the environment".to_string());
        }
    };
    match (f.check, value.as_str()) {
        (Some(check), Some(s)) => check(s).map(Value::String),
        _ => Ok(value),
    }
}
