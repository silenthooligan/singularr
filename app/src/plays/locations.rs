//! The location database the statistics place addresses with (`core::geoip`), kept current.
//!
//! The database is the operator's file when `SINGULARR_GEOIP_PATH` names one. Otherwise it is
//! DB-IP's City Lite, downloaded into the data directory. The download is on by default, as a
//! self-hosted statistics tool has it. The setting `jobs.plays.locations` or
//! `SINGULARR_GEOIP_DOWNLOAD=off` turns it off. Nothing is downloaded while it is off, while the
//! sync is off (`SINGULARR_SYNC_ENABLED=false` reads no service), or while no media server and no
//! play history is configured: then there is nothing to locate.
//!
//! The loop checks once a day that the month's file is there. The site publishes it a few days
//! into the month. Until then the month before serves. The new month not being out is then no
//! failure. A new file is put in use only once it reads whole. The older months are removed
//! after that. A file already in the data directory stays in use with the download off. Without
//! any file the statistics show addresses alone. A private address reads as the local network.

use crate::core::config::Config;
use crate::core::geoip::{self, GeoIp, Missed, Source};
use crate::core::jobs::{self, JobSpec};
use crate::core::outbound::Outbound;
use crate::settings::Settings;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;

/// The job's pass name.
pub const LOCATIONS: &str = "locations";
/// The setting that downloads the database.
pub const DOWNLOAD: &str = "jobs.plays.locations";
const EVERY: Duration = Duration::from_secs(24 * 3600);
const FIRST_WAIT: Duration = Duration::from_secs(20);

/// What the loop needs: the database in use, the way out, and the settings (with the
/// configuration).
#[derive(Clone)]
pub struct LocationDeps {
    pub geoip: GeoIp,
    pub out: Outbound,
    pub settings: Arc<Settings>,
}

/// Whether the database is downloaded: on unless the owner turns it off.
pub fn downloads(settings: &Settings) -> bool {
    settings.value(DOWNLOAD).as_bool().unwrap_or(true)
}

/// Whether plays are read at all: a media server or a play history is configured. Without one
/// there is nothing to locate.
pub fn plays_are_read(config: &Config) -> bool {
    config.media_servers.iter().any(|m| m.configured())
        || config.tautulli.as_ref().is_some_and(|t| t.configured())
}

/// Whether the data folder takes a file. It is made when it is missing, then written to once.
fn writable(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let probe = dir.join(".singularr-write-check");
    std::fs::write(&probe, b"")?;
    std::fs::remove_file(&probe)
}

/// Put a file in use, read in a blocking task: the file is read whole.
async fn load(geoip: &GeoIp, path: PathBuf) -> Result<Source, String> {
    let geo = geoip.clone();
    tokio::task::spawn_blocking(move || geo.load(&path))
        .await
        .map_err(|e| e.to_string())?
}

/// The database in use, in words for the job.
fn in_words(source: &Source) -> String {
    match geoip::month_in(&source.file) {
        Some(month) => format!("DB-IP City Lite {month} in use"),
        None => format!("{} in use", source.file),
    }
}

/// One pass: the operator's file put in use, else the downloaded one, with the month's file
/// downloaded when it is missing. Returns what is in use, in words for the job.
pub async fn pass(deps: &LocationDeps) -> Result<String, String> {
    let config = deps.settings.config();
    if !plays_are_read(config) {
        return Ok("No plays to locate".to_string());
    }
    if let Some(path) = &config.geoip.path {
        let source = match deps.geoip.source() {
            Some(s) => s,
            None => load(&deps.geoip, PathBuf::from(path)).await?,
        };
        return Ok(in_words(&source));
    }
    let dir = PathBuf::from(&config.geoip.data_dir);
    geoip::sweep_parts(&dir);
    let mut problem = None;
    if deps.geoip.source().is_none() {
        problem = put_newest(deps, &dir).await;
    }
    let switched_on = downloads(&deps.settings);
    if switched_on
        && config.sync.enabled
        && let Some(e) = refresh(deps, &dir).await
    {
        problem = Some(e);
    }
    match (deps.geoip.source(), problem) {
        (_, Some(e)) => Err(e),
        (Some(source), None) => Ok(in_words(&source)),
        (None, None) if !switched_on => Ok("Download off".to_string()),
        (None, None) if !config.sync.enabled => Ok("Sync off".to_string()),
        (None, None) => Ok("No location database".to_string()),
    }
}

/// Put the newest downloaded file that reads in use: what an earlier run left. A file that
/// does not read is removed. The next download then replaces it. Returns what went wrong.
async fn put_newest(deps: &LocationDeps, dir: &Path) -> Option<String> {
    let mut problem = None;
    for (_, path) in geoip::downloaded(dir) {
        match load(&deps.geoip, path.clone()).await {
            Ok(_) => return problem,
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                problem = Some(e);
            }
        }
    }
    problem
}

