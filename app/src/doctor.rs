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
use singularr::policy;
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

/// Where a download client's address and sign-in are set, as its source names them:
/// `QBIT_URL, QBIT_USER and QBIT_PASS`.
fn sign_in_vars(i: &Integration) -> String {
    let user = i.kind.takes_user().then(|| {
        if i.source == "environment" {
            format!("{}_USER", i.url_var.trim_end_matches("_URL"))
        } else {
            format!("the `user` of integration {}", i.id)
        }
    });
    match user {
        Some(user) => format!("{}, {user} and {}", i.url_var, i.secret_var),
        None => format!("{} and {}", i.url_var, i.secret_var),
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
        let var = slot.id.to_uppercase();
        let client = config
            .integration(&slot.id)
            .filter(|i| i.kind.client().is_some());
        let hint = if let Some(i) = client {
            format!("Set {}", sign_in_vars(i))
        } else if slot.kind == "hush" || slot.kind == "tdarr" {
            format!("Set {var}_URL")
        } else {
            format!("Set {var}_URL and {var}_API_KEY")
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
