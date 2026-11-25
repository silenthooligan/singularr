//! The music library as the API and the web app see it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One artist folder, with totals over its albums.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct ArtistSummary {
    pub id: i64,
    /// The folder name, which is the artist's name.
    pub name: String,
    pub folder: String,
    pub album_count: i64,
    pub track_count: i64,
    pub size_bytes: i64,
    /// Every audio format in the artist's albums: `flac`, `mp3`.
    pub formats: Vec<String>,
    /// Albums with no cover image in their folder.
    pub missing_covers: i64,
    /// An album of the artist that has a cover, for a picture of the artist.
    pub cover_album_id: Option<i64>,
}

/// Filters of `GET /api/v1/music/artists`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct ArtistQuery {
    /// Words of the artist's name.
    pub q: Option<String>,
    /// Artists with an album in this format: `flac`, `mp3`, ...
    pub format: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// One album folder, with its totals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct AlbumSummary {
    pub id: i64,
    pub artist_id: i64,
    pub artist: String,
    /// The folder name without the year.
    pub name: String,
    /// The folder as it is on disk: `Riding With the King (2000)`.
    pub folder: String,
    /// From `(YYYY)` in the folder name. Absent when there is none, or it is `(0000)`.
    pub year: Option<i32>,
    pub track_count: i32,
    pub size_bytes: i64,
    /// The audio formats of the tracks, by file extension.
    pub formats: Vec<String>,
    /// The one format, `mixed` when the tracks have several, absent when there is no track.
    #[cfg_attr(feature = "ssr", sqlx(skip))]
    #[serde(default)]
    pub format: Option<String>,
    pub has_cover: bool,
    /// The cover image, relative to the music folder.
    pub cover_path: Option<String>,
    /// `/api/v1/music/albums/{id}/cover` when there is a cover.
    #[cfg_attr(feature = "ssr", sqlx(skip))]
    #[serde(default)]
    pub cover_url: Option<String>,
    /// Another folder of the same artist has the same album name.
    pub duplicate: bool,
    /// Files in the folder that are neither audio nor images.
    pub other_files: i32,
    pub first_seen_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AlbumSummary {
    /// Fill the fields that are worked out from the others.
    pub fn finish(mut self) -> Self {
        self.format = match self.formats.as_slice() {
            [] => None,
            [one] => Some(one.clone()),
            _ => Some("mixed".to_string()),
        };
        self.cover_url = self
            .has_cover
            .then(|| format!("/api/v1/music/albums/{}/cover", self.id));
        self
    }
}

/// Filters of `GET /api/v1/music/albums`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct AlbumQuery {
    /// Albums with a track in this format: `flac`, `mp3`, ... `mixed`: albums with more than
    /// one format.
    pub format: Option<String>,
    /// `true`: albums without a cover. `false`: albums with one.
    pub missing_cover: Option<bool>,
    /// `true`: albums whose folder has no year (or `(0000)`). `false`: albums with a year.
    pub no_year: Option<bool>,
    /// `true`: albums whose name another folder of the same artist has too.
    pub duplicates: Option<bool>,
    /// The artist's id.
    pub artist: Option<i64>,
    /// Words of the album or the artist.
    pub q: Option<String>,
    /// `artist` (the default: artist, then year), `name`, `year`, `size`, `tracks` or `added`
    /// (newest first). Prefix `-` to reverse.
    pub sort: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// Albums of one format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct FormatCount {
    pub format: String,
    pub albums: i64,
}

/// The library's totals and the counts the filters show, over the whole library.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct MusicCounts {
    pub artists: i64,
    pub albums: i64,
    pub tracks: i64,
    pub size_bytes: i64,
    pub missing_cover: i64,
    pub no_year: i64,
    pub duplicates: i64,
    /// Album folders without an audio file.
    pub empty: i64,
    pub mixed_formats: i64,
    #[cfg_attr(feature = "ssr", sqlx(skip))]
    #[serde(default)]
    pub formats: Vec<FormatCount>,
}

/// The answer of `GET /api/v1/music/albums`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct AlbumPage {
    pub items: Vec<AlbumSummary>,
    pub page: u32,
    pub per_page: u32,
    /// Albums that match the filters, over all pages.
    pub total: i64,
    pub counts: MusicCounts,
}

