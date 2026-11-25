//! `singularrd doctor`: check what the server depends on and say how to fix what is wrong.
//!
//! One line per check: `OK`, `WARN` or `FAIL`, what was found, and for anything but `OK` a
//! hint. The exit status is 1 when a check failed. Every call it makes to an engine is a
//! read (and a download client's sign-in), through the same outbound allowlist as the server.

use singularr::core::config::{Config, Integration, redact_url};
use singularr::core::outbound::Outbound;
use singularr::core::{db, media, store, units};
use singularr::engines::Protocol;
use singularr::engines::service::{EngineSet, allowlist};
use singularr::metadata::service::ProviderSet;
use singularr::plays::locations;
use singularr::policy;
use singularr_sdk::manifest::Manifest;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Report {
    failed: bool,
}

impl Report {
    fn line(&mut self, level: Level, what: &str, found: &str, hint: Option<&str>) {
        let tag = match level {
            Level::Ok => "OK  ",
            Level::Warn => "WARN",
            Level::Fail => "FAIL",
        };
        println!("{tag}  {what}: {found}");
        if let Some(hint) = hint.filter(|_| level != Level::Ok) {
            println!("      fix: {hint}");
        }
        self.failed |= level == Level::Fail;
    }
}

/// Where an integration's address and sign-in are set, as its source names them:
/// `QBIT_URL, QBIT_USER and QBIT_PASS`.
fn sign_in_vars(i: &Integration) -> String {
    let user = i.kind.takes_user().then(|| {
        if i.source == "environment" {
            format!("{}_USER", i.url_var.trim_end_matches("_URL"))
        } else {
            format!("the `user` of integration {}", i.id)
        }
    });
    needs(
        i.kind.manifest(),
        &i.url_var,
        user.as_deref(),
        &i.secret_var,
    )
}

/// What an integration of a type needs set, as its manifest states it: the address, the user
/// name of a type that signs in with one, and the secret unless the type runs without one.
fn needs(m: &Manifest, url: &str, user: Option<&str>, secret: &str) -> String {
    let mut vars = vec![url];
    if m.takes_user {
        vars.extend(user);
    }
    if m.takes_user || !m.secret.optional {
        vars.push(secret);
    }
    match vars.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}

