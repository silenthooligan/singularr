//! Where an address is, read on this machine from a location database: DB-IP's "IP to City
//! Lite" (CC BY 4.0, <https://db-ip.com>), a MaxMind DB file. No address is sent anywhere to be
//! placed.
//!
//! - The database is the file `SINGULARR_GEOIP_PATH` names, else the newest month Singularr
//!   downloaded into its data directory (`dbip-city-lite-YYYY-MM.mmdb`). Without one, an address
//!   has no place and is shown alone.
//! - A private, loopback, link-local or shared address (RFC 1918, RFC 4193, RFC 6598) is the
//!   local network, without a lookup.
//! - The download is one declared call: GET of the month's file on `download.db-ip.com`
//!   ([`calls`]), answered with the file compressed with gzip. It is written to disk as it
//!   arrives. A file is put in use only once it reads whole.
//! - Neither an address nor a lookup is ever written to the log.

use crate::core::outbound::{Calls, OutRequest, Outbound};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

/// The credit the database's license asks for, wherever a location is shown.
pub const CREDIT: &str = "IP geolocation by DB-IP";
pub const CREDIT_URL: &str = "https://db-ip.com";
/// Where the database is downloaded from.
pub const ORIGIN: &str = "https://download.db-ip.com";
/// The one path the download asks for: a month's file, `{month}` written `YYYY-MM`.
pub const FILE_PATH: &str = "/free/dbip-city-lite-{month}.mmdb.gz";
/// How long the download of one file may take: some 60 MB.
const DOWNLOAD_LIMIT: Duration = Duration::from_secs(900);
/// The start of a downloaded file's name, before its month.
const STEM: &str = "dbip-city-lite-";

/// The calls the download sends: GET of a month's file alone.
pub fn calls() -> Calls {
    Calls::default().get(FILE_PATH)
}

/// A month as the file names write it: `2026-11`.
pub fn month_of(day: NaiveDate) -> String {
    format!("{:04}-{:02}", day.year(), day.month())
}

/// The month before.
pub fn month_before(day: NaiveDate) -> NaiveDate {
    let first = day.with_day(1).unwrap_or(day);
    (first - chrono::Duration::days(1))
        .with_day(1)
        .unwrap_or(first)
}

/// The address of a month's file.
pub fn url_of(month: &str) -> String {
    format!("{ORIGIN}{}", FILE_PATH.replace("{month}", month))
}

/// The name a month's database has in the data directory.
pub fn file_name(month: &str) -> String {
    format!("{STEM}{month}.mmdb")
}

/// The name of a month's download while it is written.
fn part_name(month: &str) -> String {
    format!(".{}.part", file_name(month))
}

/// The month a file of the data directory holds, by its name.
pub fn month_in(name: &str) -> Option<String> {
    let month = name.strip_prefix(STEM)?.strip_suffix(".mmdb")?;
    let ok = month.len() == 7
        && month.as_bytes()[4] == b'-'
        && month
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 4 || b.is_ascii_digit());
    ok.then(|| month.to_string())
}

/// The downloaded databases in a directory, newest month first.
pub fn downloaded(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            Some((month_in(&name)?, e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found
}

/// The address in a text as the servers write it: an IPv4 or IPv6 address, with or without a
/// port and brackets. An IPv4 address mapped into IPv6 is read as the IPv4 address.
pub fn plain_address(text: &str) -> Option<IpAddr> {
    let t = text.trim();
    let ip = t
        .parse::<IpAddr>()
        .ok()
        .or_else(|| t.parse::<SocketAddr>().ok().map(|s| s.ip()))
        .or_else(|| {
            t.strip_prefix('[')
                .and_then(|rest| rest.split(']').next())
                .and_then(|inner| inner.parse::<IpAddr>().ok())
        })?;
    Some(match ip {
        IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(v6)),
        v4 => v4,
    })
}

/// An address as the servers write it, in one form: the address alone when the text holds one
/// ([`plain_address`]). `::ffff:198.51.100.7` and `198.51.100.7` are then one address. Other
/// text stays as it is.
pub fn address_text(text: String) -> String {
    plain_address(&text)
        .map(|ip| ip.to_string())
        .unwrap_or(text)
}

/// Whether an address is on a network of its own and not on the internet: private, loopback,
/// link-local, shared (RFC 6598: a carrier's or a mesh network's), or unique local.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Where an address is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Place {
    /// On a network of its own: no lookup is made.
    pub local: bool,
    pub city: Option<String>,
    /// The first subdivision: a state, a province, a region.
    pub region: Option<String>,
    pub country: Option<String>,
    /// ISO 3166-1 alpha-2.
    pub country_code: Option<String>,
}

