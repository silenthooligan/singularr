use super::*;
use crate::core::jobs::{JobSpec, Jobs};
use crate::core::outbound::Access;
use crate::engines::access::{AccessState, actions_of};
use crate::engines::service::integration_view;
use crate::policy::ActivePolicy;
use std::time::Duration;

fn config() -> Config {
    Config::from(&BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("RADARR_URL", "http://admin:pw@10.0.0.61:7878/"),
        ("RADARR_API_KEY", "k1"),
        ("RADARR_NAME", "Radarr HD"),
        ("SONARR_URL", "http://10.0.0.61:8989"),
        ("PROWLARR_URL", "http://10.0.0.62:9696"),
    ]))
    .unwrap()
}

fn row(ok: bool) -> EngineStateRow {
    EngineStateRow {
        engine: "radarr".to_string(),
        kind: "radarr".to_string(),
        library_id: Some("radarr".to_string()),
        ok,
        version: Some("6.0.4".to_string()),
        last_sync_at: Some("2026-11-23T18:00:00Z".parse().unwrap()),
        last_ok_at: Some("2026-11-23T17:45:00Z".parse().unwrap()),
        last_error: (!ok).then(|| "connection refused".to_string()),
        latency_ms: Some(12),
        latency_history: vec![10, 14, 12],
        counts: serde_json::json!({"titles": 4470, "grab_history_complete": true}),
        failures: 0,
    }
}

#[test]
fn an_address_loses_its_credentials() {
    assert_eq!(
        without_credentials("http://admin:pw@10.0.0.61:7878/api"),
        "http://10.0.0.61:7878/api"
    );
    assert_eq!(
        without_credentials("http://10.0.0.61:7878"),
        "http://10.0.0.61:7878"
    );
    assert_eq!(without_credentials("not an address"), "not an address");
}

#[test]
fn an_integration_says_where_it_comes_from_and_what_is_read() {
    let c = config();
    let radarr = c.integration("radarr").unwrap();
    let check = Check {
        at: "2026-11-23T18:05:00Z".parse().unwrap(),
        ok: true,
        via: "test".to_string(),
        ms: 9,
        detail: "Radarr 6.0.4".to_string(),
    };
    let up = row(true);
    let p = integration_panel(
        radarr,
        integration_view(radarr, Some(&up), &AccessState::read_only()),
        Some(&up),
        std::slice::from_ref(&check),
        vec!["radarr:titles".to_string()],
        None,
    );
    assert_eq!(p.name, "Radarr HD");
    assert_eq!(p.address.as_deref(), Some("http://10.0.0.61:7878"));
    assert_eq!(
        (p.source.as_str(), p.env.as_deref(), p.editable),
        ("environment", Some("RADARR_URL"), false)
    );
    assert_eq!(p.state, "ok");
    assert_eq!(p.counts, serde_json::json!({"titles": 4470}));
    assert_eq!(
        p.last_check,
        Some(check.at),
        "the test is the newest contact"
    );
    assert_eq!(p.latency, vec![10, 14, 12]);
    assert!(p.writes.is_empty() && !p.reads.is_empty());
    // Read, by default: nothing is written to it.
    assert_eq!(
        (p.access.as_str(), p.access_source.as_str()),
        ("read", "default")
    );
    assert!(p.actions.is_empty());
    assert_eq!(p.error, None);
    let down = row(false);
    let p = integration_panel(
        radarr,
        integration_view(radarr, Some(&down), &AccessState::read_only()),
        Some(&down),
        &[],
        Vec::new(),
        None,
    );
    assert_eq!(
        (p.state.as_str(), p.error.as_deref()),
        ("down", Some("connection refused"))
    );
    let json = serde_json::to_value(&p).unwrap();
    for key in [
        "id",
        "type",
        "name",
        "role",
        "state",
        "detail",
        "version",
        "address",
        "source",
        "editable",
        "lastCheck",
        "lastSync",
        "latencyMs",
        "latency",
        "counts",
        "reads",
        "writes",
        "error",
        "access",
        "accessSource",
        "actions",
    ] {
        assert!(json.get(key).is_some(), "an integration has no `{key}`");
    }
    assert_eq!(p.group, "managers");

    // A bridge the owner allowed Singularr to act on: its access, where that comes from, what
    // the action is, and the same in words as what Singularr can change in it.
    let with_bridge = Config::from(&BTreeMap::from([
        ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
        ("HUSH_URL", "http://10.0.0.60:8787"),
    ]))
    .unwrap();
    let hush = with_bridge.integration("hush").unwrap();
    let allowed = AccessState {
        access: Access::Act,
        source: "saved",
        env: None,
        file: None,
        actions: actions_of(hush.kind),
    };
    let p = integration_panel(
        hush,
        integration_view(hush, None, &allowed),
        None,
        &[],
        Vec::new(),
        None,
    );
    let json = serde_json::to_value(&p).unwrap();
    assert_eq!(
        (&json["access"], &json["accessSource"]),
        (&serde_json::json!("act"), &serde_json::json!("saved"))
    );
    assert_eq!(
        json["actions"],
        serde_json::json!([{"id": "send", "label": "Send a file to a flow"}])
    );
    assert_eq!(json["writes"], serde_json::json!(["Send a file to a flow"]));
    assert_eq!(p.group, "processing");
    let prowlarr = c.integration("prowlarr").unwrap();
    assert!(
        reads(prowlarr.kind).is_empty(),
        "this version does not read it"
    );
}