pub async fn run() -> ExitCode {
    let mut report = Report { failed: false };
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(e) => {
            report.line(
                Level::Fail,
                "Configuration",
                &e.to_string(),
                Some("Set the variable and restart"),
            );
            return ExitCode::FAILURE;
        }
    };
    report.line(
        Level::Ok,
        "Configuration",
        "Read from the environment",
        None,
    );
    // Integrations that are not configured get their own line below.
    let general = config
        .warnings
        .iter()
        .filter(|w| !w.starts_with("Integration ") && !w.starts_with("No download client"));
    for warning in general {
        report.line(
            Level::Warn,
            "Configuration",
            warning,
            Some("See the environment table in the README"),
        );
    }

    if config.edge_token_placeholder {
        report.line(
            Level::Fail,
            "Edge token",
            "Still the placeholder",
            Some("Set SINGULARR_EDGE_TOKEN to the token the proxy sends as X-Edge-Token"),
        );
    }

    // Database and migrations.
    match db::connect(&config).await {
        Err(e) => report.line(
            Level::Fail,
            "Database",
            &format!("{} unreachable: {e}", redact_url(&config.database_url)),
            Some("Check DATABASE_URL and that the database exists"),
        ),
        Ok(pool) => {
            match store::ping(&pool).await {
                Ok(()) => report.line(
                    Level::Ok,
                    "Database",
                    &format!("{} reachable", redact_url(&config.database_url)),
                    None,
                ),
                Err(e) => report.line(
                    Level::Fail,
                    "Database",
                    &e.to_string(),
                    Some("Check the database server"),
                ),
            }
            match db::pending_migrations(&pool).await {
                Ok(pending) if pending.is_empty() => {
                    report.line(Level::Ok, "Migrations", "All applied", None)
                }
                Ok(pending) => report.line(
                    Level::Warn,
                    "Migrations",
                    &format!("{} pending: {}", pending.len(), pending.join(", ")),
                    Some("Start the server once to apply them"),
                ),
                Err(e) => report.line(
                    Level::Fail,
                    "Migrations",
                    &e.to_string(),
                    Some("Check the database user's rights"),
                ),
            }
        }
    }

    // Policy file.
    match policy::load(&config.policy_path) {
        Ok(loaded) => match loaded.warning {
            None => report.line(
                Level::Ok,
                "Policy",
                &format!(
                    "{}: {} {}, {}, {}, {}",
                    config.policy_path,
                    loaded.policy.policy.name,
                    loaded.policy.policy.version,
                    units::counted(loaded.policy.policy.traits.len() as i64, "trait", "traits"),
                    units::counted(
                        loaded.policy.policy.lanes.len() as i64,
                        "quality profile",
                        "quality profiles"
                    ),
                    units::counted(
                        loaded.policy.policy.protections.len() as i64,
                        "protection",
                        "protections"
                    )
                ),
                None,
            ),
            Some(warning) => report.line(
                Level::Warn,
                "Policy",
                &warning,
                Some("Add rules/policy.toml to the image or set SINGULARR_POLICY"),
            ),
        },
        Err(e) => report.line(Level::Fail, "Policy", &e.to_string(), Some("Fix the file")),
    }

    // Engines.
    let outbound =
        match allowlist(&config).and_then(|allow| Outbound::new(allow, config.http_timeout)) {
            Ok(outbound) => outbound,
            Err(e) => {
                report.line(
                    Level::Fail,
                    "Integrations",
                    &e.to_string(),
                    Some("Check the integration addresses"),
                );
                return ExitCode::FAILURE;
            }
        };
    let engines = EngineSet::from_config(&config, &outbound);
    for slot in engines.slots.iter().filter(|s| !s.configured) {
        let hint = match config.integration(&slot.id) {
            Some(i) => format!("Set {}", sign_in_vars(i)),
            None => format!("Set {}_URL", slot.id.to_uppercase()),
        };
        report.line(Level::Warn, &slot.id, "Not configured", Some(&hint));
    }
    for engine in &engines.arrs {
        let id = engine.engine().id.clone();
        let started = Instant::now();
        match engine.status().await {
            Ok(info) => report.line(
                Level::Ok,
                &id,
                &format!(
                    "{} {}, {} ms",
                    info.app.unwrap_or_else(|| "arr".to_string()),
                    info.version.unwrap_or_default(),
                    started.elapsed().as_millis()
                ),
                None,
            ),
            Err(e) => report.line(
                Level::Fail,
                &id,
                &e.to_string(),
                Some("401: wrong API key; no answer: wrong address or network path"),
            ),
        }
    }
    for client in &engines.download_clients {
        let started = Instant::now();
        match client.downloads().await {
            Ok(listed) => {
                let n = listed.len() as i64;
                let words = match client.protocol() {
                    Protocol::Torrent => units::counted(n, "torrent", "torrents"),
                    Protocol::Usenet => units::counted(n, "download", "downloads"),
                };
                report.line(
                    Level::Ok,
                    client.id(),
                    &format!("{words}, {} ms", started.elapsed().as_millis()),
                    None,
                );
            }
            Err(e) => {
                let hint = config
                    .integration(client.id())
                    .map(|i| format!("Check {}", sign_in_vars(i)));
                report.line(Level::Fail, client.id(), &e.to_string(), hint.as_deref());
            }
        }
    }

    for site in &engines.request_sites {
        let started = Instant::now();
        match site.status().await {
            Ok(info) => report.line(
                Level::Ok,
                site.id(),
                &format!(
                    "Seerr {}, {} ms",
                    info.version.unwrap_or_default(),
                    started.elapsed().as_millis()
                ),
                None,
            ),
            Err(e) => report.line(
                Level::Fail,
                site.id(),
                &e.to_string(),
                Some("401 or 403: wrong API key; no answer: wrong address or network path"),
            ),
        }
    }

    for server in &engines.media_servers {
        let started = Instant::now();
        match server.identity().await {
            Ok(identity) => report.line(
                Level::Ok,
                server.id(),
                &format!(
                    "{}{}, {} ms: {}",
                    server.kind(),
                    identity
                        .version
                        .map(|v| format!(" {v}"))
                        .unwrap_or_default(),
                    started.elapsed().as_millis(),
                    identity.name
                ),
                None,
            ),
            Err(e) => report.line(
                Level::Fail,
                server.id(),
                &e.to_string(),
                Some("401: wrong token or key; no answer: wrong address or network path"),
            ),
        }
    }

    if let Some(history) = &engines.play_history {
        let started = Instant::now();
        match history.status().await {
            Ok(info) => report.line(
                Level::Ok,
                history.id(),
                &format!(
                    "{} {}, {} ms",
                    info.app.unwrap_or_else(|| "Tautulli".to_string()),
                    info.version.unwrap_or_default(),
                    started.elapsed().as_millis()
                ),
                None,
            ),
            Err(e) => report.line(
                Level::Fail,
                history.id(),
                &e.to_string(),
                Some("A refusal: wrong API key; no answer: wrong address or network path"),
            ),
        }
    }

    for processor in &engines.processors {
        let started = Instant::now();
        match processor.status().await {
            Ok(status) => report.line(
                Level::Ok,
                processor.id(),
                &format!(
                    "{}{}, {} ms: {}",
                    processor.kind(),
                    status.version.map(|v| format!(" {v}")).unwrap_or_default(),
                    started.elapsed().as_millis(),
                    status.detail
                ),
                None,
            ),
            Err(e) => report.line(
                Level::Fail,
                processor.id(),
                &e.to_string(),
                Some("No answer: wrong address or network path"),
            ),
        }
    }

    // The two folders Singularr reads. A folder that is not mounted is a warning: the server
    // runs without it and its section stays empty.
    for (what, root) in [
        ("Music folder", &config.music.root),
        ("Home movies folder", &config.home_movies.root),
    ] {
        match std::fs::read_dir(root) {
            Ok(entries) => report.line(
                Level::Ok,
                what,
                &format!(
                    "{root}, {}",
                    units::counted(entries.count() as i64, "entry", "entries")
                ),
                None,
            ),
            Err(e) => report.line(
                Level::Warn,
                what,
                &format!("{root} unreadable: {e}"),
                Some("Mount it read-only, readable by uid 10001"),
            ),
        }
    }

    // The data folder: the one folder the server writes to (the location database, the media
    // tools' scratch files). The probe is the location loop's own: a file written, then removed.
    // The server runs without it. A folder that takes no file is a warning.
    let data = Path::new(&config.geoip.data_dir);
    match locations::writable(data) {
        Ok(()) => report.line(
            Level::Ok,
            "Data folder",
            &format!("{}, writable", data.display()),
            None,
        ),
        Err(e) => report.line(
            Level::Warn,
            "Data folder",
            &format!("{} takes no file: {e}", data.display()),
            Some("Mount a volume at SINGULARR_DATA_DIR, writable by uid 10001"),
        ),
    }

    // The media tools and the folder of their scratch files (core::media). Nothing needs them
    // yet: a missing one is a warning.
    for (what, program) in [("ffmpeg", media::FFMPEG), ("ffprobe", media::FFPROBE)] {
        match media::version(Path::new(program)).await {
            Ok(version) => report.line(Level::Ok, what, &format!("{version} at {program}"), None),
            Err(e) => report.line(
                Level::Warn,
                what,
                &e.to_string(),
                Some("The image carries it at /usr/local/bin. Outside the image, put it there"),
            ),
        }
    }
    match media::scratch_dir(Path::new(&config.geoip.data_dir)) {
        Ok(dir) => report.line(
            Level::Ok,
            "Scratch folder",
            &dir.display().to_string(),
            None,
        ),
        Err(e) => report.line(
            Level::Warn,
            "Scratch folder",
            &format!("{}/scratch: {e}", config.geoip.data_dir),
            Some("Mount the data folder (SINGULARR_DATA_DIR) writable by uid 10001"),
        ),
    }

    // Metadata providers: configured or not. They are not called here.
    for provider in ProviderSet::from_config(&config, &outbound).list() {
        if provider.configured {
            report.line(
                Level::Ok,
                &provider.id,
                &format!("Configured ({})", provider.host),
                None,
            );
        } else {
            let hint = provider.needs.map(|var| format!("Optional: set {var}"));
            report.line(Level::Warn, &provider.id, "Not configured", hint.as_deref());
        }
    }

    if report.failed {
        println!("doctor: a check failed");
        ExitCode::FAILURE
    } else {
        println!("doctor: all checks passed");
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use singularr_sdk::manifest::{CountWords, Group, Kind, Maturity, PassSpec, SecretSpec};

    /// A type no build lists, stated as a manifest alone.
    static SAMPLE: Manifest = Manifest {
        type_id: "sample",
        name: "Sample",
        label: "Sample Server",
        kinds: &[Kind::PlayHistory],
        group: Group::Server,
        role: "plays",
        description: "Plays and users",
        reads: &["Plays", "Users"],
        env_prefixes: &["SAMPLE"],
        secret: SecretSpec {
            key: "token",
            label: "Token",
            env_suffix: "TOKEN",
            optional: false,
            help: None,
        },
        takes_user: true,
        web_url: false,
        default_address: "http://sample:8400",
        max_instances: Some(1),
        actions: &[],
        passes: &[PassSpec {
            field: "syncEvery",
            label: "Sync interval",
            setting: "jobs.plays.historyEvery",
        }],
        counts: &[CountWords {
            key: "plays",
            one: "play",
            many: "plays",
        }],
        maturity: Maturity::Untested,
        http: singularr_sdk::http::HttpProfile::DEFAULT,
    };

    /// The doctor names what a type needs from its manifest: a new type's line needs no other
    /// edit. A type that runs without its secret and signs in with no user needs its address.
    #[test]
    fn the_doctor_names_what_a_type_s_manifest_says_it_needs() {
        assert_eq!(
            needs(&SAMPLE, "SAMPLE_URL", Some("SAMPLE_USER"), "SAMPLE_TOKEN"),
            "SAMPLE_URL, SAMPLE_USER and SAMPLE_TOKEN"
        );
        let alone = Manifest {
            takes_user: false,
            secret: SecretSpec {
                optional: true,
                ..SAMPLE.secret
            },
            ..SAMPLE
        };
        assert_eq!(
            needs(&alone, "SAMPLE_URL", None, "SAMPLE_TOKEN"),
            "SAMPLE_URL"
        );
        let keyed = Manifest {
            takes_user: false,
            ..SAMPLE
        };
        assert_eq!(
            needs(&keyed, "SAMPLE_URL", None, "SAMPLE_TOKEN"),
            "SAMPLE_URL and SAMPLE_TOKEN"
        );
        let plex = singularr::core::config::IntegrationType::Plex.manifest();
        assert_eq!(
            needs(plex, "PLEX_URL", None, "PLEX_TOKEN"),
            "PLEX_URL and PLEX_TOKEN"
        );
    }
}
