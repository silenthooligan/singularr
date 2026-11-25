//! Library file names, read back into fields.
//!
//! Movies:   `Title (Year) {imdb-tt0304141} {edition-Open Matte} [WEBDL-1080p][EAC3 5.1][HEVC]-PeruGuy.mkv`
//! Folders:  `Title (Year) {tmdb-673}`
//! Episodes: `Show (Year) - S01E03 - Episode Title [HDTV-1080p][EAC3 5.1][h264]-NTb.mkv`
//!
//! [`parse_library_name`] reads the fixed parts of such a name. [`LibraryName::to_release`] gives
//! the same name as a [`ParsedRelease`], so a held file can be scored like any release.
//!
//! Editions are named so Plex groups the editions of a title into one item with a version
//! picker ([`LibraryNaming`]): `Title (Year) {imdb-tt0450385} {edition-Director's Cut}
//! [Bluray-1080p]...`.

use crate::release::{self, MediaKind, ParseNote, ParsedRelease, Quality};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{(tmdb|imdb|tvdb)-([A-Za-z0-9]+)\}").unwrap());
static EDITION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{edition-([^}]+)\}").unwrap());
static BRACKET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[([^\]]+)\]").unwrap());
static TITLE_YEAR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.*?)\s*\((\d{4})\)").unwrap());
static EPISODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bS(\d{1,3})E(\d{1,3})(?:-E(\d{1,3}))?\b").unwrap());
static GROUP: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\]-([^\]\[/\\]+?)(?:\.(?:mkv|mp4|avi|m4v|ts|m2ts|wmv|mov|iso|srt|nfo))?$")
        .unwrap()
});
/// What the arrs append to the quality inside its bracket: `[Bluray-1080p Proper]`.
static REVISION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+(?:proper|repack|real|v\d)\b").unwrap());

/// How a library names the edition of a movie file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryNaming {
    /// `{edition-Director's Cut}` after the ids (the Plex editions feature).
    Plex,
}

