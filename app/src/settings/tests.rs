use super::*;
use std::collections::BTreeMap;

type Vars = BTreeMap<&'static str, &'static str>;

fn vars(extra: &[(&'static str, &'static str)]) -> Vars {
    let mut e = BTreeMap::from([
        (
            "DATABASE_URL",
            "postgres://singularr:db-password-here@10.0.0.83:5432/singularr",
        ),
        ("SINGULARR_OWNER_EMAILS", "owner@example.org"),
    ]);
    e.extend(extra.iter().copied());
    e
}

fn settings(extra: &[(&'static str, &'static str)]) -> Settings {
    let e = vars(extra);
    Settings::new(&Config::from(&e).unwrap(), &e, None)
}

fn owner() -> Caller {
    Caller::Owner {
        email: "owner@example.org".into(),
    }
}

fn reader() -> Caller {
    Caller::Key {
        id: 1,
        name: "dashboard".into(),
        scopes: vec![Scope::Read],
    }
}

fn find<'a>(view: &'a SettingsView, key: &str) -> &'a FieldView {
    view.sections
        .iter()
        .flat_map(|s| s.groups.iter())
        .flat_map(|g| g.fields.iter())
        .find(|f| f.key == key)
        .unwrap_or_else(|| panic!("{key} is not in the view"))
}

fn save(s: &Settings, scope: &str, key: &str, value: Value) {
    s.saved
        .write()
        .unwrap()
        .insert((scope.to_string(), key.to_string()), value);
    s.apply_locale();
}

#[test]
fn the_sections_are_the_contract_s_in_its_order() {
    let ids: Vec<&str> = SECTIONS.iter().map(|s| s.id).collect();
    assert_eq!(
        ids,
        vec![
            "general",
            "integrations",
            "libraries",
            "people",
            "portal",
            "notifications",
            "appearance",
            "jobs",
            "api",
            "system"
        ]
    );
    let panels: Vec<&str> = SECTIONS
        .iter()
        .flat_map(|s| s.panels.iter().copied())
        .collect();
    for p in &panels {
        assert!(
            [
                "server",
                "integrations",
                "libraries",
                "people",
                "roles",
                "channels",
                "keys",
                "jobs",
                "cache",
                "services",
                "about"
            ]
            .contains(p),
            "{p} is not a panel of the contract"
        );
    }
}

