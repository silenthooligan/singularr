//! Naming conventions: a template per kind of name (movie folder, movie file, series folder,
//! season folder, episode file), tokens with literal text between them, the presets of the common
//! conventions, the managers' own formats in these tokens, and the name a template gives a file.
//!
//! A token is written `{Title}`, or `{Season:00}` for a number padded to two digits. As in the
//! managers' formats, text that belongs to a value goes inside its braces and is written only with
//! it: `{[Quality]}`, `{-Group}`, `{ Audio Channels]}`. Every other brace or bracket is literal: in
//! `{imdb-{ImdbId}}` the outer braces are Plex's own syntax. A bracketed part whose tokens all come
//! out empty is left out with its brackets, so `{edition-{Edition}}` writes nothing for a file
//! without an edition.
//!
//! A name is written from what the file's manager knows of it, the way the manager writes it: the
//! quality with Proper or Repack, the codec, dynamic range and audio from its media info, the
//! custom formats it names, values cleaned as it cleans them (`: ` becomes ` - `, `?` becomes `!`).

use crate::profile::{Convention, NameKind};
use crate::release::{AudioCodec, HdrFormat, ParsedRelease, Quality, VideoCodec};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

const MOVIE: &[NameKind] = &[NameKind::MovieFolder, NameKind::MovieFile];
const SERIES: &[NameKind] = &[
    NameKind::SeriesFolder,
    NameKind::SeasonFolder,
    NameKind::EpisodeFile,
];
const TITLED: &[NameKind] = &[
    NameKind::MovieFolder,
    NameKind::MovieFile,
    NameKind::SeriesFolder,
    NameKind::EpisodeFile,
];
const FILES: &[NameKind] = &[NameKind::MovieFile, NameKind::EpisodeFile];

/// One token a template can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// As written between braces: `Episode Title`.
    pub name: &'static str,
    pub label: &'static str,
    /// The kinds of names it fits.
    pub kinds: &'static [NameKind],
}

const fn token(name: &'static str, label: &'static str, kinds: &'static [NameKind]) -> Token {
    Token { name, label, kinds }
}

/// Every token, in the order the picker shows them.
pub const TOKENS: &[Token] = &[
    token("Title", "Title", TITLED),
    token("Clean Title", "Title without signs or accents", TITLED),
    token("Title Year", "Title and year", TITLED),
    token("Clean Title Year", "Clean title and year", TITLED),
    token("Year", "Year", TITLED),
    token("TmdbId", "TMDB id", TITLED),
    token("ImdbId", "IMDb id", TITLED),
    token("TvdbId", "TVDB id", SERIES),
    token("Edition", "Edition", MOVIE),
    token("Quality", "Quality, with Proper or Repack", FILES),
    token("Quality Title", "Quality alone", FILES),
    token("Resolution", "Resolution", FILES),
    token("Source", "Source", FILES),
    token("HDR", "Dynamic range", FILES),
    token("Audio", "Audio codec and channels", FILES),
    token("Audio Codec", "Audio codec", FILES),
    token("Audio Channels", "Audio channels", FILES),
    token(
        "Audio Languages",
        "Audio languages, unless English alone",
        FILES,
    ),
    token("Codec", "Video codec", FILES),
    token("Custom Formats", "Custom formats the manager names", FILES),
    token("Group", "Release group", FILES),
    token(
        "Season",
        "Season",
        &[NameKind::SeasonFolder, NameKind::EpisodeFile],
    ),
    token("Episode", "Episode", &[NameKind::EpisodeFile]),
    token("Episode Title", "Episode title", &[NameKind::EpisodeFile]),
    token(
        "Episode Clean Title",
        "Episode title without signs or accents",
        &[NameKind::EpisodeFile],
    ),
];

impl NameKind {
    pub const ALL: [NameKind; 5] = [
        NameKind::MovieFolder,
        NameKind::MovieFile,
        NameKind::SeriesFolder,
        NameKind::SeasonFolder,
        NameKind::EpisodeFile,
    ];

    pub fn label(self) -> &'static str {
        match self {
            NameKind::MovieFolder => "Movie folder",
            NameKind::MovieFile => "Movie file",
            NameKind::SeriesFolder => "Series folder",
            NameKind::SeasonFolder => "Season folder",
            NameKind::EpisodeFile => "Episode file",
        }
    }

    /// The key a route takes: `movieFile`.
    pub fn key(self) -> &'static str {
        match self {
            NameKind::MovieFolder => "movieFolder",
            NameKind::MovieFile => "movieFile",
            NameKind::SeriesFolder => "seriesFolder",
            NameKind::SeasonFolder => "seasonFolder",
            NameKind::EpisodeFile => "episodeFile",
        }
    }

    pub fn from_key(key: &str) -> Option<NameKind> {
        NameKind::ALL.into_iter().find(|k| k.key() == key)
    }

    /// Movies or shows.
    pub fn is_movie(self) -> bool {
        matches!(self, NameKind::MovieFolder | NameKind::MovieFile)
    }
}

/// A convention for every kind of name, the way a media server or a guide names files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    /// Movie folder, movie file, series folder, season folder, episode file.
    pub templates: [&'static str; 5],
}

impl Preset {
    pub fn template(&self, kind: NameKind) -> &'static str {
        let i = NameKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        self.templates[i]
    }
}

