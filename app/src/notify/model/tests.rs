use super::*;
use serde_json::json;

fn map(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn facts_of(kind: &str, body: Value) -> Vec<(String, String)> {
    facts(kind, &body)
        .into_iter()
        .map(|f| (f.label, f.value))
        .collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(l, v)| (l.to_string(), v.to_string()))
        .collect()
}

#[test]
fn every_type_describes_its_fields_and_its_secrets_are_write_only() {
    assert_eq!(ChannelType::ALL.len(), 8);
    for kind in ChannelType::ALL {
        assert_eq!(ChannelType::parse(kind.as_str()), Some(kind));
        assert_eq!(
            ChannelType::parse(&kind.as_str().to_uppercase()),
            Some(kind)
        );
        assert!(!kind.fields().is_empty(), "{}", kind.as_str());
        assert!(
            kind.fields().iter().any(|f| f.required),
            "{}",
            kind.as_str()
        );
        for f in kind.fields() {
            // A field is a secret exactly when it is drawn as one.
            assert_eq!(f.is_secret(), f.input == Input::Secret, "{}", f.key);
            assert!(!f.label.is_empty() && !f.label.ends_with('.'), "{}", f.key);
            if f.input == Input::Select {
                assert!(!f.options.is_empty(), "{}", f.key);
            }
        }
    }
    assert_eq!(ChannelType::parse("teams"), None);
    // The addresses of Slack, Discord and a webhook are secrets: the path is the key.
    for kind in [
        ChannelType::Slack,
        ChannelType::Discord,
        ChannelType::Webhook,
    ] {
        let url = kind.fields().iter().find(|f| f.key == "url").unwrap();
        assert!(url.is_secret() && url.required && url.shape == Shape::Url);
    }
    let email: Vec<&str> = ChannelType::Email.fields().iter().map(|f| f.key).collect();
    assert_eq!(
        email,
        ["host", "port", "security", "user", "password", "from", "to"]
    );
}

#[test]
fn journal_kinds_map_to_events() {
    let none = json!({});
    for (kind, body, event) in [
        ("request_added", none.clone(), Some("request.created")),
        (
            "request_status_changed",
            json!({"to": "approved"}),
            Some("request.approved"),
        ),
        (
            "request_status_changed",
            json!({"to": "declined"}),
            Some("request.declined"),
        ),
        (
            "request_status_changed",
            json!({"to": "available"}),
            Some("request.available"),
        ),
        (
            "request_status_changed",
            json!({"to": "failed"}),
            Some("request.failed"),
        ),
        ("request_status_changed", json!({"to": "pending"}), None),
        ("request_status_changed", none.clone(), None),
        ("download_added", none.clone(), Some("download.added")),
        (
            "download_completed",
            none.clone(),
            Some("download.completed"),
        ),
        ("download_stuck", none.clone(), Some("download.attention")),
        ("download_orphaned", none.clone(), Some("download.orphaned")),
        ("download_removed", none.clone(), None),
        (
            "title_added",
            json!({"new_to_catalog": true}),
            Some("title.added"),
        ),
        ("title_added", json!({"new_to_catalog": false}), None),
        ("copy_added", none.clone(), Some("file.added")),
        (
            "copy_replaced",
            json!({"score_delta": 80}),
            Some("file.upgraded"),
        ),
        ("copy_replaced", json!({"score_delta": -5}), None),
        ("copy_replaced", none.clone(), None),
        ("copy_removed", none.clone(), Some("file.removed")),
        ("processing_sent", none.clone(), Some("processing.sent")),
        (
            "processing_finished",
            none.clone(),
            Some("processing.finished"),
        ),
        ("processing_failed", none.clone(), Some("processing.failed")),
        ("processing_stuck", none.clone(), Some("processing.stuck")),
        (
            "engine_down",
            json!({"first_contact": true}),
            Some("integration.down"),
        ),
        (
            "engine_up",
            json!({"first_contact": false}),
            Some("integration.up"),
        ),
        ("engine_up", json!({"first_contact": true}), None),
        (
            "integration_access_changed",
            none.clone(),
            Some("integration.access"),
        ),
        ("setting_changed", none.clone(), None),
        ("notification_channel_added", none.clone(), None),
    ] {
        assert_eq!(event_of(kind, &body), event, "{kind} {body}");
    }
    // Every event is in a group, and every group has events.
    for e in EVENTS {
        assert!(GROUPS.iter().any(|(g, _)| *g == e.group), "{}", e.id);
    }
    for (g, _) in GROUPS {
        assert!(EVENTS.iter().any(|e| e.group == *g), "{g}");
    }
    assert_eq!(label_of("file.upgraded"), "File upgraded");
    assert_eq!(group_of("file.upgraded"), "Library");
    assert_eq!(label_of(TEST), "Test message");
}