/// One audio file of an album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct Track {
    pub id: i64,
    /// Relative to the album folder; a disc folder is part of it (`CD2/01 - Intro.flac`).
    pub file_name: String,
    /// Read from the file name (`Artist - Album - 07 - Title.flac`, `07 - Title.flac`).
    /// Absent when the name has none, or it is `00`.
    pub track_number: Option<i32>,
    /// The part of the file name after the number.
    pub title: Option<String>,
    /// The file extension, lower case.
    pub format: String,
    pub size_bytes: i64,
}

/// Something about an album folder that wants attention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct AlbumProblem {
    pub album_id: i64,
    pub album: String,
    /// `missing_cover`, `no_year`, `duplicate`, `mixed_formats` or `empty`.
    pub kind: String,
    pub text: String,
}

/// Another folder of the same artist with the same album name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema, sqlx::FromRow))]
pub struct AlbumRef {
    pub id: i64,
    pub folder: String,
    pub track_count: i32,
    pub size_bytes: i64,
}

/// The answer of `GET /api/v1/music/albums/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct AlbumDetail {
    #[serde(flatten)]
    pub album: AlbumSummary,
    pub tracks: Vec<Track>,
    pub duplicates: Vec<AlbumRef>,
    pub problems: Vec<AlbumProblem>,
}

/// The answer of `GET /api/v1/music/artists/{id}`: the artist, every album with its tracks,
/// and the problems found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct ArtistDetail {
    #[serde(flatten)]
    pub artist: ArtistSummary,
    pub albums: Vec<AlbumDetail>,
    pub problems: Vec<AlbumProblem>,
}

/// The id of a media server's album or artist in the API and the search index (`kind` is
/// `album` or `artist`): `album-<key>` on the first media server, `album-<server>_<key>` on any
/// other. Two servers can hold one key: Plex and Emby number their items alike. An integration
/// id holds no `_`: the first `_` ends the server.
pub fn server_music_id(kind: &str, first: Option<&str>, server: &str, key: &str) -> String {
    if first == Some(server) && !key.contains('_') {
        format!("{kind}-{key}")
    } else {
        format!("{kind}-{server}_{key}")
    }
}

/// The server and the key of a media server's album or artist id ([`server_music_id`]). The
/// server is `None` in the first media server's form (`album-<key>`). `None` for an id of
/// another kind or form.
pub fn parse_server_music_id<'a>(kind: &str, id: &'a str) -> Option<(Option<&'a str>, &'a str)> {
    let rest = id.strip_prefix(kind)?.strip_prefix('-')?;
    match rest.split_once('_') {
        Some((server, key)) if !server.is_empty() && !key.is_empty() => Some((Some(server), key)),
        Some(_) => None,
        None => (!rest.is_empty()).then_some((None, rest)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_music_id_names_its_server_unless_it_is_the_first() {
        let first = Some("plex");
        assert_eq!(
            server_music_id("album", first, "plex", "120210"),
            "album-120210"
        );
        assert_eq!(
            server_music_id("album", first, "emby", "120210"),
            "album-emby_120210"
        );
        assert_eq!(
            server_music_id("artist", None, "plex", "120209"),
            "artist-plex_120209"
        );
        assert_eq!(
            server_music_id("album", first, "plex", "a_b"),
            "album-plex_a_b"
        );
        for (id, kind, read) in [
            ("album-120210", "album", Some((None, "120210"))),
            ("album-emby_120210", "album", Some((Some("emby"), "120210"))),
            ("album-plex_a_b", "album", Some((Some("plex"), "a_b"))),
            (
                "artist-radarr-4k_7",
                "artist",
                Some((Some("radarr-4k"), "7")),
            ),
            ("album-120210", "artist", None),
            ("album-", "album", None),
            ("album-_7", "album", None),
            ("album-emby_", "album", None),
            ("albums-7", "album", None),
            ("7", "album", None),
        ] {
            assert_eq!(parse_server_music_id(kind, id), read, "{id}");
        }
    }
}
