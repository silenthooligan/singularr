//! The `/api/v1` routes of the Plex server: what is playing; the music read model from the
//! server in the shape of the web client's `music.json`, an album with its tracks
//! (`albums/*.json`) and an artist with their albums; a plain video library's recordings
//! (`homevideo.json`), with the owner's notes on the tapes; and album covers, artist photos and
//! video stills resized by the server and served through Singularr, so the browser never needs
//! the server's address or token. Every route needs a caller with `read`.

use super::routes::{ApiPath, ApiQuery};
use super::state::AppState;
use super::stats::{Format, PlayDelivery};
use crate::catalog::store as catalog_store;
use crate::core::error::AppError;
use crate::core::schema::ProblemDoc;
use crate::homemovies::model::{Tape, TapeQuery};
use crate::homemovies::service as homemovies;
use crate::lifecycle::server::plain_video;
use crate::music::model::{AlbumDetail, ArtistDetail};
use crate::music::service as music_service;
use crate::music::store as music_store;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use utoipa::{IntoParams, OpenApi, ToSchema};

type ApiResult<T> = Result<Json<T>, AppError>;

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ImageQuery {
    /// The longer side in pixels, 64 to 1200. Default 600.
    pub size: Option<u32>,
}

/// One thing playing on the server.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Session {
    /// `movie`, `episode`, `track`.
    pub kind: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub episode: Option<i32>,
    /// Who is watching, by their Plex name.
    pub person: Option<String>,
    pub player: Option<String>,
    /// `playing`, `paused`, `buffering`.
    pub state: Option<String>,
    /// 0 to 100.
    pub progress: Option<f64>,
    /// `direct play`, `direct stream` or `transcode`.
    pub decision: String,
    /// How the video and the audio reach the player, with the file's and the stream's
    /// resolution and bitrate.
    pub delivery: PlayDelivery,
    /// What the stream takes, kbit/s.
    pub bandwidth: Option<i64>,
    /// `home` or `outside`.
    pub network: Option<String>,
    pub platform: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Sessions {
    /// False when no Plex integration is configured; the list is then empty.
    pub configured: bool,
    pub sessions: Vec<Session>,
}

/// One album of the server (`music.json`, `albums[]`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MusicAlbum {
    /// `album-<rating key>`.
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    /// `artist-<rating key>`.
    pub artist_id: Option<String>,
    pub year: Option<i32>,
    /// `YYYY-MM-DD`.
    pub released: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    /// Tracks.
    pub tracks: i64,
    /// Minutes.
    pub duration: i64,
    /// `MP3`, `FLAC`, or `Mixed` when the tracks are of several formats.
    pub format: String,
    /// `FLAC`, `MP3 320`, `MP3 VBR`, `Mixed`.
    pub quality: String,
    pub size: i64,
    pub plays: i64,
    pub added: Option<DateTime<Utc>>,
    /// An address the browser can load (made by the server, served by Singularr); null when
    /// the album has no cover.
    pub cover: Option<String>,
    /// Keys of `problemLabels`: `no-cover`, `low-bitrate`, `mixed-formats`, `track-gaps`,
    /// `untagged`, `duplicate`.
    pub problems: Vec<String>,
}

/// One artist of the server (`music.json`, `artists[]`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct MusicArtist {
    /// `artist-<rating key>`.
    pub id: String,
    pub name: String,
    pub genre: Option<String>,
    pub country: Option<String>,
    pub summary: Option<String>,
    pub albums: i64,
    pub tracks: i64,
    pub size: i64,
    pub added: Option<DateTime<Utc>>,
    /// An address the browser can load; null when the server has no photo.
    pub photo: Option<String>,
    /// The same image as `photo`, for the places that show a cover.
    pub cover: Option<String>,
}

/// The music read model from the Plex server (`GET /api/v1/music`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Music {
    pub albums: Vec<MusicAlbum>,
    pub artists: Vec<MusicArtist>,
    /// What each problem key means, in words.
    pub problem_labels: BTreeMap<String, String>,
    /// Audio files of the music folder that the server's music does not name, as
    /// `album folder/file`. At most 500 are listed.
    pub files_not_on_server: Vec<String>,
    pub files_not_on_server_total: usize,
}