#[test]
fn settings_are_checked_by_their_type() {
    // Slack: the address is required, and must be http or https.
    let err = check_settings(ChannelType::Slack, Some(&map(json!({}))), None).unwrap_err();
    assert_eq!(err["settings.url"], "Required");
    let err = check_settings(
        ChannelType::Slack,
        Some(&map(
            json!({"url": "ftp://hooks.example.com/x", "color": "red"}),
        )),
        None,
    )
    .unwrap_err();
    assert_eq!(err["settings.url"], "Not an http or https address");
    assert_eq!(err["settings.color"], "Not a field of this type");
    let ok = check_settings(
        ChannelType::Slack,
        Some(&map(
            json!({"url": " https://hooks.slack.com/services/T0/B0/xyz "}),
        )),
        None,
    )
    .unwrap();
    assert!(ok.settings.is_empty(), "the address is a secret");
    assert_eq!(
        ok.text("url"),
        Some("https://hooks.slack.com/services/T0/B0/xyz")
    );
    assert_eq!(
        ok.set(ChannelType::Slack),
        BTreeMap::from([("url".to_string(), true)])
    );
    // A secret given as empty text keeps the stored one; null clears an optional one.
    let stored = check_settings(
        ChannelType::Webhook,
        Some(&map(
            json!({"url": "https://example.com/hook", "secret": "s3cret-value"}),
        )),
        None,
    )
    .unwrap();
    let kept = check_settings(
        ChannelType::Webhook,
        Some(&map(json!({"url": "", "secret": ""}))),
        Some(&stored),
    )
    .unwrap();
    assert_eq!(kept, stored);
    let left_out = check_settings(ChannelType::Webhook, Some(&map(json!({}))), Some(&stored));
    assert_eq!(left_out.unwrap(), stored);
    let cleared = check_settings(
        ChannelType::Webhook,
        Some(&map(json!({"secret": null}))),
        Some(&stored),
    )
    .unwrap();
    assert_eq!(cleared.text("secret"), None);
    assert_eq!(cleared.text("url"), Some("https://example.com/hook"));
    assert_eq!(
        cleared.set(ChannelType::Webhook),
        BTreeMap::from([("secret".to_string(), false), ("url".to_string(), true)])
    );
    let err = check_settings(
        ChannelType::Webhook,
        Some(&map(json!({"url": null}))),
        Some(&stored),
    )
    .unwrap_err();
    assert_eq!(err["settings.url"], "Required");

    // Email: the defaults fill the port and the security; the recipients are a list.
    let mail = check_settings(
        ChannelType::Email,
        Some(&map(json!({
            "host": "smtp.example.org",
            "from": "Singularr <singularr@example.org>",
            "to": "owner@example.org, Zoë <zoe@example.net>",
            "user": "singularr",
            "password": "hunter2-password"
        }))),
        None,
    )
    .unwrap();
    assert_eq!(mail.int("port"), Some(587));
    assert_eq!(mail.text("security"), Some("starttls"));
    assert_eq!(
        mail.list("to"),
        ["owner@example.org", "Zoë <zoe@example.net>"]
    );
    assert!(!mail.settings.contains_key("password"));
    assert_eq!(mail.text("password"), Some("hunter2-password"));
    let err = check_settings(
        ChannelType::Email,
        Some(&map(json!({
            "host": "smtp example org",
            "port": "70000",
            "security": "ssl",
            "from": "not an address",
            "to": ["owner@example.org", "nobody"],
            "user": "two words"
        }))),
        None,
    )
    .unwrap_err();
    assert_eq!(
        err,
        Errors::from([
            ("settings.host".to_string(), "Not a host name".to_string()),
            (
                "settings.port".to_string(),
                "A port from 1 to 65535".to_string()
            ),
            (
                "settings.security".to_string(),
                "Not one of the choices".to_string()
            ),
            (
                "settings.from".to_string(),
                "Not an email address".to_string()
            ),
            (
                "settings.to".to_string(),
                "Not an email address: nobody".to_string()
            ),
            ("settings.user".to_string(), "No spaces".to_string()),
        ])
    );
    let typed = check_settings(
        ChannelType::Email,
        Some(&map(json!({
            "host": "smtp.example.org", "port": "465", "security": "TLS",
            "from": "singularr@example.org", "to": ["owner@example.org"]
        }))),
        None,
    )
    .unwrap();
    assert_eq!(typed.int("port"), Some(465));
    assert_eq!(typed.text("security"), Some("tls"));
    let err = check_settings(
        ChannelType::Email,
        Some(&map(
            json!({"host": "smtp.example.org", "from": "a@example.org", "to": []}),
        )),
        None,
    )
    .unwrap_err();
    assert_eq!(err["settings.to"], "Required");

    // ntfy: a topic is one word of letters, digits, - and _.
    let err = check_settings(
        ChannelType::Ntfy,
        Some(&map(json!({"topic": "my topic"}))),
        None,
    )
    .unwrap_err();
    assert_eq!(err["settings.topic"], "Letters, digits, - and _ only");
    let ntfy = check_settings(
        ChannelType::Ntfy,
        Some(&map(json!({"topic": "singularr-alerts"}))),
        None,
    )
    .unwrap();
    assert_eq!(ntfy.text("server"), Some("https://ntfy.sh"));
}

