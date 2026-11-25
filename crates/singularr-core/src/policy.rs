//! The rules model, scoring and decisions.
//!
//! A [`Policy`] is loaded from `rules/*.toml`. It has traits (a named condition with a reason), lanes
//! (a quality ladder, points per trait, thresholds) and protections (what may replace a held copy).
//! [`evaluate`] scores one release in one lane and returns the trace; [`decide`] compares a candidate
//! with the held copy. Nothing in Singularr scores a release any other way. [`decide_edition`] does
//! it edition by edition: a lane holds one copy per wanted edition.
//! [`simulate`] replays a set of releases under two policies and reports what would change.

use crate::edition::{EditionList, EditionRead};
use crate::release::{
    AudioCodec, HdrFormat, MediaKind, ParsedRelease, Quality, Resolution, Source, VideoCodec,
};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("the policy is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("lane `{lane}` gives points to `{trait_id}`, which is not a trait")]
    UnknownTrait { lane: String, trait_id: String },
    #[error("lane `{lane}` lists `{label}`, which is not a quality such as WEBDL-1080p")]
    BadQuality { lane: String, label: String },
    #[error("trait `{trait_id}` has a pattern that does not compile: {message}")]
    BadPattern { trait_id: String, message: String },
    #[error("`{0}` appears twice")]
    Duplicate(String),
    #[error("lane `{lane}` has the cutoff quality `{label}`, which is not on its ladder")]
    CutoffNotOnLadder { lane: String, label: String },
}

/// Every field that is set must match (AND). `any` matches when one of its conditions does;
/// `not` inverts. An empty condition matches everything.
///
/// Fields about the picture and the sound (`resolution`, `video_codec`, `hdr`, `sdr`,
/// `audio_codec`, `atmos`, `channels`) read the release name first. Where the name says nothing
/// they fall back to [`EvalContext::media`], what a probe of the file found. For `hdr` each
/// format is a fact of its own: the probe adds the formats the name does not mention.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Condition {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub any: Vec<Condition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub not: Option<Box<Condition>>,
    /// Release group, case-insensitive, whole name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub group: Vec<String>,
    /// The collective the group releases under (`QxR`), case-insensitive, whole name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collective: Vec<String>,
    /// Case-insensitive pattern over the raw release name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kind: Vec<MediaKind>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source: Vec<Source>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resolution: Vec<Resolution>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub video_codec: Vec<VideoCodec>,
    /// Matches when the release carries any of these. Use `sdr = true` for none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hdr: Vec<HdrFormat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sdr: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub audio_codec: Vec<AudioCodec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atmos: Option<bool>,
    /// Audio channel layouts as written: `5.1`, `7.1`, `2.0`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub service: Vec<String>,
    /// Edition names, case-insensitive. An entry that starts with `*` matches any edition that
    /// ends with the rest: `"* Cut"` matches `Final Cut` and `Encore Cut`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub edition: Vec<String>,
    /// The release name states one of these languages (English names).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub language: Vec<String>,
    /// `false`: the release states its languages and the title's original language is not one
    /// of them. `true`: it is. With no stated language, or no known original, neither matches.
    /// Where the name states no language, [`EvalContext::languages`] is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub original_language: Option<bool>,
    /// The title carries one of these tags.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub title_tag: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_fps: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ai_upscaled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prerelease: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proper_or_repack: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_size_gb: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_size_gb: Option<f64>,
}

/// What a probe of the file itself found. Conditions use it only where the name is silent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaInfo {
    pub resolution: Option<Resolution>,
    pub video_codec: Option<VideoCodec>,
    /// `None`: not probed. `Some` and empty: probed, and the picture is SDR.
    pub hdr: Option<Vec<HdrFormat>>,
    pub audio_codec: Option<AudioCodec>,
    /// As a release name writes it: `5.1`, `2.0`.
    pub audio_channels: Option<String>,
    pub atmos: Option<bool>,
}

