//! The rules, pure: every call decided without a transport.

use super::*;

fn allow() -> Allowlist {
    let mut a = Allowlist::with_metadata_providers();
    a.allow_engine("radarr", "http://192.0.2.61:7979", &[])
        .unwrap();
    a.allow_engine("sonarr", "http://192.0.2.61:8989/", &[])
        .unwrap();
    a.allow_engine(
        "qbittorrent",
        "http://192.0.2.62:8080",
        &["/api/v2/auth/login"],
    )
    .unwrap();
    a
}

#[test]
fn a_get_to_a_listed_engine_passes() {
    let a = allow();
    let (url, rule) = a
        .check(Method::Get, "http://192.0.2.61:7979/api/v3/movie")
        .unwrap();
    assert_eq!(url.path(), "/api/v3/movie");
    assert_eq!(rule.label, "radarr");
}

/// The safety guarantee of stage 1: nothing but GET reaches an arr.
#[test]
fn anything_but_get_to_an_arr_is_refused() {
    let a = allow();
    for method in [Method::Post, Method::Put, Method::Delete] {
        for url in [
            "http://192.0.2.61:7979/api/v3/command",
            "http://192.0.2.61:7979/api/v3/movie/12",
            "http://192.0.2.61:8989/api/v3/episodefile/9",
            "http://192.0.2.61:8989/api/v3/queue/3",
        ] {
            let err = a.check(method, url).unwrap_err();
            assert!(
                matches!(err, OutboundError::NotListed { .. }),
                "{} {url} must be refused, got {err:?}",
                method.as_str()
            );
        }
    }
}

#[test]
fn qbittorrent_accepts_the_login_post_and_nothing_else() {
    let a = allow();
    assert!(
        a.check(Method::Post, "http://192.0.2.62:8080/api/v2/auth/login")
            .is_ok()
    );
    assert!(
        a.check(Method::Get, "http://192.0.2.62:8080/api/v2/torrents/info")
            .is_ok()
    );
    for url in [
        "http://192.0.2.62:8080/api/v2/torrents/delete",
        "http://192.0.2.62:8080/api/v2/torrents/pause",
        "http://192.0.2.62:8080/api/v2/auth/login/extra",
    ] {
        assert!(matches!(
            a.check(Method::Post, url),
            Err(OutboundError::NotListed { .. })
        ));
    }
    assert!(matches!(
        a.check(Method::Delete, "http://192.0.2.62:8080/api/v2/auth/login"),
        Err(OutboundError::NotListed { .. })
    ));
}

#[test]
fn hosts_that_are_not_listed_are_refused() {
    let a = allow();
    for url in [
        "http://192.0.2.61:9696/api/v1/indexer",
        "https://example.com/",
        "http://192.0.2.61/api/v3/movie",
        "https://192.0.2.61:7979/api/v3/movie",
        "https://skyhook.sonarr.tv/v1/tvdb/shows/en/1",
    ] {
        assert!(
            matches!(
                a.check(Method::Get, url),
                Err(OutboundError::HostNotAllowed { .. })
            ),
            "{url} must be refused"
        );
    }
    assert!(matches!(
        a.check(Method::Get, "file:///etc/passwd"),
        Err(OutboundError::BadUrl(_))
    ));
    assert!(matches!(
        a.check(Method::Get, "not a url"),
        Err(OutboundError::BadUrl(_))
    ));
}

/// Singularr's own search service takes writes; the engines next to it stay read-only.
#[test]
fn the_search_service_is_the_one_host_that_takes_writes() {
    let mut a = allow();
    a.allow_service("meilisearch", "http://10.0.0.9:7700")
        .unwrap();
    for method in [Method::Get, Method::Post, Method::Put, Method::Delete] {
        assert!(
            a.check(
                method,
                "http://10.0.0.9:7700/indexes/singularr_library/documents"
            )
            .is_ok(),
            "{}",
            method.as_str()
        );
    }
    assert!(matches!(
        a.check(Method::Post, "http://192.0.2.61:7979/api/v3/command"),
        Err(OutboundError::NotListed { .. })
    ));
    assert!(
        a.check(Method::Put, "http://10.0.0.9:7701/indexes")
            .is_err(),
        "another port is another origin"
    );
}

#[test]
fn metadata_providers_are_listed_read_only() {
    let a = allow();
    assert!(
        a.check(Method::Get, "https://api.themoviedb.org/3/tv/1396")
            .is_ok()
    );
    assert!(
        a.check(
            Method::Get,
            "https://api.tvmaze.com/lookup/shows?thetvdb=81189"
        )
        .is_ok()
    );
    assert!(
        a.check(Method::Post, "https://api4.thetvdb.com/v4/login")
            .is_ok()
    );
    assert!(
        a.check(Method::Post, "https://api.themoviedb.org/3/list")
            .is_err()
    );
    assert!(
        a.check(Method::Delete, "https://api4.thetvdb.com/v4/series/1")
            .is_err()
    );
    // The plain-http form of a provider is another origin and is not listed.
    assert!(
        a.check(Method::Get, "http://api.tvmaze.com/shows/1")
            .is_err()
    );
}