#[test]
fn names_events_and_kinds_are_checked() {
    assert_eq!(check_name("  Ops  ").unwrap(), "Ops");
    assert_eq!(check_name(" ").unwrap_err(), "Required");
    assert_eq!(
        check_name(&"x".repeat(81)).unwrap_err(),
        "At most 80 characters"
    );
    assert_eq!(check_name("a\nb").unwrap_err(), "One line");
    let ids = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        check_events(&ids(&["file.upgraded", "request.created", "file.upgraded"])).unwrap(),
        ["request.created", "file.upgraded"]
    );
    assert_eq!(
        check_events(&ids(&["file.upgraded", "file.renamed"])).unwrap_err(),
        "Not an event: file.renamed"
    );
    assert_eq!(
        check_kinds(&ids(&["show", "movie"])).unwrap(),
        ["movie", "show"]
    );
    assert_eq!(
        check_kinds(&ids(&["music"])).unwrap_err(),
        "Not a kind: music"
    );
    assert!(check_kinds(&[]).unwrap().is_empty());
}

#[test]
fn a_channel_wants_what_it_subscribes_to_in_its_kinds() {
    let at: DateTime<Utc> = "2026-11-23T18:00:00Z".parse().unwrap();
    let notice = |event: &str, kind: Option<&str>| Notice {
        event: event.to_string(),
        seq: Some(9),
        at,
        kind: kind.map(str::to_string),
        summary: String::new(),
        subject: String::new(),
        title: None,
        facts: Vec::new(),
        link: None,
        journal: Value::Null,
        test: false,
    };
    let events = vec!["file.upgraded".to_string(), "integration.down".to_string()];
    let movies = vec!["movie".to_string()];
    assert!(wants(&events, &[], &notice("file.upgraded", Some("show"))));
    assert!(wants(
        &events,
        &movies,
        &notice("file.upgraded", Some("movie"))
    ));
    assert!(!wants(
        &events,
        &movies,
        &notice("file.upgraded", Some("show"))
    ));
    assert!(!wants(&events, &[], &notice("file.added", Some("movie"))));
    // An event about no title reaches a channel narrowed to a kind.
    assert!(wants(&events, &movies, &notice("integration.down", None)));
}