/// The keys are what the `settings` table's CHECK takes: `section.camelCase`.
fn well_formed(key: &str) -> bool {
    let parts: Vec<&str> = key.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            p.starts_with(|c: char| c.is_ascii_lowercase())
                && p.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

#[test]
fn every_key_is_unique_well_formed_and_in_its_own_section() {
    let mut seen = BTreeSet::new();
    for s in SECTIONS {
        let mut groups = BTreeSet::new();
        for g in s.groups {
            assert!(groups.insert(g.id), "group {} twice in {}", g.id, s.id);
            for f in g.fields {
                assert!(seen.insert(f.key), "{} twice", f.key);
                assert!(well_formed(f.key), "{} is not section.camelCase", f.key);
                assert!(
                    f.key.starts_with(&format!("{}.", s.id)),
                    "{} is not in its section {}",
                    f.key,
                    s.id
                );
                assert!(
                    !(f.is_secret() && f.saves),
                    "{}: a secret is never saved here",
                    f.key
                );
                assert!(
                    !(f.user && !f.saves),
                    "{}: a person's own value saves",
                    f.key
                );
            }
        }
    }
}

#[test]
fn every_default_of_a_field_that_saves_passes_its_own_check() {
    let s = settings(&[]);
    for f in registry::fields().filter(|f| f.saves) {
        let d = s.fallback(f.default);
        if d.is_null() {
            continue;
        }
        assert_eq!(validate(f, &d).as_ref(), Ok(&d), "{}", f.key);
    }
}

#[test]
fn a_value_comes_from_the_default_the_environment_the_file_or_a_save() {
    let s = settings(&[]);
    let v = s.view(&owner());
    let name = find(&v, "general.name");
    assert_eq!(
        (
            name.value.clone(),
            name.source.as_str(),
            name.editable,
            name.scope.as_str()
        ),
        (Some(json!("Singularr")), "default", true, "instance")
    );
    assert_eq!(s.text("general.name").as_deref(), Some("Singularr"));

    // The environment fixes it: read-only, and the variable is named.
    let s = settings(&[("SINGULARR_WORKSPACE_NAME", "Home")]);
    let v = s.view(&owner());
    let name = find(&v, "general.name");
    assert_eq!(
        (
            name.value.clone(),
            name.source.as_str(),
            name.env.as_deref(),
            name.editable
        ),
        (
            Some(json!("Home")),
            "environment",
            Some("SINGULARR_WORKSPACE_NAME"),
            false
        )
    );
    // A saved value does not beat the environment.
    save(&s, INSTANCE, "general.name", json!("Saved"));
    assert_eq!(s.value("general.name"), json!("Home"));

    // The instance file fixes the portal's name.
    let e = vars(&[]);
    let file = "[workspace]\nportal = \"Family Requests\"\n";
    let config = Config::with_instance(&e, Some(("/srv/instance.toml", file))).unwrap();
    let s = Settings::new(&config, &e, Some(file));
    let v = s.view(&owner());
    let portal = find(&v, "portal.name");
    assert_eq!(
        (
            portal.value.clone(),
            portal.source.as_str(),
            portal.file.as_deref(),
            portal.editable
        ),
        (
            Some(json!("Family Requests")),
            "file",
            Some("/srv/instance.toml"),
            false
        )
    );
    // What the file leaves out stays editable.
    assert!(find(&v, "general.name").editable);

    // A save is in force where nothing fixes the value.
    save(&s, INSTANCE, "general.name", json!("Our Movies"));
    let v = s.view(&owner());
    let name = find(&v, "general.name");
    assert_eq!(
        (name.value.clone(), name.source.as_str()),
        (Some(json!("Our Movies")), "saved")
    );
    assert_eq!(name.default, Some(json!("Singularr")));
}

#[test]
fn read_only_views_show_what_the_environment_fixes() {
    let s = settings(&[
        ("SINGULARR_SYNC_TITLES_SECS", "1200"),
        ("SINGULARR_SYNC_QUEUE_SECS", "5"),
    ]);
    let v = s.view(&owner());
    let titles = find(&v, "jobs.titles");
    assert_eq!(
        (
            titles.value.clone(),
            titles.source.as_str(),
            titles.editable,
            titles.restart,
            titles.kind.as_str()
        ),
        (Some(json!(1200)), "environment", false, true, "duration")
    );
    // The value in force is the configuration's, after its floor.
    assert_eq!(find(&v, "jobs.queue").value, Some(json!(15)));
    let history = find(&v, "jobs.history");
    assert_eq!(
        (history.value.clone(), history.source.as_str()),
        (Some(json!(900)), "default")
    );
    assert_eq!(find(&v, "people.requestOwners").value, Some(json!(["1"])));
    let db = find(&v, "system.database");
    assert_eq!(
        db.value,
        Some(json!("postgres://singularr:***@10.0.0.83:5432/singularr"))
    );
}

#[test]
fn secrets_are_never_sent() {
    let s = settings(&[
        ("SINGULARR_TMDB_TOKEN", "tmdb-secret-value"),
        ("SINGULARR_EDGE_TOKEN", "edge-secret-value"),
        ("SINGULARR_TVDB_KEY", "CHANGEME"),
    ]);
    for caller in [owner(), reader()] {
        let v = s.view(&caller);
        let text = serde_json::to_string(&v).unwrap();
        for secret in ["tmdb-secret-value", "edge-secret-value", "db-password-here"] {
            assert!(!text.contains(secret), "the view leaks {secret}");
        }
        let tmdb = find(&v, "integrations.tmdbToken");
        assert_eq!(
            (tmdb.value.clone(), tmdb.set, tmdb.source.as_str()),
            (None, Some(true), "environment")
        );
        // The placeholder is no value.
        assert_eq!(find(&v, "integrations.tvdbKey").set, Some(false));
        let json = serde_json::to_value(find(&v, "people.edgeToken")).unwrap();
        assert!(json.get("value").is_none() && json["set"] == json!(true));
    }
    assert_eq!(s.value("integrations.tmdbToken"), Value::Null);
}

#[test]
fn private_values_need_the_settings_scope() {
    let s = settings(&[]);
    let owners = find(&s.view(&owner()), "people.ownerEmails").clone();
    assert_eq!(owners.value, Some(json!(["owner@example.org"])));
    assert!(!owners.withheld);
    let v = s.view(&reader());
    let withheld = find(&v, "people.ownerEmails");
    assert!(withheld.withheld && withheld.value == Some(Value::Null));
    assert!(
        !serde_json::to_string(&v)
            .unwrap()
            .contains("owner@example.org")
    );
    // A reader cannot save, so nothing is editable for it.
    assert!(!find(&v, "general.name").editable);
}

#[test]
fn a_person_s_own_value_falls_back_to_the_installation_s() {
    let s = settings(&[]);
    let theme = find(&s.view(&owner()), "appearance.theme").clone();
    assert_eq!(
        (
            theme.value.clone(),
            theme.source.as_str(),
            theme.scope.as_str(),
            theme.editable
        ),
        (Some(json!("horizon")), "default", "user", true)
    );
    assert_eq!(theme.options.as_ref().map(|o| o.len()), Some(3));
    save(&s, INSTANCE, "appearance.theme", json!("graphite"));
    let theme = find(&s.view(&owner()), "appearance.theme").clone();
    assert_eq!(theme.value, Some(json!("graphite")));
    assert_eq!(theme.instance.as_ref().unwrap().source, "saved");
    save(
        &s,
        &user_scope("owner@example.org"),
        "appearance.theme",
        json!("mocha"),
    );
    let theme = find(&s.view(&owner()), "appearance.theme").clone();
    assert_eq!(
        (theme.value.clone(), theme.source.as_str()),
        (Some(json!("mocha")), "saved")
    );
    assert_eq!(theme.instance.unwrap().value, json!("graphite"));
    assert_eq!(s.value_for(&owner(), "appearance.theme"), json!("mocha"));
    assert_eq!(
        s.value_for(&reader(), "appearance.theme"),
        json!("graphite")
    );
    assert_eq!(s.value("appearance.theme"), json!("graphite"));
    // A key has no own values to set.
    assert!(!find(&s.view(&reader()), "appearance.theme").editable);
}

#[test]
fn values_are_checked_against_their_type() {
    let check = |key: &str, raw: Value| validate(registry::field(key).unwrap(), &raw);
    assert_eq!(check("general.name", json!("  Home  ")), Ok(json!("Home")));
    assert!(check("general.name", json!("")).is_err());
    assert!(check("general.name", json!("two\nlines")).is_err());
    assert!(check("general.name", json!(7)).is_err());
    assert_eq!(
        check("general.url", json!("https://media.example.org/")),
        Ok(json!("https://media.example.org"))
    );
    assert!(check("general.url", json!("ftp://media.example.org")).is_err());
    assert!(check("general.url", json!("/relative")).is_err());
    assert_eq!(
        check("general.signOutUrl", json!("/oauth2/sign_out")),
        Ok(json!("/oauth2/sign_out"))
    );
    assert!(check("general.signOutUrl", json!("//elsewhere.example.org")).is_err());
    assert!(check("general.signOutUrl", json!("https://user@example.org")).is_err());
    assert_eq!(check("general.region", json!("de")), Ok(json!("DE")));
    assert!(check("general.region", json!("Germany")).is_err());
    assert_eq!(
        check("general.language", json!("pt-br")),
        Ok(json!("pt-BR"))
    );
    assert_eq!(
        check("general.language", json!("es-419")),
        Ok(json!("es-419"))
    );
    assert!(check("general.language", json!("english")).is_err());
    assert_eq!(
        check("general.timeZone", json!("Europe/Berlin")),
        Ok(json!("Europe/Berlin"))
    );
    assert_eq!(
        check("general.timeZone", json!("America/Argentina/Buenos_Aires")),
        Ok(json!("America/Argentina/Buenos_Aires"))
    );
    assert!(check("general.timeZone", json!("Mars")).is_err());
    assert_eq!(
        check("general.dateFormat", json!("YYYY-MM-DD")),
        Ok(json!("YYYY-MM-DD"))
    );
    assert!(check("general.dateFormat", json!("dd.mm")).is_err());
    assert!(check("appearance.theme", json!(5)).is_err());
    assert!(check("integrations.tmdbToken", json!("x")).is_err());
}

#[test]
fn the_locale_follows_the_language_and_the_region() {
    let s = settings(&[]);
    assert_eq!(
        s.locale().get(),
        LocaleValue {
            language: "en-US".into(),
            region: "US".into()
        }
    );
    save(&s, INSTANCE, "general.language", json!("de-DE"));
    save(&s, INSTANCE, "general.region", json!("DE"));
    assert_eq!(s.locale().language(), "de-DE");
    assert_eq!(s.locale().region(), "DE");
    // The environment's region wins over a saved one.
    let s = settings(&[("SINGULARR_WATCH_REGION", "gb")]);
    save(&s, INSTANCE, "general.region", json!("DE"));
    assert_eq!(s.locale().region(), "GB");
    let v = s.view(&owner());
    let region = find(&v, "general.region");
    assert_eq!(
        (region.source.as_str(), region.editable),
        ("environment", false)
    );
}

#[test]
fn the_time_zone_defaults_to_the_process_s() {
    assert_eq!(settings(&[]).value("general.timeZone"), json!("UTC"));
    assert_eq!(
        settings(&[("TZ", "America/New_York")]).value("general.timeZone"),
        json!("America/New_York")
    );
    assert_eq!(
        settings(&[("TZ", ":/etc/localtime")]).value("general.timeZone"),
        json!("UTC")
    );
}

/// Every variable the configuration reads is a field somewhere, so the Settings page shows the
/// whole configuration. The integrations' own variables (`RADARR_URL`) are the integrations
/// panel's; the portal's (`SINGULARR_REQUESTS_*`, `SINGULARR_FACE`) belong to the other face.
#[test]
fn everything_the_configuration_reads_is_visible() {
    let source = include_str!("../core/config.rs");
    let listed: BTreeSet<&str> = registry::fields()
        .flat_map(|f| f.env.iter().copied())
        .collect();
    let mut names = BTreeSet::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            let start = i + 1;
            let mut j = start;
            while j < bytes.len()
                && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit() || bytes[j] == b'_')
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'"' && j > start {
                names.insert(&source[start..j]);
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    let ours = |n: &&str| {
        (n.starts_with("SINGULARR_") || *n == "DATABASE_URL")
            && !n.starts_with("SINGULARR_REQUESTS_")
            && *n != "SINGULARR_FACE"
    };
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| ours(n) && !listed.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "variables the configuration reads that no setting shows: {missing:?}"
    );
    assert!(listed.contains("SINGULARR_REQUESTS_PUBLIC_URL"));
}

