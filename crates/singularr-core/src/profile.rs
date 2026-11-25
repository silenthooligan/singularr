//! Quality profiles, lists and advanced rules as a person edits them.
//!
//! A quality profile answers a few questions: the target (kind, resolution, the sources in order
//! with the Accept from and Stop at markers), the picture, the sound, the release groups and
//! services (lists, ranked), size and upgrades. Everything else a release can be is under More,
//! each option with one of four stances. Lists of release groups and streaming services are
//! kept once and ranked by every profile.
//!
//! [`compile`] turns the model into the engine's [`Policy`], so scoring, verdict traces and the
//! shadow comparison stay as they are. [`import`] reads a policy file into the model and
//! [`export`] writes the model back in the file form. [`explain`] says a verdict's trace in the
//! model's words.

use crate::policy::{Condition, Protection, TraceItem};
use crate::release::{AudioCodec, HdrFormat, MediaKind, Resolution, Source, VideoCodec};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

mod compile;
mod import;

pub use compile::{
    BLOCK, FLOOR, UNIT, UNLISTED, compile, export, list_trait, option_label, option_trait,
    rule_trait,
};
pub use import::{ImportMode, RankMove, RankReport, guess_kind, import, import_as, rank_changes};

/// What a profile thinks of one option, one list or one rule.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Stance {
    /// Ranks a release higher; earlier in its group is stronger.
    Prefer,
    /// Neither raises nor lowers a release.
    #[default]
    Allow,
    /// Ranks a release lower.
    Avoid,
    /// Never takes the release.
    Block,
}

impl Stance {
    /// The stance from imported points: above zero prefers, zero allows, down to -10000 avoids,
    /// lower blocks.
    pub fn of_points(points: i32) -> Stance {
        match points {
            p if p > 0 => Stance::Prefer,
            0 => Stance::Allow,
            p if p <= -10_000 => Stance::Block,
            _ => Stance::Avoid,
        }
    }

    /// The word a reason uses: `preferred`, `avoided`, `blocked`; empty for Allow.
    pub fn word(self) -> &'static str {
        match self {
            Stance::Prefer => "preferred",
            Stance::Allow => "",
            Stance::Avoid => "avoided",
            Stance::Block => "blocked",
        }
    }
}

/// Movies or shows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleKind {
    #[default]
    Movie,
    Show,
}

impl TitleKind {
    /// The release kind a profile of this kind sizes: a movie, or one episode.
    pub fn media(self) -> MediaKind {
        match self {
            TitleKind::Movie => MediaKind::Movie,
            TitleKind::Show => MediaKind::Episode,
        }
    }

    /// Hours of runtime a size per hour is read against: a movie of 2 h, an episode of 1 h.
    pub fn hours(self) -> f64 {
        match self {
            TitleKind::Movie => 2.0,
            TitleKind::Show => 1.0,
        }
    }
}

/// What a list holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ListKind {
    /// Release groups. A member is the group a release names, or the collective it releases
    /// under.
    #[default]
    Group,
    /// Streaming services, as release names tag them: `AMZN`, `ATVP`.
    Service,
}

impl ListKind {
    /// The answer a list of this kind belongs to, as a reason names it.
    pub fn answer(self) -> &'static str {
        match self {
            ListKind::Group => "Release groups",
            ListKind::Service => "Streaming services",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            ListKind::Group => "group",
            ListKind::Service => "service",
        }
    }
}

/// A named set of release groups or streaming services that are treated alike.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct List {
    pub id: String,
    pub name: String,
    pub kind: ListKind,
    pub members: Vec<String>,
    /// Only releases that also match this belong to the list: `source = ["Remux"]` for a list of
    /// remux groups. It never names members itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<Condition>,
    /// One more on every change.
    pub rev: u32,
}

impl List {
    /// The condition the list's trait matches with.
    pub fn condition(&self) -> Condition {
        let scope = self.only.clone().unwrap_or_default();
        match self.kind {
            ListKind::Group => Condition {
                any: vec![
                    Condition {
                        group: self.members.clone(),
                        ..Condition::default()
                    },
                    Condition {
                        collective: self.members.clone(),
                        ..Condition::default()
                    },
                ],
                ..scope
            },
            ListKind::Service => Condition {
                service: self.members.clone(),
                ..scope
            },
        }
    }

    /// Whether a name is a member, case-insensitive.
    pub fn has(&self, name: &str) -> bool {
        self.members.iter().any(|m| m.eq_ignore_ascii_case(name))
    }
}