/// Download the month's file when it is not there yet. While the site has not published it,
/// the month before serves. Returns what went wrong. The new month not out yet is no failure
/// while a file serves.
async fn refresh(deps: &LocationDeps, dir: &Path) -> Option<String> {
    let today = Utc::now().date_naive();
    let month = geoip::month_of(today);
    let have = geoip::downloaded(dir);
    if have.first().is_some_and(|(m, _)| *m == month) {
        return None;
    }
    if let Err(e) = writable(dir) {
        return Some(format!(
            "the data folder {} cannot be written: {e}",
            dir.display()
        ));
    }
    let before = geoip::month_of(geoip::month_before(today));
    match fetch(deps, dir, &month).await {
        Ok(()) => None,
        Err(Missed::NotOut)
            if deps.geoip.source().is_some() && have.iter().any(|(m, _)| *m == before) =>
        {
            None
        }
        Err(Missed::NotOut) => match fetch(deps, dir, &before).await {
            Ok(()) => None,
            Err(Missed::NotOut) => Some(format!(
                "download.db-ip.com has no file for {month} or {before}"
            )),
            Err(Missed::Failed(e)) => Some(e),
        },
        Err(Missed::Failed(e)) => Some(e),
    }
}

/// Download one month's file and put it in use. The older months are removed after that.
async fn fetch(deps: &LocationDeps, dir: &Path, month: &str) -> Result<(), Missed> {
    let (_, db) = geoip::download(&deps.out, dir, month).await?;
    deps.geoip.put(db);
    geoip::remove_others(dir, month);
    Ok(())
}

/// The loop's job id.
fn job_id() -> String {
    format!("plays:{LOCATIONS}")
}

/// Ask the loop for a pass now: the owner turned the download on or off.
pub fn wake() {
    if let Some(job) = jobs::registry().get(&job_id()) {
        let _ = job.run_now();
    }
}

