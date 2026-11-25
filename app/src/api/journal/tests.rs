use super::*;
use serde_json::json;

fn names() -> Names {
    Names {
        integrations: BTreeMap::from([
            ("radarr".to_string(), "Radarr HD".to_string()),
            ("sonarr".to_string(), "Sonarr".to_string()),
            ("qbittorrent".to_string(), "qBittorrent".to_string()),
            ("seerr".to_string(), "Seerr".to_string()),
            ("bridge".to_string(), "The Bridge".to_string()),
        ]),
        titles: BTreeMap::from([(
            7,
            TitleRef {
                id: 7,
                kind: "show".to_string(),
                tmdb_id: Some(1396),
                title: "Breaking Bad".to_string(),
                year: Some(2008),
            },
        )]),
        portal: "Family Requests".to_string(),
    }
}

fn ev(kind: &str, subject_type: &str, subject_id: &str, actor: &str, body: Value) -> Event {
    Event {
        id: 42,
        at: "2026-11-23T18:00:00Z".parse().unwrap(),
        kind: kind.to_string(),
        subject_type: subject_type.to_string(),
        subject_id: subject_id.to_string(),
        actor: actor.to_string(),
        cause: Some(41),
        body,
    }
}

fn copy(quality: &str, group: &str, score: i64) -> Value {
    json!({"copy_id": 3, "relative_path": "S01/e.mkv", "release_name": "Show.S01E01.1080p.WEB-DL-GRP",
           "size_bytes": 3_221_225_472_i64, "lane": "shows-hd", "quality": quality, "group": group,
           "edition": null, "score": score})
}

