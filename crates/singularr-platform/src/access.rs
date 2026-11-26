//! What Singularr may do with each integration: read it, or read it and do some of its actions.
//!
//! The access is a permission of each action of an integration, with sources like any setting:
//! the default (`read`), the environment or the instance file (which fix it), or what the owner
//! saved in the interface (kept in `integration_access`, a row per action). A type can act only
//! when its adapter declares actions. For every other type the access is `read` and nothing
//! changes that. An action a type declares in a later version starts at `read` unless the
//! environment or the file says `act`.
//!
//! The way out (`outbound`) enforces the access. The book holds the same switches as the host
//! rules there. A change is in force with the next call. The app hands the book its integrations
//! ([`Listed`]): their ids and names, the actions their types declare and what the
//! configuration fixes.

use super::integrations as store;
use super::journal::{Journal, event};
use super::outbound::{Access, AccessSwitch, Allowlist, Grant};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock};

/// One thing Singularr can do to a service beyond reading it: the SDK's, at its old path.
pub use singularr_sdk::manifest::Action;

/// One thing Singularr can do to an integration beyond reading it: `{id, label, access}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ActionView {
    /// `send`.
    pub id: String,
    /// `Send a file to a flow`.
    pub label: String,
    /// What Singularr may do with this action: `read` (it is not done) or `act`.
    pub access: String,
}

/// A refused act, as a problem's sentence: `Remove a download is read-only on Radarr.`
pub fn read_only_words(label: &str, name: &str) -> String {
    format!("{label} is read-only on {name}.")
}

/// What Singularr may do with one integration, and where that comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessState {
    /// `act` while one action at least acts.
    pub access: Access,
    /// `default`, `environment`, `file` or `saved`: the words of a setting's source.
    pub source: &'static str,
    /// With `environment`: the variable that fixes it.
    pub env: Option<String>,
    /// With `file`: the instance file that fixes it.
    pub file: Option<String>,
    pub actions: &'static [Action],
    /// The actions that act, by id.
    pub acting: Vec<&'static str>,
}

impl AccessState {
    /// Reading alone, by default: what holds for anything that has no action.
    pub fn read_only() -> AccessState {
        AccessState {
            access: Access::Read,
            source: "default",
            env: None,
            file: None,
            actions: &[],
            acting: Vec::new(),
        }
    }

    /// The environment or the instance file fixes it.
    pub fn fixed(&self) -> bool {
        matches!(self.source, "environment" | "file")
    }

    /// It can be changed in the interface: the type has an action and nothing fixes it.
    pub fn editable(&self) -> bool {
        !self.actions.is_empty() && !self.fixed()
    }

    /// Whether the act `action` may be done.
    pub fn allows(&self, action: &str) -> bool {
        self.acting.contains(&action)
    }

    /// The label of an action by its id. The id itself for an action the type does not declare.
    pub fn label<'a>(&self, action: &'a str) -> &'a str {
        self.actions
            .iter()
            .find(|a| a.id == action)
            .map(|a| a.label)
            .unwrap_or(action)
    }

    /// The access of one action.
    pub fn of(&self, action: &str) -> Access {
        if self.allows(action) {
            Access::Act
        } else {
            Access::Read
        }
    }

    /// The actions, as the answers of the API list them, each with its access.
    pub fn action_views(&self) -> Vec<ActionView> {
        self.actions
            .iter()
            .map(|a| ActionView {
                id: a.id.to_string(),
                label: a.label.to_string(),
                access: self.of(a.id).as_str().to_string(),
            })
            .collect()
    }

    /// What fixes it, in words for a message: the variable, or the instance file.
    pub fn fixed_by(&self) -> Option<String> {
        match (self.source, &self.env, &self.file) {
            ("environment", Some(var), _) => Some(format!("{var} in the environment")),
            ("file", _, Some(path)) => Some(format!("the instance file {path}")),
            ("file", _, None) => Some("the instance file".to_string()),
            _ => None,
        }
    }
}

/// What the configuration says of one integration's access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixed {
    /// `read`, `act` or a list of action ids.
    pub grant: Grant,
    /// The variable of the environment form that fixes it.
    pub env: Option<String>,
    /// The instance file that fixes it.
    pub file: Option<String>,
}

/// One integration as the book takes it: its id and name, the actions its type declares, and
/// what the configuration fixes of its access.
#[derive(Debug, Clone)]
pub struct Listed {
    pub id: String,
    pub name: String,
    pub actions: &'static [Action],
    pub fixed: Option<Fixed>,
}

struct Entry {
    id: String,
    name: String,
    actions: &'static [Action],
    fixed: Option<Fixed>,
    switch: AccessSwitch,
}

impl Entry {
    /// The declared actions that act under these saved values.
    fn acting(&self, saved: &BTreeMap<String, Access>) -> Vec<&'static str> {
        self.actions
            .iter()
            .map(|a| a.id)
            .filter(|id| match &self.fixed {
                Some(f) => f.grant.of(id) == Access::Act,
                None => saved.get(*id) == Some(&Access::Act),
            })
            .collect()
    }
}

