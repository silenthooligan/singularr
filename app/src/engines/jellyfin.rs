//! The Jellyfin and Emby adapter (their HTTP APIs, JSON), read-only, behind [`MediaServer`].
//!
//! Jellyfin and Emby share a lineage and most of an API. What differs is the [`Dialect`]: the
//! header that carries the key (Jellyfin: `Authorization: MediaBrowser Token="..."`; Emby:
//! `X-Emby-Token`), where the API is (Emby's under `/emby`), the routes that list the
//! libraries and the users, and the address of an item in the web app.
//!
//! Endpoints used, all GET, with the key in its header and never in an address:
//! `/System/Info` (the server's name, id and version), the libraries
//! (`/Library/VirtualFolders`, Emby's `/Library/VirtualFolders/Query`), `/Items` (a library's
//! movies, shows, music and videos with their provider ids, files and media sources, paged with
//! `StartIndex` and `Limit`; one item by `Ids`), `/Sessions` (what plays now), the users
//! (`/Users`, Emby's `/Users/Query`), and `/Items/{id}/Images/{type}` (an image resized by the
//! server). Both servers start work on some GETs (a stream starts a transcode, a download sends
//! a file). These paths are the only GETs the way out lets through ([`calls`]).
//!
//! Neither server keeps a history of plays that these calls read: plays come from its sessions
//! (`plays::service`). A user is named by a hexadecimal id. The layer counts accounts by number:
//! each user is counted by the number [`account_number`] makes of that id. The items of a
//! library are asked as the first administrator sees them. They carry that person's play
//! counts, the way a Plex server's items carry its owner's.
//!
//! The Emby dialect is written from Emby's API reference and has not been tried against a real
//! Emby server: its tests read answers made from that reference (`tests/fixtures/emby`).
//!
//! Answers are read leniently: a field of an unexpected shape is no value. An item without an
//! id is left out.

use super::{
    Delivery, EngineError, ItemLink, ItemMedia, MediaServer, ServerAccount, ServerAlbum,
    ServerArtist, ServerIdentity, ServerImage, ServerItem, ServerSection, ServerSession,
    ServerTrack, ServerVideo, height_word,
};
use crate::core::config::MediaServerConfig;
use crate::core::outbound::{
    Calls, OutRequest, Outbound, OutboundError, encode_component, expect_ok,
};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};

/// Items asked for per page, and the most pages read of one library.
const PAGE: usize = 500;
const MAX_PAGES: usize = 200;
/// Ticks of the servers' clocks per millisecond: a tick is 100 ns.
const TICKS_PER_MS: i64 = 10_000;
/// Sessions active within this many seconds, as Jellyfin's own dashboard asks for them.
const ACTIVE_WITHIN: u32 = 960;
/// The image keys that name the Rotten Tomatoes critics' score, fresh and rotten. A critic
/// rating of these servers is that score, 0 to 100, from their metadata provider.
const RT_FRESH: &str = "rottentomatoes://image.rating.ripe";
const RT_ROTTEN: &str = "rottentomatoes://image.rating.rotten";

const DIRECT_PLAY: &str = "direct play";
const DIRECT_STREAM: &str = "direct stream";
const TRANSCODE: &str = "transcode";

/// The fields asked for with each kind of item, beyond the ones every answer carries.
const TITLE_FIELDS: &str = "ProviderIds,DateCreated,RecursiveItemCount";
const ARTIST_FIELDS: &str = "Genres,Overview,ProductionLocations,DateCreated";
const ALBUM_FIELDS: &str = "Genres,Studios,DateCreated";
const TRACK_FIELDS: &str = "Path,MediaSources";
const VIDEO_FIELDS: &str = "Path,MediaSources,DateCreated";

/// Which of the two servers an adapter talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Jellyfin,
    Emby,
}

impl Dialect {
    /// The integration type.
    pub fn kind(self) -> &'static str {
        match self {
            Dialect::Jellyfin => "jellyfin",
            Dialect::Emby => "emby",
        }
    }

    /// The name a server takes when it gives none.
    fn label(self) -> &'static str {
        match self {
            Dialect::Jellyfin => "Jellyfin",
            Dialect::Emby => "Emby",
        }
    }

    /// The route that lists the libraries.
    fn libraries(self) -> &'static str {
        match self {
            Dialect::Jellyfin => "/Library/VirtualFolders",
            Dialect::Emby => "/Library/VirtualFolders/Query",
        }
    }

    /// The route that lists the users.
    fn users(self) -> &'static str {
        match self {
            Dialect::Jellyfin => "/Users",
            Dialect::Emby => "/Users/Query",
        }
    }

    /// The header that carries the key, with its value.
    fn key_header(self, key: &str) -> (&'static str, String) {
        match self {
            Dialect::Jellyfin => ("Authorization", format!("MediaBrowser Token=\"{key}\"")),
            Dialect::Emby => ("X-Emby-Token", key.to_string()),
        }
    }

    /// The query of an image of `size` pixels on its longer side.
    fn image_size(self, size: u32) -> String {
        match self {
            Dialect::Jellyfin => format!("maxWidth={size}&maxHeight={size}&quality=90"),
            Dialect::Emby => format!("MaxWidth={size}&MaxHeight={size}&Quality=90"),
        }
    }
}

/// Where the API is, from the integration's address: Jellyfin's at the address, Emby's under
/// `/emby`. An address that ends there already is taken as it is.
pub fn api_root(address: &str, dialect: Dialect) -> String {
    let address = address.trim_end_matches('/');
    match dialect {
        Dialect::Emby if !address.ends_with("/emby") => format!("{address}/emby"),
        _ => address.to_string(),
    }
}

/// Where the web app is: the integration's web address (`web_url`) when it has one, else its
/// address. Emby's `/emby` and a `/web` at the end are left out, as the links add the app's path.
fn web_root(address: &str, dialect: Dialect) -> String {
    let address = address.trim_end_matches('/');
    let address = address.strip_suffix("/web").unwrap_or(address);
    match dialect {
        Dialect::Emby => address.strip_suffix("/emby").unwrap_or(address).to_string(),
        Dialect::Jellyfin => address.to_string(),
    }
}

/// The calls the adapter sends, below the API's root ([`api_root`]): the only GETs the server
/// is sent, each with any query.
pub fn calls(dialect: Dialect) -> Calls {
    [
        "/System/Info",
        dialect.libraries(),
        dialect.users(),
        "/Items",
        "/Items/{any}/Images/{any}",
        "/Sessions",
    ]
    .into_iter()
    .fold(Calls::default(), |calls, path| calls.get(path))
}

/// The number an account is counted by: 2^52 plus the first 13 hexadecimal digits of the
/// user's id. It is none of the small numbers another server counts its accounts by. It is
/// below 2^53: a browser reads it exactly. A version 4 GUID's 13th digit is its version digit:
/// 48 of the 52 bits are random. Two users of one server are unlikely to share a number. The
/// adapter logs a warning when two do ([`shared_numbers`]). `None` for an id that is not
/// hexadecimal.
pub fn account_number(user_id: &str) -> Option<i64> {
    let hex: String = user_id.trim().chars().filter(|c| *c != '-').collect();
    if hex.len() < 13 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let low = u64::from_str_radix(&hex[..13], 16).ok()?;
    i64::try_from((1u64 << 52) | low).ok()
}

/// The account numbers that two or more users of one answer share ([`account_number`]): their
/// plays count as one person's. A list of users (Jellyfin) or a page of them (Emby).
pub fn shared_numbers(v: &Value) -> Vec<i64> {
    let mut seen: BTreeMap<i64, BTreeSet<String>> = BTreeMap::new();
    for u in records(v) {
        if let Some(id) = text(u, "Id")
            && let Some(number) = account_number(&id)
        {
            seen.entry(number)
                .or_default()
                .insert(id.to_ascii_lowercase().replace('-', ""));
        }
    }
    seen.into_iter()
        .filter(|(_, ids)| ids.len() > 1)
        .map(|(number, _)| number)
        .collect()
}