/// One event of every kind the server writes, with the body its writer gives it.
fn every_kind() -> Vec<Event> {
    let subject = json!({"title_id": 7, "title": "Breaking Bad", "library": "sonarr", "engine": "sonarr", "season": 1, "episodes": [3]});
    let with = |extra: Value| {
        let mut b = subject.clone();
        b.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        b
    };
    let download = json!({"hash": "AB12", "name": "Show.S01E03.1080p.WEB-DL-GRP", "category": "tv",
                          "state": "stalledDL", "progress": 0.4, "size_bytes": 1000, "owner": "sonarr",
                          "owner_source": "queue", "title_id": 7});
    let request = json!({"site": "plex", "seerr_id": 9, "kind": "show", "title": "Breaking Bad", "tmdb_id": 1396,
                         "title_id": 7, "requested_by": "Sam", "status": "pending", "is_4k": false, "seasons": [1, 2]});
    let mut changed = request.clone();
    changed["from"] = json!("pending");
    changed["to"] = json!("approved");
    vec![
        ev(
            "title_added",
            "title",
            "7",
            "sync:sonarr",
            json!({"title_id": 7, "title": "Breaking Bad", "kind": "show", "year": 2008, "tmdb_id": 1396, "library": "sonarr", "engine": "sonarr", "lane": "shows-hd", "new_to_catalog": true}),
        ),
        ev(
            "title_removed",
            "title",
            "7",
            "sync:sonarr",
            json!({"title_id": 7, "title": "Breaking Bad", "kind": "show", "library": "sonarr", "engine": "sonarr", "gone_from_catalog": true}),
        ),
        ev(
            "copy_added",
            "copy",
            "3",
            "sync:sonarr",
            with(
                json!({"copy_id": 3, "quality": "WEBDL-1080p", "group": "GRP", "size_bytes": 3_221_225_472_i64, "score": 120, "protections": []}),
            ),
        ),
        ev(
            "copy_replaced",
            "copy",
            "4",
            "sync:sonarr",
            with(
                json!({"old": copy("HDTV-1080p", "OLD", 40), "new": copy("WEBDL-1080p", "GRP", 120), "score_delta": 80}),
            ),
        ),
        ev(
            "copy_removed",
            "copy",
            "3",
            "sync:sonarr",
            with(copy("WEBDL-1080p", "GRP", 120)),
        ),
        ev(
            "copy_changed",
            "copy",
            "3",
            "sync:sonarr",
            with(
                json!({"changed": ["size_bytes", "score"], "old": copy("WEBDL-1080p", "GRP", 120), "new": copy("WEBDL-1080p", "GRP", 130)}),
            ),
        ),
        ev(
            "score_changed",
            "copy",
            "3",
            "policy",
            json!({"title_id": 7, "title": "Breaking Bad", "library": "sonarr", "engine": "sonarr", "lane": "shows-hd", "release_name": "x", "old_score": 120, "new_score": 150, "delta": 30}),
        ),
        ev(
            "untracked_file_found",
            "title",
            "7",
            "scan:movies",
            json!({"title_id": 7, "title": "Breaking Bad", "engine": "radarr", "library": "radarr", "file_name": "Movie {edition-Director's Cut}.mkv", "size_bytes": 10, "edition": "directors-cut", "edition_label": "Director's Cut"}),
        ),
        ev(
            "untracked_file_gone",
            "title",
            "7",
            "scan:movies",
            json!({"title_id": 7, "engine": "radarr", "file_name": "Movie.mkv", "edition": "theatrical"}),
        ),
        ev(
            "download_added",
            "download",
            "AB12",
            "sync:qbittorrent",
            download.clone(),
        ),
        ev(
            "download_completed",
            "download",
            "AB12",
            "sync:qbittorrent",
            download.clone(),
        ),
        ev("download_stuck", "download", "AB12", "sync:qbittorrent", {
            let mut d = download.clone();
            d["reason"] = json!("No seeds for 3 days");
            d
        }),
        ev(
            "download_orphaned",
            "download",
            "AB12",
            "sync:qbittorrent",
            {
                let mut d = download.clone();
                d["reason"] = json!("No manager's queue or grab history has it");
                d
            },
        ),
        ev(
            "download_removed",
            "download",
            "AB12",
            "sync:qbittorrent",
            download,
        ),
        ev(
            "engine_down",
            "engine",
            "radarr",
            "sync:radarr",
            json!({"engine": "radarr", "pass": "titles", "error": "connection refused", "first_contact": false}),
        ),
        ev(
            "engine_up",
            "engine",
            "radarr",
            "sync:radarr",
            json!({"engine": "radarr", "pass": "titles", "version": "6.0.4", "first_contact": true}),
        ),
        ev(
            "integration_access_changed",
            "engine",
            "radarr",
            "user:owner@example.org",
            json!({"integration": "radarr", "name": "Radarr HD", "from": "read", "to": "act"}),
        ),
        ev(
            "processing_sent",
            "title",
            "7",
            "user:owner@example.org",
            json!({"title_id": 7, "title": "Breaking Bad", "kind": "show", "tmdb_id": 1396, "file": "Breaking Bad - S01E03 - And the Bag's in the River.mkv", "flow": "convert", "flow_label": "Convert to HEVC", "mode": "convert-hd", "service": "bridge", "job": "j-17", "run": "bridge:j-17", "season": 1, "episodes": [3]}),
        ),
        ev(
            "processing_rule_saved",
            "rule",
            "3",
            "user:owner@example.org",
            json!({"rule": 3, "name": "HEVC for old files", "version": 2, "enabled": true, "when": "import", "service": "bridge", "flow": "convert", "created": false}),
        ),
        ev(
            "processing_rule_removed",
            "rule",
            "3",
            "user:owner@example.org",
            json!({"rule": 3, "name": "HEVC for old files", "version": 2, "enabled": false, "when": "import", "service": "bridge", "flow": "convert"}),
        ),
        ev(
            "processing_worker_changed",
            "engine",
            "worker",
            "user:owner@example.org",
            json!({"service": "worker", "act": "limits", "steps": 1, "node": "n1", "node_name": "Node One", "kind": "transcodeGpu", "from": 2, "to": 3}),
        ),
        ev(
            "integrations_pruned",
            "config",
            "integrations",
            "config",
            json!({"catalog": {"engines": ["radarr-old"], "copies": 12, "untracked_files": 0, "title_links": 12, "titles": 10, "libraries": 1}, "downloads": 0, "requests": 0, "accounts": 0, "engine_state": 1}),
        ),
        ev(
            "policy_loaded",
            "policy",
            "abc",
            "policy",
            json!({"hash": "abc", "name": "singularr-default", "version": "2026-11-23", "source": "builtin", "previous_hash": null, "traits": 24, "lanes": 4, "protections": 0}),
        ),
        ev(
            "policy_model_saved",
            "policy",
            "2",
            "user:owner@example.org",
            json!({"version": 2, "previous": 1, "action": "list_saved", "subject": "list:tier-1", "hash": "abc", "profiles": 4, "lists": 1, "rules": 0}),
        ),
        ev(
            "key_created",
            "key",
            "5",
            "user:owner@example.org",
            json!({"name": "Home dashboard", "prefix": "mq_c91e1234", "scopes": ["read", "library"]}),
        ),
        ev(
            "key_revoked",
            "key",
            "5",
            "user:owner@example.org",
            json!({"name": "Home dashboard", "prefix": "mq_c91e1234"}),
        ),
        ev(
            "request_added",
            "request",
            "9",
            "sync:seerr",
            request.clone(),
        ),
        ev(
            "request_status_changed",
            "request",
            "9",
            "sync:seerr",
            changed,
        ),
        ev("request_removed", "request", "9", "sync:seerr", request),
        ev(
            "request_intent_shadowed",
            "request_intent",
            "2",
            "requests:shadow",
            json!({"intent_id": 2, "site": "plex", "requested_by": "Sam", "kind": "show", "tmdb_id": 1396, "title": "Breaking Bad", "seasons": [3], "gate": "approve", "would_send": {}}),
        ),
        ev(
            "album_added",
            "album",
            "11",
            "scan:music",
            json!({"artist": "Nina Simone", "album": "Pastel Blues", "folder": "Pastel Blues (1965)", "year": 1965, "tracks": 9, "formats": ["flac"], "cover": true}),
        ),
        ev(
            "album_removed",
            "album",
            "11",
            "scan:music",
            json!({"artist": "Nina Simone", "album": "Pastel Blues", "folder": "Pastel Blues (1965)"}),
        ),
        ev(
            "tape_added",
            "tape",
            "4",
            "scan:home_movies",
            json!({"path": "1986 - Easter.mp4", "size_bytes": 100, "returned": false}),
        ),
        ev(
            "tape_missing",
            "tape",
            "4",
            "scan:home_movies",
            json!({"path": "1986 - Easter.mp4", "size_bytes": 100}),
        ),
        ev(
            "tape_moved",
            "tape",
            "4",
            "scan:home_movies",
            json!({"from": "old/1986 - Easter.mp4", "to": "1986 - Easter and spring.mp4", "size_bytes": 100}),
        ),
        ev(
            "tape_annotated",
            "tape",
            "4",
            "user:owner@example.org",
            json!({"path": "1986 - Easter.mp4", "fields": ["title", "year_from"], "first": true}),
        ),
        ev(
            "setting_changed",
            "setting",
            "general.name",
            "user:owner@example.org",
            json!({"key": "general.name", "label": "Name", "scope": "instance", "from": "Singularr", "to": "Home Theater", "reset": false, "restart": false}),
        ),
        ev(
            "processing_finished",
            "title",
            "7",
            "sync:bridge",
            json!({"run": "bridge:j-17", "service": "bridge", "flow": "convert", "flow_label": "Convert to HEVC", "state": "imported", "file": "Breaking Bad - S01E03.mkv", "title_id": 7, "title": "Breaking Bad", "kind": "show", "tmdb_id": 1396, "saved": 540_857_787, "took": 675, "season": 1, "episodes": [3]}),
        ),
        ev(
            "processing_failed",
            "processing_run",
            "bridge:j-18",
            "sync:bridge",
            json!({"run": "bridge:j-18", "service": "bridge", "flow": "convert", "flow_label": "Convert to HEVC", "state": "refused", "file": "Other.mkv", "detail": "the output is another title"}),
        ),
        ev(
            "processing_stuck",
            "title",
            "7",
            "sync:bridge",
            json!({"run": "bridge:j-19", "service": "bridge", "flow": "audio", "flow_label": "Rebuild audio", "state": "staged", "file": "Breaking Bad - S01E04.mkv", "title_id": 7, "open_for": 691_200}),
        ),
        ev(
            "notification_channel_added",
            "notification_channel",
            "3",
            "user:owner@example.org",
            json!({"channel_id": 3, "name": "Ops", "type": "slack", "enabled": true, "events": ["file.upgraded"], "kinds": [], "changed": []}),
        ),
        ev(
            "notification_channel_changed",
            "notification_channel",
            "3",
            "user:owner@example.org",
            json!({"channel_id": 3, "name": "Ops", "type": "slack", "enabled": false, "events": ["file.upgraded"], "kinds": [], "changed": ["enabled", "secret.url"]}),
        ),
        ev(
            "notification_channel_removed",
            "notification_channel",
            "3",
            "user:owner@example.org",
            json!({"channel_id": 3, "name": "Ops", "type": "slack", "enabled": false, "events": [], "kinds": [], "changed": []}),
        ),
        ev(
            "search_sent",
            "title",
            "7",
            "user:owner@example.org",
            with(
                json!({"manager": "sonarr", "manager_name": "Sonarr", "command": "EpisodeSearch", "command_id": 12}),
            ),
        ),
        ev(
            "release_grabbed",
            "title",
            "7",
            "user:owner@example.org",
            with(
                json!({"manager": "sonarr", "manager_name": "Sonarr", "release": "Breaking.Bad.S01E03.1080p.WEB-DL-GRP", "indexer": "Indexer", "quality": "WEBDL-1080p", "size_bytes": 3_000_000_000_i64}),
            ),
        ),
        ev(
            "download_import_forced",
            "download",
            "AB12",
            "user:owner@example.org",
            json!({"hash": "AB12", "name": "Breaking.Bad.S01E03.1080p.WEB-DL-GRP", "title_id": 7, "title": "Breaking Bad", "manager": "sonarr", "manager_name": "Sonarr", "files": ["a.mkv"], "mode": "copy", "command_id": 13}),
        ),
        ev(
            "download_queue_removed",
            "download",
            "AB12",
            "user:owner@example.org",
            json!({"hash": "AB12", "name": "Breaking.Bad.S01E03.1080p.WEB-DL-GRP", "title_id": 7, "title": "Breaking Bad", "manager": "sonarr", "manager_name": "Sonarr", "blocklist": true}),
        ),
    ]
}