impl LibraryNaming {
    /// The edition part of a movie file name, with its leading space: ` {edition-Director's
    /// Cut}`. Empty for an empty label. A copy of the default edition
    /// carries no part, unless the title holds another edition too; the caller then passes the
    /// default's label. Characters a name or the token cannot hold are left out.
    pub fn edition_part(self, label: &str) -> String {
        let clean: String = label
            .chars()
            .filter(|c| {
                !matches!(
                    c,
                    '{' | '}' | '[' | ']' | '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
            })
            .collect();
        let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
        if clean.is_empty() {
            return String::new();
        }
        match self {
            LibraryNaming::Plex => format!(" {{edition-{clean}}}"),
        }
    }

    /// The start of a movie file name: `Title (Year)` with the ids Plex keeps (`{imdb-tt..}`)
    /// and the edition part, before the quality brackets.
    pub fn movie_stem(
        self,
        title: &str,
        year: Option<u16>,
        imdb: Option<&str>,
        edition_label: Option<&str>,
    ) -> String {
        let mut out = title.trim().to_string();
        if let Some(y) = year {
            out.push_str(&format!(" ({y})"));
        }
        if self == LibraryNaming::Plex
            && let Some(id) = imdb.filter(|i| !i.is_empty())
        {
            out.push_str(&format!(" {{imdb-{id}}}"));
        }
        if let Some(label) = edition_label {
            out.push_str(&self.edition_part(label));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LibraryName {
    pub title: Option<String>,
    pub year: Option<u16>,
    pub tmdb: Option<u64>,
    pub tvdb: Option<u64>,
    pub imdb: Option<String>,
    pub edition: Option<String>,
    pub season: Option<u16>,
    pub episodes: Vec<u16>,
    /// The first bracket when it reads as a quality: `WEBDL-1080p`.
    pub quality: Option<String>,
    /// Every bracketed token, in order.
    pub tokens: Vec<String>,
    pub group: Option<String>,
    /// The name as given, kept so [`LibraryName::to_release`] can point its evidence at it.
    #[serde(default)]
    pub raw: String,
    /// The quality bracket says Proper or Repack: `[Bluray-1080p Proper]`.
    #[serde(default)]
    pub proper: bool,
}

/// Read a library file or folder name (no directories).
pub fn parse_library_name(name: &str) -> LibraryName {
    let mut out = LibraryName {
        raw: name.to_string(),
        ..Default::default()
    };
    if let Some(c) = TITLE_YEAR.captures(name) {
        out.title = Some(c[1].trim().to_string()).filter(|t| !t.is_empty());
        out.year = c[2].parse().ok();
    }
    for c in ID.captures_iter(name) {
        match &c[1] {
            "tmdb" => out.tmdb = c[2].parse().ok(),
            "tvdb" => out.tvdb = c[2].parse().ok(),
            _ => out.imdb = Some(c[2].to_string()),
        }
    }
    out.edition = EDITION.captures(name).map(|c| c[1].trim().to_string());
    if let Some(c) = EPISODE.captures(name) {
        out.season = c[1].parse().ok();
        let first: u16 = c[2].parse().unwrap_or(0);
        let last: u16 = c
            .get(3)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(first);
        out.episodes = (first..=last.max(first)).collect();
    }
    out.tokens = BRACKET
        .captures_iter(name)
        .map(|c| c[1].to_string())
        .collect();
    // The quality bracket may carry a revision ("Bluray-1080p Proper") and, in the encode
    // pipeline's names, a service in front ("HMAX WEB-DL-1080p").
    if let Some(first) = out.tokens.first() {
        let plain = REVISION.replace_all(first, "").trim().to_string();
        let reads = |label: &str| Quality::from_label(label).is_some();
        let label = if reads(&plain) {
            Some(plain.clone())
        } else {
            plain
                .rsplit(' ')
                .next()
                .filter(|w| reads(w))
                .map(str::to_string)
        };
        if label.is_some() {
            out.quality = label;
            out.proper = plain != *first;
        }
    }
    out.group = GROUP.captures(name).map(|c| c[1].to_string());
    out
}

impl LibraryName {
    /// The quality bracket as a typed quality.
    pub fn quality(&self) -> Option<Quality> {
        self.quality.as_deref().and_then(Quality::from_label)
    }

    /// The same name as a release, so a held file can be scored like any candidate.
    ///
    /// The name goes through [`release::parse`], which reads the audio, video and HDR brackets.
    /// Fields this reader is sure of (title, year, episode, quality, edition, group) fill in
    /// whatever the parser left empty, each with a note that points at the name.
    pub fn to_release(&self) -> ParsedRelease {
        let mut p = release::parse(&self.raw);
        let raw = p.raw.clone();
        let note = |p: &mut ParsedRelease, field: &str, token: &str, read_as: &str| {
            if let Some(at) = raw.rfind(token).filter(|_| !token.is_empty()) {
                p.notes.push(ParseNote {
                    field: field.to_string(),
                    token: token.to_string(),
                    at,
                    read_as: read_as.to_string(),
                });
            }
        };
        if p.title.is_none()
            && let Some(title) = &self.title
        {
            p.title = Some(title.clone());
            note(&mut p, "title", title, title);
        }
        if p.year.is_none()
            && let Some(year) = self.year
        {
            p.year = Some(year);
            note(&mut p, "year", &year.to_string(), &year.to_string());
        }
        if p.season.is_none() && self.season.is_some() {
            p.season = self.season;
            p.episodes = self.episodes.clone();
        }
        if let (Some(q), Some(label)) = (self.quality(), &self.quality) {
            if p.source.is_none() {
                p.source = Some(q.source);
                note(&mut p, "source", label, q.source.label());
            }
            if p.resolution.is_none() {
                p.resolution = Some(q.resolution);
                note(&mut p, "resolution", label, q.resolution.label());
            }
        }
        if let Some(edition) = &self.edition {
            for name in release::editions_from_text(edition) {
                if !p.has_edition(&name) {
                    note(&mut p, "edition", edition, &name);
                    p.editions.push(name);
                }
            }
        }
        if p.group.is_none()
            && let Some(group) = &self.group
        {
            p.group = Some(group.clone());
            note(&mut p, "group", group, group);
        }
        if self.proper && !p.proper && !p.repack {
            p.proper = true;
        }
        if p.kind == MediaKind::Unknown {
            p.kind = match (p.season, p.year) {
                (Some(_), _) => MediaKind::Episode,
                (None, Some(_)) => MediaKind::Movie,
                (None, None) => MediaKind::Unknown,
            };
        }
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::{AudioCodec, HdrFormat, Resolution, Source, VideoCodec};

    #[test]
    fn movie_file() {
        let n = parse_library_name(
            "Harry Potter and the Prisoner of Azkaban (2004) {imdb-tt0304141} {edition-Open Matte} [WEBDL-1080p][EAC3 5.1][HEVC]-PeruGuy.mkv",
        );
        assert_eq!(
            n.title.as_deref(),
            Some("Harry Potter and the Prisoner of Azkaban")
        );
        assert_eq!(
            (n.year, n.imdb.as_deref(), n.edition.as_deref()),
            (Some(2004), Some("tt0304141"), Some("Open Matte"))
        );
        assert_eq!(
            (n.quality.as_deref(), n.group.as_deref()),
            (Some("WEBDL-1080p"), Some("PeruGuy"))
        );
    }

    #[test]
    fn the_plex_naming_carries_the_edition_after_the_ids() {
        let plex = LibraryNaming::Plex;
        assert_eq!(
            plex.edition_part("Director's Cut"),
            " {edition-Director's Cut}"
        );
        assert_eq!(plex.edition_part(""), "");
        assert_eq!(plex.edition_part("Odd {Cut}/x"), " {edition-Odd Cutx}");
        assert_eq!(
            plex.movie_stem(
                "1408",
                Some(2007),
                Some("tt0450385"),
                Some("Director's Cut")
            ),
            "1408 (2007) {imdb-tt0450385} {edition-Director's Cut}"
        );
        assert_eq!(
            plex.movie_stem("1408", Some(2007), None, None),
            "1408 (2007)"
        );
        // What the naming writes, the reader reads back.
        let name = format!(
            "{} [Bluray-1080p][EAC3 5.1][x265]-SM737.mkv",
            plex.movie_stem(
                "1408",
                Some(2007),
                Some("tt0450385"),
                Some("Director's Cut")
            )
        );
        let n = parse_library_name(&name);
        assert_eq!(n.edition.as_deref(), Some("Director's Cut"), "{name}");
        assert_eq!(n.title.as_deref(), Some("1408"), "{name}");
        assert_eq!(n.quality.as_deref(), Some("Bluray-1080p"), "{name}");
        // A spaced dash after the year names no edition: only the Plex token does.
        assert_eq!(
            parse_library_name("Movie (2019) - Part 2 [WEBDL-1080p]-GRP.mkv").edition,
            None
        );
        assert_eq!(
            parse_library_name("Show (2022) - S01E03 - Extended [HDTV-1080p]-NTb.mkv").edition,
            None
        );
    }

    #[test]
    fn episode_file_and_folder() {
        let n = parse_library_name(
            "Shoresy (2022) - S01E03 - Know Your Role [HDTV-1080p][EAC3 5.1][h264]-NTb.mkv",
        );
        assert_eq!(
            (n.season, n.episodes.clone(), n.group.as_deref()),
            (Some(1), vec![3], Some("NTb"))
        );
        assert_eq!(
            parse_library_name("The Matrix (1999) {tmdb-603}").tmdb,
            Some(603)
        );
    }

    #[test]
    fn the_quality_bracket_with_a_revision_or_a_service() {
        for (name, quality, proper) in [
            (
                "10 Cloverfield Lane (2016) {imdb-tt1179933} [Bluray-1080p Proper][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv",
                "Bluray-1080p",
                true,
            ),
            (
                "The Assassin (2025) - S01E01 - Episode 1 [WEBRip-1080p Proper][EAC3 5.1][x265]-d3g.mkv",
                "WEBRip-1080p",
                true,
            ),
            (
                "Band of Brothers (2001) - S01E01 - Currahee [Bluray-1080p Remux][DTS-HD MA 5.1][h264]-GRP.mkv",
                "Bluray-1080p Remux",
                false,
            ),
            (
                "Rise of the Guardians (2012) {tmdb-81188} [HMAX WEB-DL-1080p][Opus 5.1][HEVC HUSH]-[SHH].mkv",
                "WEB-DL-1080p",
                false,
            ),
            (
                "Old Show (1987) - S01E01 - Pilot [DVD][AC3 2.0][x264]-GRP.mkv",
                "DVD",
                false,
            ),
        ] {
            let n = parse_library_name(name);
            assert_eq!(n.quality.as_deref(), Some(quality), "{name}");
            assert_eq!(n.proper, proper, "{name}");
            assert!(n.quality().is_some(), "{name}");
        }
        let n = parse_library_name(
            "Band of Brothers (2001) - S01E01 - Currahee [Bluray-1080p Remux][DTS-HD MA 5.1][h264]-GRP.mkv",
        );
        assert_eq!(n.quality().map(|q| q.source), Some(Source::Remux));
    }

    #[test]
    fn a_library_name_as_a_release() {
        let p = parse_library_name(
            "Harry Potter and the Prisoner of Azkaban (2004) {imdb-tt0304141} {edition-Open Matte} [WEBDL-1080p][EAC3 5.1][HEVC]-PeruGuy.mkv",
        )
        .to_release();
        assert_eq!(p.kind, MediaKind::Movie);
        assert_eq!(
            p.title.as_deref(),
            Some("Harry Potter and the Prisoner of Azkaban")
        );
        assert_eq!(p.year, Some(2004));
        assert_eq!(
            p.quality().map(Quality::label).as_deref(),
            Some("WEBDL-1080p")
        );
        assert!(p.has_edition("Open Matte"));
        assert_eq!(
            (p.audio_codec, p.audio_channels.as_deref(), p.video_codec),
            (Some(AudioCodec::Eac3), Some("5.1"), Some(VideoCodec::H265))
        );
        assert_eq!(p.group.as_deref(), Some("PeruGuy"));
        for n in &p.notes {
            assert_eq!(&p.raw[n.at..n.at + n.token.len()], n.token);
        }

        let p = parse_library_name(
            "10 Cloverfield Lane (2016) {imdb-tt1179933} [Bluray-1080p Proper][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv",
        )
        .to_release();
        assert_eq!(p.hdr, [HdrFormat::DolbyVision, HdrFormat::Hdr10]);
        assert!(p.proper && p.atmos);
        assert_eq!(
            (p.source, p.resolution),
            (Some(Source::BluRay), Some(Resolution::R1080))
        );

        let p = parse_library_name(
            "Shoresy (2022) - S01E03 - Know Your Role [HDTV-1080p][EAC3 5.1][h264]-NTb.mkv",
        )
        .to_release();
        assert_eq!(
            (p.kind, p.season, p.episodes.clone()),
            (MediaKind::Episode, Some(1), vec![3])
        );
        assert_eq!(
            (p.title.as_deref(), p.group.as_deref()),
            (Some("Shoresy"), Some("NTb"))
        );

        // The arrs write SDTV and DVD without a line count. The quality bracket still decides.
        let p =
            parse_library_name("Old Show (1987) - S01E01 - Pilot [SDTV][AC3 2.0][x264]-GRP.mkv")
                .to_release();
        assert_eq!(
            (p.source, p.resolution),
            (Some(Source::Tv), Some(Resolution::R480))
        );

        // A name built by hand, with no raw text behind it, still gives its fields.
        let by_hand = LibraryName {
            title: Some("Cam".into()),
            year: Some(2018),
            quality: Some("WEBDL-1080p".into()),
            group: Some("NTG".into()),
            ..Default::default()
        };
        let p = by_hand.to_release();
        assert_eq!(
            (p.kind, p.title.as_deref(), p.source, p.group.as_deref()),
            (
                MediaKind::Movie,
                Some("Cam"),
                Some(Source::WebDl),
                Some("NTG")
            )
        );
        assert!(p.notes.is_empty());
    }
}