/// One track of an album.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct AlbumTrack {
    /// The track number; null when the file has none.
    pub n: Option<i32>,
    pub disc: i32,
    pub title: String,
    /// The track's artist, when it differs from the album's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Minutes, to two decimals.
    pub duration: f64,
    pub format: String,
    /// kbit/s.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<i64>,
    pub size: i64,
}

/// An album with its tracks (`GET /api/v1/music/albums/album-<key>`, `albums/*.json`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AlbumRecord {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub artist_id: Option<String>,
    pub year: Option<i32>,
    pub released: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub duration: i64,
    pub format: String,
    pub quality: String,
    pub size: i64,
    pub plays: i64,
    pub added: Option<DateTime<Utc>>,
    pub cover: Option<String>,
    pub problems: Vec<String>,
    /// The album's folder as the server names it.
    pub path: Option<String>,
    pub track_count: i64,
    pub tracks: Vec<AlbumTrack>,
}

/// An artist with their albums (`GET /api/v1/music/artists/artist-<key>`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ArtistRecord {
    #[serde(flatten)]
    pub artist: MusicArtist,
    /// Their albums, as `music.json` lists them.
    pub discography: Vec<MusicAlbum>,
}

const LOSSLESS: [&str; 7] = ["flac", "alac", "wav", "aiff", "pcm", "ape", "wv"];
/// An average below this, in kbit/s, is a low bitrate for lossy audio.
const LOW_BITRATE: i64 = 192;
/// The constant bitrates of MP3; an album whose tracks all sit at one is that bitrate, any
/// other is variable.
const CONSTANT_RATES: [i64; 14] = [
    32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
];

/// The problems an album can have, with their words.
pub const PROBLEMS: [(&str, &str); 6] = [
    ("duplicate", "Duplicate album"),
    ("low-bitrate", "Low bitrate"),
    ("mixed-formats", "Mixed formats"),
    ("no-cover", "No cover"),
    ("track-gaps", "Gaps in track numbers"),
    ("untagged", "Missing album tags"),
];

fn is_lossless(codec: &str) -> bool {
    LOSSLESS.contains(&codec.to_ascii_lowercase().as_str())
}

/// One codec's name in capitals; several are `Mixed`.
fn format_of(codecs: &[String]) -> String {
    match codecs {
        [] => String::new(),
        [one] => one.to_ascii_uppercase(),
        _ => "Mixed".to_string(),
    }
}

/// `FLAC` for lossless, `MP3 320` when every track sits at one constant rate, `MP3 VBR` when
/// they vary, `Mixed` for several formats.
fn quality_of(a: &music_store::ServerAlbumRow) -> String {
    let format = format_of(&a.codecs);
    if a.codecs.len() != 1 || is_lossless(&a.codecs[0]) {
        return format;
    }
    let constant = |rate: i64| {
        CONSTANT_RATES
            .iter()
            .find(|c| (rate - **c).abs() <= 3)
            .copied()
    };
    match (a.min_bitrate, a.max_bitrate) {
        (Some(lo), Some(hi)) => match (constant(lo), constant(hi)) {
            (Some(x), Some(y)) if x == y => format!("{format} {x}"),
            _ => format!("{format} VBR"),
        },
        _ => format,
    }
}

/// A disc's track numbers have a hole, or a track has no number while others do.
fn track_gaps(discs: &[Option<i32>], numbers: &[Option<i32>]) -> bool {
    if numbers.is_empty() {
        return false;
    }
    let mut by_disc: BTreeMap<i32, Vec<i32>> = BTreeMap::new();
    for (i, n) in numbers.iter().enumerate() {
        match n {
            Some(n) => by_disc
                .entry(discs.get(i).copied().flatten().unwrap_or(1))
                .or_default()
                .push(*n),
            None => return true,
        }
    }
    by_disc.values_mut().any(|list| {
        list.sort_unstable();
        list.dedup();
        list.iter().enumerate().any(|(i, n)| *n != i as i32 + 1)
    })
}

/// The server's word for an album without tags (`[Unknown Album]`), or no track with a
/// number.
fn untagged(a: &music_store::ServerAlbumRow) -> bool {
    let t = a.title.trim();
    (t.starts_with('[') && t.ends_with(']'))
        || (!a.numbers.is_empty() && a.numbers.iter().all(Option::is_none))
}

