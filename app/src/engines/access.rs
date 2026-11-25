//! What Singularr may do with each integration: read it, or read it and act on it.
//!
//! The access is a permission of the integration, with sources like any setting: the default
//! (`read`), the environment or the instance file (which fix it), or what the owner saved in
//! the interface (kept in `integration_access`). A type can act only when its adapter declares
//! actions; for every other type the access is `read` and nothing changes that.
//!
//! The access is enforced in `core::outbound`, not here: the book holds the same switches as
//! the host rules there, so a change is in force with the next call.

use super::model::ActionView;
use super::store;
use crate::core::config::{Config, IntegrationType};
use crate::core::journal::{Journal, event};
use crate::core::outbound::{Access, AccessSwitch, Allowlist};
use serde_json::json;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::{PoisonError, RwLock};

/// One thing Singularr can do to a service beyond reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
}

/// The actions the adapter of a type declares. None: Singularr can only read the type.
pub fn actions_of(kind: IntegrationType) -> &'static [Action] {
    if let Some(client) = kind.client() {
        return super::clients::actions(client);
    }
    match kind {
        IntegrationType::Hush => super::hush::ACTIONS,
        IntegrationType::Tdarr => super::tdarr::ACTIONS,
        IntegrationType::Radarr | IntegrationType::Sonarr => super::arr::ACTIONS,
        _ => &[],
    }
}