#[test]
fn the_request_face_reaches_tmdb_and_plex_tv_only() {
    let a = Allowlist::for_requests_face();
    assert!(
        a.check(
            Method::Get,
            "https://api.themoviedb.org/3/search/multi?query=x"
        )
        .is_ok()
    );
    assert!(
        a.check(Method::Post, "https://plex.tv/api/v2/pins?strong=true")
            .is_ok()
    );
    assert!(
        a.check(Method::Get, "https://plex.tv/api/v2/pins/42")
            .is_ok()
    );
    assert!(a.check(Method::Get, "https://plex.tv/api/v2/user").is_ok());
    for (method, url) in [
        (Method::Post, "https://plex.tv/api/v2/user"),
        (Method::Delete, "https://plex.tv/api/v2/pins/42"),
        (Method::Get, "https://app.plex.tv/auth"),
        (Method::Get, "http://192.0.2.62:5055/api/v1/request"),
        (Method::Get, "http://192.0.2.61:7979/api/v3/movie"),
        (Method::Get, "https://api4.thetvdb.com/v4/series/1"),
        (Method::Get, "http://plex.tv/api/v2/user"),
    ] {
        assert!(a.check(method, url).is_err(), "{} {url}", method.as_str());
    }
}

const WORKER: &str = "http://10.0.0.60:8265";
const BRIDGE: &str = "http://10.0.0.60:8787";

/// A worker whose records are asked with POST, and a bridge that takes one act.
fn integrations() -> Allowlist {
    let mut a = allow();
    a.allow_integration(
        "worker",
        WORKER,
        Calls::default()
            .read_when(
                Method::Post,
                "/api/v2/cruddb",
                "/data/mode",
                &["getAll", "getById"],
            )
            .read(Method::Post, "/api/v2/client/status-tables"),
    )
    .unwrap();
    a.allow_integration(
        "bridge",
        BRIDGE,
        Calls::default().act("send", Method::Post, "/stage"),
    )
    .unwrap();
    a
}

fn json(text: &str) -> Body {
    Body::Json(text.to_string())
}

#[test]
fn a_host_that_declares_its_gets_takes_those_alone() {
    const KEY: &str = "0123456789abcdef0123456789abcdef";
    let mut a = Allowlist::with_metadata_providers();
    a.allow_integration(
        "history",
        "http://10.0.0.50:8181",
        Calls::default().get_when("/api/v2", "cmd", &["get_history", "get_users"]),
    )
    .unwrap();
    a.allow_integration(
        "behind",
        "http://10.0.0.51/tautulli",
        Calls::default().get_when("/api/v2", "cmd", &["get_history"]),
    )
    .unwrap();
    a.allow_engine("radarr", "http://10.0.0.61:7878", &[])
        .unwrap();
    let at = |rest: &str| format!("http://10.0.0.50:8181/api/v2?apikey={KEY}{rest}");
    for url in [
        at("&cmd=get_history&length=10"),
        at("&cmd=get_users"),
        at("&cmd=get%5Fhistory"),
        format!("http://10.0.0.51/tautulli/api/v2?apikey={KEY}&cmd=get_history"),
    ] {
        assert!(a.check(Method::Get, &url).is_ok(), "{url}");
    }
    for url in [
        at("&cmd=delete_history"),
        at("&cmd=terminate_session"),
        at("&cmd=restart"),
        at("&cmd=Get_History"),
        at("&cmd=get_history%00"),
        at(""),
        at("&cmd="),
        at("&cmd=get_history&cmd=delete_history"),
        format!("http://10.0.0.50:8181/api/v3?apikey={KEY}&cmd=get_history"),
        format!("http://10.0.0.50:8181/?apikey={KEY}&cmd=get_history"),
        format!("http://10.0.0.51/api/v2?apikey={KEY}&cmd=get_history"),
    ] {
        let err = a.check(Method::Get, &url).unwrap_err();
        assert!(
            matches!(err, OutboundError::NotListed { .. }),
            "{url} must be refused, got {err:?}"
        );
        let text = err.to_string();
        assert!(!text.contains(KEY) && !text.contains("cmd"), "{text}");
    }
    // Every other host: GET always passes.
    assert!(
        a.check(Method::Get, "http://10.0.0.61:7878/api/v3/movie?x=1")
            .is_ok()
    );
    assert!(
        a.check(Method::Get, "https://api.themoviedb.org/3/movie/603")
            .is_ok()
    );
}

/// A media server starts work on some GETs (a transcode, a library scan, a download). Its
/// adapter declares the paths it reads: a GET of one of them passes with any query. Every
/// other GET is refused, as is any other method at the same paths.
#[test]
fn a_host_that_declares_its_paths_takes_gets_of_those_alone() {
    let mut a = Allowlist::default();
    a.allow_integration(
        "server",
        "http://10.0.0.40:8096/jellyfin",
        Calls::default()
            .get("/Items")
            .get("/Items/{any}/Images/{any}")
            .get("/library/metadata/{id}"),
    )
    .unwrap();
    let at = |rest: &str| format!("http://10.0.0.40:8096/jellyfin{rest}");
    for url in [
        at("/Items?ParentId=abc&Recursive=true&Fields=ProviderIds,Path"),
        at("/Items"),
        at("/Items/0123abcd/Images/Primary?maxWidth=300"),
        at("/library/metadata/109882"),
    ] {
        assert!(a.check(Method::Get, &url).is_ok(), "{url}");
    }
    for url in [
        at("/Items/0123abcd/Download"),
        at("/Videos/0123abcd/stream?static=true"),
        at("/Items/0123abcd/Images"),
        at("/Items/0123abcd/Images/Primary/0"),
        at("/library/metadata/abc"),
        at("/library/sections/1/refresh"),
        "http://10.0.0.40:8096/Items".to_string(),
    ] {
        assert!(
            matches!(
                a.check(Method::Get, &url),
                Err(OutboundError::NotListed { .. })
            ),
            "{url} must be refused"
        );
    }
    for method in [Method::Post, Method::Put, Method::Delete] {
        assert!(
            a.check(method, &at("/Items")).is_err(),
            "{}",
            method.as_str()
        );
    }
}

