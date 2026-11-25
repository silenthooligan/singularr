//! The Plex adapter (Plex Media Server HTTP API, JSON), read-only, behind [`MediaServer`].
//!
//! Endpoints used, all GET, with the token in `X-Plex-Token` and `Accept: application/json`:
//! `/` (the server's name, version and Plex Pass), `/library/sections`,
//! `/library/sections/{key}/all?includeGuids=1&type=` (paged with `X-Plex-Container-Start` and
//! `X-Plex-Container-Size`), `/status/sessions`, `/status/sessions/history/all` (the server's
//! own history of plays, paged the same way, newest first), `/accounts` and `/devices` (the
//! names behind that history), `/library/metadata/{key}` (the file of an item that is being
//! transcoded), and `/photo/:/transcode` (an image resized by the server, for album covers and
//! artist photos). Plex starts work on some of its GETs (a library scan, a watched mark). These
//! paths are the only GETs the way out lets through ([`calls`]).
//!
//! Answers are read leniently: a field of an unexpected shape is no value, and an item without
//! a rating key is left out.

use super::{
    Delivery, EngineError, ItemLink, ItemMedia, MediaServer, ServerAccount, ServerAlbum,
    ServerArtist, ServerDevice, ServerIdentity, ServerImage, ServerItem, ServerPlay,
    ServerPlayPage, ServerSection, ServerSession, ServerTrack, ServerVideo, height_word,
};
use crate::core::config::MediaServerConfig;
use crate::core::outbound::{Calls, OutRequest, Outbound, encode_component, expect_ok};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;

/// The kind of engine the Plex server is, in the engine list.
pub const KIND: &str = "plex";
/// Plex's web app, where an item's link opens unless the installation names another
/// (`SINGULARR_PLEX_WEB_URL`).
pub const WEB_APP: &str = "https://app.plex.tv/desktop";

/// The calls the adapter sends, the only GETs Plex is sent: the paths above, each with any
/// query.
pub fn calls() -> Calls {
    [
        "/",
        "/library/sections",
        "/library/sections/{id}/all",
        "/status/sessions",
        "/status/sessions/history/all",
        "/accounts",
        "/devices",
        "/library/metadata/{id}",
        "/photo/:/transcode",
    ]
    .into_iter()
    .fold(Calls::default(), |calls, path| calls.get(path))
}

/// The address of an item in Plex's web app: the server's machine identifier and the item's
/// key under `web`.
pub fn item_link(web: &str, machine_id: &str, rating_key: &str) -> ItemLink {
    let key = format!("/library/metadata/{rating_key}");
    ItemLink {
        url: format!(
            "{}/#!/server/{}/details?key={}",
            web.trim_end_matches('/'),
            encode_component(machine_id),
            encode_component(&key)
        ),
        key,
    }
}

/// A rating key as Plex makes them: one to ten digits, as the declared paths take them.
fn is_key(rating_key: &str) -> bool {
    (1..=10).contains(&rating_key.len()) && rating_key.chars().all(|c| c.is_ascii_digit())
}
/// Items asked for per page, and the most pages read of one section.
const PAGE: usize = 500;
const MAX_PAGES: usize = 200;

/// Plex's metadata types for `type=`: movie, show, artist, album, track.
pub const TYPE_MOVIE: u32 = 1;
pub const TYPE_SHOW: u32 = 2;
pub const TYPE_ARTIST: u32 = 8;
pub const TYPE_ALBUM: u32 = 9;
pub const TYPE_TRACK: u32 = 10;

pub struct Plex {
    id: String,
    base: String,
    token: String,
    /// Plex's web app, for an item's link.
    web: String,
    out: Outbound,
}