/// What Singularr may do with one integration, and where that comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessState {
    pub access: Access,
    /// `default`, `environment`, `file` or `saved`: the words of a setting's source.
    pub source: &'static str,
    /// With `environment`: the variable that fixes it.
    pub env: Option<String>,
    /// With `file`: the instance file that fixes it.
    pub file: Option<String>,
    pub actions: &'static [Action],
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

    /// The actions, as the answers of the API list them.
    pub fn action_views(&self) -> Vec<ActionView> {
        self.actions
            .iter()
            .map(|a| ActionView {
                id: a.id.to_string(),
                label: a.label.to_string(),
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
struct Fixed {
    access: Access,
    env: Option<String>,
    file: Option<String>,
}

struct Entry {
    id: String,
    name: String,
    actions: &'static [Action],
    fixed: Option<Fixed>,
    switch: AccessSwitch,
}

/// Why an access was not changed.
#[derive(Debug, thiserror::Error)]
pub enum AccessError {
    #[error("There is no integration `{0}`.")]
    Unknown(String),
    #[error("{name} is read-only.")]
    NoActions { name: String },
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

/// The access of every integration of the configuration.
#[derive(Default)]
pub struct AccessBook {
    entries: Vec<Entry>,
    /// What the owner saved in the interface, by integration id.
    saved: RwLock<BTreeMap<String, Access>>,
}

impl AccessBook {
    /// The book of a configuration, over the switches of its allowlist. An integration whose
    /// host is not listed (it is not asked) gets a switch of its own. Nothing saved is known
    /// until [`AccessBook::load`].
    pub fn new(config: &Config, allow: &Allowlist) -> AccessBook {
        let entries = config
            .integrations
            .iter()
            .map(|i| {
                let actions = actions_of(i.kind);
                let from_file = i.source != "environment";
                let fixed = i.access.map(|access| Fixed {
                    access,
                    env: (!from_file).then(|| i.access_var.clone()),
                    file: config.instance_path.clone().filter(|_| from_file),
                });
                if actions.is_empty() && i.access == Some(Access::Act) {
                    tracing::warn!(
                        integration = %i.id,
                        "{} says act, and Singularr has no action for a {} integration: it is read",
                        i.access_var,
                        i.kind.as_str()
                    );
                }
                let switch = allow.switch(&i.id).unwrap_or_default();
                switch.set(match (&fixed, actions.is_empty()) {
                    (Some(f), false) => f.access,
                    _ => Access::Read,
                });
                Entry {
                    id: i.id.clone(),
                    name: i.name.clone(),
                    actions,
                    fixed,
                    switch,
                }
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

    fn saved_of(&self, id: &str) -> Option<Access> {
        self.saved
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .copied()
    }

    /// What Singularr may do with an integration, and where that comes from. Reading alone for
    /// one the configuration does not list.
    pub fn of(&self, id: &str) -> AccessState {
        let Some(entry) = self.entry(id) else {
            return AccessState::read_only();
        };
        // A type without an action is read, whatever the configuration says of it.
        if entry.actions.is_empty() {
            return AccessState::read_only();
        }
        let (access, source, env, file) = match (&entry.fixed, self.saved_of(id)) {
            (Some(f), _) if f.env.is_some() => (f.access, "environment", f.env.clone(), None),
            (Some(f), _) => (f.access, "file", None, f.file.clone()),
            (None, Some(saved)) => (saved, "saved", None, None),
            (None, None) => (Access::Read, "default", None, None),
        };
        AccessState {
            access,
            source,
            env,
            file,
            actions: entry.actions,
        }
    }

    /// The integrations Singularr may act on, as (id, name), in the configuration's order.
    pub fn acting(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .filter(|e| e.switch.get() == Access::Act)
            .map(|e| (e.id.clone(), e.name.clone()))
            .collect()
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

    /// Read what the owner saved, and put it in force where nothing fixes the access. A saved
    /// value of an integration that is not listed, or whose type has no action, is not used.
    pub async fn load(&self, pool: &PgPool) -> Result<usize, sqlx::Error> {
        let rows = store::saved_access(pool).await?;
        let saved: BTreeMap<String, Access> = rows
            .into_iter()
            .filter_map(|(id, word)| Some((id, Access::parse(&word)?)))
            .filter(|(id, _)| self.entry(id).is_some_and(|e| !e.actions.is_empty()))
            .collect();
        for entry in &self.entries {
            if entry.fixed.is_none() && !entry.actions.is_empty() {
                entry
                    .switch
                    .set(saved.get(&entry.id).copied().unwrap_or(Access::Read));
            }
        }
        let n = saved.len();
        *self.saved.write().unwrap_or_else(PoisonError::into_inner) = saved;
        Ok(n)
    }

    /// Save what Singularr may do with an integration, with an `integration_access_changed`
    /// event in the same transaction, and put it in force at once. Refused for a type without
    /// an action and where the environment or the instance file fixes the access. Asking for
    /// what is in force already writes nothing.
    pub async fn set(
        &self,
        pool: &PgPool,
        journal: &Journal,
        actor: &str,
        id: &str,
        access: Access,
    ) -> Result<AccessChange, AccessError> {
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
        // One writer at a time holds the journal, so `from` is the value this change replaces.
        let mut tx = journal.begin(pool).await?;
        let from = self.saved_of(id).unwrap_or(Access::Read);
        if from == access {
            return Ok(AccessChange {
                from,
                to: access,
                changed: false,
            });
        }
        store::save_access(tx.conn(), id, access.as_str(), actor).await?;
        tx.append_one(event(
            "integration_access_changed",
            "engine",
            id,
            actor,
            json!({
                "integration": id,
                "name": entry.name,
                "from": from.as_str(),
                "to": access.as_str(),
            }),
        ))
        .await?;
        tx.commit().await?;
        self.saved
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_string(), access);
        entry.switch.set(access);
        Ok(AccessChange {
            from,
            to: access,
            changed: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::{Calls, Method};

    const BRIDGE: &str = "http://10.0.0.60:8787";

    fn config(extra: &[(&'static str, &'static str)], file: Option<&str>) -> Config {
        let mut env = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("RADARR_URL", "http://10.0.0.61:7878"),
            ("RADARR_API_KEY", "k"),
        ]);
        env.extend(extra.iter().copied());
        Config::with_instance(&env, file.map(|text| ("/srv/instance.toml", text))).unwrap()
    }

    /// The allowlist of a bridge at its address, with the one act it takes.
    fn listed(id: &str) -> Allowlist {
        let mut allow = Allowlist::default();
        allow
            .allow_integration(id, BRIDGE, Calls::default().act(Method::Post, "/stage"))
            .unwrap();
        allow
    }

    fn may_stage(allow: &Allowlist) -> bool {
        allow
            .check(Method::Post, &format!("{BRIDGE}/stage"))
            .is_ok()
    }

    #[test]
    fn only_a_type_whose_adapter_declares_actions_can_act() {
        for kind in IntegrationType::ALL {
            let actions = actions_of(kind);
            assert_eq!(
                !actions.is_empty(),
                matches!(
                    kind,
                    IntegrationType::Hush
                        | IntegrationType::Tdarr
                        | IntegrationType::Radarr
                        | IntegrationType::Sonarr
                ),
                "{}",
                kind.as_str()
            );
        }
        // The managers: search, grab, import a refused download, remove a download.
        for kind in [IntegrationType::Radarr, IntegrationType::Sonarr] {
            let ids: Vec<&str> = actions_of(kind).iter().map(|a| a.id).collect();
            assert_eq!(ids, ["search", "grab", "import", "remove"]);
        }
        assert_eq!(
            actions_of(IntegrationType::Hush),
            &[Action {
                id: "send",
                label: "Send a file to a flow"
            }]
        );
        let state = AccessState::read_only();
        assert!(!state.fixed() && !state.editable() && state.action_views().is_empty());
        assert_eq!(state.fixed_by(), None);
    }

    #[test]
    fn the_access_is_read_by_default_and_the_configuration_fixes_it() {
        // By default: read, and the owner may change it.
        let c = config(&[("HUSH_URL", BRIDGE)], None);
        let allow = listed("hush");
        let book = AccessBook::new(&c, &allow);
        let hush = book.of("hush");
        assert_eq!((hush.access, hush.source), (Access::Read, "default"));
        assert!(hush.editable() && !hush.fixed());
        assert_eq!(
            serde_json::to_value(hush.action_views()).unwrap(),
            json!([{"id": "send", "label": "Send a file to a flow"}])
        );
        assert!(book.acting().is_empty() && !may_stage(&allow));
        // A manager can be acted on and is read until that is allowed; what the configuration
        // does not list is read.
        let radarr = book.of("radarr");
        assert_eq!((radarr.access, radarr.source), (Access::Read, "default"));
        assert!(!radarr.actions.is_empty() && radarr.editable());
        assert_eq!(book.of("nothing"), AccessState::read_only());

        // The environment fixes it, and the host rule lets the act through at once.
        let c = config(&[("HUSH_URL", BRIDGE), ("HUSH_ACCESS", "act")], None);
        let allow = listed("hush");
        let book = AccessBook::new(&c, &allow);
        let hush = book.of("hush");
        assert_eq!(
            (hush.access, hush.source, hush.env.as_deref()),
            (Access::Act, "environment", Some("HUSH_ACCESS"))
        );
        assert!(hush.fixed() && !hush.editable());
        assert_eq!(
            hush.fixed_by().as_deref(),
            Some("HUSH_ACCESS in the environment")
        );
        assert_eq!(
            book.acting(),
            vec![("hush".to_string(), "HUSH".to_string())]
        );
        assert!(may_stage(&allow));
        // It can fix reading too.
        let c = config(&[("HUSH_URL", BRIDGE), ("HUSH_ACCESS", "read")], None);
        let allow = listed("hush");
        let hush = AccessBook::new(&c, &allow).of("hush");
        assert_eq!((hush.access, hush.source), (Access::Read, "environment"));
        assert!(!may_stage(&allow));

        // The instance file fixes it for an integration it lists, and is named.
        let file = "[[integration]]\nid = \"bridge\"\ntype = \"hush\"\nname = \"Bridge\"\nurl = \"http://10.0.0.60:8787\"\naccess = \"act\"\n";
        let c = config(&[], Some(file));
        let allow = listed("bridge");
        let book = AccessBook::new(&c, &allow);
        let bridge = book.of("bridge");
        assert_eq!(
            (bridge.access, bridge.source, bridge.file.as_deref()),
            (Access::Act, "file", Some("/srv/instance.toml"))
        );
        assert_eq!(bridge.env, None);
        assert_eq!(
            bridge.fixed_by().as_deref(),
            Some("the instance file /srv/instance.toml")
        );
        assert_eq!(
            book.acting(),
            vec![("bridge".to_string(), "Bridge".to_string())]
        );
        assert!(may_stage(&allow));

        // `act` for a type without an action changes nothing: it is read.
        let c = config(
            &[
                ("SEERR_URL", "http://10.0.0.62:5055"),
                ("SEERR_API_KEY", "k"),
                ("SEERR_ACCESS", "act"),
            ],
            None,
        );
        let book = AccessBook::new(&c, &Allowlist::default());
        let seerr = book.of("seerr");
        assert_eq!((seerr.access, seerr.source), (Access::Read, "default"));
        assert!(book.acting().is_empty());
        // A manager's access is fixed by the environment like any other.
        let c = config(&[("RADARR_ACCESS", "act")], None);
        let book = AccessBook::new(&c, &Allowlist::default());
        let radarr = book.of("radarr");
        assert_eq!(
            (radarr.access, radarr.source, radarr.env.as_deref()),
            (Access::Act, "environment", Some("RADARR_ACCESS"))
        );
    }
}