/// The label of the rule a GET goes by, or how it was refused.
fn rule_of(a: &Allowlist, url: &str) -> Result<String, OutboundError> {
    a.check(Method::Get, url)
        .map(|(_, rule)| rule.label.clone())
}

/// A media server at the root of a host and a manager under a path of the same host keep a
/// rule each, in either order: a call goes by the rule whose path holds it, the longest one
/// first. A path that only begins like another is not below it.
#[test]
fn a_rule_is_listed_by_its_address_and_the_longest_path_wins() {
    let server = || {
        Calls::default()
            .get("/library/sections")
            .get("/status/sessions")
    };
    let manager = || Calls::default().act("search", Method::Post, "/api/v3/command");
    for server_first in [true, false] {
        let mut a = Allowlist::default();
        let mut list = |label: &str, address: &str, calls: Calls| {
            a.allow_integration(label, address, calls).unwrap();
        };
        if server_first {
            list("plex", "https://media.example.org/", server());
            list("radarr", "https://media.example.org/radarr", manager());
        } else {
            list("radarr", "https://media.example.org/radarr/", manager());
            list("plex", "https://media.example.org", server());
        }
        let at = |path: &str| format!("https://media.example.org{path}");
        assert_eq!(rule_of(&a, &at("/library/sections")).unwrap(), "plex");
        assert_eq!(rule_of(&a, &at("/radarr/api/v3/movie")).unwrap(), "radarr");
        assert_eq!(rule_of(&a, &at("/radarr")).unwrap(), "radarr");
        // The server takes its declared GETs alone, the manager every GET and its act only
        // while it may act.
        for path in ["/library/sections/1/refresh", "/radarrx/api", "/"] {
            match a.check(Method::Get, &at(path)) {
                Err(OutboundError::NotListed { label, .. }) => assert_eq!(label, "plex"),
                other => panic!("{path}: {:?}", other.map(|(_, r)| r.label.clone())),
            }
        }
        assert!(matches!(
            a.check(Method::Post, &at("/radarr/api/v3/command")),
            Err(OutboundError::NotAllowed { .. })
        ));
        assert!(matches!(
            a.check(Method::Post, &at("/api/v3/command")),
            Err(OutboundError::NotListed { .. })
        ));
        let keys: Vec<&str> = a.hosts().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            [
                "https://media.example.org:443",
                "https://media.example.org:443/radarr"
            ]
        );
    }
}

/// Two media servers behind one host, each under its own path: each takes its own GETs.
/// A path under neither is refused. So is every other host.
#[test]
fn two_servers_behind_one_host_keep_their_own_calls() {
    let mut a = Allowlist::default();
    a.allow_integration(
        "plex",
        "https://media.example.org/plex",
        Calls::default().get("/library/sections"),
    )
    .unwrap();
    a.allow_integration(
        "jellyfin",
        "https://media.example.org/jellyfin",
        Calls::default().get("/Items").get("/Sessions"),
    )
    .unwrap();
    let at = |path: &str| format!("https://media.example.org{path}");
    assert_eq!(rule_of(&a, &at("/plex/library/sections")).unwrap(), "plex");
    assert_eq!(
        rule_of(&a, &at("/jellyfin/Items?Limit=1")).unwrap(),
        "jellyfin"
    );
    assert_eq!(rule_of(&a, &at("/jellyfin/Sessions")).unwrap(), "jellyfin");
    for (path, label) in [
        ("/plex/Items", "plex"),
        ("/jellyfin/library/sections", "jellyfin"),
        ("/jellyfin/Videos/abc/stream", "jellyfin"),
    ] {
        match a.check(Method::Get, &at(path)) {
            Err(OutboundError::NotListed { label: l, .. }) => assert_eq!(l, label, "{path}"),
            other => panic!("{path}: {:?}", other.map(|(_, r)| r.label.clone())),
        }
    }
    // Under neither path: refused. The origin still counts as listed.
    assert!(matches!(
        a.check(Method::Get, &at("/library/sections")),
        Err(OutboundError::NotListed { .. })
    ));
    assert_eq!(
        a.listed(&at("/jellyfin/Items")).as_deref(),
        Some("jellyfin")
    );
    assert!(a.listed(&at("/elsewhere")).is_some());
    assert!(matches!(
        a.check(
            Method::Get,
            "https://other.example.org/plex/library/sections"
        ),
        Err(OutboundError::HostNotAllowed { .. })
    ));
}