/// Why an access was not changed.
#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("There is no integration `{0}`.")]
    Unknown(String),
    #[error("{name} is read-only.")]
    NoActions { name: String },
    #[error("{name} has no action `{action}`.")]
    UnknownAction { name: String, action: String },
    #[error("Access to {name} is set by {by}.")]
    Fixed { name: String, by: String },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// A change of an access that went through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessChange {
    pub from: Access,
    pub to: Access,
    /// False: it was asked for what was in force already, and nothing was written.
    pub changed: bool,
}

/// The access of every integration the book was given.
#[derive(Default)]
pub struct AccessBook {
    entries: Vec<Entry>,
    /// What the owner saved in the interface, by integration id and action.
    saved: RwLock<BTreeMap<String, BTreeMap<String, Access>>>,
}

impl AccessBook {
    /// The book of these integrations, over the switches of an allowlist. An integration whose
    /// host is not listed (it is not asked) gets a switch of its own. Nothing saved is known
    /// until [`AccessBook::load`].
    pub fn new(listed: Vec<Listed>, allow: &Allowlist) -> AccessBook {
        let entries = listed
            .into_iter()
            .map(|i| {
                let entry = Entry {
                    switch: allow.switch(&i.id).unwrap_or_default(),
                    id: i.id,
                    name: i.name,
                    actions: i.actions,
                    fixed: i.fixed,
                };
                entry.switch.set_actions(entry.acting(&BTreeMap::new()));
                entry
            })
            .collect();
        AccessBook {
            entries,
            saved: RwLock::new(BTreeMap::new()),
        }
    }

    fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    fn saved_of(&self, id: &str) -> BTreeMap<String, Access> {
        self.saved
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// What Singularr may do with an integration, action by action, and where that comes from.
    /// Reading alone for one the configuration does not list.
    pub fn of(&self, id: &str) -> AccessState {
        let Some(entry) = self.entry(id) else {
            return AccessState::read_only();
        };
        // A type without an action is read, whatever the configuration says of it.
        if entry.actions.is_empty() {
            return AccessState::read_only();
        }
        let saved = self.saved_of(id);
        let acting = entry.acting(&saved);
        let (source, env, file) = match (&entry.fixed, saved.is_empty()) {
            (Some(f), _) if f.env.is_some() => ("environment", f.env.clone(), None),
            (Some(f), _) => ("file", None, f.file.clone()),
            (None, false) => ("saved", None, None),
            (None, true) => ("default", None, None),
        };
        AccessState {
            access: if acting.is_empty() {
                Access::Read
            } else {
                Access::Act
            },
            source,
            env,
            file,
            actions: entry.actions,
            acting,
        }
    }

    /// Whether Singularr may do the act `action` on an integration.
    pub fn allows(&self, id: &str, action: &str) -> bool {
        self.of(id).allows(action)
    }

    /// The integrations Singularr may act on, as (id, name), in the configuration's order.
    pub fn acting(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter(|e| e.switch.get() == Access::Act)
            .map(|e| (e.id.clone(), e.name.clone()))
            .collect()
    }

    /// The integrations with an action that acts, in the configuration's order: (id, name, the
    /// ids of the actions that act).
    pub fn acting_actions(&self) -> Vec<(String, String, Vec<&'static str>)> {
        self.entries
            .iter()
            .map(|e| (e.id.clone(), e.name.clone(), self.of(&e.id).acting))
            .filter(|(_, _, acting)| !acting.is_empty())
            .collect()
    }

    /// Record the actions each integration's type declares at this start. An `act` of the
    /// environment or the instance file covers every action of the type. That includes an action
    /// a later version adds. Each action it covers that the last start did not record is one
    /// warning. An integration recorded for the first time gives none.
    pub async fn note_declared(&self, pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
        let mut before: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, action) in store::declared_actions(pool).await? {
            before.entry(id).or_default().push(action);
        }
        let mut warnings = Vec::new();
        let mut tx = pool.begin().await?;
        for e in self.entries.iter().filter(|e| !e.actions.is_empty()) {
            let fixed_act = match &e.fixed {
                Some(f) if f.grant == Grant::Act => Some(match (&f.env, &f.file) {
                    (Some(var), _) => format!("{var}=act"),
                    (None, Some(path)) => format!("`access = \"act\"` in {path}"),
                    (None, None) => "The instance file".to_string(),
                }),
                _ => None,
            };
            if let (Some(seen), Some(by)) = (before.get(&e.id), fixed_act) {
                for a in e.actions.iter().filter(|a| !seen.iter().any(|s| s == a.id)) {
                    warnings.push(format!(
                        "{by} gives {} write access to an action the last start did not declare: {} (`{}`)",
                        e.name, a.label, a.id
                    ));
                }
            }
            let ids: Vec<&str> = e.actions.iter().map(|a| a.id).collect();
            store::save_declared(&mut tx, &e.id, &ids).await?;
        }
        let listed: Vec<String> = self.entries.iter().map(|e| e.id.clone()).collect();
        store::prune_declared(&mut tx, &listed).await?;
        tx.commit().await?;
        Ok(warnings)
    }