#[test]
fn the_types_and_groups_are_data() {
    let groups: Vec<String> = groups().into_iter().map(|g| g.id).collect();
    let kinds = kinds();
    assert_eq!(
        kinds.len(),
        IntegrationType::ALL.len() + 2,
        "and the two providers"
    );
    for k in &kinds {
        assert!(groups.contains(&k.group), "{}", k.kind);
        // Every type has a secret.
        assert!(k.fields.iter().any(|f| f.kind == "secret"), "{}", k.kind);
        // Only the processing services and the managers have something Singularr can change
        // in them.
        assert_eq!(
            !k.writes.is_empty(),
            matches!(k.kind.as_str(), "hush" | "tdarr" | "radarr" | "sonarr"),
            "{}",
            k.kind
        );
    }
    let prowlarr = kinds.iter().find(|k| k.kind == "prowlarr").unwrap();
    assert!(!prowlarr.available && prowlarr.reads.is_empty());
    assert!(!prowlarr.fields.iter().any(|f| f.key == "syncEvery"));
    // The processing services are read: a worker and a bridge, any number of each.
    for id in ["tdarr", "hush"] {
        let k = kinds.iter().find(|k| k.kind == id).unwrap();
        assert!(k.available && !k.reads.is_empty(), "{id}");
        assert_eq!((k.group.as_str(), k.max), ("processing", None), "{id}");
        assert!(k.fields.iter().any(|f| f.key == "syncEvery"), "{id}");
    }
    let hush = kinds.iter().find(|k| k.kind == "hush").unwrap();
    assert_eq!(hush.label, "HUSH");
    assert_eq!(
        hush.fields
            .iter()
            .map(|f| f.key.as_str())
            .collect::<Vec<_>>(),
        vec!["address", "apiKey", "syncEvery"]
    );
    assert_eq!(hush.writes, vec!["Send a file to a flow"]);
    let radarr = kinds.iter().find(|k| k.kind == "radarr").unwrap();
    assert_eq!(
        radarr.writes,
        vec![
            "Search for releases",
            "Grab a release",
            "Import a refused download",
            "Remove a download"
        ]
    );
    let plex = kinds.iter().find(|k| k.kind == "plex").unwrap();
    assert_eq!(plex.max, Some(1));
    let tmdb = provider_panel(
        "tmdb",
        "TMDB",
        false,
        Some("SINGULARR_TMDB_TOKEN"),
        "api.themoviedb.org",
    );
    assert_eq!(
        (tmdb.state.as_str(), tmdb.group.as_str()),
        ("warn", "metadata")
    );
    assert!(tmdb.detail.contains("SINGULARR_TMDB_TOKEN"));
}