impl Place {
    /// The place in words: `Local network`, `Paris, Île-de-France, France`, `United States`.
    /// `None` for a place the database does not know.
    pub fn label(&self) -> Option<String> {
        if self.local {
            return Some("Local network".to_string());
        }
        let parts: Vec<&str> = [&self.city, &self.region, &self.country]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .fold(Vec::new(), |mut parts, p| {
                if parts.last() != Some(&p) {
                    parts.push(p);
                }
                parts
            });
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// Names by language, as the database writes them.
#[derive(Debug, Default, Deserialize)]
struct Names {
    #[serde(default)]
    names: BTreeMap<String, String>,
}

impl Names {
    fn english(&self) -> Option<String> {
        self.names
            .get("en")
            .or_else(|| self.names.values().next())
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
    }
}

#[derive(Debug, Default, Deserialize)]
struct Country {
    #[serde(default)]
    iso_code: Option<String>,
    #[serde(default)]
    names: BTreeMap<String, String>,
}

/// The parts of a City record the places are made of.
#[derive(Debug, Default, Deserialize)]
struct CityRecord {
    #[serde(default)]
    city: Option<Names>,
    #[serde(default)]
    country: Option<Country>,
    #[serde(default)]
    subdivisions: Vec<Names>,
}

/// A database read whole and checked, ready to be put in use.
pub struct Database {
    reader: maxminddb::Reader<Vec<u8>>,
    /// The file it was read from, by name.
    file: String,
    /// When the database was built.
    built: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Database")
            .field("file", &self.file)
            .field("built", &self.built)
            .finish()
    }
}

impl Database {
    /// Read a database file whole. A file maxminddb cannot read is refused.
    pub fn open(path: &Path) -> Result<Database, String> {
        let reader = maxminddb::Reader::open_readfile(path).map_err(|e| e.to_string())?;
        let built = i64::try_from(reader.metadata().build_epoch)
            .ok()
            .and_then(|s| DateTime::from_timestamp(s, 0));
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        Ok(Database {
            reader,
            file,
            built,
        })
    }

    pub fn source(&self) -> Source {
        Source {
            file: self.file.clone(),
            built: self.built,
        }
    }
}

/// The location database in use, when there is one. A clone shares it: a new month's file
/// replaces it for every reader at once.
#[derive(Clone, Default)]
pub struct GeoIp {
    loaded: Arc<RwLock<Option<Arc<Database>>>>,
}

impl std::fmt::Debug for GeoIp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeoIp")
            .field("file", &self.source().map(|s| s.file))
            .finish()
    }
}

/// What the database in use is: its file and when it was built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub file: String,
    pub built: Option<DateTime<Utc>>,
}

impl GeoIp {
    /// Read a database file whole and put it in use.
    pub fn load(&self, path: &Path) -> Result<Source, String> {
        let db = Database::open(path)
            .map_err(|e| format!("the location database could not be read: {e}"))?;
        Ok(self.put(db))
    }

    /// Put a database in use. The one before goes once the lookups on it end: the two are in
    /// memory together only for that moment. A file that does not read never replaces one that
    /// does.
    pub fn put(&self, db: Database) -> Source {
        let source = db.source();
        *self.loaded.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(db));
        source
    }

    /// The database in use, when there is one.
    pub fn source(&self) -> Option<Source> {
        self.current().map(|db| db.source())
    }

    fn current(&self) -> Option<Arc<Database>> {
        self.loaded
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Where an address is. `None` for text that is no address. A local address is the local
    /// network whatever the database holds. A public one the database does not know, or one
    /// looked up without a database, has an empty place.
    pub fn locate(&self, address: &str) -> Option<Place> {
        let ip = plain_address(address)?;
        if is_local(ip) {
            return Some(Place {
                local: true,
                ..Place::default()
            });
        }
        let Some(db) = self.current() else {
            return Some(Place::default());
        };
        let record = db
            .reader
            .lookup(ip)
            .ok()
            .and_then(|found| found.decode::<CityRecord>().ok().flatten())
            .unwrap_or_default();
        Some(Place {
            local: false,
            city: record.city.as_ref().and_then(Names::english),
            region: record.subdivisions.first().and_then(Names::english),
            country: record.country.as_ref().and_then(|c| {
                c.names
                    .get("en")
                    .or_else(|| c.names.values().next())
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
            }),
            country_code: record
                .country
                .as_ref()
                .and_then(|c| c.iso_code.clone())
                .filter(|c| !c.trim().is_empty()),
        })
    }
}