    /// The mode of the installation: `observe` while Singularr may act on no integration, `act`
    /// once it may act on one.
    pub fn mode(&self) -> &'static str {
        if self.entries.iter().any(|e| e.switch.get() == Access::Act) {
            "act"
        } else {
            "observe"
        }
    }

    /// Read what the owner saved, and put it in force where nothing fixes the access. A row saved
    /// for every action (`*`, from before actions had rows) is written again as one row per
    /// action the type declares now. A saved value of an integration that is not listed, of a
    /// type without an action, or of an action the type does not declare is not used.
    pub async fn load(&self, pool: &PgPool) -> Result<usize, sqlx::Error> {
        let rows = store::saved_access(pool).await?;
        let mut saved: BTreeMap<String, BTreeMap<String, Access>> = BTreeMap::new();
        for (id, action, word) in rows {
            let Some(entry) = self.entry(&id).filter(|e| !e.actions.is_empty()) else {
                continue;
            };
            let Some(access) = Access::parse(&word) else {
                continue;
            };
            let mine = saved.entry(id.clone()).or_default();
            if action == "*" {
                let ids: Vec<&str> = entry.actions.iter().map(|a| a.id).collect();
                let mut conn = pool.acquire().await?;
                store::expand_saved(&mut conn, &id, &ids).await?;
                for a in ids {
                    mine.entry(a.to_string()).or_insert(access);
                }
            } else if entry.actions.iter().any(|a| a.id == action) {
                mine.insert(action, access);
            }
        }
        for entry in &self.entries {
            if entry.fixed.is_none() && !entry.actions.is_empty() {
                let none = BTreeMap::new();
                let mine = saved.get(&entry.id).unwrap_or(&none);
                entry.switch.set_actions(entry.acting(mine));
            }
        }
        let n = saved.len();
        *self.saved.write().unwrap_or_else(PoisonError::into_inner) = saved;
        Ok(n)
    }

    /// Save what Singularr may do with every action of an integration at once: the word of the
    /// whole integration. See [`AccessBook::set_actions`].
    pub async fn set(
        &self,
        pool: &PgPool,
        journal: &Journal,
        actor: &str,
        id: &str,
        access: Access,
    ) -> Result<AccessChange, AccessError> {
        let before = self.of(id).access;
        let actions: Vec<(String, Access)> = self
            .entry(id)
            .map(|e| {
                e.actions
                    .iter()
                    .map(|a| (a.id.to_string(), access))
                    .collect()
            })
            .unwrap_or_default();
        let changed = self.set_actions(pool, journal, actor, id, &actions).await?;
        Ok(AccessChange {
            from: before,
            to: access,
            changed: changed > 0,
        })
    }

    /// Save what Singularr may do with these actions of an integration, with one
    /// `integration_access_changed` event per action that changes, in one transaction. It is in
    /// force at once. Refused for a type without an action, for an action the type does not
    /// declare, and where the environment or the instance file fixes the access. An action asked
    /// for what is in force already writes nothing. Returns how many changed.
    pub async fn set_actions(
        &self,
        pool: &PgPool,
        journal: &Journal,
        actor: &str,
        id: &str,
        actions: &[(String, Access)],
    ) -> Result<usize, AccessError> {
        let entry = self
            .entry(id)
            .ok_or_else(|| AccessError::Unknown(id.to_string()))?;
        let state = self.of(id);
        if state.actions.is_empty() {
            return Err(AccessError::NoActions {
                name: entry.name.clone(),
            });
        }
        if let Some(by) = state.fixed_by() {
            return Err(AccessError::Fixed {
                name: entry.name.clone(),
                by,
            });
        }
        let mut asked = Vec::new();
        for (action, access) in actions {
            let Some(declared) = entry.actions.iter().find(|a| a.id == action.as_str()) else {
                return Err(AccessError::UnknownAction {
                    name: entry.name.clone(),
                    action: action.clone(),
                });
            };
            asked.push((declared, *access));
        }
        // One writer at a time holds the journal. `from` is the value each change replaces.
        let mut tx = journal.begin(pool).await?;
        let mut mine = self.saved_of(id);
        let mut changed = 0;
        for (action, to) in asked {
            let from = mine.get(action.id).copied().unwrap_or(Access::Read);
            if from == to {
                continue;
            }
            store::save_access(tx.conn(), id, action.id, to.as_str(), actor).await?;
            tx.append_one(event(
                "integration_access_changed",
                "engine",
                id,
                actor,
                json!({
                    "integration": id,
                    "name": entry.name,
                    "action": action.id,
                    "label": action.label,
                    "from": from.as_str(),
                    "to": to.as_str(),
                }),
            ))
            .await?;
            mine.insert(action.id.to_string(), to);
            changed += 1;
        }
        tx.commit().await?;
        entry.switch.set_actions(entry.acting(&mine));
        self.saved
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_string(), mine);
        Ok(changed)
    }
}