#[test]
fn each_type_has_one_address_and_words_for_it_without_secrets() {
    let checked = |kind, value: Value| check_settings(kind, Some(&map(value)), None).unwrap();
    let slack = checked(
        ChannelType::Slack,
        json!({"url": "https://hooks.slack.com/services/T0/B0/xyz"}),
    );
    assert_eq!(
        target(ChannelType::Slack, &slack).as_deref(),
        Some("https://hooks.slack.com/services/T0/B0/xyz")
    );
    assert_eq!(destination(ChannelType::Slack, &slack), "hooks.slack.com");
    let telegram = checked(
        ChannelType::Telegram,
        json!({"token": "123456:ABC-def_ghi", "chat": "-1001234567890"}),
    );
    assert_eq!(
        target(ChannelType::Telegram, &telegram).as_deref(),
        Some("https://api.telegram.org/bot123456:ABC-def_ghi/sendMessage")
    );
    assert_eq!(
        destination(ChannelType::Telegram, &telegram),
        "Chat -1001234567890"
    );
    let gotify = checked(
        ChannelType::Gotify,
        json!({"server": "https://gotify.example.org/", "token": "AbCdEf123"}),
    );
    assert_eq!(
        target(ChannelType::Gotify, &gotify).as_deref(),
        Some("https://gotify.example.org/message")
    );
    assert_eq!(
        destination(ChannelType::Gotify, &gotify),
        "gotify.example.org"
    );
    let ntfy = checked(
        ChannelType::Ntfy,
        json!({"topic": "alerts", "token": "tk_1"}),
    );
    assert_eq!(
        target(ChannelType::Ntfy, &ntfy).as_deref(),
        Some("https://ntfy.sh")
    );
    assert_eq!(destination(ChannelType::Ntfy, &ntfy), "ntfy.sh/alerts");
    let pushover = checked(
        ChannelType::Pushover,
        json!({"token": "azGDORePK8gMaC0QOYAMyEEuzJnyUi", "user": "uQiRzpo4DXghDmr9QzzfQu27cmVRsG"}),
    );
    assert_eq!(
        target(ChannelType::Pushover, &pushover).as_deref(),
        Some(PUSHOVER_MESSAGES)
    );
    assert_eq!(destination(ChannelType::Pushover, &pushover), "Pushover");
    let email = checked(
        ChannelType::Email,
        json!({"host": "smtp.example.org", "from": "a@example.org", "to": ["b@example.org", "c@example.org", "d@example.org"]}),
    );
    assert_eq!(target(ChannelType::Email, &email), None);
    assert_eq!(
        destination(ChannelType::Email, &email),
        "b@example.org and 2 more"
    );
    for (kind, c) in [
        (ChannelType::Slack, &slack),
        (ChannelType::Telegram, &telegram),
        (ChannelType::Gotify, &gotify),
        (ChannelType::Pushover, &pushover),
    ] {
        let words = destination(kind, c);
        for secret in c.secret_values() {
            assert!(!words.contains(&secret), "{words}");
        }
    }
}

#[test]
fn facts_name_what_matters_of_each_event() {
    let copy = |quality: &str, group: &str| json!({"quality": quality, "group": group, "size_bytes": 3_221_225_472_i64, "edition": null, "score": 120});
    assert_eq!(
        facts_of(
            "copy_replaced",
            json!({"title_id": 7, "season": 1, "episodes": [3], "old": copy("HDTV-1080p", "OLD"), "new": copy("WEBDL-1080p", "GRP"), "score_delta": 80})
        ),
        pairs(&[
            ("Episode", "S01E03"),
            ("Quality", "WEBDL-1080p"),
            ("Size", "3.00 GB"),
            ("Release group", "GRP"),
            ("Replaced", "HDTV-1080p"),
        ])
    );
    assert_eq!(
        facts_of(
            "request_added",
            json!({"requested_by": "Sam", "seasons": [1, 2], "is_4k": true})
        ),
        pairs(&[
            ("Requested by", "Sam"),
            ("Seasons", "1 and 2"),
            ("Quality", "4K")
        ])
    );
    assert_eq!(
        facts_of(
            "download_stuck",
            json!({"name": "Show.S01E03.1080p.WEB-DL-GRP", "size_bytes": 1000, "reason": "No seeds for 3 days"})
        ),
        pairs(&[
            ("Reason", "No seeds for 3 days"),
            ("Release", "Show.S01E03.1080p.WEB-DL-GRP"),
            ("Size", "1000 B")
        ])
    );
    assert_eq!(
        facts_of(
            "processing_finished",
            json!({"flow_label": "Convert to HEVC", "file": "a.mkv", "saved": 540_857_787, "took": 675})
        ),
        pairs(&[
            ("Flow", "Convert to HEVC"),
            ("File", "a.mkv"),
            ("Saved", "516 MB"),
            ("Took", "11 min")
        ])
    );
    assert_eq!(
        facts_of("engine_down", json!({"error": "connection refused"})),
        pairs(&[("Error", "connection refused")])
    );
    assert_eq!(
        facts_of("integration_access_changed", json!({"to": "act"})),
        pairs(&[("Access", "Write access")])
    );
    assert!(facts_of("title_added", json!({"title": "x"})).is_empty());
    assert_eq!(span(691_200), "8 d");
    assert_eq!(span(-5), "0 min");
}