/// What makes two albums the same: the artist and the title, ignoring case and runs of spaces.
fn same_album(a: &music_store::ServerAlbumRow) -> String {
    let norm = |s: &str| {
        s.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    format!(
        "{}\u{1f}{}",
        norm(a.artist.as_deref().unwrap_or("")),
        norm(&a.title)
    )
}

/// The keys of the albums that share their artist and title with another album.
pub fn duplicates(albums: &[music_store::ServerAlbumRow]) -> HashSet<String> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    for a in albums {
        *seen.entry(same_album(a)).or_default() += 1;
    }
    albums
        .iter()
        .filter(|a| seen.get(&same_album(a)).is_some_and(|n| *n > 1))
        .map(|a| a.rating_key.clone())
        .collect()
}

fn cover_url(a: &music_store::ServerAlbumRow) -> Option<String> {
    a.thumb
        .as_ref()
        .map(|_| format!("/api/v1/music/albums/album-{}/cover", a.rating_key))
}

pub fn music_album(a: &music_store::ServerAlbumRow, duplicate: &HashSet<String>) -> MusicAlbum {
    let lossless = !a.codecs.is_empty() && a.codecs.iter().all(|c| is_lossless(c));
    let mut problems = Vec::new();
    if duplicate.contains(&a.rating_key) {
        problems.push("duplicate");
    }
    if !lossless && a.bitrate.is_some_and(|b| b > 0 && b < LOW_BITRATE) {
        problems.push("low-bitrate");
    }
    if a.codecs.len() > 1 {
        problems.push("mixed-formats");
    }
    if a.thumb.is_none() {
        problems.push("no-cover");
    }
    if track_gaps(&a.discs, &a.numbers) {
        problems.push("track-gaps");
    }
    if untagged(a) {
        problems.push("untagged");
    }
    MusicAlbum {
        id: format!("album-{}", a.rating_key),
        title: a.title.clone(),
        artist: a.artist.clone(),
        artist_id: a.artist_key.as_ref().map(|k| format!("artist-{k}")),
        year: a.year.filter(|y| *y > 0),
        released: a.released.clone(),
        genre: a.genres.first().cloned(),
        label: a.label.clone(),
        tracks: a.tracks,
        duration: (a.duration_ms + 30_000) / 60_000,
        format: format_of(&a.codecs),
        quality: quality_of(a),
        size: a.size_bytes,
        plays: a.plays,
        added: a.added_at,
        cover: cover_url(a),
        problems: problems.into_iter().map(str::to_string).collect(),
    }
}

fn music_artist(a: &music_store::ServerArtistRow) -> MusicArtist {
    let photo = a
        .thumb
        .as_ref()
        .map(|_| format!("/api/v1/music/artists/artist-{}/photo", a.rating_key));
    MusicArtist {
        id: format!("artist-{}", a.rating_key),
        name: a.name.clone(),
        genre: a.genres.first().cloned(),
        country: a.countries.first().cloned(),
        summary: a.summary.clone(),
        albums: a.albums,
        tracks: a.tracks,
        size: a.size_bytes,
        added: a.added_at,
        cover: photo.clone(),
        photo,
    }
}

/// The folder of a file as the server names it.
fn folder_of(file: &str) -> Option<String> {
    file.rsplit_once(['/', '\\'])
        .map(|(dir, _)| dir.to_string())
}

/// The last two parts of a path, lower case: `album/file.mp3`.
fn tail(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    parts[parts.len().saturating_sub(2)..]
        .join("/")
        .to_lowercase()
}

/// The files of the folder that the server does not name, compared by album folder and file
/// name (the folder and the server see the share at different mount points).
pub fn files_not_on_server(folder: &[String], server: &[String]) -> Vec<String> {
    let known: HashSet<String> = server.iter().map(|f| tail(f)).collect();
    folder
        .iter()
        .filter(|f| !known.contains(&tail(f)))
        .cloned()
        .collect()
}

/// An image of the server, by the album or artist whose image it is.
pub async fn server_image(
    state: &AppState,
    kind: &str,
    key: &str,
    size: Option<u32>,
) -> Result<Response, AppError> {
    let thumb = music_store::server_thumb(&state.pool, kind, key)
        .await?
        .ok_or_else(|| AppError::not_found(format!("No image for {kind} {key}.")))?;
    photo(state, &thumb, size).await
}