/// A condition the questions cannot say, written in the engine's condition language. A
/// profile gives it a stance like any option.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub when: Condition,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub why: String,
    /// Where the rule sits when it is drawn: `group`, `video`, `audio`, `edition`, `penalty`,
    /// `other`.
    pub category: String,
    pub rev: u32,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One answer: an option, a list or a rule, with its stance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pick {
    pub key: String,
    #[serde(default)]
    pub stance: Stance,
    /// A preferred option a file needs before Stop at makes it final.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// Exact points in place of the ones the position gives. An imported profile keeps the
    /// file's numbers here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub points: Option<i32>,
}

impl Pick {
    pub fn new(key: &str, stance: Stance) -> Pick {
        Pick {
            key: key.to_string(),
            stance,
            required: false,
            points: None,
        }
    }
}

/// The first answer: what kind of title, at which resolution, from which sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Target {
    pub kind: TitleKind,
    pub resolution: Resolution,
    /// The source strip, best first. Sources in one step rank the same.
    pub sources: Vec<Vec<Source>>,
    /// Nothing below the step that holds this source is taken.
    pub accept_from: Source,
    /// A file at or above the step that holds this source, that also has the required
    /// preferences, is final. None (written `null`, never left out, since a new profile stops
    /// at Blu-ray): upgrades go on to the top of the strip.
    pub stop_at: Option<Source>,
}

/// The sources a strip can hold, in the order a new profile has them.
pub const STRIP: [Source; 5] = [
    Source::Remux,
    Source::BluRay,
    Source::WebDl,
    Source::WebRip,
    Source::Tv,
];

impl Default for Target {
    fn default() -> Target {
        Target {
            kind: TitleKind::Movie,
            resolution: Resolution::R1080,
            sources: STRIP.iter().map(|s| vec![*s]).collect(),
            accept_from: Source::WebDl,
            stop_at: Some(Source::BluRay),
        }
    }
}

impl Target {
    /// The step that holds a source.
    pub fn step_of(&self, source: Source) -> Option<usize> {
        self.sources.iter().position(|step| step.contains(&source))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Picture {
    /// Dolby Vision, HDR10+, HDR10, HLG, SDR.
    pub range: Vec<Pick>,
    /// HEVC, AV1, H.264.
    pub codec: Vec<Pick>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sound {
    pub formats: Vec<Pick>,
    pub channels: Vec<Pick>,
    /// The original language, and the others.
    pub languages: Vec<Pick>,
}

/// Whether a release from a lower list can rank above one from the list above it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Crossing {
    /// Never: the lists decide first.
    #[default]
    Never,
    /// When it is clearly better in picture and sound.
    ClearlyBetter,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Lists {
    /// Every list, ranked: the preferred ones best first, then the allowed, avoided and blocked.
    pub ranked: Vec<Pick>,
    /// Releases from no list of release groups. They rank between the last preferred list and
    /// the first avoided one when allowed.
    pub unlisted: Stance,
    pub crossing: Crossing,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Size {
    /// Gigabytes per hour of runtime. A smaller file is avoided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_per_hour: Option<f64>,
    /// A larger file is avoided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_per_hour: Option<f64>,
    /// Between two releases that rank the same, the smaller wins.
    pub smaller_wins_tie: bool,
}

/// When a better release replaces the file the library has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpgradeWhen {
    /// Any better release.
    #[default]
    Any,
    /// Only one that is better by at least the strongest single preference.
    ClearlyBetter,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Upgrades {
    pub when: UpgradeWhen,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct More {
    /// Repack, AI upscale, cam, hardcoded subtitles, 3D, sample, program files.
    pub release: Vec<Pick>,
    /// Editions such as IMAX or Open Matte.
    pub editions: Vec<Pick>,
    /// The advanced rules, by rule id.
    pub rules: Vec<Pick>,
}

/// The numbers a profile can fix in place of the ones its answers give. An imported profile
/// keeps the file's; a new one has none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Scores {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_step: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff: Option<i32>,
    /// A quality ladder the target cannot say (several resolutions), kept as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ladder: Option<Vec<Vec<String>>>,
    /// The cutoff quality that goes with `ladder`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutoff_quality: Option<String>,
}

impl Scores {
    pub fn is_empty(&self) -> bool {
        *self == Scores::default()
    }
}

/// A quality profile: the five answers, More, and the numbers it fixes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub id: String,
    pub name: String,
    pub rev: u32,
    /// The managers' quality profiles it stands for, written `radarr:HD-1080p`. A title the
    /// manager places in one of them is in this profile.
    pub manager_profiles: Vec<String>,
    pub target: Target,
    pub picture: Picture,
    pub sound: Sound,
    pub lists: Lists,
    pub size: Size,
    pub upgrades: Upgrades,
    pub more: More,
    #[serde(skip_serializing_if = "Scores::is_empty")]
    pub scores: Scores,
}

/// The groups of built-in options, in the order a profile shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Group {
    Range,
    Codec,
    Formats,
    Channels,
    Languages,
    Release,
    Editions,
}

impl Group {
    pub const ALL: [Group; 7] = [
        Group::Range,
        Group::Codec,
        Group::Formats,
        Group::Channels,
        Group::Languages,
        Group::Release,
        Group::Editions,
    ];

    /// The first part of the trait id of an option: `range.dv`.
    pub fn prefix(self) -> &'static str {
        match self {
            Group::Range => "range",
            Group::Codec => "codec",
            Group::Formats => "format",
            Group::Channels => "channels",
            Group::Languages => "language",
            Group::Release => "release",
            Group::Editions => "edition",
        }
    }

    /// The answer the group belongs to: `Picture`, `Sound`, `More`.
    pub fn answer(self) -> &'static str {
        match self {
            Group::Range | Group::Codec => "Picture",
            Group::Formats | Group::Channels | Group::Languages => "Sound",
            Group::Release | Group::Editions => "More",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Group::Range => "Dynamic range",
            Group::Codec => "Codec",
            Group::Formats => "Audio format",
            Group::Channels => "Channels",
            Group::Languages => "Language",
            Group::Release => "Release",
            Group::Editions => "Edition",
        }
    }

