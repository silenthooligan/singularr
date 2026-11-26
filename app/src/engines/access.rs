//! What Singularr may do with each integration: the platform's access book
//! (`singularr_platform::access`), given the integrations of this configuration and the actions
//! their types declare.

use crate::core::config::{Config, IntegrationType};
use crate::core::outbound::{Allowlist, Grant};
pub use singularr_platform::access::*;

/// The actions the adapter of a type declares, as its manifest lists them. None: Singularr can
/// only read the type.
pub fn actions_of(kind: IntegrationType) -> &'static [Action] {
    kind.manifest().actions
}

/// The integrations of a configuration as the access book takes them: their ids and names, the
/// actions their types declare and what the configuration fixes.
pub fn integrations(config: &Config) -> Vec<Listed> {
    config
        .integrations
        .iter()
        .map(|i| {
            let actions = actions_of(i.kind);
            let from_file = i.source != "environment";
            if actions.is_empty() && i.access.as_ref().is_some_and(|g| *g != Grant::Read) {
                tracing::warn!(
                    integration = %i.id,
                    "{} says act, and Singularr has no action for a {} integration: it is read",
                    i.access_var,
                    i.kind.as_str()
                );
            }
            Listed {
                id: i.id.clone(),
                name: i.name.clone(),
                actions,
                fixed: i.access.clone().map(|grant| Fixed {
                    grant,
                    env: (!from_file).then(|| i.access_var.clone()),
                    file: config.bootstrap.instance_path.clone().filter(|_| from_file),
                }),
            }
        })
        .collect()
}

/// The book of a configuration, over the switches of its allowlist. Nothing saved is known
/// until [`AccessBook::load`].
pub fn access_book(config: &Config, allow: &Allowlist) -> AccessBook {
    AccessBook::new(integrations(config), allow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::{Access, Calls, Method};
    use serde_json::json;
    use std::collections::BTreeMap;

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
            .allow_integration(
                id,
                BRIDGE,
                Calls::default().act("send", Method::Post, "/stage"),
            )
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
        let book = access_book(&c, &allow);
        let hush = book.of("hush");
        assert_eq!((hush.access, hush.source), (Access::Read, "default"));
        assert!(hush.editable() && !hush.fixed());
        assert_eq!(
            serde_json::to_value(hush.action_views()).unwrap(),
            json!([{"id": "send", "label": "Send a file to a flow", "access": "read"}])
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
        let book = access_book(&c, &allow);
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
        let hush = access_book(&c, &allow).of("hush");
        assert_eq!((hush.access, hush.source), (Access::Read, "environment"));
        assert!(!may_stage(&allow));

        // The instance file fixes it for an integration it lists, and is named.
        let file = "[[integration]]\nid = \"bridge\"\ntype = \"hush\"\nname = \"Bridge\"\nurl = \"http://10.0.0.60:8787\"\naccess = \"act\"\n";
        let c = config(&[], Some(file));
        let allow = listed("bridge");
        let book = access_book(&c, &allow);
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
        let book = access_book(&c, &Allowlist::default());
        let seerr = book.of("seerr");
        assert_eq!((seerr.access, seerr.source), (Access::Read, "default"));
        assert!(book.acting().is_empty());
        // A manager's access is fixed by the environment like any other.
        let c = config(&[("RADARR_ACCESS", "act")], None);
        let book = access_book(&c, &Allowlist::default());
        let radarr = book.of("radarr");
        assert_eq!(
            (radarr.access, radarr.source, radarr.env.as_deref()),
            (Access::Act, "environment", Some("RADARR_ACCESS"))
        );
    }
}