/// The presets. `detailed` is the one the managers' guides recommend, and a new model's.
pub const PRESETS: &[Preset] = &[
    Preset {
        id: "detailed",
        label: "Detailed",
        templates: [
            "{Title} ({Year}) {tmdb-{TmdbId}}",
            "{Title} ({Year}) {imdb-{ImdbId}} {edition-{Edition}} [{Quality}][{HDR}][{Audio}][{Codec}]-{Group}",
            "{Title} ({Year}) {tvdb-{TvdbId}}",
            "Season {Season:00}",
            "{Title} ({Year}) - S{Season:00}E{Episode:00} - {Episode Title} [{Quality}][{HDR}][{Audio}][{Codec}]-{Group}",
        ],
    },
    Preset {
        id: "plex",
        label: "Plex",
        templates: [
            "{Title} ({Year}) {tmdb-{TmdbId}}",
            "{Title} ({Year}) {edition-{Edition}}",
            "{Title} ({Year}) {tvdb-{TvdbId}}",
            "Season {Season:00}",
            "{Title} ({Year}) - S{Season:00}E{Episode:00} - {Episode Title}",
        ],
    },
    Preset {
        id: "jellyfin",
        label: "Jellyfin",
        templates: [
            "{Title} ({Year}) [tmdbid-{TmdbId}]",
            "{Title} ({Year}) - {Edition}",
            "{Title} ({Year}) [tvdbid-{TvdbId}]",
            "Season {Season:00}",
            "{Title} S{Season:00}E{Episode:00} {Episode Title}",
        ],
    },
    Preset {
        id: "emby",
        label: "Emby",
        templates: [
            "{Title} ({Year}) [tmdbid={TmdbId}]",
            "{Title} ({Year}) - {Edition}",
            "{Title} ({Year}) [tvdbid={TvdbId}]",
            "Season {Season:00}",
            "{Title} - S{Season:00}E{Episode:00} - {Episode Title}",
        ],
    },
    Preset {
        id: "kodi",
        label: "Kodi",
        templates: [
            "{Title} ({Year})",
            "{Title} ({Year})",
            "{Title} ({Year})",
            "Season {Season:00}",
            "{Title} S{Season:00}E{Episode:00}",
        ],
    },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// The conventions of a model that names none yet: the detailed preset.
pub fn defaults() -> Vec<Convention> {
    let p = &PRESETS[0];
    NameKind::ALL
        .into_iter()
        .map(|kind| Convention {
            kind,
            template: p.template(kind).to_string(),
            preset: Some(p.id.to_string()),
            rev: 0,
        })
        .collect()
}

/// The conventions in force: those a model holds, the default for any kind it names not.
pub fn in_force(saved: &[Convention]) -> Vec<Convention> {
    in_force_with(saved, &[])
}

/// The conventions in force: those a model holds; for a kind it names not, the one `first`
/// gives (the managers' own, translated), else the default.
pub fn in_force_with(saved: &[Convention], first: &[Convention]) -> Vec<Convention> {
    defaults()
        .into_iter()
        .map(|d| {
            saved
                .iter()
                .chain(first)
                .find(|c| c.kind == d.kind)
                .cloned()
                .unwrap_or(d)
        })
        .collect()
}

/// What is known of one file and its title, for a template to write from.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Facts {
    pub title: Option<String>,
    pub year: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub tvdb_id: Option<i64>,
    pub edition: Option<String>,
    /// The quality with Proper or Repack: `Bluray-1080p Proper` (the managers' Quality Full).
    pub quality: Option<String>,
    /// The quality alone: `Bluray-1080p`.
    pub quality_title: Option<String>,
    pub resolution: Option<String>,
    pub source: Option<String>,
    pub hdr: Option<String>,
    /// Codec and channels: `EAC3 Atmos 5.1`.
    pub audio: Option<String>,
    pub audio_codec: Option<String>,
    pub audio_channels: Option<String>,
    /// `[DE+EN]`; none for English alone.
    pub audio_languages: Option<String>,
    pub codec: Option<String>,
    /// The custom formats the manager writes into names, separated by spaces.
    pub custom_formats: Option<String>,
    pub group: Option<String>,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub episode_title: Option<String>,
}

/// What a manager reports of a file it named (`/moviefile`, `/episodefile`): the quality and
/// its revision, the release group, Radarr's edition, the custom formats it writes into names,
/// and the media info as its API gives it (`videoCodec`, `videoDynamicRangeType`, `audioCodec`,
/// `audioChannels`, `audioLanguages`), which the manager names files from.
#[derive(Debug, Clone, Copy, Default)]
pub struct ManagerFile<'a> {
    pub quality: Option<&'a str>,
    /// A revision above the first.
    pub proper: bool,
    pub repack: bool,
    pub group: Option<&'a str>,
    /// Radarr's edition of the file, empty for none; Sonarr has none to give.
    pub edition: Option<&'a str>,
    pub media_info: Option<&'a Value>,
    pub custom_formats: &'a [String],
}

fn hdr_word(h: HdrFormat) -> &'static str {
    match h {
        HdrFormat::DolbyVision => "DV",
        HdrFormat::Hdr10 => "HDR10",
        HdrFormat::Hdr10Plus => "HDR10Plus",
        HdrFormat::Hlg => "HLG",
        HdrFormat::Pq => "PQ",
        HdrFormat::Hdr => "HDR",
    }
}