/// An image of the server by its path there, resized by the server.
async fn photo(state: &AppState, thumb: &str, size: Option<u32>) -> Result<Response, AppError> {
    let server =
        state.engines.media_server.as_ref().ok_or_else(|| {
            AppError::not_found("No media server configured (PLEX_URL, PLEX_TOKEN).")
        })?;
    let image = server
        .photo(thumb, size.unwrap_or(600))
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok((
        [
            (header::CONTENT_TYPE, image.content_type),
            (header::CACHE_CONTROL, "private, max-age=86400".to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
        ],
        image.bytes,
    )
        .into_response())
}

/// What is playing on the Plex server: title, person, progress, how each stream is delivered
/// (direct play, direct stream or transcode, with the file's and the stream's resolution and
/// bitrate) and the bandwidth it takes.
#[utoipa::path(get, path = "/api/v1/server/sessions", tag = "server",
    responses((status = 200, body = Sessions), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn sessions(State(state): State<AppState>) -> ApiResult<Sessions> {
    let Some(server) = state.engines.media_server.as_ref() else {
        return Ok(Json(Sessions {
            configured: false,
            sessions: Vec::new(),
        }));
    };
    let list = server
        .sessions()
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;
    Ok(Json(Sessions {
        configured: true,
        sessions: list
            .into_iter()
            .map(|s| Session {
                delivery: PlayDelivery {
                    decision: Some(s.decision.clone()),
                    video: s.delivery.video.clone(),
                    audio: s.delivery.audio.clone(),
                    source: (s.delivery.source_resolution.is_some()
                        || s.delivery.source_bitrate.is_some())
                    .then(|| Format {
                        resolution: s.delivery.source_resolution.clone(),
                        bitrate: s.delivery.source_bitrate,
                    }),
                    stream: (s.delivery.stream_resolution.is_some()
                        || s.delivery.stream_bitrate.is_some())
                    .then(|| Format {
                        resolution: s.delivery.stream_resolution.clone(),
                        bitrate: s.delivery.stream_bitrate,
                    }),
                },
                bandwidth: s.bandwidth,
                network: s
                    .local
                    .map(|l| if l { "home" } else { "outside" }.to_string()),
                platform: s.platform,
                product: s.product,
                kind: s.kind,
                title: s.title,
                show: s.show,
                season: s.season,
                episode: s.episode,
                person: s.person,
                player: s.player,
                state: s.state,
                progress: s.progress,
                decision: s.decision,
            })
            .collect(),
    }))
}

/// Albums and artists as the Plex server knows them, with cover art and photos through
/// Singularr, formats, qualities, problems, and the folder's files the server does not know.
#[utoipa::path(get, path = "/api/v1/music", tag = "server",
    responses((status = 200, body = Music), (status = 401, body = ProblemDoc)))]
async fn music(State(state): State<AppState>) -> ApiResult<Music> {
    let albums = music_store::server_albums(&state.pool).await?;
    let artists = music_store::server_artists(&state.pool).await?;
    let server_files = music_store::server_track_files(&state.pool).await?;
    let missing = if server_files.is_empty() {
        Vec::new()
    } else {
        files_not_on_server(
            &music_store::folder_track_paths(&state.pool).await?,
            &server_files,
        )
    };
    let dupes = duplicates(&albums);
    Ok(Json(Music {
        albums: albums.iter().map(|a| music_album(a, &dupes)).collect(),
        artists: artists.iter().map(music_artist).collect(),
        problem_labels: PROBLEMS
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        files_not_on_server_total: missing.len(),
        files_not_on_server: missing.into_iter().take(500).collect(),
    }))
}