/// An id as the servers make them: letters, digits and `-`, at most 64 of them. A Jellyfin id is
/// 32 hexadecimal digits, an Emby item id a number.
fn is_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// An image path as the adapter makes them: `/Items/{id}/Images/{type}`.
fn is_image_path(thumb: &str) -> bool {
    let parts: Vec<&str> = thumb.split('/').collect();
    matches!(
        parts.as_slice(),
        ["", "Items", id, "Images", kind]
            if is_id(id) && !kind.is_empty() && kind.chars().all(|c| c.is_ascii_alphabetic())
    )
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
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
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

/// The names of a list: its texts, or the `Name` of each of its records.
fn names(v: &Value, key: &str) -> Vec<String> {
    list(v, key)
        .iter()
        .filter_map(|n| match n {
            Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
            other => text(other, "Name"),
        })
        .collect()
}

/// The records of a listing: the answer itself when it is a list, else its `Items` (a page).
fn records(v: &Value) -> &[Value] {
    match v {
        Value::Array(all) => all.as_slice(),
        page => list(page, "Items"),
    }
}

/// A time as the servers write it: `2026-11-25T03:48:28.3338032Z`. The year 1 the servers
/// write for no time, and any time before 1971, is no time.
fn date(v: &Value, key: &str) -> Option<DateTime<Utc>> {
    let raw = text(v, key)?;
    let at = DateTime::parse_from_rfc3339(&raw)
        .map(|d| d.with_timezone(&Utc))
        .or_else(|_| {
            NaiveDateTime::parse_from_str(raw.trim_end_matches('Z'), "%Y-%m-%dT%H:%M:%S%.f")
                .map(|d| d.and_utc())
        })
        .ok()?;
    (at.timestamp() > 31_536_000).then_some(at)
}

/// A length in ticks, in milliseconds.
fn ticks_ms(v: &Value, key: &str) -> Option<i64> {
    int(v, key).filter(|t| *t > 0).map(|t| t / TICKS_PER_MS)
}

/// The primary image of an item that has one, as [`MediaServer::photo`] takes it.
fn image_of(v: &Value, id: &str) -> Option<String> {
    v.get("ImageTags")
        .and_then(|tags| tags.get("Primary"))
        .and_then(Value::as_str)
        .map(|_| format!("/Items/{id}/Images/Primary"))
}

/// The ids in a `ProviderIds` record: `{"Imdb": "tt0074236", "Tmdb": "9878", "Tvdb": "170551"}`,
/// the names in any case.
fn provider_ids(v: &Value) -> (Option<String>, Option<i64>, Option<i64>) {
    let (mut imdb, mut tmdb, mut tvdb) = (None, None, None);
    let Some(map) = v.get("ProviderIds").and_then(Value::as_object) else {
        return (imdb, tmdb, tvdb);
    };
    for (name, value) in map {
        let id = match value {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            _ => continue,
        };
        if id.is_empty() {
            continue;
        }
        match name.to_ascii_lowercase().as_str() {
            "imdb" => imdb = Some(id),
            "tmdb" => tmdb = id.parse().ok().filter(|n: &i64| *n > 0),
            "tvdb" => tvdb = id.parse().ok().filter(|n: &i64| *n > 0),
            _ => {}
        }
    }
    (imdb, tmdb, tvdb)
}

/// An item's type in the layer's words: `movie` (a home video too, as Plex has it), `show`,
/// `episode`, `track`, `clip`, or the server's own word in lower case.
fn kind_word(v: &Value) -> String {
    match text(v, "Type").as_deref() {
        Some("Movie" | "Video") => "movie".to_string(),
        Some("Series") => "show".to_string(),
        Some("Episode") => "episode".to_string(),
        Some("Audio") => "track".to_string(),
        Some("MusicVideo") => "clip".to_string(),
        Some(other) => other.to_ascii_lowercase(),
        None => String::new(),
    }
}

/// A library's kind in the layer's words, from its collection type: `movie`, `show`, `music`,
/// `homevideo`, `photo`; `mixed` for a library of no one kind.
pub fn section_word(collection: Option<&str>) -> String {
    match collection.map(str::to_ascii_lowercase).as_deref() {
        Some("movies") => "movie",
        Some("tvshows") => "show",
        Some("music") => "music",
        Some("homevideos") => "homevideo",
        Some("photos") => "photo",
        Some("musicvideos") => "musicvideo",
        Some("boxsets") => "collection",
        Some("books") => "book",
        _ => "mixed",
    }
    .to_string()
}

/// The streams of an item: those of its first source, then its own.
fn streams(item: &Value) -> impl Iterator<Item = &Value> {
    let source = list(item, "MediaSources").first().unwrap_or(&Value::Null);
    list(source, "MediaStreams")
        .iter()
        .chain(list(item, "MediaStreams"))
}

fn stream_of<'v>(item: &'v Value, kind: &str) -> Option<&'v Value> {
    streams(item).find(|s| text(s, "Type").as_deref() == Some(kind))
}

/// The resolution and bitrate (kbit/s) of an item's file: the lines of its video stream, and
/// the bitrate of its first source or, without one, of its video stream.
fn source_of(item: &Value) -> (Option<String>, Option<i64>) {
    let source = list(item, "MediaSources").first().unwrap_or(&Value::Null);
    let video = stream_of(item, "Video");
    let height = video
        .and_then(|s| int(s, "Height"))
        .or_else(|| int(item, "Height"))
        .filter(|h| *h > 0);
    let bitrate = int(source, "Bitrate")
        .or_else(|| video.and_then(|s| int(s, "BitRate")))
        .filter(|b| *b > 0)
        .map(|b| b / 1000);
    (height.and_then(height_word), bitrate)
}

/// A container as the servers name it. Their name of the MP4 family is the list of its
/// members (`mov,mp4,m4a,3gp,3g2,mj2`), read as `mp4`.
fn container_word(raw: &str) -> Option<String> {
    let parts: Vec<&str> = raw
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let word: &str = if parts.contains(&"mp4") {
        "mp4"
    } else {
        parts.first().copied()?
    };
    Some(word.to_ascii_lowercase())
}

/// The audio track a play has: the stream at `index` (a session's `AudioStreamIndex`), else the
/// file's default audio stream, else its first.
fn audio_of(item: &Value, index: Option<i64>) -> Option<&Value> {
    let audio = || streams(item).filter(|s| text(s, "Type").as_deref() == Some("Audio"));
    index
        .and_then(|i| audio().find(|s| int(s, "Index") == Some(i)))
        .or_else(|| audio().find(|s| s.get("IsDefault").and_then(Value::as_bool) == Some(true)))
        .or_else(|| audio().next())
}

/// What an item's file is beyond its resolution and bitrate: its video codec, the codec and
/// channels of the audio track played (`audio_index`), and its container.
fn file_of(
    item: &Value,
    audio_index: Option<i64>,
) -> (Option<String>, Option<String>, Option<i32>, Option<String>) {
    let source = list(item, "MediaSources").first().unwrap_or(&Value::Null);
    let video = stream_of(item, "Video");
    let audio = audio_of(item, audio_index);
    (
        video.and_then(|s| text(s, "Codec")),
        audio.and_then(|s| text(s, "Codec")),
        audio
            .and_then(|s| int(s, "Channels"))
            .filter(|c| *c > 0)
            .and_then(|c| i32::try_from(c).ok()),
        text(source, "Container")
            .or_else(|| text(item, "Container"))
            .and_then(|c| container_word(&c)),
    )
}