fn audio_word(a: AudioCodec) -> &'static str {
    match a {
        AudioCodec::TrueHd => "TrueHD",
        AudioCodec::DtsX => "DTS-X",
        AudioCodec::DtsHdMa => "DTS-HD MA",
        AudioCodec::DtsHdHra => "DTS-HD HRA",
        AudioCodec::Dts => "DTS",
        AudioCodec::Flac => "FLAC",
        AudioCodec::Pcm => "PCM",
        AudioCodec::Eac3 => "EAC3",
        AudioCodec::Ac3 => "AC3",
        AudioCodec::Aac => "AAC",
        AudioCodec::Opus => "Opus",
        AudioCodec::Mp3 => "MP3",
    }
}

/// The video codec as the managers name it when their media info does not settle it: the first
/// of their names for the codec that the release name holds, else the first.
fn codec_word(v: VideoCodec, raw: &str) -> &'static str {
    let raw = raw.to_ascii_lowercase();
    let named = |names: &[&'static str]| {
        names
            .iter()
            .find(|n| raw.contains(&n.to_ascii_lowercase()))
            .copied()
            .unwrap_or(names[0])
    };
    match v {
        VideoCodec::H264 => named(&["AVC", "x264", "h264"]),
        VideoCodec::H265 => named(&["HEVC", "x265", "h265"]),
        VideoCodec::Av1 => "AV1",
        VideoCodec::Vc1 => "VC1",
        VideoCodec::Mpeg2 => "MPEG2",
        VideoCodec::Xvid => "XviD",
        VideoCodec::Vp9 => "VP9",
    }
}

/// Codec and channels as one: `EAC3 Atmos 5.1`.
fn audio_of(codec: Option<&str>, channels: Option<&str>) -> Option<String> {
    match (codec, channels) {
        (Some(a), Some(c)) => Some(format!("{a} {c}")),
        (a, c) => a.or(c).map(str::to_string),
    }
}

/// ISO 639-2 codes, as the managers' media info has them, and the codes names carry.
const LANGUAGES: &[(&str, &str)] = &[
    ("eng", "EN"),
    ("ger", "DE"),
    ("deu", "DE"),
    ("fre", "FR"),
    ("fra", "FR"),
    ("spa", "ES"),
    ("ita", "IT"),
    ("jpn", "JA"),
    ("kor", "KO"),
    ("chi", "ZH"),
    ("zho", "ZH"),
    ("rus", "RU"),
    ("por", "PT"),
    ("dut", "NL"),
    ("nld", "NL"),
    ("swe", "SV"),
    ("nor", "NO"),
    ("nob", "NB"),
    ("dan", "DA"),
    ("fin", "FI"),
    ("pol", "PL"),
    ("hin", "HI"),
    ("ara", "AR"),
    ("tur", "TR"),
    ("heb", "HE"),
    ("cze", "CS"),
    ("ces", "CS"),
    ("hun", "HU"),
    ("gre", "EL"),
    ("ell", "EL"),
    ("tha", "TH"),
    ("vie", "VI"),
    ("ukr", "UK"),
    ("ind", "ID"),
    ("may", "MS"),
    ("msa", "MS"),
    ("rum", "RO"),
    ("ron", "RO"),
    ("tam", "TA"),
    ("tel", "TE"),
    ("ice", "IS"),
    ("isl", "IS"),
];

/// The audio languages as the managers write them into names: `[DE+EN]`; none for English
/// alone.
fn languages_word(list: &str) -> Option<String> {
    let mut codes: Vec<&str> = Vec::new();
    for item in list.split(['/', ',']) {
        let item = item.trim().to_ascii_lowercase();
        if let Some((_, code)) = LANGUAGES.iter().find(|(iso, _)| *iso == item)
            && !codes.contains(code)
        {
            codes.push(code);
        }
    }
    (!codes.is_empty() && codes != ["EN"]).then(|| format!("[{}]", codes.join("+")))
}

const ACCENTED: &str = "ÀÁÂÃÄÅÇÈÉÊËÌÍÎÏÑÒÓÔÕÖÙÚÛÜÝàáâãäåçèéêëìíîïñòóôõöùúûüýÿĀāĂăĄąĆćĈĉĊċČčĎďĒēĔĕĖėĘęĚěĜĝĞğĠġĢģĤĥĨĩĪīĬĭĮįİĴĵĶķĹĺĻļĽľŃńŅņŇňŌōŎŏŐőŔŕŖŗŘřŚśŜŝŞşŠšŢţŤťŨũŪūŬŭŮůŰűŲųŴŵŶŷŸŹźŻżŽž";
const PLAIN: &str = "AAAAAACEEEEIIIINOOOOOUUUUYaaaaaaceeeeiiiinooooouuuuyyAaAaAaCcCcCcCcDdEeEeEeEeEeGgGgGgGgHhIiIiIiIiIJjKkLlLlLlNnNnNnOoOoOoRrRrRrSsSsSsSsTtTtUuUuUuUuUuUuWwYyYZzZzZz";

/// Letters without their accents (`é` to `e`), as the managers write clean titles.
fn plain_letters(s: &str) -> String {
    s.chars()
        .map(|c| match ACCENTED.chars().position(|a| a == c) {
            Some(i) => PLAIN.chars().nth(i).unwrap_or(c),
            None => c,
        })
        .collect()
}