    pub fn category(self) -> &'static str {
        match self {
            Group::Range => "hdr",
            Group::Codec => "video",
            Group::Formats | Group::Channels => "audio",
            Group::Languages => "language",
            Group::Release => "penalty",
            Group::Editions => "edition",
        }
    }

    /// Whether the group counts toward what a final file has, when no option is required.
    fn ideal(self) -> bool {
        !matches!(self, Group::Release | Group::Editions)
    }

    pub fn by_prefix(prefix: &str) -> Option<Group> {
        Group::ALL.into_iter().find(|g| g.prefix() == prefix)
    }
}

/// One built-in option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Choice {
    pub group: Group,
    pub key: &'static str,
    pub label: &'static str,
}

const fn choice(group: Group, key: &'static str, label: &'static str) -> Choice {
    Choice { group, key, label }
}

/// Every built-in option, by group, in the order a new profile lists them.
pub const CHOICES: &[Choice] = &[
    choice(Group::Range, "dv", "Dolby Vision"),
    choice(Group::Range, "hdr10plus", "HDR10+"),
    choice(Group::Range, "hdr10", "HDR10"),
    choice(Group::Range, "hlg", "HLG"),
    choice(Group::Range, "sdr", "SDR"),
    choice(Group::Codec, "hevc", "HEVC"),
    choice(Group::Codec, "av1", "AV1"),
    choice(Group::Codec, "h264", "H.264"),
    choice(Group::Formats, "atmos", "Atmos"),
    choice(Group::Formats, "truehd", "TrueHD"),
    choice(Group::Formats, "dtsx", "DTS:X"),
    choice(Group::Formats, "dtshdma", "DTS-HD MA"),
    choice(Group::Formats, "dtshdhra", "DTS-HD HRA"),
    choice(Group::Formats, "dts", "DTS"),
    choice(Group::Formats, "eac3", "E-AC-3"),
    choice(Group::Formats, "ac3", "AC-3"),
    choice(Group::Formats, "aac", "AAC"),
    choice(Group::Formats, "opus", "Opus"),
    choice(Group::Formats, "flac", "FLAC"),
    choice(Group::Formats, "pcm", "PCM"),
    choice(Group::Channels, "7.1", "7.1"),
    choice(Group::Channels, "6.1", "6.1"),
    choice(Group::Channels, "5.1", "5.1"),
    choice(Group::Channels, "2.0", "Stereo"),
    choice(Group::Channels, "1.0", "Mono"),
    choice(Group::Languages, "original", "Original language"),
    choice(Group::Languages, "other", "Other languages"),
    choice(Group::Release, "repack", "Repack or proper"),
    choice(Group::Release, "upscaled", "AI upscale"),
    choice(Group::Release, "prerelease", "Cam or screener"),
    choice(Group::Release, "hardsub", "Hardcoded subtitles"),
    choice(Group::Release, "3d", "3D"),
    choice(Group::Release, "sample", "Sample"),
    choice(Group::Release, "program", "Program file"),
    choice(Group::Editions, "imax", "IMAX"),
    choice(Group::Editions, "imax-enhanced", "IMAX Enhanced"),
    choice(Group::Editions, "open-matte", "Open Matte"),
    choice(Group::Editions, "extended", "Extended"),
    choice(Group::Editions, "directors-cut", "Director's Cut"),
    choice(Group::Editions, "remastered", "Remastered"),
    choice(Group::Editions, "criterion", "Criterion"),
];