#[test]
fn what_the_owner_asked_of_a_manager_reads_in_words() {
    let names = names();
    let by = |kind: &str| {
        every_kind()
            .into_iter()
            .find(|e| e.kind == kind)
            .map(|e| journal_event(&e, &names))
            .unwrap()
    };
    let searched = by("search_sent");
    assert_eq!(searched.kind, "release.searched");
    assert!(
        searched.summary.starts_with("Search sent to "),
        "{}",
        searched.summary
    );
    let grabbed = by("release_grabbed");
    assert_eq!(grabbed.kind, "release.grabbed");
    assert!(
        grabbed
            .summary
            .ends_with(": Breaking.Bad.S01E03.1080p.WEB-DL-GRP"),
        "{}",
        grabbed.summary
    );
    assert_eq!(by("download_import_forced").kind, "download.forced");
    let dropped = by("download_queue_removed");
    assert_eq!(dropped.kind, "download.dropped");
    assert!(
        dropped.summary.ends_with(", release blocklisted"),
        "{}",
        dropped.summary
    );
}

#[test]
fn runs_and_channels_read_in_words() {
    let names = names();
    let by = |kind: &str| {
        every_kind()
            .into_iter()
            .find(|e| e.kind == kind)
            .map(|e| journal_event(&e, &names))
            .unwrap()
    };
    let done = by("processing_finished");
    assert_eq!(done.kind, "processing.finished");
    assert_eq!(
        done.summary,
        "Finished Convert to HEVC: Breaking Bad - S01E03.mkv, saved 516 MB"
    );
    assert_eq!(done.subject.label, "Breaking Bad S01E03");
    assert_eq!(done.actor.name, "The Bridge");
    let refused = by("processing_failed");
    assert_eq!(
        refused.summary,
        "Refused by The Bridge: the output is another title"
    );
    assert_eq!(
        (
            refused.subject.kind.as_str(),
            refused.subject.label.as_str()
        ),
        ("run", "Other.mkv")
    );
    assert_eq!(
        by("processing_stuck").summary,
        "Stuck in Rebuild audio for 8 d: Breaking Bad - S01E04.mkv"
    );
    let added = by("notification_channel_added");
    assert_eq!(added.kind, "notification.added");
    assert_eq!(added.summary, "Created: Slack");
    assert_eq!(
        (added.subject.kind.as_str(), added.subject.label.as_str()),
        ("channel", "Ops")
    );
    assert_eq!(
        by("notification_channel_changed").summary,
        "Changed: disabled and url"
    );
    assert_eq!(by("notification_channel_removed").summary, "Deleted");
}