/// Why a month's file was not downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Missed {
    /// The site has no file for that month yet: it publishes one a few days into the month.
    NotOut,
    /// Anything else, in words.
    Failed(String),
}

/// Writes a download into its part file as it arrives. An answer compressed with gzip (the site
/// sends it so) is uncompressed on the way. Any other answer is written as it is.
struct Unpack {
    /// The first bytes, until two of them tell whether the answer is compressed.
    head: Vec<u8>,
    chosen: bool,
    file: Option<File>,
    gzip: Option<flate2::write::GzDecoder<File>>,
}

impl Unpack {
    fn new(file: File) -> Unpack {
        Unpack {
            head: Vec::new(),
            chosen: false,
            file: Some(file),
            gzip: None,
        }
    }

    /// Choose the way by the first bytes. They go down it first.
    fn choose(&mut self) -> std::io::Result<()> {
        self.chosen = true;
        let head = std::mem::take(&mut self.head);
        if head.starts_with(&[0x1f, 0x8b])
            && let Some(file) = self.file.take()
        {
            let mut gzip = flate2::write::GzDecoder::new(file);
            gzip.write_all(&head)?;
            self.gzip = Some(gzip);
            return Ok(());
        }
        match self.file.as_mut() {
            Some(file) => file.write_all(&head),
            None => Err(std::io::Error::other("the part file is closed")),
        }
    }

    /// The file, whole: the last of a compressed answer uncompressed and all of it on disk.
    fn finish(mut self) -> std::io::Result<File> {
        if !self.chosen {
            self.choose()?;
        }
        let file = match self.gzip.take() {
            Some(gzip) => gzip.finish()?,
            None => self
                .file
                .take()
                .ok_or_else(|| std::io::Error::other("the part file is closed"))?,
        };
        file.sync_all()?;
        Ok(file)
    }
}

impl Write for Unpack {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if !self.chosen {
            self.head.extend_from_slice(buf);
            if self.head.len() >= 2 {
                self.choose()?;
            }
            return Ok(buf.len());
        }
        match (self.gzip.as_mut(), self.file.as_mut()) {
            (Some(gzip), _) => gzip.write(buf),
            (None, Some(file)) => file.write(buf),
            (None, None) => Err(std::io::Error::other("the part file is closed")),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match (self.gzip.as_mut(), self.file.as_mut()) {
            (Some(gzip), _) => gzip.flush(),
            (None, Some(file)) => file.flush(),
            (None, None) => Ok(()),
        }
    }
}

/// Download a month's database into `dir` through the way out and read it. The answer is
/// written to a part file as it arrives, uncompressed. The part becomes the month's file only
/// once maxminddb reads it whole. Returns the file and the database read from it.
pub async fn download(
    out: &Outbound,
    dir: &Path,
    month: &str,
) -> Result<(PathBuf, Database), Missed> {
    let written = |e: std::io::Error| format!("the location database could not be written: {e}");
    let target = dir.join(file_name(month));
    let part = dir.join(part_name(month));
    let file = File::create(&part).map_err(|e| Missed::Failed(written(e)))?;
    let mut unpack = Unpack::new(file);
    let sent = out
        .send_into(
            OutRequest::get(url_of(month)).timeout(DOWNLOAD_LIMIT),
            &mut unpack,
        )
        .await;
    let missed = match sent {
        Ok(res) if (200..300).contains(&res.status) => None,
        Ok(res) if res.status == 404 => Some(Missed::NotOut),
        Ok(res) => Some(Missed::Failed(format!(
            "download.db-ip.com answered {} for the file of {month}",
            res.status
        ))),
        Err(e) => Some(Missed::Failed(e.to_string())),
    };
    if let Some(missed) = missed {
        drop(unpack);
        let _ = std::fs::remove_file(&part);
        return Err(missed);
    }
    let (part_path, month_file) = (part.clone(), target.clone());
    let named = month.to_string();
    let read = tokio::task::spawn_blocking(move || -> Result<Database, String> {
        let outcome = unpack
            .finish()
            .map_err(written)
            .and_then(|_| {
                Database::open(&part_path).map_err(|e| {
                    format!(
                        "the file download.db-ip.com sent for {named} is not a location database: {e}"
                    )
                })
            })
            .and_then(|mut db| {
                std::fs::rename(&part_path, &month_file).map_err(written)?;
                db.file = file_name(&named);
                Ok(db)
            });
        if outcome.is_err() {
            let _ = std::fs::remove_file(&part_path);
        }
        outcome
    })
    .await
    .map_err(|e| Missed::Failed(e.to_string()))?;
    read.map(|db| (target, db)).map_err(Missed::Failed)
}