/// The built-in options of one group.
pub fn choices(group: Group) -> impl Iterator<Item = &'static Choice> {
    CHOICES.iter().filter(move |c| c.group == group)
}

/// One built-in option.
pub fn choice_of(group: Group, key: &str) -> Option<&'static Choice> {
    choices(group).find(|c| c.key == key)
}

/// The condition of a built-in option.
pub fn condition_of(group: Group, key: &str) -> Option<Condition> {
    let c = Condition::default();
    let audio = |codec: AudioCodec| Condition {
        audio_codec: vec![codec],
        ..Condition::default()
    };
    let channels = |list: &[&str]| Condition {
        channels: list.iter().map(|x| x.to_string()).collect(),
        ..Condition::default()
    };
    let name = |pattern: &str| Condition {
        name: Some(pattern.to_string()),
        ..Condition::default()
    };
    Some(match (group, key) {
        (Group::Range, "dv") => Condition {
            hdr: vec![HdrFormat::DolbyVision],
            ..c
        },
        (Group::Range, "hdr10plus") => Condition {
            hdr: vec![HdrFormat::Hdr10Plus],
            ..c
        },
        (Group::Range, "hdr10") => Condition {
            hdr: vec![HdrFormat::Hdr10, HdrFormat::Hdr, HdrFormat::Pq],
            ..c
        },
        (Group::Range, "hlg") => Condition {
            hdr: vec![HdrFormat::Hlg],
            ..c
        },
        (Group::Range, "sdr") => Condition {
            sdr: Some(true),
            ..c
        },
        (Group::Codec, "hevc") => Condition {
            video_codec: vec![VideoCodec::H265],
            ..c
        },
        (Group::Codec, "av1") => Condition {
            video_codec: vec![VideoCodec::Av1],
            ..c
        },
        (Group::Codec, "h264") => Condition {
            video_codec: vec![VideoCodec::H264],
            ..c
        },
        (Group::Formats, "atmos") => Condition {
            atmos: Some(true),
            ..c
        },
        (Group::Formats, "truehd") => audio(AudioCodec::TrueHd),
        (Group::Formats, "dtsx") => audio(AudioCodec::DtsX),
        (Group::Formats, "dtshdma") => audio(AudioCodec::DtsHdMa),
        (Group::Formats, "dtshdhra") => audio(AudioCodec::DtsHdHra),
        (Group::Formats, "dts") => audio(AudioCodec::Dts),
        (Group::Formats, "eac3") => audio(AudioCodec::Eac3),
        (Group::Formats, "ac3") => audio(AudioCodec::Ac3),
        (Group::Formats, "aac") => audio(AudioCodec::Aac),
        (Group::Formats, "opus") => audio(AudioCodec::Opus),
        (Group::Formats, "flac") => audio(AudioCodec::Flac),
        (Group::Formats, "pcm") => audio(AudioCodec::Pcm),
        (Group::Channels, "7.1") => channels(&["7.1", "7.0", "8.0", "8.1"]),
        (Group::Channels, "6.1") => channels(&["6.1", "6.0"]),
        (Group::Channels, "5.1") => channels(&["5.1", "5.0"]),
        (Group::Channels, "2.0") => channels(&["2.0", "2.1"]),
        (Group::Channels, "1.0") => channels(&["1.0"]),
        (Group::Languages, "original") => Condition {
            original_language: Some(true),
            ..c
        },
        (Group::Languages, "other") => Condition {
            original_language: Some(false),
            ..c
        },
        (Group::Release, "repack") => Condition {
            proper_or_repack: Some(true),
            ..c
        },
        (Group::Release, "upscaled") => Condition {
            ai_upscaled: Some(true),
            ..c
        },
        (Group::Release, "prerelease") => Condition {
            prerelease: Some(true),
            ..c
        },
        (Group::Release, "hardsub") => name(r"(?i)\b(HC|hardsubs?|hardcoded[ ._-]?subs?)\b"),
        (Group::Release, "3d") => name(r"(?i)\b(3D|SBS|HSBS|Half[ ._-]?OU)\b"),
        (Group::Release, "sample") => name(r"(?i)\bsample\b"),
        (Group::Release, "program") => name(r"(?i)\.(exe|lnk|scr|bat|cmd|msi|vbs)$"),
        (Group::Editions, key) => Condition {
            edition: vec![choice_of(Group::Editions, key)?.label.to_string()],
            ..c
        },
        _ => return None,
    })
}