impl MediaInfo {
    /// Build from the media info strings the arrs report: video codec (`x265`), dynamic range
    /// (`DV HDR10`, empty for SDR), audio codec (`EAC3 Atmos`), channels (`5.1`) and frame size
    /// (`1920x800`). An empty string or `None` leaves that field unknown, except the dynamic
    /// range: once a video codec is known, an empty dynamic range means SDR.
    pub fn from_arr(
        video_codec: &str,
        dynamic_range: &str,
        audio_codec: &str,
        audio_channels: Option<f64>,
        frame_size: &str,
    ) -> MediaInfo {
        let v = video_codec.to_ascii_lowercase();
        let video = if v.contains("265") || v.contains("hevc") {
            Some(VideoCodec::H265)
        } else if v.contains("264") || v.contains("avc") {
            Some(VideoCodec::H264)
        } else if v.contains("av1") {
            Some(VideoCodec::Av1)
        } else if v.contains("vc1") || v.contains("vc-1") {
            Some(VideoCodec::Vc1)
        } else if v.contains("mpeg2") || v.contains("mpeg-2") {
            Some(VideoCodec::Mpeg2)
        } else if v.contains("xvid") || v.contains("divx") {
            Some(VideoCodec::Xvid)
        } else if v.contains("vp9") {
            Some(VideoCodec::Vp9)
        } else {
            None
        };
        let hdr: Vec<HdrFormat> = dynamic_range
            .split_whitespace()
            .filter_map(|word| {
                Some(match word.to_ascii_lowercase().as_str() {
                    "dv" => HdrFormat::DolbyVision,
                    "hdr10plus" | "hdr10+" => HdrFormat::Hdr10Plus,
                    "hdr10" => HdrFormat::Hdr10,
                    "hdr" => HdrFormat::Hdr,
                    "hlg" => HdrFormat::Hlg,
                    "pq" => HdrFormat::Pq,
                    _ => return None,
                })
            })
            .collect();
        let a = audio_codec.to_ascii_lowercase();
        let audio = if a.contains("truehd") {
            Some(AudioCodec::TrueHd)
        } else if a.contains("dts-x") || a.contains("dts:x") || a.contains("dts x") {
            Some(AudioCodec::DtsX)
        } else if a.contains("dts-hd ma") || a.contains("dts-ma") {
            Some(AudioCodec::DtsHdMa)
        } else if a.contains("dts-hd") {
            Some(AudioCodec::DtsHdHra)
        } else if a.contains("dts") {
            Some(AudioCodec::Dts)
        } else if a.contains("eac3") || a.contains("e-ac-3") {
            Some(AudioCodec::Eac3)
        } else if a.contains("ac3") || a.contains("ac-3") {
            Some(AudioCodec::Ac3)
        } else if a.contains("aac") {
            Some(AudioCodec::Aac)
        } else if a.contains("opus") {
            Some(AudioCodec::Opus)
        } else if a.contains("flac") {
            Some(AudioCodec::Flac)
        } else if a.contains("pcm") {
            Some(AudioCodec::Pcm)
        } else if a.contains("mp3") {
            Some(AudioCodec::Mp3)
        } else {
            None
        };
        let resolution = frame_size.split_once('x').and_then(|(w, h)| {
            let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
            // Width decides: a 1920x800 frame is a cropped 1080p picture.
            Some(match (w, h) {
                (3200.., _) => Resolution::R2160,
                (2200.., _) => Resolution::R1440,
                (1600.., _) => Resolution::R1080,
                (1100.., _) => Resolution::R720,
                (_, 520..) => Resolution::R576,
                _ => Resolution::R480,
            })
        });
        MediaInfo {
            resolution,
            video_codec: video,
            hdr: (video.is_some() || !hdr.is_empty()).then_some(hdr),
            audio_codec: audio,
            audio_channels: audio_channels
                .filter(|c| *c > 0.0)
                .map(|c| format!("{c:.1}")),
            atmos: audio.map(|_| a.contains("atmos")),
        }
    }
}

/// What is known beyond the name.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct EvalContext {
    pub size_bytes: Option<u64>,
    /// Tags on the title (protections can key on them).
    pub title_tags: Vec<String>,
    /// What a probe of the file found, for a held copy. Used where the name is silent.
    #[serde(default)]
    pub media: Option<MediaInfo>,
    /// The title's original language, as an English name (`English`, `Korean`).
    #[serde(default)]
    pub original_language: Option<String>,
    /// Languages known for the copy from outside the name. Used where the name states none.
    #[serde(default)]
    pub languages: Vec<String>,
}

fn contains_ci(list: &[String], value: &str) -> bool {
    list.iter().any(|x| x.eq_ignore_ascii_case(value))
}

/// Where a fact came from: the name, or the file probe.
fn origin(from_file: bool) -> &'static str {
    if from_file { " (from the file)" } else { "" }
}