/// The loop: a pass soon after the start, then once a day, and when the download setting
/// changes. It runs whether or not the sync does.
pub fn spawn(deps: LocationDeps) -> JoinHandle<()> {
    tokio::spawn(async move {
        let job = jobs::registry().register(JobSpec {
            id: job_id(),
            pass: LOCATIONS.to_string(),
            integration: None,
            every: EVERY,
        });
        job.wait(FIRST_WAIT).await;
        loop {
            let started = job.begin();
            let outcome = pass(&deps).await;
            if let Err(e) = &outcome {
                tracing::warn!(error = %e, "the location database could not be brought up to date");
            }
            job.end(started, outcome);
            job.wait(EVERY).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::Method;
    use crate::core::outbound::recorded::Recorded;
    use std::collections::BTreeMap;

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(bytes).unwrap();
        gz.finish().unwrap()
    }

    fn fixture() -> Vec<u8> {
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geoip/test-city.mmdb"),
        )
        .unwrap()
    }

    /// A data folder of its own, as a text that lives as long as the test.
    fn folder(test: &str) -> (PathBuf, &'static str) {
        let dir = std::env::temp_dir().join(format!(
            "singularr-{test}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let text: &'static str = Box::leak(dir.to_string_lossy().into_owned().into_boxed_str());
        (dir, text)
    }

    /// The loop's needs over a configuration that reads plays from Tautulli. `extra` adds to
    /// the environment. `plays: false` leaves Tautulli out.
    fn deps(
        extra: &[(&'static str, &'static str)],
        dir: &'static str,
        recorded: &Arc<Recorded>,
        plays: bool,
    ) -> LocationDeps {
        let mut env = BTreeMap::from([
            ("DATABASE_URL", "postgres://x@127.0.0.1:1/x"),
            ("SINGULARR_DATA_DIR", dir),
        ]);
        if plays {
            env.extend([
                ("TAUTULLI_URL", "http://10.0.0.50:8181"),
                ("TAUTULLI_API_KEY", "f00dfeedf00dfeedf00dfeedf00dfeed"),
            ]);
        }
        env.extend(extra.iter().copied());
        let config = Config::from(&env).unwrap();
        let allow = crate::engines::service::allowlist(&config).unwrap();
        LocationDeps {
            geoip: GeoIp::default(),
            out: Outbound::with_transport(allow, recorded.clone()),
            settings: Arc::new(Settings::new(&config, &env, None)),
        }
    }

    fn months() -> (String, String) {
        let today = Utc::now().date_naive();
        (
            geoip::month_of(today),
            geoip::month_of(geoip::month_before(today)),
        )
    }

    #[tokio::test]
    async fn the_database_is_downloaded_once_a_month_unless_turned_off() {
        let (dir, dir_text) = folder("locations");
        let (month, _) = months();
        let recorded = Recorded::new();
        recorded.answer_bytes(
            Method::Get,
            &format!("dbip-city-lite-{month}.mmdb.gz"),
            200,
            &gzip(&fixture()),
        );
        // Turned off, or on with the sync off, or with no plays to locate: nothing is asked and
        // no database is in use.
        for (extra, plays, said) in [
            ([("SINGULARR_GEOIP_DOWNLOAD", "off")], true, "Download off"),
            ([("SINGULARR_SYNC_ENABLED", "false")], true, "Sync off"),
            (
                [("SINGULARR_GEOIP_DOWNLOAD", "on")],
                false,
                "No plays to locate",
            ),
        ] {
            let off = deps(&extra, dir_text, &recorded, plays);
            assert_eq!(pass(&off).await.unwrap(), said);
            assert!(off.geoip.source().is_none());
        }
        assert!(recorded.methods_seen().is_empty());
        // On, the default: the month's file, asked once.
        let on = deps(&[], dir_text, &recorded, true);
        assert!(downloads(&on.settings));
        assert_eq!(
            pass(&on).await.unwrap(),
            format!("DB-IP City Lite {month} in use")
        );
        assert_eq!(on.geoip.source().unwrap().file, geoip::file_name(&month));
        assert!(on.geoip.locate("203.0.113.7").unwrap().city.is_some());
        pass(&on).await.unwrap();
        assert_eq!(recorded.methods_seen(), [Method::Get]);
        // Turned off later, the file there stays in use and nothing more is asked.
        let later = deps(
            &[("SINGULARR_GEOIP_DOWNLOAD", "off")],
            dir_text,
            &recorded,
            true,
        );
        assert_eq!(
            pass(&later).await.unwrap(),
            format!("DB-IP City Lite {month} in use")
        );
        assert_eq!(recorded.methods_seen(), [Method::Get]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn the_month_before_serves_until_the_new_one_is_out() {
        let (dir, dir_text) = folder("locations-early");
        let (month, before) = months();
        let recorded = Recorded::new();
        recorded.answer_bytes(
            Method::Get,
            &format!("dbip-city-lite-{month}.mmdb.gz"),
            404,
            b"Not Found",
        );
        recorded.answer_bytes(
            Method::Get,
            &format!("dbip-city-lite-{before}.mmdb.gz"),
            200,
            &gzip(&fixture()),
        );
        let on = deps(&[], dir_text, &recorded, true);
        // The new month is not out: the month before is downloaded and serves. That is no
        // failure. The next day the new month alone is asked again.
        assert_eq!(
            pass(&on).await.unwrap(),
            format!("DB-IP City Lite {before} in use")
        );
        assert_eq!(
            pass(&on).await.unwrap(),
            format!("DB-IP City Lite {before} in use")
        );
        let asked = recorded.urls_seen();
        assert_eq!(asked.len(), 3, "{asked:?}");
        assert!(
            asked[0].contains(&month) && asked[1].contains(&before) && asked[2].contains(&month)
        );
        // Neither month out: a failure.
        let none = Recorded::new();
        let (dir2, dir2_text) = folder("locations-none");
        for m in [&month, &before] {
            none.answer_bytes(
                Method::Get,
                &format!("dbip-city-lite-{m}.mmdb.gz"),
                404,
                b"Not Found",
            );
        }
        let early = deps(&[], dir2_text, &none, true);
        assert!(pass(&early).await.unwrap_err().contains("has no file"));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[tokio::test]
    async fn a_download_that_does_not_read_keeps_the_database_in_service() {
        let (dir, dir_text) = folder("locations-broken");
        let (month, before) = months();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(geoip::file_name(&before)), fixture()).unwrap();
        // A part an interrupted download left.
        std::fs::write(
            dir.join(format!(".{}.part", geoip::file_name(&month))),
            b"half",
        )
        .unwrap();
        let mut broken = b"no search tree".to_vec();
        broken.extend_from_slice(b"\xAB\xCD\xEFMaxMind.com\xe0");
        let recorded = Recorded::new();
        recorded.answer_bytes(
            Method::Get,
            &format!("dbip-city-lite-{month}.mmdb.gz"),
            200,
            &gzip(&broken),
        );
        let on = deps(&[], dir_text, &recorded, true);
        let e = pass(&on).await.unwrap_err();
        assert!(e.contains("not a location database"), "{e}");
        // The month before stays on disk and in use; no file and no part of the broken month.
        assert_eq!(on.geoip.source().unwrap().file, geoip::file_name(&before));
        assert!(on.geoip.locate("203.0.113.7").unwrap().city.is_some());
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(left, [geoip::file_name(&before)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