#[test]
fn every_kind_the_server_writes_has_a_line_in_words() {
    let names = names();
    let events = every_kind();
    let stored: BTreeSet<&str> = KINDS.iter().map(|(k, _)| *k).collect();
    let covered: BTreeSet<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(stored, covered, "a body for every kind of the table");
    for e in &events {
        let line = summary(e, &names);
        assert!(!line.is_empty(), "{}", e.kind);
        assert!(
            !line.contains(&e.kind),
            "{} shows its raw kind: {line}",
            e.kind
        );
        assert_ne!(line, humanize(&e.kind), "{} has no line of its own", e.kind);
        assert!(
            !line.contains('\u{2014}') && !line.contains('\u{2013}'),
            "{}: {line}",
            e.kind
        );
        for word in ["held", "copies", "copy "] {
            assert!(!line.to_lowercase().contains(word), "{}: {line}", e.kind);
        }
        let j = journal_event(e, &names);
        assert!(
            j.kind.contains('.'),
            "{} is the page's word: {}",
            e.kind,
            j.kind
        );
        assert_eq!(j.code, e.kind);
    }
}

#[test]
fn lines_name_what_happened() {
    let names = names();
    let by = |kind: &str| {
        every_kind()
            .into_iter()
            .find(|e| e.kind == kind)
            .map(|e| journal_event(&e, &names))
            .unwrap()
    };
    let replaced = by("copy_replaced");
    assert_eq!(replaced.kind, "file.upgraded");
    assert_eq!(
        replaced.summary,
        "Upgraded to WEBDL-1080p by GRP, 3.00 GB, score 120, was HDTV-1080p by OLD, 3.00 GB, score 40"
    );
    assert_eq!(
        replaced.subject,
        JournalSubject {
            kind: "title".into(),
            id: "show-1396".into(),
            label: "Breaking Bad S01E03".into()
        }
    );
    let asked = by("request_added");
    assert_eq!(asked.summary, "Requested by Sam: seasons 1 and 2");
    assert_eq!(
        (asked.actor.kind.as_str(), asked.actor.name.as_str()),
        ("user", "Sam")
    );
    let approved = by("request_status_changed");
    assert_eq!(approved.kind, "request.approved");
    assert_eq!(approved.summary, "Approved, was pending");
    let down = by("engine_down");
    assert_eq!(down.summary, "Went offline: connection refused");
    assert_eq!(down.subject.kind, "integration");
    assert_eq!(down.actor.kind, "integration");
    assert_eq!(down.actor.name, "Radarr HD");
    assert_eq!(
        by("setting_changed").summary,
        "Changed from Singularr to Home Theater"
    );
    assert!(
        by("request_intent_shadowed")
            .summary
            .contains("Family Requests")
    );
    assert_eq!(by("policy_loaded").kind, "policy.changed");
    // What Singularr may do with an integration: the owner's change, about the integration.
    let allowed = by("integration_access_changed");
    assert_eq!(allowed.kind, "integration.access");
    assert_eq!(allowed.summary, "Access changed to write");
    assert_eq!(
        (allowed.subject.kind.as_str(), allowed.subject.id.as_str()),
        ("integration", "radarr")
    );
    assert_eq!(allowed.subject.label, "Radarr HD");
    assert_eq!(
        (allowed.actor.kind.as_str(), allowed.actor.id.as_str()),
        ("user", "owner@example.org")
    );
    // A file the owner sent to a flow: about the title, by the person, naming the file.
    let sent = by("processing_sent");
    assert_eq!(sent.kind, "processing.sent");
    assert_eq!(
        sent.summary,
        "Sent to The Bridge for Convert to HEVC: Breaking Bad - S01E03 - And the Bag's in the River.mkv"
    );
    assert_eq!(
        sent.subject,
        JournalSubject {
            kind: "title".into(),
            id: "show-1396".into(),
            label: "Breaking Bad S01E03".into()
        }
    );
    assert_eq!(
        (sent.actor.kind.as_str(), sent.actor.id.as_str()),
        ("user", "owner@example.org")
    );
    let taken_back = ev(
        "integration_access_changed",
        "engine",
        "radarr",
        "user:owner@example.org",
        json!({"integration": "radarr", "from": "act", "to": "read"}),
    );
    assert_eq!(
        journal_event(&taken_back, &names).summary,
        "Access changed to read-only"
    );
    let owner = by("key_created").actor;
    assert_eq!(
        (owner.kind.as_str(), owner.id.as_str()),
        ("user", "owner@example.org")
    );
    let scan = by("album_added");
    assert_eq!(scan.actor.kind, "system");
    assert_eq!(scan.summary, "Added: 1965, 9 tracks");
    assert_eq!(scan.cause.as_deref(), Some("e-41"));
    assert_eq!(scan.id, "e-42");
}