/// The stance a new profile gives an option.
pub fn default_stance(group: Group, key: &str, target: &Target) -> Stance {
    match (group, key) {
        (Group::Range, "dv" | "hdr10plus" | "hdr10") => Stance::Prefer,
        (Group::Range, "sdr") if target.resolution == Resolution::R2160 => Stance::Avoid,
        (Group::Codec, "hevc") => Stance::Prefer,
        (Group::Formats, "atmos" | "truehd" | "dtsx" | "dtshdma") => Stance::Prefer,
        (Group::Channels, "7.1" | "5.1") => Stance::Prefer,
        (Group::Languages, "original") => Stance::Prefer,
        (Group::Languages, "other") => Stance::Avoid,
        (Group::Release, "repack") => Stance::Prefer,
        (Group::Release, "hardsub") => Stance::Avoid,
        (Group::Release, "upscaled" | "prerelease" | "3d" | "sample" | "program") => Stance::Block,
        _ => Stance::Allow,
    }
}

/// The product's words for a source: `Blu-ray`, `WEB-DL`.
pub fn source_label(source: Source) -> &'static str {
    match source {
        Source::Remux => "Remux",
        Source::BluRay => "Blu-ray",
        Source::WebDl => "WEB-DL",
        Source::WebRip => "WEBRip",
        Source::Tv => "HDTV",
        Source::Dvd => "DVD",
        Source::Disc => "Disc",
        Source::Workprint => "Workprint",
        Source::Cam => "Cam",
        Source::Telesync => "Telesync",
        Source::Telecine => "Telecine",
        Source::Screener => "Screener",
    }
}

/// How files and folders are named: the kinds of things a convention names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NameKind {
    MovieFolder,
    MovieFile,
    SeriesFolder,
    SeasonFolder,
    EpisodeFile,
}

/// One naming convention: a template for one kind of name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Convention {
    pub kind: NameKind,
    pub template: String,
    /// The preset it was taken from, while it is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default)]
    pub rev: u32,
}

/// Everything a person edits: lists, advanced rules, profiles, the protections the policy
/// keeps, and the naming conventions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Model {
    pub name: String,
    pub lists: Vec<List>,
    pub rules: Vec<Rule>,
    pub profiles: Vec<Profile>,
    /// What may replace the file the library has, kept as the policy writes it.
    pub protections: Vec<Protection>,
    /// The managers' custom formats a compiled trait stands for, by trait id: kept from an
    /// import, for the shadow comparison. Scoring ignores it.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub formats: BTreeMap<String, Vec<String>>,
    pub naming: Vec<Convention>,
}

impl Default for Model {
    fn default() -> Model {
        Model {
            name: "singularr".to_string(),
            lists: Vec::new(),
            rules: Vec::new(),
            profiles: Vec::new(),
            protections: Vec::new(),
            formats: BTreeMap::new(),
            naming: Vec::new(),
        }
    }
}

/// Why a model cannot be saved or compiled.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    /// A field is wrong; the text names it.
    #[error("{0}")]
    Invalid(String),
    /// The compiled policy does not load. A pattern that does not compile, for example.
    #[error("the compiled policy does not load: {0}")]
    Policy(String),
}

fn bad(why: impl Into<String>) -> ModelError {
    ModelError::Invalid(why.into())
}

/// A lower-case id from a name: `Remux, tier 1` gives `remux-tier-1`.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c.to_ascii_lowercase());
        } else {
            gap = true;
        }
    }
    out.truncate(48);
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() {
        "item".to_string()
    } else {
        out
    }
}

/// `base`, or `base-2`, `base-3`... whichever is not taken.
pub fn unique_id(base: &str, taken: &BTreeSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken.contains(id))
        .unwrap_or_else(|| base.to_string())
}

fn good_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Gigabytes for a summary: `4`, `2.5`.
fn gb(n: f64) -> String {
    let text = format!("{n:.1}");
    text.trim_end_matches(".0").to_string()
}

impl Profile {
    /// A new profile with the default answers for its target.
    pub fn new(id: &str, name: &str, target: Target) -> Profile {
        let mut p = Profile {
            id: id.to_string(),
            name: name.to_string(),
            target,
            ..Profile::default()
        };
        p.fill(&Model::default());
        p
    }