/// Remove the part files an interrupted download left in `dir`.
pub fn sweep_parts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let start = format!(".{STEM}");
    for e in entries.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(&start) && name.ends_with(".part") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Remove the downloaded databases of every month but `keep`.
pub fn remove_others(dir: &Path, keep: &str) {
    for (month, path) in downloaded(dir) {
        if month != keep {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method, OutboundError};

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/geoip/test-city.mmdb")
    }

    #[test]
    fn an_address_is_placed_from_the_file_and_a_local_one_without_it() {
        let geo = GeoIp::default();
        // Without a database: a public address has no place, a local one is the local network.
        assert_eq!(geo.locate("203.0.113.7"), Some(Place::default()));
        assert_eq!(
            geo.locate("10.0.0.20").and_then(|p| p.label()).as_deref(),
            Some("Local network")
        );
        assert_eq!(geo.locate("not an address"), None);
        let source = geo.load(&fixture()).unwrap();
        assert_eq!(source.file, "test-city.mmdb");
        assert!(source.built.is_some());
        let paris = geo.locate("203.0.113.7").unwrap();
        assert_eq!(
            (
                paris.city.as_deref(),
                paris.region.as_deref(),
                paris.country.as_deref(),
                paris.country_code.as_deref()
            ),
            (
                Some("Paris"),
                Some("Île-de-France"),
                Some("France"),
                Some("FR")
            )
        );
        assert_eq!(
            paris.label().as_deref(),
            Some("Paris, Île-de-France, France")
        );
        // A port, brackets, an IPv4 address mapped into IPv6; IPv6 itself.
        for text in [
            "198.51.100.20:32400",
            "::ffff:198.51.100.20",
            "[::ffff:198.51.100.20]:443",
        ] {
            assert_eq!(
                geo.locate(text).and_then(|p| p.label()).as_deref(),
                Some("Toronto, Ontario, Canada"),
                "{text}"
            );
        }
        assert_eq!(
            geo.locate("2001:db8::5").and_then(|p| p.label()).as_deref(),
            Some("Berlin, Germany")
        );
        // A country without a city; an address the file does not hold.
        assert_eq!(
            geo.locate("192.0.2.1").and_then(|p| p.label()).as_deref(),
            Some("United States")
        );
        assert_eq!(geo.locate("8.8.8.8"), Some(Place::default()));
        for local in [
            "172.16.5.4",
            "10.0.0.20",
            "127.0.0.1",
            "169.254.3.4",
            "fd00::1",
            "fe80::1",
            "::1",
        ] {
            assert!(geo.locate(local).unwrap().local, "{local}");
        }
        // A shared address (RFC 6598), as a carrier or a mesh network hands one out.
        let shared = std::net::Ipv4Addr::new(100, 101, 102, 103);
        assert!(geo.locate(&shared.to_string()).unwrap().local);
        assert!(!is_local(IpAddr::V4(std::net::Ipv4Addr::new(
            100, 128, 0, 1
        ))));
        assert!(!is_local("203.0.113.7".parse().unwrap()));
        assert!(GeoIp::default().load(Path::new("/nowhere/x.mmdb")).is_err());
    }

    #[test]
    fn the_download_asks_for_the_month_s_file_alone() {
        assert_eq!(
            url_of("2026-11"),
            "https://download.db-ip.com/free/dbip-city-lite-2026-11.mmdb.gz"
        );
        let day = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        assert_eq!(
            (month_of(day), month_of(month_before(day))),
            ("2026-01".to_string(), "2025-12".to_string())
        );
        assert_eq!(
            month_in("dbip-city-lite-2026-11.mmdb").as_deref(),
            Some("2026-11")
        );
        assert_eq!(month_in("dbip-city-lite-2026-1.mmdb"), None);
        assert_eq!(month_in(".dbip-city-lite-2026-11.mmdb.part"), None);
        let mut allow = Allowlist::default();
        allow.allow_integration("db-ip", ORIGIN, calls()).unwrap();
        assert!(allow.check(Method::Get, &url_of("2026-11")).is_ok());
        for refused in [
            "https://download.db-ip.com/free/dbip-country-lite-2026-11.mmdb.gz",
            "https://download.db-ip.com/free/dbip-city-lite-2026-1.mmdb.gz",
            "https://download.db-ip.com/free/dbip-city-lite-2026-13.mmdb.gz",
            "https://download.db-ip.com/free/dbip-city-lite-2026-11.csv.gz",
            "https://download.db-ip.com/",
        ] {
            assert!(
                matches!(
                    allow.check(Method::Get, refused),
                    Err(OutboundError::NotListed { .. })
                ),
                "{refused}"
            );
        }
        assert!(allow.check(Method::Post, &url_of("2026-11")).is_err());
    }

    #[tokio::test]
    async fn a_downloaded_month_is_written_whole_and_read() {
        use std::io::Write;
        let gzip = |bytes: &[u8]| {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            gz.write_all(bytes).unwrap();
            gz.finish().unwrap()
        };
        let mmdb = std::fs::read(fixture()).unwrap();
        let recorded = Recorded::new();
        recorded.answer_bytes(
            Method::Get,
            "dbip-city-lite-2026-11.mmdb.gz",
            200,
            &gzip(&mmdb),
        );
        // A month not out yet; a page in place of the file; a file with a database's metadata
        // mark that maxminddb cannot read; the file sent without its compression.
        recorded.answer_bytes(
            Method::Get,
            "dbip-city-lite-2026-12.mmdb.gz",
            404,
            b"Not Found",
        );
        recorded.answer_bytes(
            Method::Get,
            "dbip-city-lite-2026-09.mmdb.gz",
            200,
            &gzip(b"<html>Not here</html>"),
        );
        let mut broken = b"no search tree".to_vec();
        broken.extend_from_slice(b"\xAB\xCD\xEFMaxMind.com\xe0");
        recorded.answer_bytes(
            Method::Get,
            "dbip-city-lite-2026-07.mmdb.gz",
            200,
            &gzip(&broken),
        );
        recorded.answer_bytes(Method::Get, "dbip-city-lite-2026-08.mmdb.gz", 200, &mmdb);
        let mut allow = Allowlist::default();
        allow.allow_integration("db-ip", ORIGIN, calls()).unwrap();
        let out = Outbound::with_transport(allow, recorded.clone());
        let dir = std::env::temp_dir().join(format!(
            "singularr-geoip-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file_name("2026-10")), b"older").unwrap();
        let (written, db) = download(&out, &dir, "2026-11").await.unwrap();
        assert_eq!(std::fs::read(&written).unwrap(), mmdb);
        assert_eq!(db.source().file, "dbip-city-lite-2026-11.mmdb");
        assert_eq!(
            downloaded(&dir)
                .into_iter()
                .map(|(m, _)| m)
                .collect::<Vec<_>>(),
            ["2026-11", "2026-10"]
        );
        remove_others(&dir, "2026-11");
        assert_eq!(downloaded(&dir).len(), 1);
        let geo = GeoIp::default();
        geo.put(db);
        assert_eq!(geo.source().unwrap().file, "dbip-city-lite-2026-11.mmdb");
        assert!(geo.locate("203.0.113.9").unwrap().city.is_some());
        // A month not out yet, a page and a file that does not read: nothing is written. No part
        // is left behind.
        assert_eq!(
            download(&out, &dir, "2026-12").await.unwrap_err(),
            Missed::NotOut
        );
        assert!(!dir.join(file_name("2026-12")).exists());
        for month in ["2026-09", "2026-07"] {
            let Missed::Failed(why) = download(&out, &dir, month).await.unwrap_err() else {
                panic!("{month} is no location database");
            };
            assert!(why.contains("not a location database"), "{why}");
            assert!(!dir.join(file_name(month)).exists(), "{month}");
            assert!(!dir.join(part_name(month)).exists(), "{month}");
        }
        // The file as it is, when it comes without its compression.
        let (plain, _) = download(&out, &dir, "2026-08").await.unwrap();
        assert_eq!(std::fs::read(&plain).unwrap(), mmdb);
        assert_eq!(recorded.methods_seen(), [Method::Get; 5]);
        // The part an interrupted download left is swept; the months stay.
        std::fs::write(dir.join(part_name("2026-06")), b"half").unwrap();
        sweep_parts(&dir);
        assert!(!dir.join(part_name("2026-06")).exists());
        assert_eq!(downloaded(&dir).len(), 2);
        // One form of an address, whatever the server wrote.
        assert_eq!(address_text("::ffff:198.51.100.7".into()), "198.51.100.7");
        assert_eq!(address_text("[2001:db8::5]:443".into()), "2001:db8::5");
        assert_eq!(address_text("a player".into()), "a player");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