fn text(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn int(v: &Value, key: &str) -> Option<i64> {
    match v.get(key)? {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn float(v: &Value, key: &str) -> Option<f64> {
    match v.get(key)? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn list<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn tags(v: &Value, key: &str) -> Vec<String> {
    list(v, key).iter().filter_map(|t| text(t, "tag")).collect()
}

fn unix(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    int(v, key)
        .filter(|s| *s > 0 && *s < 4_000_000_000)
        .and_then(|s| DateTime::from_timestamp(s, 0))
}

/// The ids in a `Guid` list: `imdb://tt0133093`, `tmdb://603`, `tvdb://81189`.
fn guids(v: &Value) -> (Option<String>, Option<i64>, Option<i64>) {
    let mut imdb = None;
    let mut tmdb = None;
    let mut tvdb = None;
    for g in list(v, "Guid") {
        let Some(id) = text(g, "id") else {
            continue;
        };
        if let Some(x) = id.strip_prefix("imdb://") {
            imdb = Some(x.to_string());
        } else if let Some(x) = id.strip_prefix("tmdb://") {
            tmdb = x.parse().ok().filter(|n: &i64| *n > 0);
        } else if let Some(x) = id.strip_prefix("tvdb://") {
            tvdb = x.parse().ok().filter(|n: &i64| *n > 0);
        }
    }
    (imdb, tmdb, tvdb)
}

/// The items of a `MediaContainer` answer.
fn metadata(v: &Value) -> &[Value] {
    v.get("MediaContainer")
        .map(|c| list(c, "Metadata"))
        .unwrap_or(&[])
}

pub fn identity(v: &Value) -> ServerIdentity {
    let c = v.get("MediaContainer").unwrap_or(&Value::Null);
    ServerIdentity {
        name: text(c, "friendlyName").unwrap_or_else(|| "Plex".to_string()),
        machine_id: text(c, "machineIdentifier"),
        version: text(c, "version"),
        platform: text(c, "platform"),
        plex_pass: c
            .get("myPlexSubscription")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

pub fn sections(v: &Value) -> Vec<ServerSection> {
    v.get("MediaContainer")
        .map(|c| list(c, "Directory"))
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| {
            Some(ServerSection {
                key: text(d, "key")?,
                kind: text(d, "type")?,
                title: text(d, "title")?,
                agent: text(d, "agent"),
                locations: list(d, "Location")
                    .iter()
                    .filter_map(|l| text(l, "path"))
                    .collect(),
            })
        })
        .collect()
}

pub fn item(v: &Value) -> Option<ServerItem> {
    let (imdb, tmdb, tvdb) = guids(v);
    Some(ServerItem {
        rating_key: text(v, "ratingKey")?,
        kind: text(v, "type").unwrap_or_default(),
        title: text(v, "title")?,
        year: int(v, "year").and_then(|y| i32::try_from(y).ok()),
        imdb,
        tmdb,
        tvdb,
        critic_rating: float(v, "rating"),
        critic_image: text(v, "ratingImage"),
        audience_rating: float(v, "audienceRating"),
        audience_image: text(v, "audienceRatingImage"),
        content_rating: text(v, "contentRating"),
        plays: int(v, "viewCount").unwrap_or(0),
        last_played: unix(v, "lastViewedAt"),
        edition: text(v, "editionTitle"),
        added: unix(v, "addedAt"),
        episodes: int(v, "leafCount"),
        watched_episodes: int(v, "viewedLeafCount"),
        thumb: text(v, "thumb"),
    })
}

pub fn artist(v: &Value) -> Option<ServerArtist> {
    Some(ServerArtist {
        rating_key: text(v, "ratingKey")?,
        name: text(v, "title")?,
        genres: tags(v, "Genre"),
        countries: tags(v, "Country"),
        summary: text(v, "summary"),
        thumb: text(v, "thumb"),
        added: unix(v, "addedAt"),
    })
}

pub fn album(v: &Value) -> Option<ServerAlbum> {
    Some(ServerAlbum {
        rating_key: text(v, "ratingKey")?,
        title: text(v, "title")?,
        artist: text(v, "parentTitle"),
        artist_key: text(v, "parentRatingKey"),
        year: int(v, "year").and_then(|y| i32::try_from(y).ok()),
        released: text(v, "originallyAvailableAt"),
        genres: tags(v, "Genre"),
        label: text(v, "studio"),
        added: unix(v, "addedAt"),
        thumb: text(v, "thumb"),
        plays: int(v, "viewCount").unwrap_or(0),
    })
}

pub fn track(v: &Value) -> Option<ServerTrack> {
    let media = list(v, "Media").first().unwrap_or(&Value::Null);
    let part = list(media, "Part").first().unwrap_or(&Value::Null);
    Some(ServerTrack {
        rating_key: text(v, "ratingKey")?,
        album_key: text(v, "parentRatingKey")?,
        number: int(v, "index").and_then(|n| i32::try_from(n).ok()),
        disc: int(v, "parentIndex").and_then(|n| i32::try_from(n).ok()),
        title: text(v, "title").unwrap_or_default(),
        artist: text(v, "originalTitle"),
        duration_ms: int(v, "duration"),
        codec: text(media, "audioCodec"),
        bitrate: int(media, "bitrate"),
        channels: int(media, "audioChannels"),
        container: text(media, "container"),
        size: int(part, "size"),
        file: text(part, "file"),
    })
}

pub fn video(v: &Value) -> Option<ServerVideo> {
    let media = list(v, "Media").first().unwrap_or(&Value::Null);
    let part = list(media, "Part").first().unwrap_or(&Value::Null);
    Some(ServerVideo {
        rating_key: text(v, "ratingKey")?,
        title: text(v, "title").unwrap_or_default(),
        year: int(v, "year").and_then(|y| i32::try_from(y).ok()),
        duration_ms: int(v, "duration").or_else(|| int(media, "duration")),
        codec: text(media, "videoCodec"),
        height: int(media, "height"),
        file: text(part, "file"),
        size: int(part, "size"),
        added: unix(v, "addedAt"),
        thumb: text(v, "thumb"),
        plays: int(v, "viewCount").unwrap_or(0),
    })
}

/// A resolution in the client's words: `4K`, `1080p`, `720p`, `SD`.
pub fn resolution_word(raw: &str) -> Option<String> {
    let t = raw.trim().to_ascii_lowercase();
    match t.as_str() {
        "" => None,
        "4k" | "2160" | "2160p" => Some("4K".to_string()),
        "sd" => Some("SD".to_string()),
        other => {
            let digits = other.trim_end_matches('p');
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                Some(format!("{digits}p"))
            } else {
                Some(raw.trim().to_string())
            }
        }
    }
}

/// One stream's decision in the product's words.
fn decision_word(raw: &str) -> Option<String> {
    let word = match raw.trim().to_ascii_lowercase().as_str() {
        "transcode" => "transcode",
        "copy" | "directstream" | "direct stream" => "direct stream",
        "directplay" | "direct play" => "direct play",
        _ => return None,
    };
    Some(word.to_string())
}

/// The version of an item a session plays: the one the server marks as selected, else the
/// first.
fn selected_media(v: &Value) -> &Value {
    let all = list(v, "Media");
    all.iter()
        .find(|m| m.get("selected").and_then(Value::as_bool) == Some(true))
        .or_else(|| all.first())
        .unwrap_or(&Value::Null)
}

/// How a session's video and audio reach the player, with the resolutions and bitrates. While
/// the video is transcoded the session describes what is sent, so the file's own resolution is
/// left for [`MediaServer::item_media`].
pub fn delivery(v: &Value) -> Delivery {
    let media = selected_media(v);
    let transcode = v.get("TranscodeSession");
    let stream = |key: &str, present: bool| match transcode {
        Some(t) => text(t, key)
            .and_then(|w| decision_word(&w))
            .or_else(|| present.then(|| "direct play".to_string())),
        None => present.then(|| "direct play".to_string()),
    };
    let has_video = text(media, "videoCodec").is_some()
        || text(media, "videoResolution").is_some()
        || transcode.and_then(|t| text(t, "videoDecision")).is_some();
    let has_audio = text(media, "audioCodec").is_some()
        || transcode.and_then(|t| text(t, "audioDecision")).is_some();
    let video = stream("videoDecision", has_video);
    let audio = stream("audioDecision", has_audio);
    let resolution = text(media, "videoResolution").and_then(|r| resolution_word(&r));
    let bitrate = int(media, "bitrate").filter(|b| *b > 0);
    let transcoded = video.as_deref() == Some("transcode");
    Delivery {
        stream_resolution: if transcoded {
            transcode
                .and_then(|t| int(t, "height"))
                .and_then(height_word)
                .or_else(|| resolution.clone())
        } else {
            resolution.clone()
        },
        stream_bitrate: bitrate,
        source_resolution: if transcoded { None } else { resolution },
        source_bitrate: if transcoded { None } else { bitrate },
        video,
        audio,
    }
}

/// How a session plays: `direct play`, `direct stream` (the container is changed, nothing is
/// encoded again) or `transcode`.
fn decision(v: &Value) -> String {
    let Some(t) = v.get("TranscodeSession") else {
        return "direct play".to_string();
    };
    let video = text(t, "videoDecision").unwrap_or_default();
    let audio = text(t, "audioDecision").unwrap_or_default();
    if video == "transcode" || audio == "transcode" {
        "transcode".to_string()
    } else {
        "direct stream".to_string()
    }
}

pub fn session(v: &Value) -> Option<ServerSession> {
    let duration = int(v, "duration");
    let offset = int(v, "viewOffset");
    let user = v.get("User").unwrap_or(&Value::Null);
    let player = v.get("Player").unwrap_or(&Value::Null);
    let place = v.get("Session").unwrap_or(&Value::Null);
    let local = player
        .get("local")
        .and_then(|l| match l {
            Value::Bool(b) => Some(*b),
            Value::Number(n) => n.as_i64().map(|n| n != 0),
            Value::String(s) => Some(s == "1" || s.eq_ignore_ascii_case("true")),
            _ => None,
        })
        .or_else(|| text(place, "location").map(|l| l.eq_ignore_ascii_case("lan")));
    let address = if local == Some(true) {
        text(player, "address")
    } else {
        text(player, "remotePublicAddress").or_else(|| text(player, "address"))
    };
    Some(ServerSession {
        server: String::new(),
        kind: text(v, "type").unwrap_or_default(),
        title: text(v, "title")?,
        show: text(v, "grandparentTitle"),
        season: int(v, "parentIndex").and_then(|n| i32::try_from(n).ok()),
        episode: int(v, "index").and_then(|n| i32::try_from(n).ok()),
        person: text(user, "title"),
        player: text(player, "title").or_else(|| text(player, "product")),
        state: text(player, "state"),
        progress: match (offset, duration) {
            (Some(o), Some(d)) if d > 0 => Some(((o as f64 / d as f64) * 100.0).clamp(0.0, 100.0)),
            _ => None,
        },
        decision: decision(v),
        rating_key: text(v, "ratingKey"),
        show_key: text(v, "grandparentRatingKey"),
        session_id: text(place, "id").or_else(|| text(v, "sessionKey").map(|k| format!("key-{k}"))),
        account_id: int(user, "id"),
        library_key: text(v, "librarySectionID"),
        year: int(v, "year").and_then(|y| i32::try_from(y).ok()),
        offset_ms: offset,
        duration_ms: duration,
        device: text(player, "device"),
        platform: text(player, "platform"),
        product: text(player, "product"),
        local,
        address,
        delivery: delivery(v),
        bandwidth: int(place, "bandwidth").filter(|b| *b > 0),
        raw: v.clone(),
    })
}

/// The last part of a server path: `8692` of `/status/sessions/history/8692`.
fn last_part(path: &str) -> Option<String> {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .map(str::to_string)
        .filter(|p| !p.is_empty())
}

/// One entry of the server's history. An entry without an id or a time is left out.
pub fn server_play(v: &Value) -> Option<ServerPlay> {
    let id = text(v, "historyKey")
        .and_then(|k| last_part(&k))
        .and_then(|k| k.parse::<i64>().ok())?;
    let kind = text(v, "type").unwrap_or_default();
    let episode = kind == "episode";
    let year = int(v, "year").or_else(|| {
        (kind == "movie")
            .then(|| text(v, "originallyAvailableAt"))
            .flatten()
            .and_then(|d| d.get(..4).and_then(|y| y.parse().ok()))
    });
    Some(ServerPlay {
        id,
        rating_key: text(v, "ratingKey").or_else(|| text(v, "key").and_then(|k| last_part(&k))),
        show_key: text(v, "grandparentRatingKey")
            .or_else(|| text(v, "grandparentKey").and_then(|k| last_part(&k))),
        title: text(v, "title")?,
        show: text(v, "grandparentTitle"),
        season: episode
            .then(|| int(v, "parentIndex"))
            .flatten()
            .and_then(|n| i32::try_from(n).ok()),
        episode: episode
            .then(|| int(v, "index"))
            .flatten()
            .and_then(|n| i32::try_from(n).ok()),
        year: year.and_then(|y| i32::try_from(y).ok()),
        library_key: text(v, "librarySectionID"),
        viewed_at: unix(v, "viewedAt")?,
        account_id: int(v, "accountID"),
        device_id: int(v, "deviceID"),
        kind,
        raw: v.clone(),
    })
}

/// A page of the server's history, with the size of the whole history.
pub fn history_page(v: &Value) -> ServerPlayPage {
    let c = v.get("MediaContainer").unwrap_or(&Value::Null);
    ServerPlayPage {
        plays: metadata(v).iter().filter_map(server_play).collect(),
        total: int(c, "totalSize").or_else(|| int(c, "size")).unwrap_or(0),
    }
}

pub fn accounts(v: &Value) -> Vec<ServerAccount> {
    v.get("MediaContainer")
        .map(|c| list(c, "Account"))
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| {
            Some(ServerAccount {
                id: int(a, "id")?,
                name: text(a, "name"),
            })
        })
        .collect()
}

pub fn devices(v: &Value) -> Vec<ServerDevice> {
    v.get("MediaContainer")
        .map(|c| list(c, "Device"))
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| {
            Some(ServerDevice {
                id: int(d, "id")?,
                name: text(d, "name"),
                platform: text(d, "platform"),
            })
        })
        .collect()
}

/// The resolution and bitrate of an item's file, from its record.
pub fn item_media(v: &Value) -> Option<ItemMedia> {
    let item = metadata(v).first()?;
    let media = selected_media(item);
    Some(ItemMedia {
        resolution: text(media, "videoResolution").and_then(|r| resolution_word(&r)),
        bitrate: int(media, "bitrate").filter(|b| *b > 0),
    })
}

impl Plex {
    /// `None` when the integration has no address or no token.
    pub fn new(config: &MediaServerConfig, out: Outbound) -> Option<Self> {
        Some(Plex {
            id: config.id.clone(),
            base: config.url.clone()?,
            token: config.secret.clone()?,
            web: WEB_APP.to_string(),
            out,
        })
    }

    /// The same adapter with items opening in the web app at `web`.
    pub fn with_web(mut self, web: &str) -> Plex {
        self.web = web.trim_end_matches('/').to_string();
        self
    }

    fn request(&self, path: &str) -> OutRequest {
        OutRequest::get(format!("{}{path}", self.base))
            .header("X-Plex-Token", self.token.clone())
            .header("Accept", "application/json")
            .header("X-Plex-Product", "Singularr")
            .header("X-Plex-Client-Identifier", format!("singularr-{}", self.id))
    }

    async fn get(&self, what: &'static str, path: &str) -> Result<Value, EngineError> {
        let req = self.request(path);
        let url = format!("{}{path}", self.base);
        let res = self.out.send(req).await?;
        expect_ok(&url, &res)?;
        serde_json::from_slice(&res.body).map_err(|e| EngineError::Decode {
            engine: self.id.clone(),
            what,
            message: e.to_string(),
        })
    }

    /// Every item of one type in a section, page by page.
    async fn all(
        &self,
        what: &'static str,
        section: &str,
        kind: u32,
    ) -> Result<Vec<Value>, EngineError> {
        let mut out: Vec<Value> = Vec::new();
        for page in 0..MAX_PAGES {
            let start = page * PAGE;
            let answer = self
                .get(
                    what,
                    &format!(
                        "/library/sections/{}/all?includeGuids=1&type={kind}&X-Plex-Container-Start={start}&X-Plex-Container-Size={PAGE}",
                        encode_component(section)
                    ),
                )
                .await?;
            let items = metadata(&answer);
            let total = answer
                .get("MediaContainer")
                .and_then(|c| int(c, "totalSize").or_else(|| int(c, "size")))
                .unwrap_or(0);
            let got = items.len();
            out.extend(items.iter().cloned());
            if got < PAGE || (total > 0 && out.len() as i64 >= total) {
                break;
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl MediaServer for Plex {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        KIND
    }

    fn keeps_history(&self) -> bool {
        true
    }

    fn item_thumb(&self, rating_key: &str) -> Option<String> {
        is_key(rating_key).then(|| format!("/library/metadata/{rating_key}/thumb"))
    }

    fn item_link(&self, machine_id: Option<&str>, rating_key: &str) -> Option<ItemLink> {
        let machine = machine_id.filter(|m| !m.is_empty())?;
        is_key(rating_key).then(|| item_link(&self.web, machine, rating_key))
    }

    async fn identity(&self) -> Result<ServerIdentity, EngineError> {
        Ok(identity(&self.get("identity", "/").await?))
    }

    async fn sections(&self) -> Result<Vec<ServerSection>, EngineError> {
        Ok(sections(&self.get("sections", "/library/sections").await?))
    }

    async fn items(&self, section: &ServerSection) -> Result<Vec<ServerItem>, EngineError> {
        let kind = if section.kind == "show" {
            TYPE_SHOW
        } else {
            TYPE_MOVIE
        };
        Ok(self
            .all("items", &section.key, kind)
            .await?
            .iter()
            .filter_map(item)
            .collect())
    }

    async fn artists(&self, section: &str) -> Result<Vec<ServerArtist>, EngineError> {
        Ok(self
            .all("artists", section, TYPE_ARTIST)
            .await?
            .iter()
            .filter_map(artist)
            .collect())
    }

    async fn albums(&self, section: &str) -> Result<Vec<ServerAlbum>, EngineError> {
        Ok(self
            .all("albums", section, TYPE_ALBUM)
            .await?
            .iter()
            .filter_map(album)
            .collect())
    }

    async fn tracks(&self, section: &str) -> Result<Vec<ServerTrack>, EngineError> {
        Ok(self
            .all("tracks", section, TYPE_TRACK)
            .await?
            .iter()
            .filter_map(track)
            .collect())
    }

    async fn sessions(&self) -> Result<Vec<ServerSession>, EngineError> {
        Ok(metadata(&self.get("sessions", "/status/sessions").await?)
            .iter()
            .filter_map(session)
            .map(|s| ServerSession {
                server: self.id.clone(),
                ..s
            })
            .collect())
    }

    async fn history(&self, start: u32, size: u32) -> Result<ServerPlayPage, EngineError> {
        let size = size.clamp(1, 1000);
        Ok(history_page(
            &self
                .get(
                    "history",
                    &format!(
                        "/status/sessions/history/all?sort=viewedAt:desc&X-Plex-Container-Start={start}&X-Plex-Container-Size={size}"
                    ),
                )
                .await?,
        ))
    }

    async fn accounts(&self) -> Result<Vec<ServerAccount>, EngineError> {
        Ok(accounts(&self.get("accounts", "/accounts").await?))
    }

    async fn devices(&self) -> Result<Vec<ServerDevice>, EngineError> {
        Ok(devices(&self.get("devices", "/devices").await?))
    }

    async fn item_media(&self, rating_key: &str) -> Result<Option<ItemMedia>, EngineError> {
        if !is_key(rating_key) {
            return Ok(None);
        }
        Ok(item_media(
            &self
                .get("item", &format!("/library/metadata/{rating_key}"))
                .await?,
        ))
    }

    async fn videos(&self, section: &str) -> Result<Vec<ServerVideo>, EngineError> {
        Ok(self
            .all("videos", section, TYPE_MOVIE)
            .await?
            .iter()
            .filter_map(video)
            .collect())
    }

    async fn photo(&self, thumb: &str, size: u32) -> Result<ServerImage, EngineError> {
        let size = size.clamp(64, 1200);
        let path = format!(
            "/photo/:/transcode?url={}&width={size}&height={size}&minSize=1&upscale=1",
            encode_component(thumb)
        );
        let url = format!("{}{path}", self.base);
        let req = OutRequest::get(&url)
            .header("X-Plex-Token", self.token.clone())
            .header("Accept", "image/jpeg,image/png,image/webp");
        let res = self.out.send(req).await?;
        expect_ok(&url, &res)?;
        let content_type = res
            .headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.clone())
            .filter(|v| v.starts_with("image/"))
            .unwrap_or_else(|| "image/jpeg".to_string());
        Ok(ServerImage {
            content_type,
            bytes: res.body.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method};
    use std::sync::Arc;

    const BASE: &str = "http://10.0.0.30:32400";

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    /// The adapter over recorded answers, held to the calls it declares.
    fn plex(recorded: Arc<Recorded>) -> Plex {
        let mut allow = Allowlist::default();
        allow.allow_integration("plex", BASE, calls()).unwrap();
        let config = MediaServerConfig {
            id: "plex".to_string(),
            kind: crate::core::config::MediaServerKind::Plex,
            url: Some(BASE.to_string()),
            secret: Some("test-token".to_string()),
            web_url: None,
        };
        Plex::new(&config, Outbound::with_transport(allow, recorded)).unwrap()
    }

    /// Plex starts work on some GETs: the adapter's own paths pass, those do not.
    #[tokio::test]
    async fn plex_is_sent_its_declared_reads_and_nothing_else() {
        let mut allow = Allowlist::default();
        allow.allow_integration("plex", BASE, calls()).unwrap();
        for path in [
            "/",
            "/library/sections",
            "/library/sections/6/all?includeGuids=1&type=9&X-Plex-Container-Start=0",
            "/status/sessions",
            "/status/sessions/history/all?sort=viewedAt:desc",
            "/accounts",
            "/devices",
            "/library/metadata/109882",
            "/photo/:/transcode?url=%2Flibrary%2Fmetadata%2F1%2Fthumb&width=300",
        ] {
            assert!(
                allow.check(Method::Get, &format!("{BASE}{path}")).is_ok(),
                "{path}"
            );
        }
        for path in [
            "/library/sections/1/refresh",
            "/:/scrobble?key=109882&identifier=com.plexapp.plugins.library",
            "/:/unscrobble?key=109882",
            "/library/optimize",
            "/butler/StartTasks",
            "/library/metadata/109882/refresh",
            "/library/sections/1/emptyTrash",
        ] {
            assert!(
                allow.check(Method::Get, &format!("{BASE}{path}")).is_err(),
                "{path} must be refused"
            );
        }
        for method in [Method::Post, Method::Put, Method::Delete] {
            assert!(
                allow
                    .check(method, &format!("{BASE}/library/sections"))
                    .is_err()
            );
        }
        // An image goes through the declared transcoder; a key Plex cannot have makes none.
        let recorded = Recorded::new();
        recorded.answer_with(
            Method::Get,
            &format!("{BASE}/photo/:/transcode"),
            200,
            "jpeg",
            &[("content-type", "image/jpeg")],
        );
        let p = plex(recorded.clone());
        let thumb = p.item_thumb("29499").unwrap();
        assert_eq!(thumb, "/library/metadata/29499/thumb");
        let image = p.photo(&thumb, 200).await.unwrap();
        assert_eq!(image.content_type, "image/jpeg");
        assert!(p.item_thumb("1/../x").is_none() && p.item_thumb("").is_none());
        // An item opens in Plex's web app by the server's machine identifier.
        let link = p.item_link(Some("abc123"), "77").unwrap();
        assert_eq!(link.key, "/library/metadata/77");
        assert_eq!(
            link.url,
            "https://app.plex.tv/desktop/#!/server/abc123/details?key=%2Flibrary%2Fmetadata%2F77"
        );
        assert!(p.item_link(None, "77").is_none());
        let other = plex(Recorded::new()).with_web("https://plex.example.org/web/");
        assert!(
            other
                .item_link(Some("abc123"), "77")
                .unwrap()
                .url
                .starts_with("https://plex.example.org/web/#!/server/abc123/")
        );
        assert_eq!((p.kind(), p.keeps_history()), ("plex", true));
    }

    #[tokio::test]
    async fn the_server_its_sections_and_items_are_read_with_get_alone() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}/"),
            200,
            &fixture("plex_root.json"),
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE}/library/sections"),
            200,
            &fixture("plex_library_sections.json"),
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE}/library/sections/1/all?includeGuids=1&type=1"),
            200,
            &fixture("plex_section_movies.json"),
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE}/status/sessions"),
            200,
            &fixture("plex_sessions.json"),
        );
        let p = plex(recorded.clone());
        let id = p.identity().await.unwrap();
        assert_eq!(
            (id.name.as_str(), id.version.as_deref(), id.plex_pass),
            ("Home Server", Some("1.43.5.11079-b3b13e506"), true)
        );
        assert_eq!(id.machine_id.as_deref(), Some("0123456789abcdef"));
        let s = p.sections().await.unwrap();
        assert_eq!(s.len(), 4);
        assert_eq!(
            (
                s[0].key.as_str(),
                s[0].kind.as_str(),
                s[0].locations.clone()
            ),
            (
                "1",
                "movie",
                vec!["/4kmovies".to_string(), "/movies".to_string()]
            )
        );
        let items = p.items(&s[0]).await.unwrap();
        assert_eq!(items.len(), 2);
        let burbs = &items[0];
        assert_eq!(
            (burbs.tmdb, burbs.imdb.as_deref(), burbs.critic_rating),
            (Some(11974), Some("tt0096734"), Some(5.7))
        );
        assert_eq!(
            burbs.critic_image.as_deref(),
            Some("rottentomatoes://image.rating.rotten")
        );
        assert_eq!(items[1].edition.as_deref(), Some("Open Matte"));
        assert_eq!(items[1].plays, 3);
        assert!(items[1].last_played.is_some());
        let sessions = p.sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|s| s.server == "plex"));
        assert_eq!(
            (sessions[0].person.as_deref(), sessions[0].decision.as_str()),
            (Some("friend"), "transcode")
        );
        assert_eq!(sessions[0].progress.map(|p| p.round()), Some(50.0));
        assert_eq!(sessions[1].decision, "direct play");
        assert_eq!(sessions[1].show.as_deref(), Some("Breaking Bad"));
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
        // The token travels in the header, never in the address.
        assert!(
            recorded
                .urls_seen()
                .iter()
                .all(|u| !u.contains("test-token"))
        );
    }

    #[tokio::test]
    async fn music_is_read_as_artists_albums_and_tracks() {
        let recorded = Recorded::new();
        for (kind, file) in [
            (TYPE_ARTIST, "plex_music_artists.json"),
            (TYPE_ALBUM, "plex_music_albums.json"),
            (TYPE_TRACK, "plex_music_tracks.json"),
        ] {
            recorded.answer(
                Method::Get,
                &format!("{BASE}/library/sections/6/all?includeGuids=1&type={kind}"),
                200,
                &fixture(file),
            );
        }
        let p = plex(recorded);
        let artists = p.artists("6").await.unwrap();
        assert_eq!(artists[0].name, "+44");
        assert_eq!(artists[0].genres, vec!["Pop/Rock"]);
        let albums = p.albums("6").await.unwrap();
        assert_eq!(
            (
                albums[0].title.as_str(),
                albums[0].label.as_deref(),
                albums[0].year
            ),
            (
                "When Your Heart Stops Beating",
                Some("Interscope Records"),
                Some(2006)
            )
        );
        assert_eq!(albums[0].artist_key.as_deref(), Some("120209"));
        assert!(albums[0].thumb.is_some() && albums[1].thumb.is_none());
        let tracks = p.tracks("6").await.unwrap();
        assert_eq!(
            (
                tracks[0].codec.as_deref(),
                tracks[0].bitrate,
                tracks[0].album_key.as_str()
            ),
            (Some("mp3"), Some(320), "120210")
        );
        assert!(tracks[0].file.as_deref().unwrap().ends_with(".mp3"));
    }

    #[test]
    fn a_plain_video_is_read_with_its_file() {
        let v = video(&serde_json::json!({
            "ratingKey": "29499", "title": "1986 - montana", "year": 1986, "duration": 3720000,
            "addedAt": 1695674537, "thumb": "/library/metadata/29499/thumb/1",
            "Media": [{"videoCodec": "h264", "height": 480,
                       "Part": [{"file": "/homemovies/1986 - montana.mp4", "size": 1197602196}]}]
        }))
        .unwrap();
        assert_eq!(
            (v.rating_key.as_str(), v.duration_ms, v.height),
            ("29499", Some(3_720_000), Some(480))
        );
        assert_eq!(v.file.as_deref(), Some("/homemovies/1986 - montana.mp4"));
        assert!(v.added.is_some() && v.thumb.is_some());
    }

    #[tokio::test]
    async fn the_history_its_accounts_and_devices_are_read_with_get_alone() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{BASE}/status/sessions/history/all"),
            200,
            &fixture("plex_history.json"),
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE}/accounts"),
            200,
            &fixture("plex_accounts.json"),
        );
        recorded.answer(
            Method::Get,
            &format!("{BASE}/devices"),
            200,
            &fixture("plex_devices.json"),
        );
        let p = plex(recorded.clone());
        let page = p.history(0, 40).await.unwrap();
        assert_eq!((page.plays.len(), page.total), (40, 7175));
        let first = &page.plays[0];
        assert_eq!(
            (
                first.id,
                first.rating_key.as_deref(),
                first.kind.as_str(),
                first.title.as_str()
            ),
            (8692, Some("139879"), "movie", "The Odyssey")
        );
        assert_eq!(
            (first.account_id, first.device_id, first.year),
            (Some(108_218_570), Some(161), Some(2026))
        );
        assert_eq!(first.viewed_at.timestamp(), 1_795_488_084);
        let friends = page.plays.iter().find(|p| p.id == 8677).unwrap();
        assert_eq!(
            (
                friends.show.as_deref(),
                friends.show_key.as_deref(),
                friends.season,
                friends.episode
            ),
            (Some("Friends"), Some("8565"), Some(8), Some(16))
        );
        assert_eq!(friends.library_key.as_deref(), Some("2"));
        let accounts = p.accounts().await.unwrap();
        assert_eq!(accounts.len(), 27);
        assert!(accounts.iter().any(|a| a.id == 1 && a.name.is_some()));
        // The account without a name stays, nameless.
        assert!(accounts.iter().any(|a| a.id == 0 && a.name.is_none()));
        let devices = p.devices().await.unwrap();
        let d = devices.iter().find(|d| d.id == 161).unwrap();
        assert_eq!(d.platform.as_deref(), Some("Android"));
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
        // Newest first, one page at a time.
        assert!(
            recorded
                .urls_seen()
                .iter()
                .any(|u| u.contains("sort=viewedAt:desc&X-Plex-Container-Start=0"))
        );
    }

    #[test]
    fn a_session_says_who_where_and_how_it_is_delivered() {
        let v = serde_json::json!({
            "type": "movie", "title": "The 'Burbs", "ratingKey": "109882", "year": 1989,
            "librarySectionID": "1", "sessionKey": "37", "duration": 6000000, "viewOffset": 3000000,
            "User": {"id": "1", "title": "owner"},
            "Player": {"title": "Living Room TV", "state": "playing", "device": "SHIELD Android TV",
                       "platform": "Android", "product": "Plex for Android (TV)", "local": true,
                       "address": "10.0.0.20", "remotePublicAddress": "203.0.113.9"},
            "Session": {"id": "abc123", "bandwidth": 9200, "location": "lan"},
            "Media": [{"videoResolution": "1080", "bitrate": 8000, "videoCodec": "h264",
                       "audioCodec": "aac", "selected": true}],
            "TranscodeSession": {"videoDecision": "transcode", "audioDecision": "copy", "height": 720}
        });
        let s = session(&v).unwrap();
        assert_eq!(
            (
                s.session_id.as_deref(),
                s.account_id,
                s.library_key.as_deref()
            ),
            (Some("abc123"), Some(1), Some("1"))
        );
        assert_eq!(
            (s.local, s.address.as_deref(), s.bandwidth),
            (Some(true), Some("10.0.0.20"), Some(9200))
        );
        assert_eq!(
            (
                s.device.as_deref(),
                s.platform.as_deref(),
                s.product.as_deref()
            ),
            (
                Some("SHIELD Android TV"),
                Some("Android"),
                Some("Plex for Android (TV)")
            )
        );
        assert_eq!(
            s.delivery,
            Delivery {
                video: Some("transcode".into()),
                audio: Some("direct stream".into()),
                source_resolution: None,
                source_bitrate: None,
                stream_resolution: Some("720p".into()),
                stream_bitrate: Some(8000),
            }
        );
        assert_eq!(
            (s.offset_ms, s.duration_ms),
            (Some(3_000_000), Some(6_000_000))
        );
        // Played directly, the file is what is sent; outside, the address is the public one.
        let mut direct = v.clone();
        direct.as_object_mut().unwrap().remove("TranscodeSession");
        direct["Player"]["local"] = serde_json::json!(false);
        direct["Media"][0]["videoResolution"] = serde_json::json!("4k");
        let d = session(&direct).unwrap();
        assert_eq!(
            (
                d.delivery.video.as_deref(),
                d.delivery.source_resolution.as_deref(),
                d.delivery.stream_resolution.as_deref()
            ),
            (Some("direct play"), Some("4K"), Some("4K"))
        );
        assert_eq!(
            (d.local, d.address.as_deref()),
            (Some(false), Some("203.0.113.9"))
        );
        // Without the session's id, its key stands in.
        let mut bare = direct.clone();
        bare.as_object_mut().unwrap().remove("Session");
        assert_eq!(
            session(&bare).unwrap().session_id.as_deref(),
            Some("key-37")
        );
        assert_eq!(resolution_word("sd").as_deref(), Some("SD"));
        assert_eq!(resolution_word("480").as_deref(), Some("480p"));
        assert_eq!(resolution_word(" ").as_deref(), None);
        let item = item_media(&serde_json::json!({"MediaContainer": {"Metadata": [
            {"Media": [{"videoResolution": "4k", "bitrate": 41000}]}]}}))
        .unwrap();
        assert_eq!(
            (item.resolution.as_deref(), item.bitrate),
            (Some("4K"), Some(41000))
        );
    }

    #[test]
    fn a_session_without_a_transcode_is_direct_play() {
        let s = session(&serde_json::json!({"type": "movie", "title": "X"})).unwrap();
        assert_eq!((s.decision.as_str(), s.progress), ("direct play", None));
        let copy = serde_json::json!({"title": "X", "TranscodeSession": {"videoDecision": "copy", "audioDecision": "copy"}});
        assert_eq!(session(&copy).unwrap().decision, "direct stream");
    }
}