/// A title the catalog no longer holds keeps the TMDB id its event named.
#[test]
fn a_title_gone_from_the_catalog_keeps_its_page_id() {
    let e = ev(
        "title_removed",
        "title",
        "99",
        "sync:radarr",
        json!({"title_id": 99, "title": "Gone Movie", "kind": "movie", "tmdb_id": 603, "engine": "radarr", "gone_from_catalog": true}),
    );
    let j = journal_event(&e, &names());
    assert_eq!(j.subject.id, "movie-603");
    assert_eq!(j.summary, "Removed by Radarr HD");
}

#[test]
fn filters_read_both_the_page_s_words_and_the_stored_ones() {
    assert_eq!(seq("e-123"), Some(123));
    assert_eq!(seq("123"), Some(123));
    assert_eq!(seq("x"), None);
    assert_eq!(stored_actors("integration:radarr"), vec!["sync:radarr"]);
    assert_eq!(stored_actors("system:policy"), vec!["policy"]);
    assert_eq!(stored_actors("system:portal"), vec!["requests:shadow"]);
    assert_eq!(stored_actors("sync:radarr"), vec!["sync:radarr"]);
    for (_, word) in KINDS {
        assert!(word.contains('.'), "{word}");
    }
}

/// The keys the Journal page reads, from a reduced copy of its sample
/// (`web/data/journal.json`).
#[test]
fn an_event_answers_the_page_s_shape() {
    let sample: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contract_journal.json"
    ))
    .unwrap();
    let theirs = &sample["events"][0];
    let ours = serde_json::to_value(journal_event(&every_kind()[3], &names())).unwrap();
    for key in theirs.as_object().unwrap().keys() {
        assert!(ours.get(key).is_some(), "an event has no `{key}`");
    }
    for part in ["actor", "subject"] {
        for key in theirs[part].as_object().unwrap().keys() {
            assert!(ours[part].get(key).is_some(), "`{part}` has no `{key}`");
        }
    }
}