    /// The picks of one group of built-in options.
    pub fn picks(&self, group: Group) -> &[Pick] {
        match group {
            Group::Range => &self.picture.range,
            Group::Codec => &self.picture.codec,
            Group::Formats => &self.sound.formats,
            Group::Channels => &self.sound.channels,
            Group::Languages => &self.sound.languages,
            Group::Release => &self.more.release,
            Group::Editions => &self.more.editions,
        }
    }

    pub fn picks_mut(&mut self, group: Group) -> &mut Vec<Pick> {
        match group {
            Group::Range => &mut self.picture.range,
            Group::Codec => &mut self.picture.codec,
            Group::Formats => &mut self.sound.formats,
            Group::Channels => &mut self.sound.channels,
            Group::Languages => &mut self.sound.languages,
            Group::Release => &mut self.more.release,
            Group::Editions => &mut self.more.editions,
        }
    }

    /// Add what the profile does not answer yet: every built-in option with its default
    /// stance, every list and rule of the model as allowed. Picks of lists and rules the model
    /// no longer has are dropped.
    pub fn fill(&mut self, model: &Model) {
        let target = self.target.clone();
        for group in Group::ALL {
            let picks = self.picks_mut(group);
            for c in choices(group) {
                if !picks.iter().any(|p| p.key == c.key) {
                    picks.push(Pick::new(c.key, default_stance(group, c.key, &target)));
                }
            }
        }
        let lists: Vec<&str> = model.lists.iter().map(|l| l.id.as_str()).collect();
        self.lists
            .ranked
            .retain(|p| lists.contains(&p.key.as_str()));
        for id in &lists {
            if !self.lists.ranked.iter().any(|p| p.key == *id) {
                self.lists.ranked.push(Pick::new(id, Stance::Allow));
            }
        }
        let rules: Vec<&str> = model.rules.iter().map(|r| r.id.as_str()).collect();
        self.more.rules.retain(|p| rules.contains(&p.key.as_str()));
        for id in &rules {
            if !self.more.rules.iter().any(|p| p.key == *id) {
                self.more.rules.push(Pick::new(id, Stance::Allow));
            }
        }
    }

