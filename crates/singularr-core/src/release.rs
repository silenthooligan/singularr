//! Release names into typed fields.
//!
//! The contract: [`parse`] never fails and never guesses silently. Every field it sets is backed by a
//! [`ParseNote`] naming the token it came from, so a wrong reading can be seen and fixed in one place.
//! A word of the title is never read as a quality ("Cam (2018)" is a movie called Cam, not a cam rip).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Resolution {
    #[serde(rename = "480p")]
    R480,
    #[serde(rename = "576p")]
    R576,
    #[serde(rename = "720p")]
    R720,
    #[serde(rename = "1080p")]
    R1080,
    #[serde(rename = "1440p")]
    R1440,
    #[serde(rename = "2160p")]
    R2160,
}

impl Resolution {
    pub fn label(self) -> &'static str {
        match self {
            Resolution::R480 => "480p",
            Resolution::R576 => "576p",
            Resolution::R720 => "720p",
            Resolution::R1080 => "1080p",
            Resolution::R1440 => "1440p",
            Resolution::R2160 => "2160p",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        Some(match s.to_ascii_lowercase().as_str() {
            "480p" | "sd" => Resolution::R480,
            "576p" => Resolution::R576,
            "720p" => Resolution::R720,
            "1080p" | "1080i" => Resolution::R1080,
            "1440p" => Resolution::R1440,
            "2160p" | "4k" | "uhd" => Resolution::R2160,
            _ => return None,
        })
    }
}

/// Where the picture came from, worst to best within a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Source {
    Workprint,
    Cam,
    Telesync,
    Telecine,
    Screener,
    Dvd,
    Tv,
    WebRip,
    WebDl,
    BluRay,
    Remux,
    Disc,
}

impl Source {
    /// The name used in a quality label such as `WEBDL-1080p`.
    pub fn label(self) -> &'static str {
        match self {
            Source::Workprint => "WORKPRINT",
            Source::Cam => "CAM",
            Source::Telesync => "TELESYNC",
            Source::Telecine => "TELECINE",
            Source::Screener => "DVDSCR",
            Source::Dvd => "DVD",
            Source::Tv => "HDTV",
            Source::WebRip => "WEBRip",
            Source::WebDl => "WEBDL",
            Source::BluRay => "Bluray",
            Source::Remux => "Remux",
            Source::Disc => "BR-DISK",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        Some(
            match s.to_ascii_lowercase().replace(['-', ' ', '.'], "").as_str() {
                "workprint" => Source::Workprint,
                "cam" => Source::Cam,
                "telesync" | "ts" => Source::Telesync,
                "telecine" | "tc" => Source::Telecine,
                "dvdscr" | "screener" => Source::Screener,
                "dvd" | "dvdrip" => Source::Dvd,
                "hdtv" | "tv" | "sdtv" => Source::Tv,
                "webrip" => Source::WebRip,
                "webdl" | "web" => Source::WebDl,
                "bluray" | "bdrip" | "brrip" => Source::BluRay,
                "remux" => Source::Remux,
                "brdisk" | "disc" => Source::Disc,
                _ => return None,
            },
        )
    }

    /// Recorded in a theater or leaked before release.
    pub fn is_prerelease(self) -> bool {
        matches!(
            self,
            Source::Workprint
                | Source::Cam
                | Source::Telesync
                | Source::Telecine
                | Source::Screener
        )
    }
}

/// A source and a resolution together, written `WEBDL-1080p`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Quality {
    pub source: Source,
    pub resolution: Resolution,
}

impl Quality {
    pub fn label(self) -> String {
        format!("{}-{}", self.source.label(), self.resolution.label())
    }