/// The five download clients are types of one group, each read, any number of each, each with
/// the sign-in its program takes and nothing to change in it.
#[test]
fn the_download_clients_are_types_of_one_group() {
    let kinds = kinds();
    for (id, label, fields) in [
        (
            "qbittorrent",
            "qBittorrent",
            &["address", "username", "password", "syncEvery"][..],
        ),
        (
            "transmission",
            "Transmission",
            &["address", "username", "password", "syncEvery"][..],
        ),
        (
            "deluge",
            "Deluge",
            &["address", "password", "syncEvery"][..],
        ),
        (
            "sabnzbd",
            "SABnzbd",
            &["address", "apiKey", "syncEvery"][..],
        ),
        (
            "nzbget",
            "NZBGet",
            &["address", "username", "password", "syncEvery"][..],
        ),
    ] {
        let k = kinds.iter().find(|k| k.kind == id).unwrap();
        assert_eq!(
            (k.label.as_str(), k.group.as_str(), k.max),
            (label, "downloads", None)
        );
        assert!(
            k.available && !k.reads.is_empty() && k.writes.is_empty(),
            "{id}"
        );
        assert_eq!(
            k.fields.iter().map(|f| f.key.as_str()).collect::<Vec<_>>(),
            fields,
            "{id}"
        );
    }
    assert_eq!(secret_label(IntegrationType::Sabnzbd), "API key");
    assert_eq!(secret_label(IntegrationType::Deluge), "Password");
    assert_eq!(secret_label(IntegrationType::Plex), "Token");
}

fn library(id: &str, kind: &str, section: Option<&str>, managed_by: Option<&str>) -> Library {
    Library {
        id: id.to_string(),
        name: id.to_uppercase(),
        kind: Some(kind.to_string()),
        integration: Some(if section.is_some() { "plex" } else { id }.to_string()),
        section_key: section.map(str::to_string),
        managed_by: managed_by.map(str::to_string),
        locations: vec![format!("/{id}")],
        root_folders: if section.is_none() {
            vec![format!("/data/{id}")]
        } else {
            Vec::new()
        },
        engines: Vec::new(),
        titles: 10,
        copies: 12,
        size_bytes: 1000,
    }
}

#[test]
fn libraries_are_the_server_s_sections_with_what_their_manager_holds() {
    let rows = vec![
        library("plex-1", "movie", Some("1"), Some("radarr")),
        library("plex-6", "music", Some("6"), None),
        library("radarr", "movie", None, None),
        library("sonarr", "show", None, None),
    ];
    let holdings = vec![
        LibraryHoldings {
            library_id: "radarr".to_string(),
            with_file: 8,
            episodes: 0,
            episode_files: 0,
        },
        LibraryHoldings {
            library_id: "sonarr".to_string(),
            with_file: 9,
            episodes: 50,
            episode_files: 40,
        },
    ];
    let policy = ActivePolicy::builtin().policy;
    let last_ok = BTreeMap::from([(
        "radarr".to_string(),
        "2026-11-23T18:00:00Z".parse::<DateTime<Utc>>().unwrap(),
    )]);
    let facts = LibraryFacts {
        holdings: &holdings,
        policy: &policy,
        albums: Some((2051, 9570, 5000)),
        last_ok: &last_ok,
    };
    let libs = libraries_panel(&rows, &facts);
    let ids: Vec<&str> = libs.iter().map(|l| l.id.as_str()).collect();
    assert_eq!(ids, vec!["plex-1", "plex-6", "sonarr"]);
    let movies = &libs[0];
    assert_eq!(
        (
            movies.kind.as_str(),
            movies.plex_key.as_deref(),
            movies.manager.as_deref()
        ),
        ("movie", Some("1"), Some("radarr"))
    );
    assert_eq!(
        (movies.titles, movies.have, movies.size),
        (Some(10), Some(8), Some(1000))
    );
    assert_eq!(movies.locations, vec!["/plex-1"]);
    assert_eq!(movies.roots, vec!["/data/radarr"]);
    assert_eq!(movies.profiles, vec!["movies-hd", "movies-uhd"]);
    assert_eq!(movies.profile.as_deref(), Some("movies-hd"));
    assert_eq!(movies.scanned, last_ok.get("radarr").copied());
    let music = &libs[1];
    assert_eq!(
        (
            music.kind.as_str(),
            music.titles,
            music.tracks,
            music.manager.as_deref()
        ),
        ("music", Some(2051), Some(9570), None)
    );
    assert_eq!(music.roots, music.locations);
    let shows = &libs[2];
    assert_eq!(
        (shows.have, shows.episodes, shows.plex_key.as_deref()),
        (Some(40), Some(50), None)
    );
    // Without the server, the managers' own libraries are the libraries.
    let libs = libraries_panel(&rows[2..], &facts);
    assert_eq!(libs.len(), 2);
    let json = serde_json::to_value(&libs[0]).unwrap();
    for key in [
        "id",
        "name",
        "kind",
        "plexKey",
        "manager",
        "locations",
        "roots",
        "titles",
        "have",
        "episodes",
        "albums",
        "tracks",
        "size",
        "profile",
        "profiles",
        "scanned",
    ] {
        assert!(json.get(key).is_some(), "a library has no `{key}`");
    }
}