    /// The one-line summary a profile's card shows: `1080p, Remux to WEB-DL, prefers Dolby
    /// Vision and Atmos, Remux, tier 1 first, 4 to 30 GB per hour`.
    pub fn summary(&self, model: &Model) -> String {
        let t = &self.target;
        let first = t.sources.first().and_then(|s| s.first()).copied();
        let sources = first.map(|f| {
            if f == t.accept_from {
                format!("{} only", source_label(f))
            } else {
                format!("{} to {}", source_label(f), source_label(t.accept_from))
            }
        });
        let strongest = |group: Group| {
            self.picks(group)
                .iter()
                .find(|p| p.stance == Stance::Prefer)
                .and_then(|p| choice_of(group, &p.key))
                .map(|c| c.label)
        };
        let liked: Vec<&str> = [strongest(Group::Range), strongest(Group::Formats)]
            .into_iter()
            .flatten()
            .collect();
        let prefers = (!liked.is_empty()).then(|| format!("prefers {}", liked.join(" and ")));
        let top = self
            .lists
            .ranked
            .iter()
            .filter(|p| p.stance == Stance::Prefer)
            .find_map(|p| model.lists.iter().find(|l| l.id == p.key))
            .map(|l| format!("{} first", l.name));
        let size = match (self.size.min_per_hour, self.size.max_per_hour) {
            (Some(a), Some(b)) => Some(format!("{} to {} GB per hour", gb(a), gb(b))),
            (Some(a), None) => Some(format!("at least {} GB per hour", gb(a))),
            (None, Some(b)) => Some(format!("at most {} GB per hour", gb(b))),
            (None, None) => None,
        };
        [
            Some(t.resolution.label().to_string()),
            sources,
            prefers,
            top,
            size,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ")
    }

    fn check(&self, model: &Model) -> Result<(), ModelError> {
        let who = format!("quality profile `{}`", self.id);
        if !good_id(&self.id) {
            return Err(bad(format!(
                "`{}` is not an id: letters, digits, dots, dashes, at most 64",
                self.id
            )));
        }
        if self.name.trim().is_empty() {
            return Err(bad(format!("{who} has no name")));
        }
        let t = &self.target;
        let mut seen = BTreeSet::new();
        for step in &t.sources {
            if step.is_empty() {
                return Err(bad(format!("{who}: a step of the source strip is empty")));
            }
            for s in step {
                if !STRIP.contains(s) {
                    return Err(bad(format!(
                        "{who}: {} cannot be on the source strip",
                        source_label(*s)
                    )));
                }
                if !seen.insert(*s) {
                    return Err(bad(format!(
                        "{who}: {} is twice on the source strip",
                        source_label(*s)
                    )));
                }
            }
        }
        let accept = t.step_of(t.accept_from).ok_or_else(|| {
            bad(format!(
                "{who}: Accept from is {}, which is not on the source strip",
                source_label(t.accept_from)
            ))
        })?;
        if let Some(stop) = t.stop_at {
            let at = t.step_of(stop).ok_or_else(|| {
                bad(format!(
                    "{who}: Stop at is {}, which is not on the source strip",
                    source_label(stop)
                ))
            })?;
            if at > accept {
                return Err(bad(format!(
                    "{who}: Stop at ({}) is below Accept from ({})",
                    source_label(stop),
                    source_label(t.accept_from)
                )));
            }
        }
        for group in Group::ALL {
            let mut keys = BTreeSet::new();
            for p in self.picks(group) {
                if choice_of(group, &p.key).is_none() {
                    return Err(bad(format!(
                        "{who}: `{}` is not an option of {}",
                        p.key,
                        group.label().to_lowercase()
                    )));
                }
                if !keys.insert(p.key.as_str()) {
                    return Err(bad(format!("{who}: `{}` is listed twice", p.key)));
                }
            }
        }
        let mut keys = BTreeSet::new();
        for p in &self.lists.ranked {
            if !model.lists.iter().any(|l| l.id == p.key) {
                return Err(bad(format!("{who}: there is no list `{}`", p.key)));
            }
            if !keys.insert(p.key.as_str()) {
                return Err(bad(format!("{who}: the list `{}` is ranked twice", p.key)));
            }
        }
        let mut keys = BTreeSet::new();
        for p in &self.more.rules {
            if !model.rules.iter().any(|r| r.id == p.key) {
                return Err(bad(format!("{who}: there is no rule `{}`", p.key)));
            }
            if !keys.insert(p.key.as_str()) {
                return Err(bad(format!("{who}: the rule `{}` is listed twice", p.key)));
            }
        }
        if self.lists.unlisted == Stance::Prefer {
            return Err(bad(format!(
                "{who}: releases from no list can be allowed, avoided or blocked, not preferred"
            )));
        }
        let s = &self.size;
        for (name, v) in [("smallest", s.min_per_hour), ("largest", s.max_per_hour)] {
            if let Some(v) = v
                && (!v.is_finite() || !(0.0..=1000.0).contains(&v))
            {
                return Err(bad(format!(
                    "{who}: the {name} size per hour is not between 0 and 1000 GB"
                )));
            }
        }
        if let (Some(a), Some(b)) = (s.min_per_hour, s.max_per_hour)
            && a > b
        {
            return Err(bad(format!(
                "{who}: the smallest size per hour is above the largest"
            )));
        }
        for m in &self.manager_profiles {
            if !m.contains(':') {
                return Err(bad(format!(
                    "{who}: a manager profile is written `<integration>:<profile>`, not `{m}`"
                )));
            }
        }
        Ok(())
    }
}

impl Model {
    /// Fill every profile's missing answers and drop picks of lists and rules that are gone.
    pub fn fill(&mut self) {
        let snapshot = Model {
            profiles: Vec::new(),
            ..self.clone()
        };
        for p in &mut self.profiles {
            p.fill(&snapshot);
        }
    }

    /// Whether the model can be compiled: ids, names, the source strips, the picks.
    pub fn check(&self) -> Result<(), ModelError> {
        let mut ids = BTreeSet::new();
        for l in &self.lists {
            if !good_id(&l.id) || l.id == "none" {
                return Err(bad(format!("`{}` is not a list id", l.id)));
            }
            if !ids.insert(l.id.as_str()) {
                return Err(bad(format!("there are two lists `{}`", l.id)));
            }
            if l.name.trim().is_empty() {
                return Err(bad(format!("the list `{}` has no name", l.id)));
            }
            let mut names = BTreeSet::new();
            for m in &l.members {
                if m.trim().is_empty() || m.trim() != m {
                    return Err(bad(format!(
                        "the list `{}` has a member that is empty or has spaces around it",
                        l.id
                    )));
                }
                if !names.insert(m.to_lowercase()) {
                    return Err(bad(format!("the list `{}` has `{m}` twice", l.id)));
                }
            }
            if let Some(only) = &l.only
                && (!only.group.is_empty()
                    || !only.collective.is_empty()
                    || !only.service.is_empty()
                    || !only.any.is_empty())
            {
                return Err(bad(format!(
                    "the list `{}` names members in `only`; members belong in `members`",
                    l.id
                )));
            }
        }
        let mut ids = BTreeSet::new();
        for r in &self.rules {
            if !good_id(&r.id) {
                return Err(bad(format!("`{}` is not a rule id", r.id)));
            }
            if !ids.insert(r.id.as_str()) {
                return Err(bad(format!("there are two rules `{}`", r.id)));
            }
            if r.name.trim().is_empty() {
                return Err(bad(format!("the rule `{}` has no name", r.id)));
            }
        }
        let mut ids = BTreeSet::new();
        for p in &self.profiles {
            if !ids.insert(p.id.as_str()) {
                return Err(bad(format!("there are two quality profiles `{}`", p.id)));
            }
            p.check(self)?;
        }
        let mut kinds = BTreeSet::new();
        for c in &self.naming {
            if !kinds.insert(c.kind) {
                return Err(bad(format!("two naming conventions for {:?}", c.kind)));
            }
        }
        Ok(())
    }