impl Condition {
    /// Does the release match, and on what evidence. `None` means no match.
    pub fn matches(&self, r: &ParsedRelease, ctx: &EvalContext) -> Option<String> {
        let mut evidence: Vec<String> = Vec::new();
        let media = ctx.media.as_ref();
        if !self.any.is_empty() {
            evidence.push(self.any.iter().find_map(|c| c.matches(r, ctx))?);
        }
        if let Some(n) = &self.not
            && n.matches(r, ctx).is_some()
        {
            return None;
        }
        if !self.group.is_empty() {
            let g = r.group.as_deref().filter(|g| contains_ci(&self.group, g))?;
            evidence.push(format!("group {g}"));
        }
        if !self.collective.is_empty() {
            let c = r
                .collective
                .as_deref()
                .filter(|c| contains_ci(&self.collective, c))?;
            evidence.push(format!("collective {c}"));
        }
        if let Some(pat) = &self.name {
            let re = RegexBuilder::new(pat).case_insensitive(true).build().ok()?;
            evidence.push(format!("name has \"{}\"", re.find(&r.raw)?.as_str()));
        }
        if !self.kind.is_empty() && !self.kind.contains(&r.kind) {
            return None;
        }
        if !self.source.is_empty() {
            let s = r.source.filter(|s| self.source.contains(s))?;
            evidence.push(format!("source {}", s.label()));
        }
        if !self.resolution.is_empty() {
            let (x, from_file) = match r.resolution {
                Some(x) => (x, false),
                None => (media?.resolution?, true),
            };
            if !self.resolution.contains(&x) {
                return None;
            }
            evidence.push(format!("{}{}", x.label(), origin(from_file)));
        }
        if !self.video_codec.is_empty() {
            let (v, from_file) = match r.video_codec {
                Some(v) => (v, false),
                None => (media?.video_codec?, true),
            };
            if !self.video_codec.contains(&v) {
                return None;
            }
            evidence.push(format!("{v:?}{}", origin(from_file)));
        }
        if !self.hdr.is_empty() || self.sdr.is_some() {
            // Each HDR format is a fact of its own: the probe adds the formats the name does
            // not mention ("DoVi HDR10" on a file that also carries HDR10+). A name that says
            // SDR outright is believed.
            let probed: &[HdrFormat] = match media.and_then(|m| m.hdr.as_ref()) {
                Some(probed) if !r.sdr => probed,
                _ => &[],
            };
            if !self.hdr.is_empty() {
                let wanted = |h: &&HdrFormat| self.hdr.contains(h);
                let (h, from_file) = match r.hdr.iter().find(wanted) {
                    Some(h) => (h, false),
                    None => (probed.iter().find(wanted)?, true),
                };
                evidence.push(format!("{h:?}{}", origin(from_file)));
            }
            if let Some(sdr) = self.sdr
                && (r.hdr.is_empty() && probed.is_empty()) != sdr
            {
                return None;
            }
        }
        // The name speaks about the audio when it names a codec or Atmos.
        let audio_named = r.audio_codec.is_some() || r.atmos;
        if !self.audio_codec.is_empty() {
            let (a, from_file) = match r.audio_codec {
                Some(a) => (a, false),
                None => (media?.audio_codec?, true),
            };
            if !self.audio_codec.contains(&a) {
                return None;
            }
            evidence.push(format!("{a:?}{}", origin(from_file)));
        }
        if let Some(want) = self.atmos {
            let has = if audio_named {
                r.atmos
            } else {
                media.and_then(|m| m.atmos).unwrap_or(false)
            };
            if has != want {
                return None;
            }
            if want {
                evidence.push(format!("Atmos{}", origin(!audio_named)));
            }
        }
        if !self.channels.is_empty() {
            let (c, from_file) = match r.audio_channels.as_deref() {
                Some(c) => (c, false),
                None => (media?.audio_channels.as_deref()?, true),
            };
            if !self.channels.iter().any(|x| x == c) {
                return None;
            }
            evidence.push(format!("{c} audio{}", origin(from_file)));
        }
        if !self.service.is_empty() {
            let s = r
                .service
                .as_deref()
                .filter(|s| contains_ci(&self.service, s))?;
            evidence.push(format!("service {s}"));
        }
        if !self.edition.is_empty() {
            let e = r.editions.iter().find(|e| {
                self.edition
                    .iter()
                    .any(|want| match want.strip_prefix('*') {
                        Some(tail) => e.to_ascii_lowercase().ends_with(&tail.to_ascii_lowercase()),
                        None => want.eq_ignore_ascii_case(e),
                    })
            })?;
            evidence.push(format!("edition {e}"));
        }
        if !self.language.is_empty() {
            let l = r
                .languages
                .iter()
                .find(|l| contains_ci(&self.language, l))?;
            evidence.push(format!("language {l}"));
        }
        {
            let (stated, outside) = if r.languages.is_empty() {
                (&ctx.languages, true)
            } else {
                (&r.languages, false)
            };
            let source = if outside { " (from the file)" } else { "" };
            if let Some(want) = self.original_language {
                let original = ctx.original_language.as_deref()?;
                if stated.is_empty() {
                    return None;
                }
                let has = contains_ci(stated, original) || contains_ci(stated, "Multi");
                if has != want {
                    return None;
                }
                let list = stated.join(", ");
                evidence.push(format!("languages {list}{source}, original {original}"));
            }
        }
        if !self.title_tag.is_empty() {
            let t = ctx
                .title_tags
                .iter()
                .find(|t| contains_ci(&self.title_tag, t))?;
            evidence.push(format!("title tag {t}"));
        }
        if let Some(f) = self.min_fps {
            let fps = r.fps.filter(|x| *x >= f)?;
            evidence.push(format!("{fps} fps"));
        }
        if let Some(a) = self.ai_upscaled
            && r.ai_upscaled != a
        {
            return None;
        }
        if let Some(pre) = self.prerelease
            && r.source.is_some_and(Source::is_prerelease) != pre
        {
            return None;
        }
        if let Some(pr) = self.proper_or_repack {
            if (r.proper || r.repack) != pr {
                return None;
            }
            if pr {
                evidence.push(if r.proper { "proper" } else { "repack" }.to_string());
            }
        }
        if self.min_size_gb.is_some() || self.max_size_gb.is_some() {
            let gb = ctx.size_bytes? as f64 / 1_073_741_824.0;
            if self.min_size_gb.is_some_and(|m| gb < m) || self.max_size_gb.is_some_and(|m| gb > m)
            {
                return None;
            }
            evidence.push(format!("{gb:.1} GiB"));
        }
        Some(if evidence.is_empty() {
            "always".to_string()
        } else {
            evidence.join(", ")
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trait {
    pub id: String,
    pub label: String,
    /// The reason the rule exists, shown wherever it scores.
    pub why: String,
    /// For grouping on the Policy page: `group`, `service`, `edition`, `video`, `audio`, `penalty`, `other`.
    #[serde(default = "other")]
    pub category: String,
    pub when: Condition,
    /// The custom formats of the legacy arrs this trait stands for, written `radarr:QxR`.
    /// The shadow comparison uses it to line a trait up with the arr's own match. Scoring
    /// ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arr_formats: Vec<String>,
}

fn other() -> String {
    "other".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lane {
    pub id: String,
    pub label: String,
    /// Quality steps, best first. Qualities in one step rank the same. Written `WEBDL-1080p`.
    pub ladder: Vec<Vec<String>>,
    /// Points per trait id.
    #[serde(default)]
    pub points: BTreeMap<String, i32>,
    /// A release scoring below this is never taken.
    #[serde(default)]
    pub min_score: i32,
    /// A same-quality candidate must beat the held copy by at least this much.
    #[serde(default = "one")]
    pub upgrade_step: i32,
    /// Once a held copy scores this, stop looking for same-quality upgrades.
    #[serde(default = "max_score")]
    pub cutoff_score: i32,
    /// The quality step that is good enough. A held copy at this step or a better one, whose
    /// score has also reached `cutoff_score`, is final: a better quality no longer replaces it.
    /// Absent: a better quality step always replaces a worse one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutoff_quality: Option<String>,
    /// The arr quality profiles this lane stands for, written `radarr:HD-1080p`. It places a
    /// title synced from an arr in its lane. Scoring ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arr_profiles: Vec<String>,
}

fn one() -> i32 {
    1
}

fn max_score() -> i32 {
    i32::MAX
}

impl Lane {
    /// Position of a quality in the ladder, 0 is best. `None` when the lane does not take it.
    pub fn rank(&self, q: Quality) -> Option<usize> {
        self.ladder
            .iter()
            .position(|step| step.iter().any(|s| Quality::from_label(s) == Some(q)))
    }

    /// The ladder position of `cutoff_quality`.
    fn cutoff_rank(&self) -> Option<usize> {
        self.cutoff_quality
            .as_deref()
            .and_then(Quality::from_label)
            .and_then(|q| self.rank(q))
    }
}

/// When a protection applies to a title, and what may still replace its held copy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protection {
    pub id: String,
    pub label: String,
    pub why: String,
    /// Applies when the title carries one of these tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Applies when the file the library has matches. Written `current` in a policy file;
    /// `held`, the older keyword, is still read. (The field keeps its older name for the
    /// earlier pages that read it.)
    #[serde(default, rename = "current", alias = "held")]
    pub held: Option<Condition>,
    /// A candidate must match this to replace the held copy. Absent: nothing replaces it.
    #[serde(default)]
    pub allow: Option<Condition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub traits: Vec<Trait>,
    #[serde(default)]
    pub lanes: Vec<Lane>,
    #[serde(default)]
    pub protections: Vec<Protection>,
}

impl Policy {
    pub fn from_toml(text: &str) -> Result<Policy, PolicyError> {
        let p: Policy = toml::from_str(text)?;
        p.check()?;
        Ok(p)
    }

    fn check(&self) -> Result<(), PolicyError> {
        let mut seen = std::collections::BTreeSet::new();
        for t in &self.traits {
            if !seen.insert(format!("trait {}", t.id)) {
                return Err(PolicyError::Duplicate(format!("trait {}", t.id)));
            }
            check_patterns(&t.id, &t.when)?;
        }
        for l in &self.lanes {
            if !seen.insert(format!("lane {}", l.id)) {
                return Err(PolicyError::Duplicate(format!("lane {}", l.id)));
            }
            for id in l.points.keys() {
                if !self.traits.iter().any(|t| &t.id == id) {
                    return Err(PolicyError::UnknownTrait {
                        lane: l.id.clone(),
                        trait_id: id.clone(),
                    });
                }
            }
            for label in l.ladder.iter().flatten() {
                if Quality::from_label(label).is_none() {
                    return Err(PolicyError::BadQuality {
                        lane: l.id.clone(),
                        label: label.clone(),
                    });
                }
            }
            if let Some(label) = &l.cutoff_quality
                && l.cutoff_rank().is_none()
            {
                return Err(PolicyError::CutoffNotOnLadder {
                    lane: l.id.clone(),
                    label: label.clone(),
                });
            }
        }
        for p in &self.protections {
            if !seen.insert(format!("protection {}", p.id)) {
                return Err(PolicyError::Duplicate(format!("protection {}", p.id)));
            }
            for c in p.held.iter().chain(p.allow.iter()) {
                check_patterns(&p.id, c)?;
            }
        }
        Ok(())
    }

    pub fn lane(&self, id: &str) -> Option<&Lane> {
        self.lanes
            .iter()
            .find(|l| l.id.eq_ignore_ascii_case(id) || l.label.eq_ignore_ascii_case(id))
    }

    /// The lane that stands for an arr's quality profile: `lane_for_arr("radarr", "HD-1080p")`.
    pub fn lane_for_arr(&self, arr: &str, profile: &str) -> Option<&Lane> {
        let key = format!("{arr}:{profile}");
        self.lanes
            .iter()
            .find(|l| contains_ci(&l.arr_profiles, &key))
    }

    /// The traits that stand for an arr's custom format: `traits_for_arr_format("sonarr", "QxR")`.
    pub fn traits_for_arr_format(&self, arr: &str, format: &str) -> Vec<&Trait> {
        let key = format!("{arr}:{format}");
        self.traits
            .iter()
            .filter(|t| contains_ci(&t.arr_formats, &key))
            .collect()
    }
}

fn check_patterns(trait_id: &str, c: &Condition) -> Result<(), PolicyError> {
    if let Some(p) = &c.name {
        RegexBuilder::new(p)
            .case_insensitive(true)
            .build()
            .map_err(|e| PolicyError::BadPattern {
                trait_id: trait_id.to_string(),
                message: e.to_string(),
            })?;
    }
    for a in &c.any {
        check_patterns(trait_id, a)?;
    }
    if let Some(n) = &c.not {
        check_patterns(trait_id, n)?;
    }
    Ok(())
}

/// One rule that scored, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraceItem {
    pub trait_id: String,
    pub label: String,
    pub category: String,
    pub points: i32,
    pub why: String,
    /// What in the release made the rule match.
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub lane: String,
    pub score: i32,
    pub quality: Option<String>,
    /// Ladder position, 0 is best. `None`: the lane does not take this quality.
    pub rank: Option<usize>,
    pub trace: Vec<TraceItem>,
    /// Traits that matched but carry no points in this lane (shown dimmed).
    pub matched_unscored: Vec<String>,
}

/// Score one release in one lane.
pub fn evaluate(policy: &Policy, lane: &Lane, r: &ParsedRelease, ctx: &EvalContext) -> Verdict {
    let mut trace = Vec::new();
    let mut unscored = Vec::new();
    for t in &policy.traits {
        let Some(evidence) = t.when.matches(r, ctx) else {
            continue;
        };
        match lane.points.get(&t.id) {
            Some(points) if *points != 0 => trace.push(TraceItem {
                trait_id: t.id.clone(),
                label: t.label.clone(),
                category: t.category.clone(),
                points: *points,
                why: t.why.clone(),
                evidence,
            }),
            _ => unscored.push(t.id.clone()),
        }
    }
    trace.sort_by_key(|t| std::cmp::Reverse(t.points.abs()));
    let quality = r.quality();
    Verdict {
        lane: lane.id.clone(),
        score: trace.iter().map(|t| t.points).sum(),
        quality: quality.map(Quality::label),
        rank: quality.and_then(|q| lane.rank(q)),
        trace,
        matched_unscored: unscored,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Nothing is held: take it.
    Grab,
    /// Replace the held copy.
    Upgrade,
    /// Leave things as they are.
    Hold,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub action: Action,
    /// Plain sentences, most important first.
    pub reasons: Vec<String>,
    pub candidate: Verdict,
    pub held: Option<Verdict>,
    /// The protection that stopped it, if one did.
    pub blocked_by: Option<String>,
}

/// Should `candidate` be taken, given what is held. `held` is the held copy's parsed release.
pub fn decide(
    policy: &Policy,
    lane: &Lane,
    held: Option<(&ParsedRelease, &EvalContext)>,
    candidate: &ParsedRelease,
    ctx: &EvalContext,
) -> Decision {
    let cv = evaluate(policy, lane, candidate, ctx);
    let hv = held.map(|(h, hctx)| evaluate(policy, lane, h, hctx));
    let hold = |reasons: Vec<String>,
                blocked_by: Option<String>,
                cv: Verdict,
                hv: Option<Verdict>| Decision {
        action: Action::Hold,
        reasons,
        candidate: cv,
        held: hv,
        blocked_by,
    };

    let Some(c_rank) = cv.rank else {
        let reason = match (candidate.source, &cv.quality) {
            (Some(s), _) if s.is_prerelease() => format!(
                "{} is a pre-release source, recorded in a theater or leaked before release. {} does not take it.",
                s.label(),
                lane.label
            ),
            (_, Some(q)) => format!("{} does not take {q}.", lane.label),
            (_, None) => format!(
                "The name does not give a source and a resolution, so {} cannot place it.",
                lane.label
            ),
        };
        return hold(vec![reason], None, cv, hv);
    };
    if cv.score < lane.min_score {
        return hold(
            vec![format!(
                "It scores {}, below the lane's minimum of {}.",
                cv.score, lane.min_score
            )],
            None,
            cv,
            hv,
        );
    }
    let (Some((h, hctx)), Some(held_verdict)) = (held, hv.as_ref()) else {
        return Decision {
            action: Action::Grab,
            reasons: vec!["Nothing is held for this title in this lane.".to_string()],
            candidate: cv,
            held: None,
            blocked_by: None,
        };
    };

    for p in &policy.protections {
        let tagged = |tags: &[String]| p.tags.iter().any(|t| contains_ci(tags, t));
        let by_tag = tagged(&ctx.title_tags) || tagged(&hctx.title_tags);
        let by_held = p
            .held
            .as_ref()
            .is_some_and(|c| c.matches(h, hctx).is_some());
        if !(by_tag || by_held) {
            continue;
        }
        let allowed = p
            .allow
            .as_ref()
            .is_some_and(|a| a.matches(candidate, ctx).is_some());
        if !allowed {
            return hold(
                vec![format!("Protected: {}. {}", p.label, p.why)],
                Some(p.id.clone()),
                cv,
                hv,
            );
        }
    }

    let h_rank = held_verdict.rank;
    let at_score_cutoff = held_verdict.score >= lane.cutoff_score;
    // With a cutoff quality, a held copy at that step or better that has also reached the
    // cutoff score is final.
    let final_copy = at_score_cutoff
        && matches!((h_rank, lane.cutoff_rank()), (Some(hr), Some(cut)) if hr <= cut);
    let (action, reasons) = match h_rank {
        None => (
            Action::Upgrade,
            vec![format!(
                "The held copy's quality is outside {}; this one is inside.",
                lane.label
            )],
        ),
        Some(hr) if c_rank < hr && final_copy => (
            Action::Hold,
            vec![format!(
                "The held copy is {} and scores {}: it has reached the lane's cutoff quality ({}) and cutoff score ({}).",
                held_verdict.quality.as_deref().unwrap_or("?"),
                held_verdict.score,
                lane.cutoff_quality.as_deref().unwrap_or("?"),
                lane.cutoff_score
            )],
        ),
        Some(hr) if c_rank < hr => (
            Action::Upgrade,
            vec![format!(
                "Better quality: {} over {}.",
                cv.quality.as_deref().unwrap_or("?"),
                held_verdict.quality.as_deref().unwrap_or("?")
            )],
        ),
        Some(hr) if c_rank > hr => (
            Action::Hold,
            vec![format!(
                "Lower quality: {} against the held {}.",
                cv.quality.as_deref().unwrap_or("?"),
                held_verdict.quality.as_deref().unwrap_or("?")
            )],
        ),
        Some(_) if at_score_cutoff => (
            Action::Hold,
            vec![format!(
                "The held copy already scores {}, at or past the cutoff of {}.",
                held_verdict.score, lane.cutoff_score
            )],
        ),
        Some(_) if cv.score >= held_verdict.score.saturating_add(lane.upgrade_step) => (
            Action::Upgrade,
            vec![format!(
                "Same quality, better score: {} over {}.",
                cv.score, held_verdict.score
            )],
        ),
        Some(_) => (
            Action::Hold,
            vec![format!(
                "Same quality and {} against the held {}: it must gain at least {}.",
                cv.score, held_verdict.score, lane.upgrade_step
            )],
        ),
    };
    Decision {
        action,
        reasons,
        candidate: cv,
        held: hv,
        blocked_by: None,
    }
}

/// A decision made edition by edition: the candidate's edition, the held copy it was compared
/// with, and the decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EditionDecision {
    pub edition: EditionRead,
    /// The held copy of the same edition, by its position in the `held` slice. `None` when the
    /// title holds nothing of that edition in this lane.
    pub against: Option<usize>,
    pub decision: Decision,
}

/// Should `candidate` be taken, given the copies a title holds in one lane.
///
/// A lane holds one copy per wanted edition, and a candidate competes only with the held copy
/// of its own edition: a better Theatrical never replaces a Director's Cut, and a standard
/// copy never replaces an open matte one. `wanted` lists the edition keys the title wants;
/// `None` means the editions it holds (and, when it holds nothing, whatever comes first).
/// Against the held copy of its own edition the candidate is judged by [`decide`], so the
/// policy's protections still apply.
pub fn decide_edition(
    policy: &Policy,
    editions: &EditionList,
    lane: &Lane,
    held: &[(&ParsedRelease, &EvalContext)],
    wanted: Option<&[String]>,
    candidate: &ParsedRelease,
    ctx: &EvalContext,
) -> EditionDecision {
    let edition = editions.read_release(candidate);
    let held_keys: Vec<String> = held
        .iter()
        .map(|(h, _)| editions.read_release(h).key)
        .collect();
    let is_wanted = match wanted {
        Some(list) => list.contains(&edition.key),
        None => held.is_empty() || held_keys.contains(&edition.key),
    };
    if !is_wanted {
        let mut labels: Vec<(usize, String)> = held
            .iter()
            .map(|(h, _)| {
                let r = editions.read_release(h);
                (editions.rank(&r.key), r.label)
            })
            .collect();
        labels.sort();
        labels.dedup();
        let holds = labels
            .into_iter()
            .map(|(_, l)| l)
            .collect::<Vec<_>>()
            .join(", ");
        let reason = if holds.is_empty() {
            format!("{} is not an edition this title wants.", edition.label)
        } else {
            format!(
                "{} is not an edition this title wants: it holds {holds}. A copy competes only with the held copy of its own edition, so it replaces none of them.",
                edition.label
            )
        };
        return EditionDecision {
            edition,
            against: None,
            decision: Decision {
                action: Action::Hold,
                reasons: vec![reason],
                candidate: evaluate(policy, lane, candidate, ctx),
                held: None,
                blocked_by: None,
            },
        };
    }
    let against = held_keys.iter().position(|k| *k == edition.key);
    let mut decision = decide(
        policy,
        lane,
        against.map(|i| (held[i].0, held[i].1)),
        candidate,
        ctx,
    );
    if against.is_none() && !held.is_empty() && decision.action == Action::Grab {
        decision.reasons = vec![format!(
            "Nothing of the {} edition is held in this lane.",
            edition.label
        )];
    }
    EditionDecision {
        edition,
        against,
        decision,
    }
}

/// How one trait's contribution to a score differs between two policies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TraitChange {
    pub trait_id: String,
    pub label: String,
    /// Points the trait gave under the first policy, 0 when it did not score.
    pub before: i32,
    /// Points it gives under the second policy.
    pub after: i32,
    /// In plain words: `now matches`, `no longer matches`, `points 100 to 50`.
    pub what: String,
}

/// One release whose score or ladder position differs between two policies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SimulatedChange {
    /// Position in the slice given to [`simulate`].
    pub index: usize,
    /// The raw release name.
    pub name: String,
    pub lane: String,
    /// `None`: the first policy has no such lane.
    pub before: Option<i32>,
    /// `None`: the second policy has no such lane.
    pub after: Option<i32>,
    /// `after - before`, counting a missing lane as 0.
    pub delta: i32,
    pub rank_before: Option<usize>,
    pub rank_after: Option<usize>,
    /// The traits behind the difference, largest effect first.
    pub causes: Vec<TraitChange>,
}

/// Replay releases under two policies and return the ones that would change: by how much the
/// score moves and which traits cause it. `items` are (release, context, lane id). A release
/// whose score and ladder position stay the same is left out.
pub fn simulate(
    before: &Policy,
    after: &Policy,
    items: &[(ParsedRelease, EvalContext, String)],
) -> Vec<SimulatedChange> {
    let mut out = Vec::new();
    for (index, (release, ctx, lane_id)) in items.iter().enumerate() {
        let a = before
            .lane(lane_id)
            .map(|l| evaluate(before, l, release, ctx));
        let b = after
            .lane(lane_id)
            .map(|l| evaluate(after, l, release, ctx));
        let score = |v: &Option<Verdict>| v.as_ref().map(|v| v.score);
        let rank = |v: &Option<Verdict>| v.as_ref().and_then(|v| v.rank);
        if score(&a) == score(&b) && rank(&a) == rank(&b) && a.is_some() == b.is_some() {
            continue;
        }
        let empty = Vec::new();
        let trace_a = a.as_ref().map_or(&empty, |v| &v.trace);
        let trace_b = b.as_ref().map_or(&empty, |v| &v.trace);
        let points = |trace: &[TraceItem], id: &str| {
            trace
                .iter()
                .find(|t| t.trait_id == id)
                .map_or(0, |t| t.points)
        };
        let mut causes: Vec<TraitChange> = Vec::new();
        for item in trace_a.iter().chain(trace_b.iter()) {
            if causes.iter().any(|c| c.trait_id == item.trait_id) {
                continue;
            }
            let (pa, pb) = (
                points(trace_a, &item.trait_id),
                points(trace_b, &item.trait_id),
            );
            if pa == pb {
                continue;
            }
            // Did the condition change, or only the points?
            let matched = |v: &Option<Verdict>| {
                v.as_ref().is_some_and(|v| {
                    v.trace.iter().any(|t| t.trait_id == item.trait_id)
                        || v.matched_unscored.contains(&item.trait_id)
                })
            };
            let what = match (matched(&a), matched(&b)) {
                (false, true) => "now matches".to_string(),
                (true, false) => "no longer matches".to_string(),
                _ => format!("points {pa} to {pb}"),
            };
            causes.push(TraitChange {
                trait_id: item.trait_id.clone(),
                label: item.label.clone(),
                before: pa,
                after: pb,
                what,
            });
        }
        causes.sort_by_key(|c| std::cmp::Reverse((c.after - c.before).abs()));
        out.push(SimulatedChange {
            index,
            name: release.raw.clone(),
            lane: lane_id.clone(),
            before: score(&a),
            after: score(&b),
            delta: score(&b).unwrap_or(0) - score(&a).unwrap_or(0),
            rank_before: rank(&a),
            rank_after: rank(&b),
            causes,
        });
    }
    out
}

#[cfg(test)]
mod tests;