/// A title as the managers clean it (`{Movie CleanTitle}`, `{Series CleanTitle}`, `{Episode
/// CleanTitle}`): `&` becomes `and` and `/` a space; a sign standing alone between spaces, an
/// apostrophe, colon, question mark or comma before a space, the end or a contraction (`'s`,
/// `'t`, `'ll`), and every bracket are left out; letters lose their accents.
pub fn clean_title(title: &str) -> String {
    let title = title.replace('&', "and").replace('/', " ");
    let chars: Vec<char> = title.chars().collect();
    let space = |i: usize| chars.get(i).is_some_and(|c| c.is_whitespace());
    let contraction = |from: usize| {
        ["s", "m", "t", "ve", "ll", "d", "re"].iter().any(|w| {
            let n = w.len();
            chars.len() > from + n
                && chars[from..from + n]
                    .iter()
                    .collect::<String>()
                    .eq_ignore_ascii_case(w)
                && chars[from + n].is_whitespace()
        })
    };
    let mut out = String::with_capacity(title.len());
    for (i, &c) in chars.iter().enumerate() {
        let alone =
            i > 0 && space(i - 1) && space(i + 1) && ",<>/\\;:'\"|`\u{2019}~!?@%*-_=".contains(c);
        let ending = "'`\u{2019}:?,".contains(c)
            && (i + 1 == chars.len() || space(i + 1) || contraction(i + 1));
        if alone || ending || "()[]{}".contains(c) {
            continue;
        }
        out.push(c);
    }
    plain_letters(&out)
}

/// A title with its year, as `{Series TitleYear}` writes it: `1883 (2021)`; the year is not
/// written twice when the title already ends with it.
fn title_year(title: &str, year: Option<i64>) -> String {
    let title = title.trim();
    match year.filter(|y| *y > 0) {
        Some(y) if !title.ends_with(&format!("({y})")) => format!("{title} ({y})"),
        _ => title.to_string(),
    }
}

impl Facts {
    /// The file's own facts, as a release: quality, picture, sound, group. Where a manager's
    /// media info is wanting, the codec is named as the managers name it from a release name.
    pub fn of_release(r: &ParsedRelease) -> Facts {
        let audio_codec = r.audio_codec.map(|a| match (a, r.atmos) {
            (AudioCodec::TrueHd, true) => "TrueHD Atmos".to_string(),
            (AudioCodec::Eac3 | AudioCodec::Ac3, true) => "EAC3 Atmos".to_string(),
            _ => audio_word(a).to_string(),
        });
        let hdr: Vec<&str> = r.hdr.iter().map(|h| hdr_word(*h)).collect();
        let quality_title = r.quality().map(Quality::label);
        let revision = if r.repack {
            " Repack"
        } else if r.proper {
            " Proper"
        } else {
            ""
        };
        Facts {
            quality: quality_title.as_ref().map(|q| format!("{q}{revision}")),
            quality_title,
            resolution: r.resolution.map(|x| x.label().to_string()),
            source: r.source.map(|s| s.label().to_string()),
            hdr: (!hdr.is_empty()).then(|| hdr.join(" ")),
            audio: audio_of(audio_codec.as_deref(), r.audio_channels.as_deref()),
            audio_codec,
            audio_channels: r.audio_channels.clone(),
            codec: r.video_codec.map(|v| codec_word(v, &r.raw).to_string()),
            group: r.group.clone(),
            ..Facts::default()
        }
    }

    /// These facts as the manager that named the file has them: its quality with Proper or
    /// Repack, group, edition and named custom formats, and from its media info the codec, the
    /// dynamic range and the audio. What the manager does not report stays as it was.
    pub fn with_manager(mut self, m: &ManagerFile) -> Facts {
        fn some(s: Option<&str>) -> Option<String> {
            s.map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        }
        if let Some(q) = some(m.quality) {
            let revision = match (m.proper, m.repack) {
                (true, true) => " Repack",
                (true, false) => " Proper",
                _ => "",
            };
            self.quality = Some(format!("{q}{revision}"));
            self.quality_title = Some(q);
            self.group = some(m.group);
            if m.edition.is_some() {
                self.edition = some(m.edition);
            }
            self.custom_formats =
                (!m.custom_formats.is_empty()).then(|| m.custom_formats.join(" "));
        }
        if let Some(info) = m.media_info.filter(|v| v.is_object()) {
            let text = |key: &str| some(info.get(key).and_then(Value::as_str));
            self.codec = text("videoCodec");
            self.hdr = text("videoDynamicRangeType");
            self.audio_codec = text("audioCodec");
            self.audio_channels = info
                .get("audioChannels")
                .and_then(|v| {
                    v.as_f64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                })
                .filter(|c| *c > 0.0)
                .map(|c| format!("{c:.1}"));
            self.audio = audio_of(self.audio_codec.as_deref(), self.audio_channels.as_deref());
            self.audio_languages = text("audioLanguages").and_then(|l| languages_word(&l));
        }
        self
    }