/// One album with its tracks, from the Plex server (`album-<rating key>`). A numeric id is an
/// album of the music folder's scan and answers in that model (`AlbumDetail`).
#[utoipa::path(get, path = "/api/v1/music/albums/{id}", tag = "server",
    params(("id" = String, Path, description = "`album-<rating key>`, or the folder album's number")),
    responses((status = 200, body = AlbumRecord), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn album(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> Result<Response, AppError> {
    let Some(key) = id.strip_prefix("album-") else {
        let n: i64 = id
            .parse()
            .map_err(|_| AppError::bad_request(format!("`{id}` is not an album id")))?;
        return Ok(Json(music_service::album(&state.pool, n).await?).into_response());
    };
    let all = music_store::server_albums(&state.pool).await?;
    let a = all
        .iter()
        .find(|a| a.rating_key == key)
        .ok_or_else(|| AppError::not_found(format!("The server has no album {id}.")))?;
    let summary = music_album(a, &duplicates(&all));
    let tracks = music_store::server_album_tracks(&state.pool, key).await?;
    let record = AlbumRecord {
        id: summary.id,
        title: summary.title,
        artist: summary.artist,
        artist_id: summary.artist_id,
        year: summary.year,
        released: summary.released,
        genre: summary.genre,
        label: summary.label,
        duration: summary.duration,
        format: summary.format,
        quality: summary.quality,
        size: summary.size,
        plays: summary.plays,
        added: summary.added,
        cover: summary.cover,
        problems: summary.problems,
        path: a.file.as_deref().and_then(folder_of),
        track_count: summary.tracks,
        tracks: tracks
            .into_iter()
            .map(|t| AlbumTrack {
                n: t.number.filter(|n| *n > 0),
                disc: t.disc.filter(|d| *d > 0).unwrap_or(1),
                title: t.title,
                artist: t.artist.filter(|ar| Some(ar) != a.artist.as_ref()),
                duration: (t.duration_ms.unwrap_or(0) as f64 / 600.0).round() / 100.0,
                format: t.codec.map(|c| c.to_ascii_uppercase()).unwrap_or_default(),
                bitrate: t.bitrate.filter(|b| *b > 0),
                size: t.size_bytes.unwrap_or(0),
            })
            .collect(),
    };
    Ok(Json(record).into_response())
}

/// One artist with their albums, from the Plex server (`artist-<rating key>`). A numeric id is
/// an artist of the music folder's scan and answers in that model (`ArtistDetail`).
#[utoipa::path(get, path = "/api/v1/music/artists/{id}", tag = "server",
    params(("id" = String, Path, description = "`artist-<rating key>`, or the folder artist's number")),
    responses((status = 200, body = ArtistRecord), (status = 404, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn artist(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> Result<Response, AppError> {
    let Some(key) = id.strip_prefix("artist-") else {
        let n: i64 = id
            .parse()
            .map_err(|_| AppError::bad_request(format!("`{id}` is not an artist id")))?;
        return Ok(Json(music_service::artist(&state.pool, n).await?).into_response());
    };
    let artists = music_store::server_artists(&state.pool).await?;
    let a = artists
        .iter()
        .find(|a| a.rating_key == key)
        .ok_or_else(|| AppError::not_found(format!("The server has no artist {id}.")))?;
    let albums = music_store::server_albums(&state.pool).await?;
    let dupes = duplicates(&albums);
    let mut discography: Vec<MusicAlbum> = albums
        .iter()
        .filter(|al| al.artist_key.as_deref() == Some(key))
        .map(|al| music_album(al, &dupes))
        .collect();
    discography.sort_by(|x, y| y.year.cmp(&x.year).then_with(|| x.title.cmp(&y.title)));
    Ok(Json(ArtistRecord {
        artist: music_artist(a),
        discography,
    })
    .into_response())
}

/// An artist's photo, made by the Plex server at `?size=` pixels.
#[utoipa::path(get, path = "/api/v1/music/artists/{id}/photo", tag = "server",
    params(("id" = String, Path, description = "`artist-<rating key>`"), ImageQuery),
    responses((status = 200, description = "The image"), (status = 404, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn artist_photo(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<ImageQuery>,
) -> Result<Response, AppError> {
    let key = id.strip_prefix("artist-").unwrap_or(&id);
    server_image(&state, "artist", key, q.size).await
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct VideosQuery {
    /// The library's id (`<server>-<section key>`). The first plain video library when absent.
    pub id: Option<String>,
}

/// A plain video library.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct VideoLibrary {
    pub id: String,
    /// `homevideo`.
    pub kind: String,
    pub name: String,
    /// Bytes over its recordings.
    pub size: i64,
    /// Recordings.
    pub titles: usize,
}

/// A part of a recording, from the owner's notes on the tape.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct VideoEvent {
    pub label: String,
    /// Minutes from the start, when set.
    pub start: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Years {
    pub from: Option<i32>,
    pub to: Option<i32>,
}

/// One recording of a plain video library: what the server has of the file, and what the
/// owner noted about the tape (by the file's name).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Recording {
    /// `video-<rating key>`, or `tape-<id>` for a tape of the folder the server does not list.
    pub id: String,
    pub title: String,
    pub year: Option<i32>,
    pub years: Years,
    /// Minutes.
    pub duration: Option<i64>,
    /// `H.264`, `HEVC`, `MPEG-2`.
    pub codec: Option<String>,
    /// Lines.
    pub height: Option<i64>,
    /// The file's name.
    pub file: String,
    pub size: i64,
    pub added: Option<DateTime<Utc>>,
    /// A still made by the server, through Singularr; null when there is none.
    pub still: Option<String>,
    pub events: Vec<VideoEvent>,
    pub people: Vec<String>,
    pub places: Vec<String>,
    /// The tape the notes belong to (`PATCH /api/v1/home-movies/{id}`), when there is one.
    pub tape: Option<i64>,
    pub uncertain: bool,
    pub private: bool,
    pub plays: i64,
}

/// A plain video library and its recordings (`GET /api/v1/library/videos`).
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Videos {
    pub library: VideoLibrary,
    pub recordings: Vec<Recording>,
}

/// A video codec as people write it.
fn codec_name(codec: &str) -> String {
    match codec.to_ascii_lowercase().as_str() {
        "h264" | "avc" => "H.264".to_string(),
        "hevc" | "h265" => "HEVC".to_string(),
        "mpeg2video" | "mpeg2" => "MPEG-2".to_string(),
        "mpeg4" => "MPEG-4".to_string(),
        "vc1" => "VC-1".to_string(),
        other => other.to_ascii_uppercase(),
    }
}

fn file_name(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// Every tape of the home movies folder, page by page.
async fn all_tapes(pool: &sqlx::PgPool) -> Result<Vec<Tape>, AppError> {
    let mut out: Vec<Tape> = Vec::new();
    for page in 1..=50u32 {
        let got = homemovies::list(
            pool,
            &TapeQuery {
                page: Some(page),
                per_page: Some(200),
                ..TapeQuery::default()
            },
        )
        .await?;
        let done = got.items.len() < 200 || out.len() + got.items.len() >= got.total as usize;
        out.extend(got.items);
        if done {
            break;
        }
    }
    Ok(out)
}

fn events_of(t: &Tape) -> Vec<VideoEvent> {
    t.chapters
        .iter()
        .map(|c| VideoEvent {
            label: c.label.clone(),
            start: c.start_seconds.map(|s| (f64::from(s) / 6.0).round() / 10.0),
        })
        .collect()
}

/// A plain video library (a movie section of the Plex server without a metadata agent) with
/// its recordings: each file as the server has it, with the owner's notes on the tape of the
/// same name from the home movies folder (title, years, events, people, places). Without a
/// Plex server the tapes alone are listed.
#[utoipa::path(get, path = "/api/v1/library/videos", tag = "server", params(VideosQuery),
    responses((status = 200, body = Videos), (status = 404, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn videos(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<VideosQuery>,
) -> ApiResult<Videos> {
    let libraries = catalog_store::library_names(&state.pool).await?;
    let wanted = q.id.as_deref().map(str::trim).filter(|i| !i.is_empty());
    // The same rule as the workspace record, which is where the page got the library's id.
    let plain = |l: &&catalog_store::LibraryName| {
        plain_video(
            l.kind.as_deref(),
            l.section_key.is_some(),
            l.managed_by.is_some(),
        )
    };
    let library = match wanted {
        Some(id) => libraries
            .iter()
            .find(|l| l.id == id)
            .filter(plain)
            .ok_or_else(|| AppError::not_found(format!("`{id}` is not a plain video library.")))?,
        None => libraries
            .iter()
            .find(plain)
            .ok_or_else(|| AppError::not_found("The server has no plain video library."))?,
    };
    let tapes = all_tapes(&state.pool).await?;
    let by_file: HashMap<String, &Tape> = tapes
        .iter()
        .map(|t| (t.file_name.to_lowercase(), t))
        .collect();
    let server = state.engines.media_server.as_ref();
    let mut recordings: Vec<Recording> = match (server, library.section_key.as_deref()) {
        (Some(server), Some(section)) => server
            .videos(section)
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?
            .into_iter()
            .map(|v| {
                let file = v.file.as_deref().map(file_name).unwrap_or_default();
                let tape = by_file.get(&file.to_lowercase()).copied();
                let year = tape.and_then(|t| t.year_from).or(v.year);
                Recording {
                    id: format!("video-{}", v.rating_key),
                    title: tape
                        .map(|t| t.title.clone())
                        .filter(|t| !t.is_empty())
                        .unwrap_or_else(|| v.title.clone()),
                    year,
                    years: Years {
                        from: year,
                        to: tape.and_then(|t| t.year_to).or(year),
                    },
                    duration: v.duration_ms.map(|ms| (ms + 30_000) / 60_000),
                    codec: v.codec.as_deref().map(codec_name),
                    height: v.height,
                    size: v.size.unwrap_or(0),
                    added: v.added,
                    still: v
                        .thumb
                        .as_ref()
                        .map(|_| format!("/api/v1/library/videos/video-{}/still", v.rating_key)),
                    events: tape.map(events_of).unwrap_or_default(),
                    people: tape.map(|t| t.people.clone()).unwrap_or_default(),
                    places: tape.map(|t| t.places.clone()).unwrap_or_default(),
                    tape: tape.map(|t| t.id),
                    uncertain: tape.is_some_and(|t| t.uncertain),
                    private: tape.is_some_and(|t| t.private),
                    plays: v.plays,
                    file,
                }
            })
            .collect(),
        _ => tapes
            .iter()
            .filter(|t| !t.missing)
            .map(|t| Recording {
                id: format!("tape-{}", t.id),
                title: t.title.clone(),
                year: t.year_from,
                years: Years {
                    from: t.year_from,
                    to: t.year_to.or(t.year_from),
                },
                duration: None,
                codec: None,
                height: None,
                file: t.file_name.clone(),
                size: t.size_bytes,
                added: Some(t.first_seen_at),
                still: None,
                events: events_of(t),
                people: t.people.clone(),
                places: t.places.clone(),
                tape: Some(t.id),
                uncertain: t.uncertain,
                private: t.private,
                plays: 0,
            })
            .collect(),
    };
    recordings.sort_by(|a, b| {
        a.year
            .unwrap_or(i32::MAX)
            .cmp(&b.year.unwrap_or(i32::MAX))
            .then_with(|| a.file.cmp(&b.file))
    });
    Ok(Json(Videos {
        library: VideoLibrary {
            id: library.id.clone(),
            kind: "homevideo".to_string(),
            name: library.name.clone(),
            size: recordings.iter().map(|r| r.size).sum(),
            titles: recordings.len(),
        },
        recordings,
    }))
}

/// A recording's still, made by the Plex server at `?size=` pixels.
#[utoipa::path(get, path = "/api/v1/library/videos/{id}/still", tag = "server",
    params(("id" = String, Path, description = "`video-<rating key>`"), ImageQuery),
    responses((status = 200, description = "The image"), (status = 400, body = ProblemDoc), (status = 404, body = ProblemDoc), (status = 502, body = ProblemDoc), (status = 401, body = ProblemDoc)))]
async fn video_still(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
    ApiQuery(q): ApiQuery<ImageQuery>,
) -> Result<Response, AppError> {
    let key = id
        .strip_prefix("video-")
        .filter(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_digit()))
        .ok_or_else(|| AppError::bad_request(format!("`{id}` is not a recording id")))?;
    photo(&state, &format!("/library/metadata/{key}/thumb"), q.size).await
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/server/sessions", get(sessions))
        .route("/music", get(music))
        .route("/music/albums/{id}", get(album))
        .route("/music/artists/{id}", get(artist))
        .route("/music/artists/{id}/photo", get(artist_photo))
        .route("/library/videos", get(videos))
        .route("/library/videos/{id}/still", get(video_still))
}

#[derive(OpenApi)]
#[openapi(
    paths(sessions, music, album, artist, artist_photo, videos, video_still),
    components(schemas(AlbumDetail, ArtistDetail)),
    tags((name = "server", description = "The Plex server: what is playing, its music, its plain video libraries, its images"))
)]
struct ServerDoc;

pub fn openapi() -> utoipa::openapi::OpenApi {
    ServerDoc::openapi()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album(codecs: &[&str], bitrates: (i64, i64), thumb: bool) -> music_store::ServerAlbumRow {
        music_store::ServerAlbumRow {
            rating_key: "120210".into(),
            artist_key: Some("120209".into()),
            artist: Some("+44".into()),
            title: "When Your Heart Stops Beating".into(),
            year: Some(2006),
            released: Some("2006-10-03".into()),
            genres: vec!["Pop/Rock".into()],
            label: Some("Interscope Records".into()),
            thumb: thumb.then(|| "/library/metadata/120210/thumb/1".into()),
            added_at: None,
            plays: 0,
            tracks: 2,
            duration_ms: 2_700_000,
            size_bytes: 100,
            codecs: codecs.iter().map(|c| c.to_string()).collect(),
            bitrate: Some((bitrates.0 + bitrates.1) / 2),
            min_bitrate: Some(bitrates.0),
            max_bitrate: Some(bitrates.1),
            discs: vec![Some(1), Some(1)],
            numbers: vec![Some(1), Some(2)],
            file: Some("/music/+44/When Your Heart Stops Beating/01 Lycanthrope.mp3".into()),
        }
    }

    #[test]
    fn an_album_says_its_format_quality_cover_and_problems() {
        let none = HashSet::new();
        let a = music_album(&album(&["mp3"], (320, 320), true), &none);
        assert_eq!(
            (a.id.as_str(), a.artist_id.as_deref()),
            ("album-120210", Some("artist-120209"))
        );
        assert_eq!((a.format.as_str(), a.quality.as_str()), ("MP3", "MP3 320"));
        assert_eq!(a.duration, 45);
        assert_eq!(
            a.cover.as_deref(),
            Some("/api/v1/music/albums/album-120210/cover")
        );
        assert!(a.problems.is_empty(), "{:?}", a.problems);
        let low = music_album(&album(&["mp3"], (128, 128), false), &none);
        assert_eq!(low.problems, vec!["low-bitrate", "no-cover"]);
        assert_eq!(low.quality, "MP3 128");
        assert!(low.cover.is_none());
        assert_eq!(
            music_album(&album(&["mp3"], (190, 260), true), &none).quality,
            "MP3 VBR"
        );
        let flac = music_album(&album(&["flac"], (900, 1100), true), &none);
        assert_eq!(
            (flac.format.as_str(), flac.quality.as_str()),
            ("FLAC", "FLAC")
        );
        let mixed = music_album(&album(&["flac", "mp3"], (320, 900), true), &none);
        assert_eq!(
            (mixed.format.as_str(), mixed.quality.as_str()),
            ("Mixed", "Mixed")
        );
        assert!(mixed.problems.iter().any(|p| p == "mixed-formats"));
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["artistId"], "artist-120209");
        assert_eq!(json["released"], "2006-10-03");
    }

    #[test]
    fn gaps_untagged_albums_and_duplicates_are_found() {
        assert!(!track_gaps(&[Some(1), Some(1)], &[Some(1), Some(2)]));
        assert!(track_gaps(&[Some(1), Some(1)], &[Some(1), Some(3)]));
        assert!(track_gaps(&[None, None], &[Some(1), None]));
        assert!(
            !track_gaps(&[Some(1), Some(2)], &[Some(1), Some(1)]),
            "two discs"
        );
        let mut unknown = album(&["mp3"], (128, 128), true);
        unknown.title = "[Unknown Album]".into();
        assert!(untagged(&unknown));
        let mut again = album(&["mp3"], (128, 128), true);
        again.rating_key = "999".into();
        again.title = "When Your Heart  Stops beating".into();
        let dupes = duplicates(&[album(&["mp3"], (128, 128), true), again, unknown]);
        assert!(dupes.contains("120210") && dupes.contains("999"));
        assert_eq!(dupes.len(), 2);
    }

    #[test]
    fn files_are_compared_by_album_folder_and_name() {
        let folder = vec![
            "+44/When Your Heart Stops Beating (2006)/01 Lycanthrope.mp3".to_string(),
            "Some Artist/Lost Album/01 Track.flac".to_string(),
        ];
        let server =
            vec!["/music/+44/When Your Heart Stops Beating (2006)/01 Lycanthrope.mp3".to_string()];
        assert_eq!(
            files_not_on_server(&folder, &server),
            vec!["Some Artist/Lost Album/01 Track.flac"]
        );
        assert_eq!(
            folder_of("/music/+44/Album/01.mp3").as_deref(),
            Some("/music/+44/Album")
        );
        assert_eq!(codec_name("h264"), "H.264");
        assert_eq!(codec_name("mpeg2video"), "MPEG-2");
    }
}