/// A GET whose other keys are listed takes those keys alone. A host that takes no GET takes
/// none. Its listed POSTs still pass.
#[test]
fn a_get_can_be_held_to_its_keys_and_a_host_can_take_no_get() {
    const KEY: &str = "0123456789abcdef0123456789abcdef";
    let mut a = Allowlist::default();
    a.allow_integration(
        "usenet",
        "http://10.0.0.70:8080",
        Calls::default().get_only(
            "/api",
            "mode",
            &["queue", "history"],
            &["output", "apikey", "limit"],
        ),
    )
    .unwrap();
    a.allow_integration(
        "rpc",
        "http://10.0.0.71:6789",
        Calls::default()
            .read_when(Method::Post, "/jsonrpc", "/method", &["listgroups"])
            .no_get(),
    )
    .unwrap();
    let at = |rest: &str| format!("http://10.0.0.70:8080/api?{rest}");
    for rest in [
        format!("mode=queue&output=json&apikey={KEY}"),
        format!("mode=history&output=json&limit=100&apikey={KEY}"),
        "mode=queue".to_string(),
    ] {
        assert!(a.check(Method::Get, &at(&rest)).is_ok(), "{rest}");
    }
    for rest in [
        format!("mode=queue&name=delete&value=all&apikey={KEY}"),
        format!("mode=history&name=delete&value=all&apikey={KEY}"),
        format!("mode=queue&output=json&apikey={KEY}&del_files=1"),
        format!("mode=shutdown&apikey={KEY}"),
        format!("mode=queue&mode=pause&apikey={KEY}"),
        format!("output=json&apikey={KEY}"),
    ] {
        let err = a.check(Method::Get, &at(&rest)).unwrap_err();
        assert!(
            matches!(err, OutboundError::NotListed { .. }),
            "{rest} must be refused, got {err:?}"
        );
        assert!(!err.to_string().contains(KEY), "{err}");
    }
    // The listed POST passes; no GET does, a method in the path included.
    let rpc = "http://10.0.0.71:6789/jsonrpc";
    let listgroups = Body::Json(r#"{"method": "listgroups", "params": [0]}"#.to_string());
    assert!(a.check_call(Method::Post, rpc, &listgroups).is_ok());
    for url in [
        rpc.to_string(),
        format!("{rpc}/shutdown"),
        format!("{rpc}/listgroups"),
        "http://10.0.0.71:6789/".to_string(),
    ] {
        assert!(
            matches!(
                a.check(Method::Get, &url),
                Err(OutboundError::NotListed { .. })
            ),
            "{url}"
        );
    }
    assert!(
        a.check_call(
            Method::Post,
            &format!("{rpc}/shutdown"),
            &Body::Json(r#"{"method": "listgroups"}"#.to_string())
        )
        .is_err()
    );
}

#[test]
fn a_listed_read_passes_only_with_the_body_that_makes_it_a_read() {
    let a = integrations();
    let records = format!("{WORKER}/api/v2/cruddb");
    for body in [
        r#"{"data": {"collection": "LibrarySettingsJSONDB", "mode": "getAll"}}"#,
        r#"{"data": {"collection": "FileJSONDB", "mode": "getById", "docID": "/q/a.mkv"}}"#,
    ] {
        let (url, rule) = a
            .check_call(Method::Post, &records, &json(body))
            .unwrap_or_else(|e| panic!("{body}: {e}"));
        assert_eq!(
            (url.path(), rule.label.as_str()),
            ("/api/v2/cruddb", "worker")
        );
    }
    // The same path with another mode, with the mode somewhere else, with a mode that is
    // no text, with a body that is no JSON, as a form, or with no body at all.
    for body in [
        json(r#"{"data": {"collection": "FileJSONDB", "mode": "removeOne", "docID": "/q/a.mkv"}}"#),
        json(r#"{"data": {"collection": "FileJSONDB", "mode": "update"}}"#),
        json(r#"{"data": {"collection": "FileJSONDB"}}"#),
        json(r#"{"mode": "getAll"}"#),
        json(r#"{"data": {"mode": ["getAll"]}}"#),
        json(r#"{"data": {"mode": "getall"}}"#),
        json(r#"{"data": {"mode": "getAll"}"#),
        json(""),
        Body::Form(vec![("mode".to_string(), "getAll".to_string())]),
        Body::None,
    ] {
        let err = a.check_call(Method::Post, &records, &body).unwrap_err();
        assert!(
            matches!(err, OutboundError::NotListed { .. }),
            "{body:?} must be refused, got {err:?}"
        );
    }
    // A mode written twice is refused, whichever comes first: the services differ on
    // which one counts. So is a member written twice anywhere in the body.
    let twice = |first: &str, last: &str| {
        json(&format!(
            r#"{{"data": {{"mode": "{first}", "collection": "FileJSONDB", "mode": "{last}"}}}}"#
        ))
    };
    for (first, last) in [
        ("getAll", "removeOne"),
        ("removeOne", "getAll"),
        ("getAll", "getAll"),
    ] {
        assert!(
            a.check_call(Method::Post, &records, &twice(first, last))
                .is_err(),
            "{first} then {last}"
        );
    }
    assert!(
        a.check_call(
            Method::Post,
            &records,
            &json(r#"{"data": {"collection": "LibrarySettingsJSONDB", "mode": "getAll", "x": {"y": 1, "y": 2}}}"#)
        )
        .is_err()
    );
    // A read by its path alone takes any body; the method and the path are exact.
    let tables = format!("{WORKER}/api/v2/client/status-tables");
    assert!(
        a.check_call(Method::Post, &tables, &json(r#"{"data": {"start": 0}}"#))
            .is_ok()
    );
    assert!(a.check(Method::Post, &tables).is_ok());
    for (method, url) in [
        (Method::Put, records.clone()),
        (Method::Delete, records.clone()),
        (Method::Post, format!("{records}/")),
        (Method::Post, format!("{WORKER}/api/v2/cruddb2")),
        (
            Method::Post,
            format!("{WORKER}/api/v2/client/status-tables/x"),
        ),
        (Method::Post, format!("{WORKER}/api/v2/delete-file")),
        (Method::Put, tables.clone()),
    ] {
        let body = json(r#"{"data": {"mode": "getAll"}}"#);
        assert!(
            matches!(
                a.check_call(method, &url, &body),
                Err(OutboundError::NotListed { .. })
            ),
            "{} {url}",
            method.as_str()
        );
    }
    // A listed read of one host is none of another's.
    assert!(
        a.check_call(
            Method::Post,
            "http://192.0.2.61:7979/api/v2/cruddb",
            &json(r#"{"data": {"mode": "getAll"}}"#)
        )
        .is_err()
    );
}

#[test]
fn an_act_passes_only_while_the_access_is_act() {
    let a = integrations();
    let stage = format!("{BRIDGE}/stage");
    let body = json(r#"{"path": "/library/a.mkv", "flow": "convert"}"#);
    // The access is `read` until its switch says otherwise.
    let switch = a.switch("bridge").expect("the bridge is listed");
    assert_eq!(switch.get(), Access::Read);
    let err = a.check_call(Method::Post, &stage, &body).unwrap_err();
    assert!(matches!(err, OutboundError::NotAllowed { .. }), "{err:?}");
    assert!(err.to_string().contains("may only read bridge"), "{err}");
    // Reading stays possible either way.
    assert!(
        a.check(Method::Get, &format!("{stage}?path=/library/a.mkv"))
            .is_ok()
    );

    switch.set(Access::Act);
    let (url, rule) = a.check_call(Method::Post, &stage, &body).unwrap();
    assert_eq!((url.path(), rule.label.as_str()), ("/stage", "bridge"));
    assert_eq!(rule.access.get(), Access::Act);
    // Acting never turns a host into a free one: any other call is refused as before, and
    // says that it is not a listed call.
    for (method, url) in [
        (Method::Post, format!("{BRIDGE}/strip-subs")),
        (Method::Post, format!("{BRIDGE}/stage/now")),
        (Method::Put, stage.clone()),
        (Method::Delete, stage.clone()),
        (Method::Delete, format!("{BRIDGE}/jobs")),
    ] {
        let err = a.check_call(method, &url, &body).unwrap_err();
        assert!(
            matches!(err, OutboundError::NotListed { .. }),
            "{} {url}: {err:?}",
            method.as_str()
        );
    }
    // One integration's access is not another's: the worker has no act, and an arr none.
    assert!(a.switch("worker").is_none() && a.switch("radarr").is_none());
    assert!(
        a.check_call(Method::Post, &format!("{WORKER}/stage"), &body)
            .is_err()
    );
    assert!(
        a.check_call(Method::Post, "http://192.0.2.61:7979/api/v3/command", &body)
            .is_err()
    );

    // Taken back, it is refused again with the next call.
    switch.set(Access::Read);
    assert!(matches!(
        a.check_call(Method::Post, &stage, &body),
        Err(OutboundError::NotAllowed { .. })
    ));
    // Only a host with an act has a switch: a provider's label or a service's finds none.
    let mut with_service = integrations();
    with_service
        .allow_service("meilisearch", "http://10.0.0.9:7700")
        .unwrap();
    for label in ["meilisearch", "tmdb", "tvdb", "nothing"] {
        assert!(with_service.switch(label).is_none(), "{label}");
    }
    // An integration reached under a path is called below that path, and nowhere else.
    let mut below = Allowlist::default();
    below
        .allow_integration(
            "bridge",
            "https://media.example.org/bridge/",
            Calls::default()
                .act("send", Method::Post, "/stage")
                .read(Method::Post, "/sign-in"),
        )
        .unwrap();
    below.switch("bridge").unwrap().set(Access::Act);
    for (url, passes) in [
        ("https://media.example.org/bridge/stage", true),
        ("https://media.example.org/bridge/sign-in", true),
        ("https://media.example.org/stage", false),
        ("https://media.example.org/other/stage", false),
        ("https://media.example.org/bridge/stage/x", false),
    ] {
        assert_eq!(below.check(Method::Post, url).is_ok(), passes, "{url}");
    }
    assert_eq!(Access::parse(" Act "), Some(Access::Act));
    assert_eq!(Access::parse("read"), Some(Access::Read));
    assert_eq!(Access::parse("write"), None);
    assert_eq!(
        (Access::Read.as_str(), Access::Act.as_str()),
        ("read", "act")
    );
}

/// One path that reads and writes (a record store): its reads pass with their bodies, one
/// narrow act passes with its own body while the access is `act`, and any other body is
/// no listed call at all, whatever the access.
#[test]
fn an_act_with_a_body_rule_is_that_act_and_nothing_else() {
    let mut a = Allowlist::default();
    a.allow_integration(
        "worker",
        WORKER,
        Calls::default()
            .read_when(Method::Post, "/api/v2/cruddb", "/data/mode", &["getAll"])
            .read_with(
                Method::Post,
                "/api/v2/reports",
                BodyRule::new().text("/data/footprintId"),
            )
            .read_with(
                Method::Post,
                "/api/v2/stats",
                BodyRule::new().only("/data", &[]),
            )
            .act_with(
                "change",
                Method::Post,
                "/api/v2/cruddb",
                BodyRule::new()
                    .one_of("/data/collection", &["LibrarySettingsJSONDB"])
                    .one_of("/data/mode", &["update"])
                    .text("/data/docID")
                    .only("/data/obj", &["flowId"])
                    .text("/data/obj/flowId"),
            )
            .act_with(
                "change",
                Method::Post,
                "/api/v2/toggle",
                BodyRule::new()
                    .equals("/data/auto", serde_json::json!(false))
                    .flag("/data/status")
                    .texts("/data/files", 1),
            ),
    )
    .unwrap();
    let at = |path: &str| format!("{WORKER}{path}");
    let assign = r#"{"data": {"collection": "LibrarySettingsJSONDB", "mode": "update", "docID": "lib1", "obj": {"flowId": "f2"}}}"#;
    let switch = a.switch("worker").expect("a host with an act has a switch");
    // Under read access the act is known and not allowed; the reads pass.
    assert!(matches!(
        a.check_call(Method::Post, &at("/api/v2/cruddb"), &json(assign)),
        Err(OutboundError::NotAllowed { .. })
    ));
    for (path, body) in [
        ("/api/v2/cruddb", r#"{"data": {"mode": "getAll"}}"#),
        ("/api/v2/reports", r#"{"data": {"footprintId": "QEdffjm"}}"#),
        ("/api/v2/stats", r#"{"data": {}}"#),
    ] {
        assert!(
            a.check_call(Method::Post, &at(path), &json(body)).is_ok(),
            "{path} {body}"
        );
    }
    switch.set(Access::Act);
    assert!(
        a.check_call(Method::Post, &at("/api/v2/cruddb"), &json(assign))
            .is_ok()
    );
    let toggle = r#"{"data": {"auto": false, "status": true, "files": ["/q/a.mkv"]}}"#;
    assert!(
        a.check_call(Method::Post, &at("/api/v2/toggle"), &json(toggle))
            .is_ok()
    );
    // Every other body on the same paths is no listed call, even under act: another
    // collection, another mode, more than the flow, no document, a flag that is text,
    // `auto` true, two files, an empty footprint, a question with a filter.
    for (path, body) in [
        (
            "/api/v2/cruddb",
            assign.replace("LibrarySettingsJSONDB", "FileJSONDB"),
        ),
        (
            "/api/v2/cruddb",
            assign.replace("\"update\"", "\"removeOne\""),
        ),
        (
            "/api/v2/cruddb",
            assign.replace(
                "{\"flowId\": \"f2\"}",
                "{\"flowId\": \"f2\", \"folder\": \"/\"}",
            ),
        ),
        (
            "/api/v2/cruddb",
            assign.replace("\"docID\": \"lib1\", ", ""),
        ),
        ("/api/v2/cruddb", assign.replace("\"f2\"", "\"\"")),
        ("/api/v2/toggle", toggle.replace("true", "\"true\"")),
        ("/api/v2/toggle", toggle.replace("false", "true")),
        (
            "/api/v2/toggle",
            toggle.replace("[\"/q/a.mkv\"]", "[\"/q/a.mkv\", \"/q/b.mkv\"]"),
        ),
        ("/api/v2/toggle", toggle.replace("[\"/q/a.mkv\"]", "[]")),
        (
            "/api/v2/reports",
            r#"{"data": {"footprintId": " "}}"#.to_string(),
        ),
        (
            "/api/v2/reports",
            r#"{"data": {"footprintId": 7}}"#.to_string(),
        ),
        (
            "/api/v2/stats",
            r#"{"data": {"libraryId": "lib1"}}"#.to_string(),
        ),
        ("/api/v2/stats", r#"{"data": []}"#.to_string()),
        ("/api/v2/stats", "{}".to_string()),
    ] {
        let err = a
            .check_call(Method::Post, &at(path), &json(&body))
            .unwrap_err();
        assert!(
            matches!(err, OutboundError::NotListed { .. }),
            "{path} {body}: {err:?}"
        );
    }
    // No body, or a form, passes no rule.
    for body in [Body::None, Body::Form(vec![("data".into(), "{}".into())])] {
        assert!(
            a.check_call(Method::Post, &at("/api/v2/stats"), &body)
                .is_err()
        );
    }
    switch.set(Access::Read);
    assert!(matches!(
        a.check_call(Method::Post, &at("/api/v2/toggle"), &json(toggle)),
        Err(OutboundError::NotAllowed { .. })
    ));
}

/// A declared path names a record by its number (`{id}`) or any one segment (`{any}`); an
/// act can take its choices in the query, each listed key once and nothing else; a body
/// rule reads ids, whole numbers and lists of objects.
#[test]
fn patterns_queries_and_lists_say_exactly_what_passes() {
    assert!(path_is("/q/{id}", "/q/7") && path_is("/q/{id}", "/q/1234567890"));
    for path in [
        "/q/",
        "/q/x",
        "/q/7/8",
        "/q/12345678901",
        "/q/-7",
        "/q",
        "/q/7a",
    ] {
        assert!(!path_is("/q/{id}", path), "{path}");
    }
    assert!(path_is("/c/{any}/{id}", "/c/naming/3") && !path_is("/c/{any}/{id}", "/c//3"));
    assert!(path_under("/b", "/b/x") && path_under("/b", "/b/"));
    assert!(!path_under("/b", "/b") && !path_under("/b", "/bx/y"));
    assert!(path_under("/m/{id}", "/m/3/folder") && !path_under("/m/{id}", "/m/x/folder"));

    let mut a = Allowlist::default();
    a.allow_integration(
        "m",
        "http://10.0.0.5:7878",
        Calls::default()
            .act_query(
                "remove",
                Method::Delete,
                "/q/{id}",
                &[
                    ("a", QueryValue::OneOf(&["true", "false"])),
                    ("b", QueryValue::OneOf(&["true"])),
                ],
            )
            .act_with(
                "change",
                Method::Post,
                "/c",
                BodyRule::new()
                    .only("", &["name", "ids", "files"])
                    .ids("/ids", 2)
                    .each(
                        "/files",
                        2,
                        BodyRule::new()
                            .only("", &["path", "n"])
                            .text("/path")
                            .whole("/n"),
                    ),
            )
            .never("/m/{id}"),
    )
    .unwrap();
    let at = |rest: &str| format!("http://10.0.0.5:7878{rest}");
    let delete = |rest: &str| a.check_call(Method::Delete, &at(rest), &Body::None);
    let post = |body: serde_json::Value| {
        a.check_call(Method::Post, &at("/c"), &Body::Json(body.to_string()))
    };
    let good = serde_json::json!({"name": "x", "ids": [1, 2], "files": [{"path": "/a", "n": 0}]});
    // Under read access the acts are known and not allowed.
    assert!(matches!(
        delete("/q/7?a=true&b=true"),
        Err(OutboundError::NotAllowed { .. })
    ));
    assert!(matches!(
        post(good.clone()),
        Err(OutboundError::NotAllowed { .. })
    ));
    a.switch("m").unwrap().set(Access::Act);
    for rest in ["/q/7?a=true&b=true", "/q/12?b=true&a=false"] {
        assert!(delete(rest).is_ok(), "{rest}");
    }
    assert!(post(good).is_ok());
    for rest in [
        "/q/7",
        "/q/7?a=true",
        "/q/7?a=true&b=false",
        "/q/7?a=true&b=true&c=1",
        "/q/7?a=true&a=false&b=true",
        "/q/7?a=TRUE&b=true",
        "/q/x?a=true&b=true",
        "/q?a=true&b=true",
    ] {
        assert!(
            matches!(delete(rest), Err(OutboundError::NotListed { .. })),
            "{rest}"
        );
    }
    for body in [
        serde_json::json!({"ids": [1, 2, 3], "files": [{"path": "/a", "n": 0}]}),
        serde_json::json!({"ids": [0], "files": [{"path": "/a", "n": 0}]}),
        serde_json::json!({"ids": [1.0], "files": [{"path": "/a", "n": 0}]}),
        serde_json::json!({"ids": [], "files": [{"path": "/a", "n": 0}]}),
        serde_json::json!({"ids": [1], "files": []}),
        serde_json::json!({"ids": [1], "files": [{"path": "/a"}]}),
        serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": -1}]}),
        serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": 1, "x": 2}]}),
        serde_json::json!({"ids": [1], "files": [{"path": " ", "n": 1}]}),
        serde_json::json!({"ids": [1], "files": [1]}),
        serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": 0}, {"path": "/b", "n": 0}, {"path": "/c", "n": 0}]}),
        serde_json::json!({"ids": [1], "files": [{"path": "/a", "n": 0}], "more": true}),
    ] {
        assert!(
            matches!(post(body.clone()), Err(OutboundError::NotListed { .. })),
            "{body}"
        );
    }
    // A never path with a number is refused for any method; the list beside it is not.
    for method in [Method::Get, Method::Post, Method::Put, Method::Delete] {
        assert!(matches!(
            a.check(method, &at("/m/7")),
            Err(OutboundError::NotListed { .. })
        ));
    }
    assert!(a.check(Method::Get, &at("/m")).is_ok());
    assert!(a.check(Method::Get, &at("/m/lookup")).is_ok());
    // A call can carry a limit of its own.
    let call = OutRequest::get(at("/m")).timeout(Duration::from_secs(120));
    assert_eq!(call.timeout, Some(Duration::from_secs(120)));
    assert_eq!(OutRequest::get(at("/m")).timeout, None);
}

/// A media server that takes a command on GET: the act passes only while the access is `act`,
/// and it is that act. The GETs that read pass whatever the access.
#[test]
fn a_get_act_is_refused_under_read_and_passes_under_act() {
    let mut a = Allowlist::default();
    let calls = Calls::default().get("/library/sections").act(
        "scan",
        Method::Get,
        "/library/sections/{id}/refresh",
    );
    check_calls(&calls).unwrap();
    a.allow_integration("plex", "http://10.0.0.40:32400", calls)
        .unwrap();
    let scan = "http://10.0.0.40:32400/library/sections/2/refresh";
    assert!(matches!(
        a.classify(Method::Get, scan, &Body::None),
        Err(OutboundError::NotAllowed { .. })
    ));
    let read = "http://10.0.0.40:32400/library/sections";
    assert_eq!(
        a.classify(Method::Get, read, &Body::None).unwrap().2,
        Class::Read
    );
    a.switch("plex").unwrap().set(Access::Act);
    assert_eq!(
        a.classify(Method::Get, scan, &Body::None).unwrap().2,
        Class::Act("scan")
    );
    // Another GET is no act and no declared read.
    assert!(matches!(
        a.classify(
            Method::Get,
            "http://10.0.0.40:32400/library/sections/2/all",
            &Body::None
        ),
        Err(OutboundError::NotListed { .. })
    ));
    // A GET act with no GET declared beside it would leave every other GET free.
    let alone = Calls::default().act("add", Method::Get, "/api");
    assert!(check_calls(&alone).is_err());
}

/// A query value can be one of a list, text, a number or a flag.
#[test]
fn an_act_s_query_values_are_held_to_their_kinds() {
    let mut a = Allowlist::default();
    a.allow_integration(
        "usenet",
        "http://10.0.0.70:8080",
        Calls::default()
            .get_only("/api", "mode", &["queue"], &["apikey", "output"])
            .act_query(
                "add",
                Method::Get,
                "/api",
                &[
                    ("mode", QueryValue::OneOf(&["addurl"])),
                    ("name", QueryValue::Text),
                    ("priority", QueryValue::Number),
                    ("paused", QueryValue::Flag),
                ],
            ),
    )
    .unwrap();
    a.switch("usenet").unwrap().set(Access::Act);
    let at = |q: &str| format!("http://10.0.0.70:8080/api?{q}");
    let class = |q: &str| {
        a.classify(Method::Get, &at(q), &Body::None)
            .map(|(_, _, c)| c)
    };
    assert_eq!(
        class("mode=addurl&name=x&priority=2&paused=true").unwrap(),
        Class::Act("add")
    );
    assert_eq!(
        class("mode=addurl&name=x&priority=0&paused=0").unwrap(),
        Class::Act("add")
    );
    for refused in [
        "mode=addurl&name=&priority=2&paused=true",
        "mode=addurl&name=x&priority=-1&paused=true",
        "mode=addurl&name=x&priority=2&paused=yes",
        "mode=addurl&name=x&priority=2",
        "mode=addurl&name=x&priority=2&paused=true&extra=1",
    ] {
        assert!(class(refused).is_err(), "{refused}");
    }
    assert_eq!(class("mode=queue&output=json").unwrap(), Class::Read);
}

/// A file upload is an act of its own parts, each once and no other.
#[test]
fn a_multipart_body_passes_only_as_the_act_of_its_parts() {
    let mut a = Allowlist::default();
    a.allow_integration(
        "torrents",
        "http://10.0.0.62:8080",
        Calls::default().act_parts(
            "add",
            Method::Post,
            "/api/v2/torrents/add",
            &["torrents", "category"],
        ),
    )
    .unwrap();
    a.switch("torrents").unwrap().set(Access::Act);
    let part = |name: &str, bytes: &[u8]| Part {
        name: name.to_string(),
        file_name: (name == "torrents").then(|| "a.torrent".to_string()),
        content_type: None,
        bytes: bytes.to_vec(),
    };
    let add = "http://10.0.0.62:8080/api/v2/torrents/add";
    let body = Body::Multipart(vec![
        part("torrents", b"d8:announce"),
        part("category", b"movies"),
    ]);
    assert_eq!(
        a.classify(Method::Post, add, &body).unwrap().2,
        Class::Act("add")
    );
    for parts in [
        vec![part("torrents", b"x")],
        vec![
            part("torrents", b"x"),
            part("category", b"m"),
            part("savepath", b"/"),
        ],
        vec![part("torrents", b"x"), part("torrents", b"y")],
    ] {
        assert!(
            a.classify(Method::Post, add, &Body::Multipart(parts))
                .is_err()
        );
    }
    assert!(
        a.classify(Method::Post, add, &Body::Json("{}".into()))
            .is_err()
    );
    // On the wire: a boundary no part holds, each part with its name, then the end.
    let tricky = vec![part("torrents", b"--singularr-part-0 inside")];
    let (kind, bytes) = multipart(&tricky);
    assert_eq!(kind, "multipart/form-data; boundary=singularr-part-1");
    let text = String::from_utf8(bytes).unwrap();
    let head = "--singularr-part-1\r\nContent-Disposition: form-data; name=\"torrents\"; filename=\"a.torrent\"\r\n\r\n";
    assert!(text.starts_with(head), "{text}");
    assert!(text.ends_with("\r\n--singularr-part-1--\r\n"), "{text}");
}

/// The waits: full jitter under a doubling ceiling. A `Retry-After` in seconds, up to a minute.
#[test]
fn the_waits_of_a_read_sent_again_are_bounded() {
    let retry = HttpProfile::DEFAULT.retry;
    assert_eq!(retry.delay(0, 1.0), Duration::from_millis(500));
    assert_eq!(retry.delay(2, 1.0), Duration::from_secs(2));
    assert_eq!(retry.delay(9, 1.0), Duration::from_secs(8));
    assert_eq!(retry.delay(3, 0.0), Duration::ZERO);
    assert_eq!(retry_after(Some("3")), Some(Duration::from_secs(3)));
    assert_eq!(retry_after(Some("7200")), Some(RETRY_AFTER_MAX));
    assert_eq!(retry_after(Some("Wed, 21 Oct 2026 07:28:00 GMT")), None);
    assert_eq!(retry_after(None), None);
}

/// Every error has its class: what a read and an act may conclude from it.
#[test]
fn every_error_has_its_place_in_the_taxonomy() {
    use crate::error::{ConnectorError, TIMED_OUT, UNREACHABLE};
    let transport = |m: &str| OutboundError::Transport {
        host: "h".into(),
        message: m.into(),
    };
    let late = transport(TIMED_OUT).taxonomy();
    assert!(!late.retry_read() && late.act_unknown() && late.class() == "timeout");
    let down = transport(UNREACHABLE).taxonomy();
    assert!(down.retry_read() && down.act_settled_no() && down.class() == "unreachable");
    let broke = transport("connection reset").taxonomy();
    assert!(broke.retry_read() && broke.act_unknown());
    let status = |status: u16| {
        OutboundError::Status {
            host: "h".into(),
            path: "/p".into(),
            status,
        }
        .taxonomy()
    };
    assert!(status(503).retry_read() && status(503).act_unknown());
    assert!(!status(500).retry_read());
    assert!(matches!(status(429), ConnectorError::RateLimited { .. }));
    assert_eq!(status(401).class(), "auth");
    assert_eq!(status(404).class(), "not_found");
    assert!(status(422).act_settled_no());
    let refused = OutboundError::HostNotAllowed { host: "h".into() }.taxonomy();
    assert!(refused.act_settled_no() && !refused.retry_read() && refused.class() == "refused");
}