#[test]
fn roles_and_permissions_are_data() {
    let roles = roles();
    let ids: Vec<&str> = roles.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["owner", "admin", "member", "guest"]);
    assert_eq!(roles[0].permissions, vec!["*"]);
    assert!(!roles[1].permissions.iter().any(|p| p == "keys.manage"));
    assert_eq!(roles[2].permissions, vec!["portal.open", "requests.create"]);
    let known: Vec<String> = permissions().into_iter().map(|p| p.id).collect();
    for r in &roles[1..] {
        for p in &r.permissions {
            assert!(known.contains(p), "{p}");
        }
    }
}

#[test]
fn a_job_is_named_for_what_it_reads() {
    let jobs = Jobs::default();
    let job = jobs.register(JobSpec::of("radarr", "titles", Duration::from_secs(900)));
    let v = job_view(&job, &config());
    assert_eq!(v.label, "Radarr HD titles and files");
    assert_eq!(
        (v.every, v.can_run, v.last_result.as_deref()),
        (900, true, None)
    );
    job.end(job.begin(), Err("connection refused".into()));
    let v = job_view(&job, &config());
    assert_eq!(v.last_result.as_deref(), Some("failed"));
    let folder = jobs.register(JobSpec::folder("music", Duration::from_secs(21_600)));
    let v = job_view(&folder, &config());
    assert_eq!(
        (v.label.as_str(), v.integration),
        ("Music folder scan", None)
    );
}

#[test]
fn the_configuration_s_own_checks() {
    let mut e = BTreeMap::from([("DATABASE_URL", "postgres://x@127.0.0.1:1/x")]);
    e.insert("SINGULARR_EDGE_TOKEN", "CHANGEME");
    let c = Config::from(&e).unwrap();
    let checks = config_checks(&c);
    let edge = checks.iter().find(|c| c.id == "edge-token").unwrap();
    assert_eq!(edge.state, "down");
    assert!(
        checks.iter().any(|c| c.state == "warn"),
        "the owner list is empty"
    );
    // No token: a warning only when a proxy is trusted, since only then is a header honored.
    let edge_state = |e: &BTreeMap<&str, &str>| {
        let checks = config_checks(&Config::from(e).unwrap());
        checks
            .into_iter()
            .find(|c| c.id == "edge-token")
            .unwrap()
            .state
    };
    let mut e = BTreeMap::from([("DATABASE_URL", "postgres://x@127.0.0.1:1/x")]);
    assert_eq!(edge_state(&e), "ok");
    e.insert("SINGULARR_TRUSTED_PROXY_IPS", "192.0.2.10");
    assert_eq!(edge_state(&e), "warn");
    assert_eq!(worst(["ok", "warn", "ok"]), "warn");
    assert_eq!(worst(["warn", "down"]), "down");
    assert_eq!(worst([]), "ok");
}

/// The groups and types of `GET /api/v1/integrations`, pinned as JSON: the groups with their
/// names, the types with their group, name, label, counts, reads, writes and fields. A change to
/// any of them fails this test. Its message prints the JSON as it is now, for
/// `tests/fixtures/integration_types.json`.
#[test]
fn the_groups_and_types_of_the_integrations_are_pinned() {
    let now = serde_json::json!({ "groups": groups(), "types": kinds() });
    let pinned: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/integration_types.json"
    ))
    .unwrap();
    assert!(
        now == pinned,
        "the integrations' groups or types changed. They are now:\n{}",
        serde_json::to_string_pretty(&now).unwrap()
    );
    // Every type's group is a listed group.
    let ids: Vec<String> = groups().into_iter().map(|g| g.id).collect();
    assert!(kinds().iter().all(|k| ids.contains(&k.group)));
}