/// Why the server transcodes, as its transcode record lists the reasons: a list, or the
/// reasons written in one text.
fn reasons(transcode: Option<&Value>) -> Vec<String> {
    match transcode.and_then(|t| t.get("TranscodeReasons")) {
        Some(Value::Array(all)) => all
            .iter()
            .filter_map(|r| r.as_str().map(str::trim))
            .filter(|r| !r.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether an item has pictures: a movie, an episode, a video.
fn is_video(item: &Value) -> bool {
    text(item, "MediaType").as_deref() == Some("Video")
        || matches!(kind_word(item).as_str(), "movie" | "episode" | "clip")
}

/// How one stream reaches the player, as the servers' own dashboards read it. The play method
/// `DirectPlay` is direct play. With a transcode under way, a stream the server copies is
/// direct stream and one it encodes is transcode. Without one, the play method `Transcode` is
/// transcode and any other sends the file as it is (the servers' "direct stream" sends the
/// file unchanged).
fn stream_word(method: Option<&str>, transcode: Option<&Value>, direct: &str) -> &'static str {
    let is = |word: &str| method.is_some_and(|m| m.eq_ignore_ascii_case(word));
    if is("DirectPlay") {
        return DIRECT_PLAY;
    }
    match transcode.map(|t| t.get(direct).and_then(Value::as_bool)) {
        Some(Some(true)) => DIRECT_STREAM,
        Some(Some(false)) => TRANSCODE,
        Some(None) if is("Transcode") => TRANSCODE,
        Some(None) => DIRECT_STREAM,
        None if is("Transcode") => TRANSCODE,
        None => DIRECT_PLAY,
    }
}

/// How a play is delivered as a whole: transcode when one of its streams is transcoded, direct
/// stream when one is copied into another container, else direct play.
fn overall(words: &[Option<&'static str>]) -> &'static str {
    if words.contains(&Some(TRANSCODE)) {
        TRANSCODE
    } else if words.contains(&Some(DIRECT_STREAM)) {
        DIRECT_STREAM
    } else {
        DIRECT_PLAY
    }
}

/// Whether a player's address is on the server's own network: a private, loopback or
/// link-local address. The servers do not say; this is how their own setting of the local
/// network reads by default. `None` for text that is no address.
pub fn is_home_address(address: &str) -> Option<bool> {
    let host = address.trim();
    let host = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None if host.matches(':').count() == 1 => host.split(':').next().unwrap_or(host),
        None => host,
    };
    let ip: IpAddr = host.parse().ok()?;
    let v4_home =
        |v4: std::net::Ipv4Addr| v4.is_private() || v4.is_loopback() || v4.is_link_local();
    Some(match ip {
        IpAddr::V4(v4) => v4_home(v4),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || v6.to_ipv4_mapped().is_some_and(v4_home)
        }
    })
}

pub fn identity(v: &Value, dialect: Dialect) -> ServerIdentity {
    ServerIdentity {
        name: text(v, "ServerName").unwrap_or_else(|| dialect.label().to_string()),
        machine_id: text(v, "Id"),
        version: text(v, "Version"),
        platform: text(v, "OperatingSystemDisplayName").or_else(|| text(v, "OperatingSystem")),
        plex_pass: false,
    }
}

/// The libraries: a list (Jellyfin) or a page of them (Emby).
pub fn sections(v: &Value) -> Vec<ServerSection> {
    records(v)
        .iter()
        .filter_map(|d| {
            Some(ServerSection {
                key: text(d, "ItemId").or_else(|| text(d, "Id"))?,
                kind: section_word(text(d, "CollectionType").as_deref()),
                title: text(d, "Name")?,
                agent: None,
                locations: list(d, "Locations")
                    .iter()
                    .filter_map(|l| l.as_str().map(str::trim))
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect()
}

pub fn item(v: &Value) -> Option<ServerItem> {
    let id = text(v, "Id")?;
    let (imdb, tmdb, tvdb) = provider_ids(v);
    let user = v.get("UserData").unwrap_or(&Value::Null);
    let critic = float(v, "CriticRating").filter(|c| (0.0..=100.0).contains(c));
    let episodes = int(v, "RecursiveItemCount").filter(|n| *n >= 0);
    Some(ServerItem {
        kind: kind_word(v),
        title: text(v, "Name")?,
        year: int(v, "ProductionYear").and_then(|y| i32::try_from(y).ok()),
        imdb,
        tmdb,
        tvdb,
        critic_rating: critic.map(|c| c / 10.0),
        critic_image: critic.map(|c| if c >= 60.0 { RT_FRESH } else { RT_ROTTEN }.to_string()),
        audience_rating: None,
        audience_image: None,
        content_rating: text(v, "OfficialRating"),
        plays: int(user, "PlayCount").unwrap_or(0),
        last_played: date(user, "LastPlayedDate"),
        edition: None,
        added: date(v, "DateCreated"),
        episodes,
        watched_episodes: episodes
            .zip(int(user, "UnplayedItemCount"))
            .map(|(all, left)| (all - left).max(0)),
        thumb: image_of(v, &id),
        rating_key: id,
    })
}

pub fn artist(v: &Value) -> Option<ServerArtist> {
    let id = text(v, "Id")?;
    Some(ServerArtist {
        name: text(v, "Name")?,
        genres: names(v, "Genres"),
        countries: names(v, "ProductionLocations"),
        summary: text(v, "Overview"),
        thumb: image_of(v, &id),
        added: date(v, "DateCreated"),
        rating_key: id,
    })
}

pub fn album(v: &Value) -> Option<ServerAlbum> {
    let id = text(v, "Id")?;
    let first = list(v, "AlbumArtists")
        .first()
        .or_else(|| list(v, "ArtistItems").first());
    Some(ServerAlbum {
        title: text(v, "Name")?,
        artist: text(v, "AlbumArtist").or_else(|| first.and_then(|a| text(a, "Name"))),
        artist_key: first.and_then(|a| text(a, "Id")),
        year: int(v, "ProductionYear").and_then(|y| i32::try_from(y).ok()),
        released: text(v, "PremiereDate").and_then(|d| d.get(..10).map(str::to_string)),
        genres: names(v, "Genres"),
        label: names(v, "Studios").into_iter().next(),
        added: date(v, "DateCreated"),
        thumb: image_of(v, &id),
        plays: v
            .get("UserData")
            .and_then(|u| int(u, "PlayCount"))
            .unwrap_or(0),
        rating_key: id,
    })
}

pub fn track(v: &Value) -> Option<ServerTrack> {
    let source = list(v, "MediaSources").first().unwrap_or(&Value::Null);
    let audio = stream_of(v, "Audio");
    let artists = names(v, "Artists");
    Some(ServerTrack {
        rating_key: text(v, "Id")?,
        album_key: text(v, "AlbumId")?,
        number: int(v, "IndexNumber").and_then(|n| i32::try_from(n).ok()),
        disc: int(v, "ParentIndexNumber").and_then(|n| i32::try_from(n).ok()),
        title: text(v, "Name").unwrap_or_default(),
        artist: (!artists.is_empty()).then(|| artists.join(", ")),
        duration_ms: ticks_ms(v, "RunTimeTicks"),
        codec: audio
            .and_then(|s| text(s, "Codec"))
            .or_else(|| text(source, "Container")),
        bitrate: int(source, "Bitrate")
            .or_else(|| audio.and_then(|s| int(s, "BitRate")))
            .filter(|b| *b > 0)
            .map(|b| b / 1000),
        channels: audio.and_then(|s| int(s, "Channels")),
        container: text(source, "Container").or_else(|| text(v, "Container")),
        size: int(source, "Size"),
        file: text(v, "Path").or_else(|| text(source, "Path")),
    })
}

pub fn video(v: &Value) -> Option<ServerVideo> {
    let id = text(v, "Id")?;
    let source = list(v, "MediaSources").first().unwrap_or(&Value::Null);
    let picture = stream_of(v, "Video");
    Some(ServerVideo {
        title: text(v, "Name").unwrap_or_default(),
        year: int(v, "ProductionYear").and_then(|y| i32::try_from(y).ok()),
        duration_ms: ticks_ms(v, "RunTimeTicks").or_else(|| ticks_ms(source, "RunTimeTicks")),
        codec: picture.and_then(|s| text(s, "Codec")),
        height: picture
            .and_then(|s| int(s, "Height"))
            .or_else(|| int(v, "Height")),
        file: text(v, "Path").or_else(|| text(source, "Path")),
        size: int(source, "Size"),
        added: date(v, "DateCreated"),
        thumb: image_of(v, &id),
        plays: v
            .get("UserData")
            .and_then(|u| int(u, "PlayCount"))
            .unwrap_or(0),
        rating_key: id,
    })
}

/// Something playing now. A session with nothing playing is left out. The server keeps one
/// session per connected player, whatever it plays one after another. A play is the session and
/// its item together (`<session>-<item>`). The pair names the play while it is open: the same
/// item played again later on that player is another play (`plays::store::by_source`).
pub fn session(v: &Value) -> Option<ServerSession> {
    let item = v.get("NowPlayingItem").filter(|i| i.is_object())?;
    let session = text(v, "Id")?;
    let item_id = text(item, "Id")?;
    let title = text(item, "Name")?;
    let play = v.get("PlayState").unwrap_or(&Value::Null);
    let transcode = v.get("TranscodingInfo").filter(|t| t.is_object());
    let method = text(play, "PlayMethod");
    let kind = kind_word(item);
    let episode = kind == "episode";
    let video = is_video(item).then(|| stream_word(method.as_deref(), transcode, "IsVideoDirect"));
    let audio = Some(stream_word(method.as_deref(), transcode, "IsAudioDirect"));
    let (source_resolution, source_bitrate) = source_of(item);
    let sent = transcode
        .and_then(|t| int(t, "Bitrate"))
        .filter(|b| *b > 0)
        .map(|b| b / 1000);
    let stream_bitrate = sent.or(source_bitrate);
    let stream_resolution = if video == Some(TRANSCODE) {
        transcode
            .and_then(|t| int(t, "Height"))
            .and_then(height_word)
            .or_else(|| source_resolution.clone())
    } else {
        source_resolution.clone()
    };
    let (video_codec, audio_codec, audio_channels, container) =
        file_of(item, int(play, "AudioStreamIndex"));
    let sent = |key: &str| transcode.and_then(|t| text(t, key));
    let sent_channels = transcode
        .and_then(|t| int(t, "AudioChannels"))
        .filter(|c| *c > 0)
        .and_then(|c| i32::try_from(c).ok());
    let duration = ticks_ms(item, "RunTimeTicks");
    let offset = int(play, "PositionTicks")
        .filter(|t| *t >= 0)
        .map(|t| t / TICKS_PER_MS);
    // The address alone: without a port, an IPv4 address mapped into IPv6 as IPv4.
    let address = text(v, "RemoteEndPoint").map(crate::core::geoip::address_text);
    let number = |key: &str| {
        episode
            .then(|| int(item, key))
            .flatten()
            .and_then(|n| i32::try_from(n).ok())
    };
    Some(ServerSession {
        server: String::new(),
        show: if episode {
            text(item, "SeriesName")
        } else if kind == "track" {
            text(item, "AlbumArtist").or_else(|| names(item, "Artists").into_iter().next())
        } else {
            None
        },
        season: number("ParentIndexNumber"),
        episode: number("IndexNumber"),
        person: text(v, "UserName"),
        player: text(v, "DeviceName"),
        state: Some(
            if play.get("IsPaused").and_then(Value::as_bool) == Some(true) {
                "paused"
            } else {
                "playing"
            }
            .to_string(),
        ),
        progress: match (offset, duration) {
            (Some(o), Some(d)) if d > 0 => Some(((o as f64 / d as f64) * 100.0).clamp(0.0, 100.0)),
            _ => None,
        },
        decision: overall(&[video, audio]).to_string(),
        rating_key: Some(item_id.clone()),
        show_key: episode.then(|| text(item, "SeriesId")).flatten(),
        session_id: Some(format!("{session}-{item_id}")),
        account_id: text(v, "UserId").as_deref().and_then(account_number),
        library_key: None,
        year: int(item, "ProductionYear").and_then(|y| i32::try_from(y).ok()),
        offset_ms: offset,
        duration_ms: duration,
        device: text(v, "DeviceType"),
        platform: text(v, "DeviceType").or_else(|| text(v, "Client")),
        product: text(v, "Client"),
        product_version: text(v, "ApplicationVersion"),
        platform_version: None,
        device_id: text(v, "DeviceId"),
        local: address.as_deref().and_then(is_home_address),
        secure: None,
        relayed: None,
        address,
        delivery: Delivery {
            video: video.map(str::to_string),
            audio: audio.map(str::to_string),
            source_resolution,
            source_bitrate,
            stream_resolution,
            stream_bitrate,
            stream_video_codec: sent("VideoCodec").or_else(|| video_codec.clone()),
            stream_audio_codec: sent("AudioCodec").or_else(|| audio_codec.clone()),
            stream_audio_channels: sent_channels.or(audio_channels),
            stream_container: sent("Container")
                .and_then(|c| container_word(&c))
                .or_else(|| container.clone()),
            source_video_codec: video_codec,
            source_audio_codec: audio_codec,
            source_audio_channels: audio_channels,
            source_container: container,
            transcode_reasons: reasons(transcode),
        },
        bandwidth: stream_bitrate,
        kind,
        title,
        raw: v.clone(),
    })
}

/// The users, by the number each is counted by: a list (Jellyfin) or a page of them (Emby).
pub fn accounts(v: &Value) -> Vec<ServerAccount> {
    records(v)
        .iter()
        .filter_map(|u| {
            Some(ServerAccount {
                id: account_number(&text(u, "Id")?)?,
                name: text(u, "Name"),
            })
        })
        .collect()
}

/// The id of the first administrator who is not disabled: the person whose play counts the
/// items are asked with.
pub fn owner(v: &Value) -> Option<String> {
    records(v)
        .iter()
        .find(|u| {
            let flag = |key: &str| {
                u.get("Policy")
                    .and_then(|p| p.get(key))
                    .and_then(Value::as_bool)
            };
            flag("IsAdministrator") == Some(true) && flag("IsDisabled") != Some(true)
        })
        .and_then(|u| text(u, "Id"))
        .filter(|id| is_id(id))
}

/// The resolution and bitrate of the first item of an answer.
pub fn item_media(v: &Value) -> Option<ItemMedia> {
    let item = records(v).first()?;
    let (resolution, bitrate) = source_of(item);
    let (video_codec, audio_codec, audio_channels, container) = file_of(item, None);
    Some(ItemMedia {
        resolution,
        bitrate,
        video_codec,
        audio_codec,
        audio_channels,
        container,
    })
}

pub struct Jellyfin {
    id: String,
    dialect: Dialect,
    /// The API's root, without a trailing `/`.
    api: String,
    /// The web app's root.
    web: String,
    key: String,
    out: Outbound,
    /// The administrator whose play counts the items carry, once known.
    owner: Mutex<Option<String>>,
}

impl Jellyfin {
    /// `None` when the integration has no address or no key.
    pub fn new(config: &MediaServerConfig, dialect: Dialect, out: Outbound) -> Option<Self> {
        let address = config.url.as_deref()?;
        Some(Jellyfin {
            id: config.id.clone(),
            dialect,
            api: api_root(address, dialect),
            web: web_root(config.web_url.as_deref().unwrap_or(address), dialect),
            key: config.secret.clone()?,
            out,
            owner: Mutex::new(None),
        })
    }

    async fn get(&self, what: &'static str, path: &str) -> Result<Value, EngineError> {
        let url = format!("{}{path}", self.api);
        let (name, value) = self.dialect.key_header(&self.key);
        let req = OutRequest::get(&url)
            .header(name, value)
            .header("Accept", "application/json");
        let res = self.out.send(req).await?;
        expect_ok(&url, &res)?;
        serde_json::from_slice(&res.body).map_err(|e| EngineError::Decode {
            engine: self.id.clone(),
            what,
            message: e.to_string(),
        })
    }

    /// A warning for each account number two users of the server share ([`shared_numbers`]).
    fn warn_shared(&self, users: &Value) {
        for number in shared_numbers(users) {
            tracing::warn!(server = %self.id, account = number, "two users of the server have one account number; their plays count as one person's");
        }
    }

    /// The part of a query that asks for items as the first administrator sees them: they then
    /// carry that person's play counts. The users are asked once. A warning names an account
    /// number two of them share. Without an answer the items are asked without it. The users
    /// are asked again next time.
    async fn as_owner(&self) -> String {
        let known = self
            .owner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let owner = match known {
            Some(id) => Some(id),
            None => match self.get("users", self.dialect.users()).await {
                Ok(users) => {
                    self.warn_shared(&users);
                    let found = owner(&users);
                    if let Some(id) = &found {
                        *self.owner.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(id.clone());
                    }
                    found
                }
                Err(e) => {
                    tracing::debug!(server = %self.id, error = %e, "the users could not be read");
                    None
                }
            },
        };
        owner
            .map(|id| format!("&UserId={}", encode_component(&id)))
            .unwrap_or_default()
    }

    /// Every item of these types below a library, page by page.
    async fn all(
        &self,
        what: &'static str,
        library: &str,
        types: &str,
        fields: &str,
    ) -> Result<Vec<Value>, EngineError> {
        let user = self.as_owner().await;
        let mut out: Vec<Value> = Vec::new();
        for page in 0..MAX_PAGES {
            let start = page * PAGE;
            let path = format!(
                "/Items?ParentId={}&IncludeItemTypes={types}&Recursive=true&Fields={fields}&StartIndex={start}&Limit={PAGE}&EnableTotalRecordCount=true{user}",
                encode_component(library)
            );
            let answer = match self.get(what, &path).await {
                Ok(answer) => answer,
                Err(e) => {
                    // The administrator may be gone: the users are asked again next time.
                    *self.owner.lock().unwrap_or_else(PoisonError::into_inner) = None;
                    return Err(e);
                }
            };
            let items = records(&answer);
            let total = int(&answer, "TotalRecordCount").unwrap_or(0);
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
impl MediaServer for Jellyfin {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        self.dialect.kind()
    }

    async fn identity(&self) -> Result<ServerIdentity, EngineError> {
        Ok(identity(
            &self.get("identity", "/System/Info").await?,
            self.dialect,
        ))
    }

    async fn sections(&self) -> Result<Vec<ServerSection>, EngineError> {
        Ok(sections(
            &self.get("libraries", self.dialect.libraries()).await?,
        ))
    }

    async fn items(&self, section: &ServerSection) -> Result<Vec<ServerItem>, EngineError> {
        let types = if section.kind == "show" {
            "Series"
        } else {
            "Movie"
        };
        Ok(self
            .all("items", &section.key, types, TITLE_FIELDS)
            .await?
            .iter()
            .filter_map(item)
            .collect())
    }

    async fn artists(&self, section: &str) -> Result<Vec<ServerArtist>, EngineError> {
        Ok(self
            .all("artists", section, "MusicArtist", ARTIST_FIELDS)
            .await?
            .iter()
            .filter_map(artist)
            .collect())
    }

    async fn albums(&self, section: &str) -> Result<Vec<ServerAlbum>, EngineError> {
        Ok(self
            .all("albums", section, "MusicAlbum", ALBUM_FIELDS)
            .await?
            .iter()
            .filter_map(album)
            .collect())
    }

    async fn tracks(&self, section: &str) -> Result<Vec<ServerTrack>, EngineError> {
        Ok(self
            .all("tracks", section, "Audio", TRACK_FIELDS)
            .await?
            .iter()
            .filter_map(track)
            .collect())
    }

    async fn sessions(&self) -> Result<Vec<ServerSession>, EngineError> {
        let path = match self.dialect {
            Dialect::Jellyfin => format!("/Sessions?ActiveWithinSeconds={ACTIVE_WITHIN}"),
            Dialect::Emby => "/Sessions".to_string(),
        };
        Ok(records(&self.get("sessions", &path).await?)
            .iter()
            .filter_map(session)
            .map(|s| ServerSession {
                server: self.id.clone(),
                ..s
            })
            .collect())
    }

    async fn accounts(&self) -> Result<Vec<ServerAccount>, EngineError> {
        let users = self.get("users", self.dialect.users()).await?;
        self.warn_shared(&users);
        Ok(accounts(&users))
    }

    async fn item_media(&self, rating_key: &str) -> Result<Option<ItemMedia>, EngineError> {
        if !is_id(rating_key) {
            return Ok(None);
        }
        Ok(item_media(
            &self
                .get(
                    "item",
                    &format!(
                        "/Items?Ids={}&Fields=MediaSources",
                        encode_component(rating_key)
                    ),
                )
                .await?,
        ))
    }

    async fn videos(&self, section: &str) -> Result<Vec<ServerVideo>, EngineError> {
        Ok(self
            .all("videos", section, "Video,Movie", VIDEO_FIELDS)
            .await?
            .iter()
            .filter_map(video)
            .collect())
    }

    async fn photo(&self, thumb: &str, size: u32) -> Result<ServerImage, EngineError> {
        if !is_image_path(thumb) {
            return Err(EngineError::Outbound(OutboundError::BadUrl(
                thumb.to_string(),
            )));
        }
        let size = size.clamp(64, 1200);
        let url = format!("{}{thumb}?{}", self.api, self.dialect.image_size(size));
        let (name, value) = self.dialect.key_header(&self.key);
        let req = OutRequest::get(&url)
            .header(name, value)
            .header("Accept", "image/jpeg,image/png,image/webp");
        let res = self.out.send(req).await?;
        expect_ok(&url, &res)?;
        let content_type = res
            .header("content-type")
            .filter(|v| v.starts_with("image/"))
            .unwrap_or("image/jpeg")
            .to_string();
        Ok(ServerImage {
            content_type,
            bytes: res.body,
        })
    }

    fn item_thumb(&self, rating_key: &str) -> Option<String> {
        is_id(rating_key).then(|| format!("/Items/{rating_key}/Images/Primary"))
    }

    fn item_link(&self, machine_id: Option<&str>, rating_key: &str) -> Option<ItemLink> {
        if !is_id(rating_key) {
            return None;
        }
        let server = machine_id
            .filter(|m| is_id(m))
            .map(|m| format!("&serverId={m}"))
            .unwrap_or_default();
        let url = match self.dialect {
            Dialect::Jellyfin => format!("{}/web/#/details?id={rating_key}{server}", self.web),
            Dialect::Emby => format!("{}/web/index.html#!/item?id={rating_key}{server}", self.web),
        };
        Some(ItemLink {
            key: rating_key.to_string(),
            url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::MediaServerKind;
    use crate::core::outbound::recorded::Recorded;
    use crate::core::outbound::{Allowlist, Method};
    use std::sync::Arc;

    const JELLYFIN: &str = "http://10.0.0.40:8096";
    const EMBY: &str = "http://10.0.0.41:8096";
    const KEY: &str = "0123456789abcdef0123456789abcdef";
    /// The demo's libraries, by their ids.
    const MOVIES: &str = "f137a2dd21bbc1b99aa5c0f6bf02a805";
    const MUSIC: &str = "7e64e319657a9516ec78490da03edccb";
    /// The demo's administrator and its account number.
    const ADMIN: &str = "554d873d210d499b877a54bdda50a3c1";
    const ADMIN_NUMBER: i64 = 6_004_263_143_739_604;

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
    }

    fn json(name: &str) -> Value {
        serde_json::from_str(&fixture(name)).unwrap()
    }

    /// An adapter over recorded answers, held to the calls its dialect declares.
    fn server(dialect: Dialect, base: &str, recorded: Arc<Recorded>) -> Jellyfin {
        let mut allow = Allowlist::default();
        allow
            .allow_integration(dialect.kind(), &api_root(base, dialect), calls(dialect))
            .unwrap();
        let config = MediaServerConfig {
            id: dialect.kind().to_string(),
            kind: match dialect {
                Dialect::Jellyfin => MediaServerKind::Jellyfin,
                Dialect::Emby => MediaServerKind::Emby,
            },
            url: Some(base.to_string()),
            secret: Some(KEY.to_string()),
            web_url: None,
        };
        Jellyfin::new(&config, dialect, Outbound::with_transport(allow, recorded)).unwrap()
    }

    #[tokio::test]
    async fn jellyfin_is_read_from_its_answers_with_get_alone() {
        let recorded = Recorded::new();
        let at = |path: &str| format!("{JELLYFIN}{path}");
        for (path, file) in [
            ("/System/Info", "jellyfin/system_info.json"),
            ("/Library/VirtualFolders", "jellyfin/virtual_folders.json"),
            ("/Users", "jellyfin/users.json"),
            ("/Sessions", "jellyfin/sessions.json"),
            (
                "/Items?ParentId=f137a2dd21bbc1b99aa5c0f6bf02a805&IncludeItemTypes=Movie&",
                "jellyfin/items_movies.json",
            ),
            (
                "/Items?ParentId=a656b907eb3a73532e40e44b968d0225&IncludeItemTypes=Series&",
                "jellyfin/items_shows.json",
            ),
            (
                "/Items?ParentId=7e64e319657a9516ec78490da03edccb&IncludeItemTypes=MusicArtist&",
                "jellyfin/items_artists.json",
            ),
            (
                "/Items?ParentId=7e64e319657a9516ec78490da03edccb&IncludeItemTypes=MusicAlbum&",
                "jellyfin/items_albums.json",
            ),
            (
                "/Items?ParentId=7e64e319657a9516ec78490da03edccb&IncludeItemTypes=Audio&",
                "jellyfin/items_tracks.json",
            ),
        ] {
            recorded.answer(Method::Get, &at(path), 200, &fixture(file));
        }
        let j = server(Dialect::Jellyfin, JELLYFIN, recorded.clone());
        assert_eq!((j.id(), j.kind()), ("jellyfin", "jellyfin"));
        assert!(!j.keeps_history());
        let id = j.identity().await.unwrap();
        assert_eq!(
            (id.name.as_str(), id.version.as_deref(), id.plex_pass),
            ("Stable Demo", Some("12.4.0"), false)
        );
        assert_eq!(
            id.machine_id.as_deref(),
            Some("f0b3381645f04afb9a0e392e74b6a1b0")
        );
        // The server writes no operating system.
        assert_eq!(id.platform, None);

        let s = j.sections().await.unwrap();
        let kinds: Vec<(&str, &str, Vec<String>)> = s
            .iter()
            .map(|s| (s.title.as_str(), s.kind.as_str(), s.locations.clone()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("Movies", "movie", vec!["/media/Movies".to_string()]),
                ("Shows", "show", vec!["/media/Shows".to_string()]),
                ("Music", "music", vec!["/media/Music".to_string()])
            ]
        );
        assert_eq!(s[0].key, MOVIES);

        let movies = j.items(&s[0]).await.unwrap();
        assert_eq!(movies.len(), 3);
        let bubble = &movies[0];
        assert_eq!(
            (
                bubble.title.as_str(),
                bubble.year,
                bubble.tmdb,
                bubble.imdb.as_deref()
            ),
            (
                "The Boy in the Plastic Bubble",
                Some(1976),
                Some(9878),
                Some("tt0074236")
            )
        );
        assert_eq!(bubble.rating_key, "edb39341c5039551a5157e51fe4a3364");
        assert_eq!((bubble.kind.as_str(), bubble.plays), ("movie", 1));
        assert!(bubble.last_played.is_some() && bubble.added.is_some());
        assert_eq!(
            bubble.thumb.as_deref(),
            Some("/Items/edb39341c5039551a5157e51fe4a3364/Images/Primary")
        );
        let shows = j.items(&s[1]).await.unwrap();
        let pioneer = shows.iter().find(|i| i.title == "Pioneer One").unwrap();
        assert_eq!(
            (
                pioneer.kind.as_str(),
                pioneer.tvdb,
                pioneer.tmdb,
                pioneer.episodes,
                pioneer.watched_episodes
            ),
            ("show", Some(170_551), Some(33_050), Some(6), Some(6))
        );
        // The items are asked as the first administrator sees them.
        assert!(
            recorded
                .urls_seen()
                .iter()
                .filter(|u| u.contains("/Items?"))
                .all(|u| u.contains(&format!("UserId={ADMIN}"))),
            "{:?}",
            recorded.urls_seen()
        );

        let artists = j.artists(MUSIC).await.unwrap();
        assert_eq!(artists[0].name, "Binaerpilot");
        assert_eq!(artists[0].genres, vec!["Electronic"]);
        let albums = j.albums(MUSIC).await.unwrap();
        let nemesis = albums.iter().find(|a| a.title == "Nemesis").unwrap();
        assert_eq!(
            (
                nemesis.artist.as_deref(),
                nemesis.artist_key.as_deref(),
                nemesis.year,
                nemesis.released.as_deref()
            ),
            (
                Some("Leap Fidei"),
                Some("78e6763b3d9b58c349a58ce5eb7877c1"),
                Some(2024),
                Some("2024-01-01")
            )
        );
        assert!(nemesis.thumb.is_some());
        let tracks = j.tracks(MUSIC).await.unwrap();
        let first = &tracks[0];
        assert_eq!(
            (
                first.album_key.as_str(),
                first.codec.as_deref(),
                first.bitrate,
                first.channels,
                first.number
            ),
            (
                "f7e6d83954e06a0d46724a6068fa21ff",
                Some("flac"),
                Some(764),
                Some(2),
                Some(10)
            )
        );
        assert!(first.file.as_deref().unwrap().ends_with("Jellyfin.flac"));

        let accounts = j.accounts().await.unwrap();
        assert_eq!(accounts.len(), 2);
        assert!(
            accounts
                .iter()
                .any(|a| a.id == ADMIN_NUMBER && a.name.as_deref() == Some("admin"))
        );

        // What plays now: two players; the idle sessions are left out. Each names its server.
        let playing = j.sessions().await.unwrap();
        assert_eq!(playing.len(), 2);
        assert!(playing.iter().all(|s| s.server == "jellyfin"));
        let episode = &playing[0];
        assert_eq!(
            (
                episode.kind.as_str(),
                episode.title.as_str(),
                episode.show.as_deref(),
                episode.season,
                episode.episode
            ),
            (
                "episode",
                "The Man From Mars",
                Some("Pioneer One"),
                Some(1),
                Some(2)
            )
        );
        assert_eq!(
            episode.show_key.as_deref(),
            Some("9bd5926326d651153928e0d11c6ee7cb")
        );
        assert_eq!(
            episode.session_id.as_deref(),
            Some("f1a90ee9283b19f079fd05309fe89d0e-b80159226d16576d4c0613750e27c072")
        );
        assert_eq!(
            (
                episode.person.as_deref(),
                episode.player.as_deref(),
                episode.product.as_deref()
            ),
            (Some("demo"), Some("Apple TV"), Some("Tide"))
        );
        assert_eq!(
            (episode.decision.as_str(), episode.state.as_deref()),
            ("direct play", Some("playing"))
        );
        assert_eq!(
            (episode.local, episode.address.as_deref()),
            (Some(false), Some("2001:db8::17"))
        );
        assert_eq!(
            episode.account_id,
            account_number("4ed1b8b42a7c4ea682f0fe5d08d4e278")
        );
        assert!(episode.progress.is_some_and(|p| p > 0.0 && p < 100.0));
        let movie = &playing[1];
        assert_eq!(
            (
                movie.kind.as_str(),
                movie.title.as_str(),
                movie.year,
                movie.delivery.source_resolution.as_deref(),
                movie.delivery.stream_resolution.as_deref()
            ),
            ("movie", "Dracula", Some(1931), Some("1080p"), Some("1080p"))
        );
        assert_eq!(movie.delivery.video.as_deref(), Some("direct play"));

        // GET alone, the key in its header and never in an address.
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
        assert!(recorded.urls_seen().iter().all(|u| !u.contains(KEY)));
        assert!(recorded.calls_sent().iter().all(|c| {
            c.headers
                .iter()
                .any(|(n, v)| n == "Authorization" && v == &format!("MediaBrowser Token=\"{KEY}\""))
        }));
    }

    #[tokio::test]
    async fn a_transcode_says_what_the_file_is_and_what_is_sent() {
        let recorded = Recorded::new();
        recorded.answer(
            Method::Get,
            &format!("{JELLYFIN}/Sessions"),
            200,
            &fixture("jellyfin/sessions_transcode.json"),
        );
        let playing = server(Dialect::Jellyfin, JELLYFIN, recorded)
            .sessions()
            .await
            .unwrap();
        let s = &playing[0];
        assert_eq!(s.title, "Caminandes: Llama Drama");
        assert_eq!(
            (
                s.decision.as_str(),
                s.state.as_deref(),
                s.person.as_deref(),
                s.account_id
            ),
            (
                "transcode",
                Some("paused"),
                Some("admin"),
                Some(ADMIN_NUMBER)
            )
        );
        // The file is what the item's video stream says: no source rides along with it.
        assert_eq!(
            s.delivery,
            Delivery {
                video: Some("transcode".into()),
                audio: Some("direct stream".into()),
                source_resolution: Some("1080p".into()),
                source_bitrate: Some(2925),
                stream_resolution: Some("720p".into()),
                stream_bitrate: Some(2616),
                source_video_codec: Some("h264".into()),
                source_audio_codec: Some("aac".into()),
                source_audio_channels: Some(2),
                source_container: Some("mp4".into()),
                stream_video_codec: Some("h264".into()),
                stream_audio_codec: Some("aac".into()),
                stream_audio_channels: Some(2),
                stream_container: Some("ts".into()),
                transcode_reasons: vec!["VideoBitrateNotSupported".into()],
            }
        );
        // The app with its version and the device's id; the servers say nothing of a secure or
        // relayed connection.
        assert_eq!(
            (
                s.product.as_deref(),
                s.product_version.as_deref(),
                s.device_id.as_deref(),
                s.secure
            ),
            (
                Some("Jellyfin Android TV"),
                Some("0.18.11"),
                Some("fixtures-device-4"),
                None
            )
        );
        assert_eq!(
            container_word("mov,mp4,m4a,3gp,3g2,mj2").as_deref(),
            Some("mp4")
        );
        assert_eq!(container_word("MKV").as_deref(), Some("mkv"));
        // The audio track played: the stream at the session's index, not the first. Without an
        // index, the file's default.
        let item = serde_json::json!({"MediaStreams": [
            {"Type": "Video", "Index": 0, "Codec": "hevc"},
            {"Type": "Audio", "Index": 1, "Codec": "truehd", "Channels": 8},
            {"Type": "Audio", "Index": 2, "Codec": "ac3", "Channels": 6, "IsDefault": true},
            {"Type": "Audio", "Index": 3, "Codec": "aac", "Channels": 2}
        ]});
        assert_eq!(
            (file_of(&item, Some(3)).1, file_of(&item, Some(3)).2),
            (Some("aac".to_string()), Some(2))
        );
        assert_eq!(file_of(&item, None).1.as_deref(), Some("ac3"));
        assert_eq!(file_of(&item, Some(9)).1.as_deref(), Some("ac3"));
        assert_eq!(
            reasons(Some(
                &serde_json::json!({"TranscodeReasons": "AudioCodecNotSupported, ContainerNotSupported"})
            )),
            vec!["AudioCodecNotSupported", "ContainerNotSupported"]
        );
        assert_eq!((s.local, s.bandwidth), (Some(true), Some(2616)));
        assert_eq!(
            (s.offset_ms, s.duration_ms.is_some()),
            (Some(135_000), true)
        );
        // How each stream reaches the player, as the server's own dashboard reads it.
        let t = |video: Option<bool>| serde_json::json!({"IsVideoDirect": video});
        assert_eq!(
            stream_word(Some("DirectPlay"), Some(&t(Some(false))), "IsVideoDirect"),
            DIRECT_PLAY
        );
        assert_eq!(
            stream_word(Some("DirectStream"), Some(&t(Some(true))), "IsVideoDirect"),
            DIRECT_STREAM
        );
        assert_eq!(
            stream_word(Some("DirectStream"), None, "IsVideoDirect"),
            DIRECT_PLAY
        );
        assert_eq!(
            stream_word(Some("Transcode"), None, "IsVideoDirect"),
            TRANSCODE
        );
        assert_eq!(
            stream_word(Some("Transcode"), Some(&t(None)), "IsVideoDirect"),
            TRANSCODE
        );
        assert_eq!(stream_word(None, None, "IsVideoDirect"), DIRECT_PLAY);
        assert_eq!(overall(&[Some(DIRECT_STREAM), Some(TRANSCODE)]), TRANSCODE);
        assert_eq!(overall(&[None, Some(DIRECT_STREAM)]), DIRECT_STREAM);
        assert_eq!(overall(&[Some(DIRECT_PLAY), None]), DIRECT_PLAY);
    }

    #[tokio::test]
    async fn emby_differs_in_its_header_its_root_and_its_routes() {
        let recorded = Recorded::new();
        let at = |path: &str| format!("{EMBY}/emby{path}");
        for (path, file) in [
            ("/System/Info", "emby/system_info.json"),
            ("/Library/VirtualFolders/Query", "emby/virtual_folders.json"),
            ("/Users/Query", "emby/users.json"),
            ("/Sessions", "emby/sessions.json"),
            (
                "/Items?ParentId=3&IncludeItemTypes=Movie&",
                "emby/items_movies.json",
            ),
        ] {
            recorded.answer(Method::Get, &at(path), 200, &fixture(file));
        }
        // An address that names the API's root already is taken as it is.
        assert_eq!(api_root(&format!("{EMBY}/emby/"), Dialect::Emby), at(""));
        assert_eq!(api_root(EMBY, Dialect::Jellyfin), EMBY);
        let e = server(Dialect::Emby, EMBY, recorded.clone());
        assert_eq!(e.kind(), "emby");
        let id = e.identity().await.unwrap();
        assert_eq!(
            (
                id.name.as_str(),
                id.version.as_deref(),
                id.platform.as_deref()
            ),
            ("Den", Some("4.9.1.90"), Some("Linux"))
        );
        let s = e.sections().await.unwrap();
        let kinds: Vec<(&str, &str, &str)> = s
            .iter()
            .map(|s| (s.key.as_str(), s.kind.as_str(), s.title.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("3", "movie", "Movies"),
                ("4", "show", "TV Shows"),
                ("5", "homevideo", "Home Videos")
            ]
        );
        let movies = e.items(&s[0]).await.unwrap();
        assert_eq!(movies.len(), 2);
        let bunny = &movies[0];
        assert_eq!(
            (
                bunny.rating_key.as_str(),
                bunny.tmdb,
                bunny.imdb.as_deref(),
                bunny.plays
            ),
            ("4321", Some(10_378), Some("tt1254207"), 2)
        );
        // A critic rating is the Rotten Tomatoes score, fresh at 60 and above.
        assert_eq!(
            (bunny.critic_rating, bunny.critic_image.as_deref()),
            (Some(7.7), Some(RT_FRESH))
        );
        assert_eq!(movies[1].critic_image.as_deref(), Some(RT_ROTTEN));
        let accounts = e.accounts().await.unwrap();
        assert_eq!(
            accounts.iter().map(|a| a.id).collect::<Vec<_>>(),
            vec![8_509_173_170_540_481, 7_348_226_215_534_432]
        );
        let playing = e.sessions().await.unwrap();
        assert_eq!(playing.len(), 2, "the idle session is left out");
        assert!(playing.iter().all(|s| s.server == "emby"));
        assert_eq!(
            (
                playing[0].decision.as_str(),
                playing[0].delivery.stream_resolution.as_deref(),
                playing[0].delivery.source_resolution.as_deref(),
                playing[0].local
            ),
            ("transcode", Some("720p"), Some("1080p"), Some(false))
        );
        assert_eq!(
            (
                playing[1].kind.as_str(),
                playing[1].show.as_deref(),
                playing[1].decision.as_str(),
                playing[1].state.as_deref(),
                playing[1].platform.as_deref(),
                playing[1].local
            ),
            (
                "episode",
                Some("Pioneer One"),
                "direct play",
                Some("paused"),
                Some("Android"),
                Some(true)
            )
        );
        // The key travels in `X-Emby-Token`, never in an address.
        assert!(recorded.methods_seen().iter().all(|m| *m == Method::Get));
        assert!(recorded.urls_seen().iter().all(|u| !u.contains(KEY)));
        assert!(recorded.calls_sent().iter().all(|c| {
            c.headers
                .iter()
                .any(|(n, v)| n == "X-Emby-Token" && v == KEY)
        }));
        assert!(
            recorded
                .urls_seen()
                .iter()
                .all(|u| u.starts_with(&format!("{EMBY}/emby/")))
        );
        // An item opens in each server's own web app.
        assert_eq!(
            e.item_link(Some("7b6a8c9d0e1f42a3b4c5d6e7f8091a2b"), "4321")
                .unwrap()
                .url,
            format!(
                "{EMBY}/web/index.html#!/item?id=4321&serverId=7b6a8c9d0e1f42a3b4c5d6e7f8091a2b"
            )
        );
        let j = server(Dialect::Jellyfin, JELLYFIN, Recorded::new());
        let link = j
            .item_link(
                Some("f0b3381645f04afb9a0e392e74b6a1b0"),
                "c541e7545e58b28cd4d0da7aac89d7e8",
            )
            .unwrap();
        assert_eq!(link.key, "c541e7545e58b28cd4d0da7aac89d7e8");
        assert_eq!(
            link.url,
            format!(
                "{JELLYFIN}/web/#/details?id=c541e7545e58b28cd4d0da7aac89d7e8&serverId=f0b3381645f04afb9a0e392e74b6a1b0"
            )
        );
        assert!(j.item_link(None, "x/../y").is_none());
        // With a web address of its own, an item opens there: the address people reach the
        // server at. It need not be the one Singularr reads it at.
        for (dialect, web, link) in [
            (
                Dialect::Jellyfin,
                "https://watch.example.org/web/",
                "https://watch.example.org/web/#/details?id=4321",
            ),
            (
                Dialect::Emby,
                "https://emby.example.org",
                "https://emby.example.org/web/index.html#!/item?id=4321",
            ),
        ] {
            let config = MediaServerConfig {
                id: dialect.kind().to_string(),
                kind: match dialect {
                    Dialect::Jellyfin => MediaServerKind::Jellyfin,
                    Dialect::Emby => MediaServerKind::Emby,
                },
                url: Some("http://10.0.0.31:8096".to_string()),
                secret: Some(KEY.to_string()),
                web_url: Some(web.to_string()),
            };
            let out = Outbound::with_transport(Allowlist::default(), Recorded::new());
            let public = Jellyfin::new(&config, dialect, out).unwrap();
            assert_eq!(public.item_link(None, "4321").unwrap().url, link);
        }
    }

    #[tokio::test]
    async fn the_servers_are_sent_their_declared_reads_and_nothing_else() {
        for (dialect, base) in [(Dialect::Jellyfin, JELLYFIN), (Dialect::Emby, EMBY)] {
            let root = api_root(base, dialect);
            let mut allow = Allowlist::default();
            allow
                .allow_integration(dialect.kind(), &root, calls(dialect))
                .unwrap();
            for path in [
                "/System/Info",
                dialect.libraries(),
                dialect.users(),
                "/Items?ParentId=abc&IncludeItemTypes=Movie&Recursive=true",
                "/Items/c541e7545e58b28cd4d0da7aac89d7e8/Images/Primary?maxWidth=300",
                "/Sessions",
            ] {
                assert!(
                    allow.check(Method::Get, &format!("{root}{path}")).is_ok(),
                    "{} {path}",
                    dialect.kind()
                );
            }
            // A stream starts a transcode, a download sends the file, a refresh scans the
            // library: none of them is sent.
            for path in [
                "/Videos/c541e7545e58b28cd4d0da7aac89d7e8/stream?static=true",
                "/Videos/c541e7545e58b28cd4d0da7aac89d7e8/master.m3u8",
                "/Audio/90b750a2cec4eb338c0dab3b40b8bb13/universal",
                "/Items/c541e7545e58b28cd4d0da7aac89d7e8/Download",
                "/Items/c541e7545e58b28cd4d0da7aac89d7e8/Refresh",
                "/Library/Refresh",
                "/System/Logs/Log?name=log.txt",
                "/Sessions/Logout",
            ] {
                assert!(
                    allow.check(Method::Get, &format!("{root}{path}")).is_err(),
                    "{} {path} must be refused",
                    dialect.kind()
                );
            }
            for method in [Method::Post, Method::Put, Method::Delete] {
                for path in ["/Items", "/Sessions", "/Library/VirtualFolders"] {
                    assert!(
                        allow.check(method, &format!("{root}{path}")).is_err(),
                        "{} {} {path}",
                        dialect.kind(),
                        method.as_str()
                    );
                }
            }
        }
        // An image is asked at its declared path. Nothing else is taken for one.
        let recorded = Recorded::new();
        recorded.answer_with(
            Method::Get,
            &format!("{JELLYFIN}/Items/edb39341c5039551a5157e51fe4a3364/Images/Primary"),
            200,
            "jpeg",
            &[("content-type", "image/jpeg")],
        );
        let j = server(Dialect::Jellyfin, JELLYFIN, recorded.clone());
        let thumb = j.item_thumb("edb39341c5039551a5157e51fe4a3364").unwrap();
        let image = j.photo(&thumb, 300).await.unwrap();
        assert_eq!(
            (image.content_type.as_str(), image.bytes.len()),
            ("image/jpeg", 4)
        );
        assert!(recorded.urls_seen()[0].contains("maxWidth=300&maxHeight=300"));
        for bad in [
            "/Videos/x/stream",
            "/Items/x/Images/Primary/../../Download",
            "/Items/a b/Images/Primary",
        ] {
            assert!(j.photo(bad, 300).await.is_err(), "{bad}");
        }
        assert_eq!(
            recorded.urls_seen().len(),
            1,
            "a refused image is never asked"
        );
        assert!(j.item_thumb("x/../y").is_none());
    }

    #[test]
    fn an_account_is_a_number_exact_in_a_browser() {
        assert_eq!(account_number(ADMIN), Some(ADMIN_NUMBER));
        assert_eq!(
            account_number("554d873d-210d-499b-877a-54bdda50a3c1"),
            Some(ADMIN_NUMBER),
            "the same id with dashes"
        );
        let highest = account_number("ffffffffffffffffffffffffffffffff").unwrap();
        assert_eq!(highest, (1i64 << 53) - 1);
        let lowest = account_number("0000000000000000").unwrap();
        assert_eq!(lowest, 1i64 << 52);
        assert!(account_number("5").is_none() && account_number("not-hex-at-all-zz").is_none());
        let users = json("jellyfin/users.json");
        assert_eq!(owner(&users).as_deref(), Some(ADMIN));
        assert_eq!(
            owner(&json("emby/users.json")).as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb924")
        );
        // The fixtures' users have a number each. Two ids that differ past their 13th digit
        // share one. That number is named. The same id written with dashes is one user.
        assert!(shared_numbers(&users).is_empty());
        assert!(shared_numbers(&json("emby/users.json")).is_empty());
        let collide = serde_json::json!([
            {"Id": "554d873d210d499b877a54bdda50a3c1", "Name": "one"},
            {"Id": "554d873d210d4fffffffffffffffffff", "Name": "two"},
            {"Id": "554d873d-210d-499b-877a-54bdda50a3c1", "Name": "one again"},
            {"Id": "4ed1b8b42a7c4ea682f0fe5d08d4e278", "Name": "other"}
        ]);
        assert_eq!(shared_numbers(&collide), vec![ADMIN_NUMBER]);
        assert_eq!(
            shared_numbers(&serde_json::json!({"Items": collide, "TotalRecordCount": 4})),
            vec![ADMIN_NUMBER]
        );
    }

    #[test]
    fn a_home_address_is_private_loopback_or_link_local() {
        for home in [
            "10.0.0.20",
            "172.16.4.2",
            "127.0.0.1",
            "169.254.1.1",
            "10.0.0.20:52144",
            "::1",
            "[::1]:8096",
            "fd12:3456::1",
            "fe80::1",
            "::ffff:10.0.0.9",
        ] {
            assert_eq!(is_home_address(home), Some(true), "{home}");
        }
        for outside in [
            "203.0.113.24",
            "2001:db8::17",
            "8.8.8.8",
            "[2001:db8::1]:443",
            "::ffff:198.51.100.4",
        ] {
            assert_eq!(is_home_address(outside), Some(false), "{outside}");
        }
        assert_eq!(is_home_address("a player"), None);
    }

    #[test]
    fn libraries_and_videos_read_in_the_layer_s_words() {
        assert_eq!(section_word(Some("movies")), "movie");
        assert_eq!(section_word(Some("tvshows")), "show");
        assert_eq!(section_word(Some("homevideos")), "homevideo");
        assert_eq!(section_word(None), "mixed");
        let v = video(&serde_json::json!({
            "Id": "0a1b2c3d4e5f60718293a4b5c6d7e8f9", "Name": "1986 - montana", "Type": "Video",
            "ProductionYear": 1986, "RunTimeTicks": 37_200_000_000i64, "Path": "/home/1986 - montana.mp4",
            "DateCreated": "2023-09-25T20:42:17.0000000Z", "ImageTags": {"Primary": "abc"},
            "MediaSources": [{"Size": 1_197_602_196i64, "MediaStreams": [
                {"Type": "Video", "Codec": "h264", "Height": 480}, {"Type": "Audio", "Codec": "aac"}]}]
        }))
        .unwrap();
        assert_eq!(
            (v.duration_ms, v.codec.as_deref(), v.height, v.size),
            (
                Some(3_720_000),
                Some("h264"),
                Some(480),
                Some(1_197_602_196)
            )
        );
        assert_eq!(v.file.as_deref(), Some("/home/1986 - montana.mp4"));
        assert!(v.added.is_some() && v.thumb.is_some());
        // The servers write no time as the year 1.
        assert!(
            date(
                &serde_json::json!({"At": "0001-01-01T00:00:00.0000000Z"}),
                "At"
            )
            .is_none()
        );
        assert!(
            date(
                &serde_json::json!({"At": "2026-11-25T03:48:28.3338032Z"}),
                "At"
            )
            .is_some()
        );
        assert!(
            date(
                &serde_json::json!({"At": "2026-11-25T03:48:28.3338032"}),
                "At"
            )
            .is_some()
        );
        let media = item_media(&json("jellyfin/items_movies.json")).unwrap();
        assert_eq!(
            (media.resolution.as_deref(), media.bitrate),
            (Some("SD"), Some(831))
        );
    }
}