#[test]
fn the_registry_names_no_installation() {
    for s in SECTIONS {
        let mut texts = vec![s.label, s.summary];
        for g in s.groups {
            texts.extend([g.label, g.help]);
            for f in g.fields {
                texts.extend([f.label, f.help]);
            }
        }
        for t in texts {
            let lower = t.to_lowercase();
            for word in ["this installation", "jellyfin", "held", "copies"] {
                assert!(!lower.contains(word), "`{t}` says {word}");
            }
            assert!(
                !t.contains('\u{2014}') && !t.contains('\u{2013}'),
                "`{t}` has a dash"
            );
        }
    }
}

/// The words follow `docs/copy.md`: no label, summary or help ends with a period, a help is
/// one line of twelve words at most, and none speaks of the product or its version.
#[test]
fn the_registry_s_words_follow_the_copy_rules() {
    for s in SECTIONS {
        let mut texts = vec![s.label, s.summary];
        let mut helps = Vec::new();
        for g in s.groups {
            texts.push(g.label);
            helps.push(g.help);
            for f in g.fields {
                texts.push(f.label);
                helps.push(f.help);
                if let Kind::Select(choices) = f.kind {
                    texts.extend(choices.iter().map(|c| c.label));
                }
            }
        }
        for h in &helps {
            assert!(
                h.split_whitespace().count() <= 12 && !h.contains('\n'),
                "`{h}` is more than one line of twelve words"
            );
        }
        for t in texts.into_iter().chain(helps) {
            assert!(!t.ends_with('.'), "`{t}` ends with a period");
            let words: Vec<String> = t
                .split(|c: char| !c.is_alphanumeric())
                .map(str::to_lowercase)
                .collect();
            for word in ["singularr", "its", "yet", "version"] {
                assert!(!words.iter().any(|w| w == word), "`{t}` says `{word}`");
            }
        }
    }
}

#[test]
fn a_view_serializes_in_the_contract_s_field_names() {
    let v = serde_json::to_value(settings(&[]).view(&owner())).unwrap();
    let general = &v["sections"][0];
    assert_eq!(general["id"], "general");
    let name = &general["groups"][0]["fields"][0];
    for key in [
        "key", "label", "type", "value", "default", "source", "editable", "restart", "scope",
        "help",
    ] {
        assert!(name.get(key).is_some(), "general.name has no `{key}`");
    }
    assert_eq!(name["type"], "text");
    assert!(name.get("withheld").is_none());
    let date = &general["groups"][2]["fields"][1];
    assert_eq!(date["key"], "general.dateFormat");
    assert_eq!(
        date["options"][0],
        json!({"value": "D MMM YYYY", "label": "23 Nov 2026"})
    );
}