    fn get(&self, token: &str) -> Option<String> {
        let num = |n: Option<i64>| n.map(|n| n.to_string());
        match token {
            "Title" => self.title.clone(),
            "Clean Title" => self.title.as_deref().map(clean_title),
            "Title Year" => self.title.as_deref().map(|t| title_year(t, self.year)),
            "Clean Title Year" => self
                .title
                .as_deref()
                .map(|t| clean_title(&title_year(t, self.year))),
            "Year" => num(self.year),
            "TmdbId" => num(self.tmdb_id),
            "ImdbId" => self.imdb_id.clone(),
            "TvdbId" => num(self.tvdb_id),
            "Edition" => self.edition.clone(),
            "Quality" => self.quality.clone(),
            "Quality Title" => self.quality_title.clone(),
            "Resolution" => self.resolution.clone(),
            "Source" => self.source.clone(),
            "HDR" => self.hdr.clone(),
            "Audio" => self.audio.clone(),
            "Audio Codec" => self.audio_codec.clone(),
            "Audio Channels" => self.audio_channels.clone(),
            "Audio Languages" => self.audio_languages.clone(),
            "Codec" => self.codec.clone(),
            "Custom Formats" => self.custom_formats.clone(),
            "Group" => self.group.clone(),
            "Season" => num(self.season),
            "Episode" => num(self.episode),
            "Episode Title" => self.episode_title.clone(),
            "Episode Clean Title" => self.episode_title.as_deref().map(clean_title),
            _ => None,
        }
        .filter(|v| !v.trim().is_empty())
    }
}

/// A part of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    /// A token, with the text written before and after its value when it has one.
    Token {
        name: String,
        pad: usize,
        before: String,
        after: String,
    },
    /// A bracketed part: its opening and closing characters and what is inside.
    Group(char, char, Vec<Part>),
}

/// What a token's braces may hold before its name and after it, as in the managers' formats.
const BEFORE: &[char] = &['-', ' ', '.', '_', '[', '('];
const AFTER: &[char] = &['-', ' ', '.', '_', ')', ']'];

/// A token found in a template: its length, name, padding, and the text that comes with its
/// value.
struct Found {
    len: usize,
    name: String,
    pad: usize,
    before: String,
    after: String,
}

/// A token at `i`: `{Name}`, `{Name:00}`, or with text that comes only with its value,
/// `{[Name]}`, `{-Name}`. None when the braces hold no token.
fn token_at(chars: &[char], i: usize) -> Option<Found> {
    if chars.get(i) != Some(&'{') {
        return None;
    }
    let end = chars[i + 1..].iter().position(|c| *c == '}')? + i + 1;
    let inner: String = chars[i + 1..end].iter().collect();
    let body = inner.trim_start_matches(BEFORE);
    let core = body.trim_end_matches(AFTER);
    let (name, pad) = match core.split_once(':') {
        Some((name, zeros)) if !zeros.is_empty() && zeros.chars().all(|c| c == '0') => {
            (name, zeros.len())
        }
        Some(_) => return None,
        None => (core, 0),
    };
    TOKENS.iter().any(|t| t.name == name).then(|| Found {
        len: end - i + 1,
        name: name.to_string(),
        pad,
        before: inner[..inner.len() - body.len()].to_string(),
        after: body[core.len()..].to_string(),
    })
}

/// The names written like tokens (letters and spaces between braces) that are no token.
pub fn unknown_tokens(template: &str) -> Vec<String> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some(found) = token_at(&chars, i) {
                i += found.len;
                continue;
            }
            if let Some(end) = chars[i + 1..].iter().position(|c| *c == '}' || *c == '{') {
                let inner: String = chars[i + 1..i + 1 + end].iter().collect();
                let word = inner
                    .trim_start_matches(BEFORE)
                    .trim_end_matches(AFTER)
                    .split(':')
                    .next()
                    .unwrap_or("");
                let looks = !word.is_empty()
                    && chars[i + 1 + end] == '}'
                    && word.chars().all(|c| c.is_ascii_alphabetic() || c == ' ');
                if looks && !out.iter().any(|w| w == word) {
                    out.push(word.to_string());
                }
            }
        }
        i += 1;
    }
    out
}

fn closing(open: char) -> Option<char> {
    match open {
        '[' => Some(']'),
        '(' => Some(')'),
        '{' => Some('}'),
        _ => None,
    }
}