    /// Reads `WEBDL-1080p`, `Remux-2160p`, `Bluray-720p`, `HDTV-1080p`, and the names the arrs
    /// use for qualities without a resolution of their own: `DVD` and `SDTV` (480p), and Sonarr's
    /// `Bluray-1080p Remux`.
    pub fn from_label(s: &str) -> Option<Self> {
        let s = s.trim();
        let sd = |source| Quality {
            source,
            resolution: Resolution::R480,
        };
        match s.to_ascii_lowercase().as_str() {
            "dvd" => return Some(sd(Source::Dvd)),
            "sdtv" => return Some(sd(Source::Tv)),
            _ => {}
        }
        if let Some(disc) = s
            .strip_suffix(" Remux")
            .or_else(|| s.strip_suffix(" remux"))
        {
            let (_, res) = disc.rsplit_once('-')?;
            return Some(Quality {
                source: Source::Remux,
                resolution: Resolution::from_label(res)?,
            });
        }
        let (src, res) = s.rsplit_once('-')?;
        Some(Quality {
            source: Source::from_label(src)?,
            resolution: Resolution::from_label(res)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
    Vc1,
    Mpeg2,
    Xvid,
    Vp9,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HdrFormat {
    DolbyVision,
    Hdr10,
    Hdr10Plus,
    Hlg,
    Pq,
    /// The name says `HDR` and nothing more exact.
    Hdr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioCodec {
    TrueHd,
    DtsX,
    DtsHdMa,
    DtsHdHra,
    Dts,
    Flac,
    Pcm,
    Eac3,
    Ac3,
    Aac,
    Opus,
    Mp3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MediaKind {
    Movie,
    Episode,
    SeasonPack,
    #[default]
    Unknown,
}

/// One piece of evidence: which token of the name set which field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseNote {
    /// The field that was set, for example `source` or `group`.
    pub field: String,
    /// The text in the name that set it, as written.
    pub token: String,
    /// Byte offset of the token in the raw name.
    pub at: usize,
    /// What it was read as, for display.
    pub read_as: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ParsedRelease {
    pub raw: String,
    pub kind: MediaKind,
    pub title: Option<String>,
    pub year: Option<u16>,
    pub season: Option<u16>,
    pub episodes: Vec<u16>,
    pub resolution: Option<Resolution>,
    pub source: Option<Source>,
    pub video_codec: Option<VideoCodec>,
    pub bit_depth: Option<u8>,
    pub hdr: Vec<HdrFormat>,
    pub audio_codec: Option<AudioCodec>,
    /// As written, for example `5.1` or `7.1`.
    pub audio_channels: Option<String>,
    pub atmos: bool,
    /// Streaming service tag, upper case: `AMZN`, `NF`, `ATVP`.
    pub service: Option<String>,
    /// Normalized edition names: `Open Matte`, `Extended`, `Director's Cut`, `IMAX`, `Remastered`.
    /// The full list is in [`EDITIONS`]. A 4K remaster carries both `Remastered` and `4K Remaster`.
    /// An edition named in the release and not on the list is kept as written ("Encore Cut").
    pub editions: Vec<String>,
    /// Frames per second when the name states it (60 for the interpolated releases).
    pub fps: Option<u16>,
    pub ai_upscaled: bool,
    pub proper: bool,
    pub repack: bool,
    pub group: Option<String>,
    /// English names of the languages the name states: `English`, `Hindi`, `Multi`.
    pub languages: Vec<String>,
    pub notes: Vec<ParseNote>,
    /// The collective a group releases under, from a trailing tag: `QxR` in `(.. Tigole) [QxR]`.
    #[serde(default)]
    pub collective: Option<String>,
    /// The air date of a daily show episode, `2026-11-20`.
    #[serde(default)]
    pub air_date: Option<String>,
    /// The name says `SDR` outright. An empty `hdr` alone only means the name does not say.
    #[serde(default)]
    pub sdr: bool,
}

impl ParsedRelease {
    pub fn quality(&self) -> Option<Quality> {
        Some(Quality {
            source: self.source?,
            resolution: self.resolution?,
        })
    }

    pub fn has_edition(&self, name: &str) -> bool {
        self.editions.iter().any(|e| e.eq_ignore_ascii_case(name))
    }
}

/// The edition names the parser normalizes to.
pub const EDITIONS: &[&str] = &[
    "Open Matte",
    "Extended",
    "Director's Cut",
    "Theatrical",
    "Unrated",
    "Uncut",
    "Uncensored",
    "Final Cut",
    "Special Edition",
    "Ultimate",
    "Collector's",
    "Alternate Cut",
    "Anniversary",
    "Criterion",
    "IMAX",
    "IMAX Enhanced",
    "Remastered",
    "4K Remaster",
    "Restored",
    "Black and Chrome",
];

/// Parse a release name (a torrent or NZB title, a scene name, or a file name with or without its extension).
pub fn parse(name: &str) -> ParsedRelease {
    crate::release::parser::parse(name)
}

/// Normalized edition names found in free text, such as an arr's edition field
/// ("4K Remastered - Director's Cut" gives `Remastered`, `4K Remaster`, `Director's Cut`).
pub fn editions_from_text(text: &str) -> Vec<String> {
    crate::release::parser::editions(text)
}

mod parser;

#[cfg(test)]
mod tests;