    pub fn profile(&self, id: &str) -> Option<&Profile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    pub fn list(&self, id: &str) -> Option<&List> {
        self.lists.iter().find(|l| l.id == id)
    }

    /// The profiles that rank a list, with the stance each gives it.
    pub fn uses_of_list(&self, id: &str) -> Vec<(&Profile, Stance)> {
        self.profiles
            .iter()
            .filter_map(|p| {
                p.lists
                    .ranked
                    .iter()
                    .find(|x| x.key == id)
                    .map(|x| (p, x.stance))
            })
            .collect()
    }
}

/// One line of a verdict in the model's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    /// The answer that earned or cost the points: `target`, `picture`, `sound`, `lists`,
    /// `size`, `more`, `rules`.
    pub answer: String,
    /// `Release groups: Remux, tier 1`, `Picture: Dolby Vision preferred`.
    pub text: String,
    pub stance: Stance,
    pub points: i32,
    /// The trait the points come from, for the numbers on request.
    pub trait_id: String,
}

fn with_word(text: String, stance: Stance) -> String {
    match stance.word() {
        "" => text,
        word => format!("{text} {word}"),
    }
}

/// A verdict's trace in the model's words: each line names the answer behind it.
pub fn explain(model: &Model, profile: &Profile, trace: &[TraceItem]) -> Vec<Reason> {
    trace
        .iter()
        .map(|item| {
            let id = item.trait_id.as_str();
            let by_sign = Stance::of_points(item.points);
            let (prefix, rest) = id.split_once('.').unwrap_or((id, ""));
            let found = |picks: &[Pick], key: &str| {
                picks
                    .iter()
                    .find(|p| p.key == key)
                    .map(|p| p.stance)
                    .unwrap_or(by_sign)
            };
            let (answer, text, stance) = match prefix {
                "list" if rest == "none" => {
                    let stance = profile.lists.unlisted;
                    ("lists", "Release groups: no list".to_string(), stance)
                }
                "list" => {
                    let stance = found(&profile.lists.ranked, rest);
                    let text = model
                        .list(rest)
                        .map(|l| format!("{}: {}", l.kind.answer(), l.name))
                        .unwrap_or_else(|| item.label.clone());
                    ("lists", text, stance)
                }
                "rule" => {
                    let stance = found(&profile.more.rules, rest);
                    let text = model
                        .rules
                        .iter()
                        .find(|r| r.id == rest)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| item.label.clone());
                    ("rules", text, stance)
                }
                "size" => {
                    let text = if rest.ends_with(".small") {
                        "Size: below range".to_string()
                    } else if rest.ends_with(".large") {
                        "Size: above range".to_string()
                    } else {
                        "Size: smaller file".to_string()
                    };
                    ("size", text, by_sign)
                }
                other => match Group::by_prefix(other) {
                    Some(group) => {
                        let stance = found(profile.picks(group), rest);
                        let label = choice_of(group, rest).map_or(rest, |c| c.label);
                        let text = match group {
                            Group::Release => label.to_string(),
                            Group::Editions => format!("Edition: {label}"),
                            _ => format!("{}: {label}", group.answer()),
                        };
                        let answer = match group.answer() {
                            "Picture" => "picture",
                            "Sound" => "sound",
                            _ => "more",
                        };
                        (answer, text, stance)
                    }
                    None => ("rules", item.label.clone(), by_sign),
                },
            };
            let plain = (answer == "lists" && stance == Stance::Prefer) || answer == "size";
            let text = if plain { text } else { with_word(text, stance) };
            Reason {
                answer: answer.to_string(),
                text,
                stance,
                points: item.points,
                trait_id: item.trait_id.clone(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