/// The parts from `i` until the closing character `until` (or the end). Returns the parts and
/// where it stopped (after the closing character), or None when `until` never comes.
fn parse_from(chars: &[char], mut i: usize, until: Option<char>) -> Option<(Vec<Part>, usize)> {
    let mut parts: Vec<Part> = Vec::new();
    let mut text = String::new();
    let flush = |text: &mut String, parts: &mut Vec<Part>| {
        if !text.is_empty() {
            parts.push(Part::Text(std::mem::take(text)));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        if Some(c) == until {
            flush(&mut text, &mut parts);
            return Some((parts, i + 1));
        }
        if let Some(found) = token_at(chars, i) {
            flush(&mut text, &mut parts);
            i += found.len;
            parts.push(Part::Token {
                name: found.name,
                pad: found.pad,
                before: found.before,
                after: found.after,
            });
            continue;
        }
        if let Some(close) = closing(c)
            && let Some((inner, next)) = parse_from(chars, i + 1, Some(close))
        {
            flush(&mut text, &mut parts);
            parts.push(Part::Group(c, close, inner));
            i = next;
            continue;
        }
        text.push(c);
        i += 1;
    }
    flush(&mut text, &mut parts);
    until.is_none().then_some((parts, i))
}

fn parse(template: &str) -> Vec<Part> {
    let chars: Vec<char> = template.chars().collect();
    parse_from(&chars, 0, None)
        .map(|(parts, _)| parts)
        .unwrap_or_else(|| vec![Part::Text(template.to_string())])
}

/// A value as the managers write it into a name (their colon replacement "smart", and the
/// replacement of the characters a file system refuses): `: ` becomes ` - ` and `:` `-`, `/`
/// and `\` become `+`, `?` `!`, `*` `-`; `<`, `>`, `|` and `"` are left out.
fn clean(value: &str) -> String {
    let value = value.replace(": ", " - ").replace(':', "-");
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '/' | '\\' => out.push('+'),
            '?' => out.push('!'),
            '*' => out.push('-'),
            '<' | '>' | '|' | '"' => {}
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The text of some parts, and whether they held a token and all of them came out empty.
fn render_parts(parts: &[Part], facts: &Facts) -> (String, bool) {
    let mut out = String::new();
    let mut tokens = 0;
    let mut filled = 0;
    for p in parts {
        match p {
            Part::Text(t) => out.push_str(t),
            Part::Token {
                name,
                pad,
                before,
                after,
            } => {
                tokens += 1;
                if let Some(v) = facts.get(name).map(|v| clean(&v)).filter(|v| !v.is_empty()) {
                    filled += 1;
                    out.push_str(before);
                    if *pad > 0 && v.chars().all(|c| c.is_ascii_digit()) {
                        out.push_str(&format!("{v:0>pad$}", pad = *pad));
                    } else {
                        out.push_str(&v);
                    }
                    out.push_str(after);
                }
            }
            Part::Group(open, close, inner) => {
                let (text, empty) = render_parts(inner, facts);
                if !empty {
                    out.push(*open);
                    out.push_str(&text);
                    out.push(*close);
                }
            }
        }
    }
    (out, tokens > 0 && filled == 0)
}

/// The name a template gives: tokens filled in, the bracketed parts that came out empty left
/// out, spaces and dangling dashes tidied.
pub fn render(template: &str, facts: &Facts) -> String {
    let (text, _) = render_parts(&parse(template), facts);
    let mut out = text.split_whitespace().collect::<Vec<_>>().join(" ");
    while out.contains(" - - ") {
        out = out.replace(" - - ", " - ");
    }
    out.trim()
        .trim_end_matches(['-', ' '])
        .trim_start_matches(['-', ' '])
        .trim()
        .to_string()
}

/// A file or folder name without its extension: `Name (2001) [x].mkv` gives `Name (2001) [x]`.
pub fn stem(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (1..=4).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem
        }
        _ => name,
    }
}

/// Why a template cannot be saved.
pub fn check(template: &str) -> Result<(), String> {
    if template.trim().is_empty() {
        return Err("A template cannot be empty.".to_string());
    }
    if template.contains(['/', '\\']) {
        return Err("A template names one folder or file: no `/` or `\\`.".to_string());
    }
    let unknown = unknown_tokens(template);
    if !unknown.is_empty() {
        return Err(format!(
            "Not a token: {}.",
            unknown
                .iter()
                .map(|u| format!("{{{u}}}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    Ok(())
}

/// A manager's naming format in Singularr's tokens, and the manager's tokens that have none here
/// (left out of the template).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Translated {
    pub template: String,
    pub dropped: Vec<String>,
}

/// The managers' tokens (Radarr's and Sonarr's), keyed without spaces, dots, dashes or
/// underscores and in small letters, and the token each is here.
const MANAGER_TOKENS: &[(&str, &str)] = &[
    ("movietitle", "Title"),
    ("seriestitle", "Title"),
    ("moviecleantitle", "Clean Title"),
    ("seriescleantitle", "Clean Title"),
    ("movietitleyear", "Title Year"),
    ("seriestitleyear", "Title Year"),
    ("moviecleantitleyear", "Clean Title Year"),
    ("seriescleantitleyear", "Clean Title Year"),
    ("releaseyear", "Year"),
    ("seriesyear", "Year"),
    ("imdbid", "ImdbId"),
    ("tmdbid", "TmdbId"),
    ("tvdbid", "TvdbId"),
    ("editiontags", "Edition"),
    ("qualityfull", "Quality"),
    ("qualitytitle", "Quality Title"),
    ("mediainfovideodynamicrangetype", "HDR"),
    ("mediainfoaudiocodec", "Audio Codec"),
    ("mediainfoaudiochannels", "Audio Channels"),
    ("mediainfoaudiolanguages", "Audio Languages"),
    ("mediainfovideocodec", "Codec"),
    ("releasegroup", "Group"),
    ("customformats", "Custom Formats"),
    ("season", "Season"),
    ("episode", "Episode"),
    ("episodetitle", "Episode Title"),
    ("episodecleantitle", "Episode Clean Title"),
];

/// A token as the managers write it: `{` text before, the token's name, `:` and a format, text
/// after `}`; `{{` and `}}` are braces.
static MANAGER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(\{\{|\}\})|\{([- ._\[(]*)([a-z0-9]+(?:[- ._]+[a-z0-9]+)?)(?::([ ,a-z0-9+-]+))?([- ._)\]]*)\}",
    )
    .unwrap()
});

/// A manager's format (Radarr's movie and folder formats, Sonarr's episode, series and season
/// folder formats) written with Singularr's tokens: the text between tokens and the text a token
/// brings with its value are kept, a number's padding too; a token without a match here is left
/// out and named.
pub fn from_manager(format: &str) -> Translated {
    let mut template = String::new();
    let mut dropped = Vec::new();
    let mut last = 0;
    for m in MANAGER_TOKEN.captures_iter(format) {
        let Some(whole) = m.get(0) else {
            continue;
        };
        template.push_str(&format[last..whole.start()]);
        last = whole.end();
        if let Some(brace) = m.get(1) {
            template.push_str(&brace.as_str()[..1]);
            continue;
        }
        let part = |n: usize| m.get(n).map_or("", |x| x.as_str());
        let spec = part(4);
        // A format does not end with a dash or a space: those are written after the value.
        let kept = spec.trim_end_matches(['-', ' ']);
        let after = format!("{}{}", &spec[kept.len()..], part(5));
        let key = part(3)
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_ascii_lowercase();
        let Some((_, name)) = MANAGER_TOKENS.iter().find(|(k, _)| *k == key) else {
            dropped.push(whole.as_str().to_string());
            continue;
        };
        let padded = matches!(*name, "Season" | "Episode")
            && !kept.is_empty()
            && kept.chars().all(|c| c == '0');
        let pad = if padded {
            format!(":{kept}")
        } else {
            String::new()
        };
        template.push_str(&format!("{{{}{name}{pad}{after}}}", part(2)));
    }
    template.push_str(&format[last..]);
    Translated { template, dropped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::parse as parse_release;

    fn movie() -> Facts {
        Facts {
            title: Some("Harry Potter and the Prisoner of Azkaban".to_string()),
            year: Some(2004),
            tmdb_id: Some(673),
            imdb_id: Some("tt0304141".to_string()),
            ..Facts::of_release(&parse_release(
                "Harry.Potter.and.the.Prisoner.of.Azkaban.2004.1080p.WEB-DL.DDP5.1.H.265-PeruGuy",
            ))
        }
    }

    #[test]
    fn a_template_writes_the_name_and_leaves_out_what_is_empty() {
        let detailed = PRESETS[0].template(NameKind::MovieFile);
        assert_eq!(
            render(detailed, &movie()),
            "Harry Potter and the Prisoner of Azkaban (2004) {imdb-tt0304141} [WEBDL-1080p][EAC3 5.1][HEVC]-PeruGuy"
        );
        let mut open_matte = movie();
        open_matte.edition = Some("Open Matte".to_string());
        assert!(render(detailed, &open_matte).contains("{imdb-tt0304141} {edition-Open Matte} ["));
        // Nothing known but the title: every bracket and the group's dash go.
        let bare = Facts {
            title: Some("Cam".to_string()),
            ..Facts::default()
        };
        assert_eq!(render(detailed, &bare), "Cam");
        assert_eq!(
            render(PRESETS[2].template(NameKind::MovieFile), &bare),
            "Cam",
            "Jellyfin's ` - {{Edition}}` leaves no dash behind"
        );
    }

    #[test]
    fn numbers_are_padded_and_values_are_made_safe() {
        let ep = Facts {
            title: Some("Show: The Return".to_string()),
            year: Some(2022),
            season: Some(1),
            episode: Some(3),
            episode_title: Some("Who/What?".to_string()),
            ..Facts::default()
        };
        assert_eq!(
            render(PRESETS[1].template(NameKind::EpisodeFile), &ep),
            "Show - The Return (2022) - S01E03 - Who+What!"
        );
        assert_eq!(
            render(PRESETS[0].template(NameKind::SeasonFolder), &ep),
            "Season 01"
        );
    }

    #[test]
    fn unknown_tokens_are_named_and_refused() {
        assert_eq!(
            unknown_tokens("{Titel} ({Year}) {imdb-{ImdbId}}"),
            ["Titel"]
        );
        assert!(unknown_tokens(PRESETS[0].template(NameKind::EpisodeFile)).is_empty());
        assert!(check("{Titel}").unwrap_err().contains("{Titel}"));
        assert!(check("{Title}/{Year}").is_err());
        assert!(check("  ").is_err());
        for p in PRESETS {
            for kind in NameKind::ALL {
                check(p.template(kind)).unwrap_or_else(|e| panic!("{} {kind:?}: {e}", p.id));
            }
        }
    }

    #[test]
    fn a_library_name_written_by_the_detailed_preset_reads_back_the_same() {
        // A file named by the managers the detailed way matches the convention.
        let name = "10 Cloverfield Lane (2016) {imdb-tt1179933} [Bluray-1080p][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv";
        let read = crate::naming::parse_library_name(name).to_release();
        let facts = Facts {
            title: Some("10 Cloverfield Lane".to_string()),
            year: Some(2016),
            imdb_id: Some("tt1179933".to_string()),
            ..Facts::of_release(&read)
        };
        assert_eq!(
            render(PRESETS[0].template(NameKind::MovieFile), &facts),
            stem(name)
        );
        assert_eq!(stem("Season 01"), "Season 01");
        assert_eq!(stem("Movie (2001).mkv"), "Movie (2001)");
    }

    #[test]
    fn the_conventions_in_force_fill_what_a_model_names_not() {
        let saved = vec![Convention {
            kind: NameKind::SeasonFolder,
            template: "S{Season:00}".to_string(),
            preset: None,
            rev: 2,
        }];
        let all = in_force(&saved);
        assert_eq!(all.len(), 5);
        assert_eq!(all[3].template, "S{Season:00}");
        assert_eq!(all[1].preset.as_deref(), Some("detailed"));
        assert_eq!(
            NameKind::from_key("episodeFile"),
            Some(NameKind::EpisodeFile)
        );
    }

    #[test]
    fn text_in_a_token_s_braces_comes_only_with_its_value() {
        let facts = Facts {
            title: Some("Heat".to_string()),
            group: Some("SPARKS".to_string()),
            ..Facts::default()
        };
        assert_eq!(
            render("{Title}{-Group}{[Quality]}{ (Year)}", &facts),
            "Heat-SPARKS"
        );
        assert_eq!(unknown_tokens("{[Qualty]} {-Group}"), ["Qualty"]);
        assert!(unknown_tokens("{[Quality]}{ Audio Channels]}").is_empty());
    }

    #[test]
    fn titles_are_cleaned_as_the_managers_clean_them() {
        assert_eq!(clean_title("Ocean's Eleven"), "Oceans Eleven");
        assert_eq!(clean_title("Am\u{e9}lie"), "Amelie");
        assert_eq!(clean_title("Behind Us, a Cliff"), "Behind Us a Cliff");
        assert_eq!(clean_title("Fast & Furious"), "Fast and Furious");
        assert_eq!(clean_title("What If...?"), "What If...");
        assert_eq!(
            clean_title("Birds of Prey (and the Fantabulous Emancipation of One Harley Quinn)"),
            "Birds of Prey and the Fantabulous Emancipation of One Harley Quinn"
        );
        let mi = Facts {
            title: Some("Mission: Impossible - Dead Reckoning Part One".to_string()),
            year: Some(2023),
            ..Facts::default()
        };
        assert_eq!(
            render("{Clean Title}", &mi),
            "Mission Impossible Dead Reckoning Part One"
        );
        assert_eq!(
            render("{Title}", &mi),
            "Mission - Impossible - Dead Reckoning Part One"
        );
        assert_eq!(
            render("{Title Year}", &mi),
            "Mission - Impossible - Dead Reckoning Part One (2023)"
        );
    }

    #[test]
    fn a_manager_s_format_becomes_tokens() {
        let radarr = from_manager(
            "{Movie CleanTitle} {(Release Year)} {imdb-{ImdbId}} {edition-{Edition Tags}} {[Custom Formats]}{[Quality Full]}{[MediaInfo 3D]}{[MediaInfo VideoDynamicRangeType]}{[Mediainfo AudioCodec}{ Mediainfo AudioChannels]}{[MediaInfo VideoCodec]}{-Release Group}",
        );
        assert_eq!(
            radarr.template,
            "{Clean Title} {(Year)} {imdb-{ImdbId}} {edition-{Edition}} {[Custom Formats]}{[Quality]}{[HDR]}{[Audio Codec}{ Audio Channels]}{[Codec]}{-Group}"
        );
        assert_eq!(radarr.dropped, ["{[MediaInfo 3D]}"]);
        let sonarr = from_manager(
            "{Series TitleYear} - S{season:00}E{episode:00} - {Episode CleanTitle} [{Custom Formats }{Quality Full}]{MediaInfo AudioLanguages}{-Release Group}",
        );
        assert_eq!(
            sonarr.template,
            "{Title Year} - S{Season:00}E{Episode:00} - {Episode Clean Title} [{Custom Formats }{Quality}]{Audio Languages}{-Group}"
        );
        assert!(sonarr.dropped.is_empty());
        assert_eq!(
            from_manager("Season {season:00}").template,
            "Season {Season:00}"
        );
        check(&radarr.template).unwrap();
        check(&sonarr.template).unwrap();
    }

    #[test]
    fn a_name_is_written_from_the_manager_s_facts() {
        let info = serde_json::json!({
            "videoCodec": "HEVC", "videoDynamicRangeType": "DV HDR10",
            "audioCodec": "EAC3 Atmos", "audioChannels": 5.1, "audioLanguages": "eng"
        });
        let formats = vec!["IMAX".to_string()];
        let facts = Facts {
            title: Some("Dune".to_string()),
            ..Facts::of_release(&parse_release("Dune.2021.1080p.BluRay.DD5.1.x264-GRP"))
        }
        .with_manager(&ManagerFile {
            quality: Some("Bluray-1080p"),
            proper: true,
            repack: false,
            group: Some("FLUX"),
            edition: Some(""),
            media_info: Some(&info),
            custom_formats: &formats,
        });
        assert_eq!(
            render(
                "{[Custom Formats]}{[Quality]}{[HDR]}{[Audio]}{[Codec]}{-Group}{Audio Languages}",
                &facts
            ),
            "[IMAX][Bluray-1080p Proper][DV HDR10][EAC3 Atmos 5.1][HEVC]-FLUX"
        );
        let repack = facts.clone().with_manager(&ManagerFile {
            quality: Some("WEBDL-2160p"),
            proper: true,
            repack: true,
            ..ManagerFile::default()
        });
        assert_eq!(repack.quality.as_deref(), Some("WEBDL-2160p Repack"));
        let stereo = serde_json::json!({"audioCodec": "AAC", "audioChannels": 2, "audioLanguages": "ger/eng"});
        let two = Facts::default().with_manager(&ManagerFile {
            quality: Some("HDTV-720p"),
            media_info: Some(&stereo),
            ..ManagerFile::default()
        });
        assert_eq!(
            (two.audio.as_deref(), two.audio_languages.as_deref()),
            (Some("AAC 2.0"), Some("[DE+EN]"))
        );
    }
}
