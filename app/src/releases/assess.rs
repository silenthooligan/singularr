//! Assess one held file.
//!
//! Two readings are made and both are kept:
//!
//! - **The name as written.** The scene name, else the last segment of the original path,
//!   else the library file name, parsed as it stands. The shadow comparison puts this next
//!   to what the arr recorded.
//! - **The release as held.** The arr's own record comes first: its release group, its
//!   quality (source and resolution), its proper or repack mark and its edition. The parsed
//!   name fills what the record does not say. This is what is scored, with the title's
//!   tags, its original language, the languages on record and the probe of the file. About
//!   930 files carry a name from one library's encode pipeline whose group is stale; the
//!   record is right about them.
//!
//! Parsing and scoring are `singularr_core`'s. This file chooses the inputs and compares the
//! outputs.

use crate::catalog::ingest::{Assess, Assessment, FileFacts, TitleContext};
use crate::catalog::model::{ArrReading, SingularrReading};
use crate::policy::{
    ActiveEditions, ActivePolicy, EditionsHandle, PolicyCell, PolicyHandle, held_context, lane_for,
    protections_on,
};
use singularr_core::naming::parse_library_name;
use singularr_core::policy::evaluate;
use singularr_core::release::{self, Quality, editions_from_text};
use singularr_core::{ParsedRelease, Policy, Verdict};
use std::collections::HashSet;

/// The last segment of a path written with either kind of slash.
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn set(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// The name to parse and where it came from: the scene name, else the last segment of the
/// original path, else the library file name. This is the order in which the arr picks the
/// title it scores.
pub fn pick_name(file: &FileFacts) -> (String, &'static str) {
    if let Some(scene) = set(&file.scene_name) {
        return (scene.to_string(), "scene_name");
    }
    if let Some(original) = set(&file.original_path) {
        let name = file_name(original).trim();
        if !name.is_empty() {
            return (name.to_string(), "original_path");
        }
    }
    (file_name(&file.relative_path).to_string(), "library_name")
}

/// Parse a name by its source. A library file name goes through `singularr_core::naming`,
/// which knows the fixed places a library's naming puts the edition, the quality and the
/// group in.
pub fn parse_named(name: &str, source: &str) -> ParsedRelease {
    if source == "library_name" {
        parse_library_name(name).to_release()
    } else {
        release::parse(name)
    }
}

/// The release as the arr holds it: the parsed name with the arr's recorded group, quality,
/// revision and edition in place of the name's. What the record does not say stays as parsed.
pub fn as_recorded(parsed: &ParsedRelease, file: &FileFacts) -> ParsedRelease {
    let mut held = parsed.clone();
    if let Some(group) = set(&file.arr_group) {
        // Passed through as recorded: Radarr stores `60fps-DirtyHippie`, and the policy
        // lists that spelling.
        held.group = Some(group.to_string());
        if held
            .collective
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(group))
        {
            held.collective = None;
        }
    }
    if let Some(quality) = set(&file.arr_quality).and_then(Quality::from_label) {
        held.source = Some(quality.source);
        held.resolution = Some(quality.resolution);
    }
    held.proper |= file.arr_proper;
    held.repack |= file.arr_repack;
    if let Some(edition) = set(&file.arr_edition) {
        for name in editions_from_text(edition) {
            if !held.has_edition(&name) {
                held.editions.push(name);
            }
        }
    }
    held
}

/// Singularr's reading for the shadow comparison: quality, group and edition as the name
/// states them, and the score the held release gets.
pub fn singularr_reading(parsed: &ParsedRelease, verdict: Option<&Verdict>) -> SingularrReading {
    SingularrReading {
        quality: parsed.quality().map(Quality::label),
        group: parsed.group.clone(),
        edition: (!parsed.editions.is_empty()).then(|| parsed.editions.join(" ")),
        score: verdict.map(|v| v.score),
    }
}

/// Two quality labels name the same quality. The arrs' spellings read through
/// `Quality::from_label` (`Bluray-1080p Remux`, `DVD`, `SDTV`); a label it does not know is
/// compared as text.
fn same_quality(arr: &str, singularr: &str) -> bool {
    match (Quality::from_label(arr), Quality::from_label(singularr)) {
        (Some(a), Some(m)) => a == m,
        _ => arr.trim().eq_ignore_ascii_case(singularr.trim()),
    }
}

fn squash(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Where the two readings differ: `(quality, group, edition, score)`.
///
/// A field the arr did not record is not a difference: an `Unknown` or missing quality, a
/// missing score, and the edition for Sonarr, which has none. A group the arr recorded as
/// the collective the name carries (`QxR` for `(.. Tigole) [QxR]`) is the same group.
pub fn compare(
    arr: &ArrReading,
    singularr: &SingularrReading,
    collective: Option<&str>,
) -> (bool, bool, bool, bool) {
    let quality = match (set(&arr.quality), set(&singularr.quality)) {
        (Some(a), _) if a.eq_ignore_ascii_case("unknown") => false,
        (Some(a), Some(m)) => !same_quality(a, m),
        (Some(_), None) => true,
        (None, _) => false,
    };
    let group = match (set(&arr.group), set(&singularr.group)) {
        (Some(a), Some(m)) => {
            !(a.eq_ignore_ascii_case(m) || collective.is_some_and(|c| c.eq_ignore_ascii_case(a)))
        }
        (Some(a), None) => !collective.is_some_and(|c| c.eq_ignore_ascii_case(a)),
        (None, None) => false,
        (None, Some(_)) => true,
    };
    let edition = match (&arr.edition, set(&singularr.edition)) {
        (None, _) => false,
        (Some(a), m) => {
            // Compared as sets of normalized edition names, so wording and order do not count.
            // An edition the reader does not know is compared as text.
            let names = |text: &str| {
                let mut found: Vec<String> =
                    editions_from_text(text).iter().map(|e| squash(e)).collect();
                if found.is_empty() && !squash(text).is_empty() {
                    found.push(squash(text));
                }
                found.sort();
                found
            };
            names(a) != names(m.unwrap_or(""))
        }
    };
    let score = matches!((arr.score, singularr.score), (Some(a), Some(m)) if a != m);
    (quality, group, edition, score)
}

pub fn differs(flags: (bool, bool, bool, bool)) -> Vec<String> {
    [
        (flags.0, "quality"),
        (flags.1, "group"),
        (flags.2, "edition"),
        (flags.3, "score"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, name)| name.to_string())
    .collect()
}

/// Line the arr's matched custom formats up with the traits Singularr matched, through the
/// `arr_formats` the policy gives each trait. Returns the formats only the arr matched and
/// the scored traits only Singularr matched.
pub fn format_differences(
    policy: &Policy,
    arr: &str,
    formats: &[String],
    verdict: &Verdict,
) -> (Vec<String>, Vec<String>) {
    let matched: HashSet<&str> = verdict
        .trace
        .iter()
        .map(|t| t.trait_id.as_str())
        .chain(verdict.matched_unscored.iter().map(String::as_str))
        .collect();
    let arr_only = formats
        .iter()
        .filter(|format| {
            !policy
                .traits_for_arr_format(arr, format)
                .iter()
                .any(|t| matched.contains(t.id.as_str()))
        })
        .cloned()
        .collect();
    let prefix = format!("{}:", arr.to_ascii_lowercase());
    let singularr_only = verdict
        .trace
        .iter()
        .filter(|item| {
            let Some(t) = policy.traits.iter().find(|t| t.id == item.trait_id) else {
                return false;
            };
            // The formats of this arr the trait stands for. A trait with none has nothing
            // on the arr's side to be compared with.
            let theirs: Vec<&str> = t
                .arr_formats
                .iter()
                .filter(|f| f.to_ascii_lowercase().starts_with(&prefix))
                .map(|f| &f[prefix.len()..])
                .collect();
            !theirs.is_empty()
                && !theirs
                    .iter()
                    .any(|f| formats.iter().any(|x| x.eq_ignore_ascii_case(f)))
        })
        .map(|item| item.trait_id.clone())
        .collect();
    (arr_only, singularr_only)
}

/// Assesses files with the policy in force and the edition list. This is the catalog's
/// [`Assess`] port.
pub struct Assessor {
    policy: PolicyHandle,
    editions: EditionsHandle,
    build_id: String,
}

impl Assessor {
    /// With one fixed policy and the edition list as shipped.
    pub fn new(policy: std::sync::Arc<ActivePolicy>, build_id: &str) -> Self {
        Assessor::with_editions(
            policy,
            std::sync::Arc::new(ActiveEditions::shipped()),
            build_id,
        )
    }

    /// With one fixed policy.
    pub fn with_editions(
        policy: std::sync::Arc<ActivePolicy>,
        editions: EditionsHandle,
        build_id: &str,
    ) -> Self {
        Assessor::live(PolicyCell::from_arc(policy), editions, build_id)
    }

    /// With the policy in force, whatever it is when a file is assessed. `build_id` names the
    /// build of the parser; with the policy hash and the edition list's hash it stamps every
    /// assessment, so a new policy, a new list or a new build assesses the stored copies again.
    pub fn live(policy: PolicyHandle, editions: EditionsHandle, build_id: &str) -> Self {
        Assessor {
            policy,
            editions,
            build_id: build_id.to_string(),
        }
    }
}

impl Assess for Assessor {
    fn stamp(&self) -> String {
        let short = |h: &str| h[..16.min(h.len())].to_string();
        format!(
            "{}:{}:{}",
            short(&self.policy.current().hash),
            short(&self.editions.hash),
            self.build_id
        )
    }

    fn lane_for(&self, engine: &str, profile: Option<&str>) -> Option<String> {
        lane_for(&self.policy.current().policy, engine, profile)
    }

    fn default_edition(&self) -> String {
        self.editions.list.default_edition().key.clone()
    }

    fn assess(&self, file: &FileFacts, title: &TitleContext<'_>) -> Assessment {
        let active = self.policy.current();
        let policy = &active.policy;
        let (release_name, name_source) = pick_name(file);
        let parsed = parse_named(&release_name, name_source);
        let held = as_recorded(&parsed, file);
        let ctx = held_context(
            file.size_bytes,
            title.tags,
            title.original_language,
            &file.languages,
            file.media_info.as_ref(),
        );
        let lane = title.lane.and_then(|id| policy.lane(id));
        let verdict = lane.map(|l| evaluate(policy, l, &held, &ctx));
        let reading = singularr_reading(&parsed, verdict.as_ref());
        let arr = ArrReading {
            quality: file.arr_quality.clone(),
            group: file.arr_group.clone(),
            edition: file.arr_edition.clone(),
            custom_formats: Vec::new(),
            score: file.arr_score,
        };
        let (diff_quality, diff_group, diff_edition, diff_score) =
            compare(&arr, &reading, parsed.collective.as_deref());
        // Without a score the arr's format list was not read, so there is nothing to line up.
        let (arr_only_formats, singularr_only_traits) = match (&verdict, file.arr_score) {
            (Some(v), Some(_)) => {
                format_differences(policy, title.engine, &file.arr_custom_formats, v)
            }
            _ => (Vec::new(), Vec::new()),
        };
        let list = &self.editions.list;
        let edition = list.read_copy(&held, file.arr_edition.as_deref());
        let edition_rank = i32::try_from(list.rank(&edition.key)).unwrap_or(i32::MAX);
        Assessment {
            edition,
            edition_rank,
            protections: protections_on(policy, &held, &ctx),
            release_name,
            name_source,
            mq_quality: reading.quality,
            mq_group: reading.group,
            mq_edition: reading.edition,
            mq_score: reading.score,
            diff_quality,
            diff_group,
            diff_edition,
            diff_score,
            arr_only_formats,
            singularr_only_traits,
            ladder_steps: lane.map(|l| l.ladder.len()),
            parsed,
            held,
            verdict,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::ActivePolicy;
    use singularr_core::EvalContext;
    use std::sync::Arc;

    pub fn file(scene: Option<&str>, original: Option<&str>, relative: &str) -> FileFacts {
        FileFacts {
            engine_file_id: 1,
            season_number: None,
            episode_numbers: vec![],
            episode_name: None,
            relative_path: relative.to_string(),
            scene_name: scene.map(str::to_string),
            original_path: original.map(str::to_string),
            size_bytes: 5_000_000_000,
            date_added: None,
            arr_quality: Some("Bluray-1080p".to_string()),
            arr_group: Some("SM737".to_string()),
            arr_edition: Some(String::new()),
            arr_proper: false,
            arr_repack: false,
            arr_custom_formats: vec![],
            arr_score: None,
            languages: vec![],
            media_info: None,
        }
    }

    fn title<'a>(lane: Option<&'a str>, tags: &'a [String]) -> TitleContext<'a> {
        TitleContext {
            engine: "radarr",
            lane,
            tags,
            original_language: Some("English"),
        }
    }

    const LIBRARY: &str = "10 Cloverfield Lane (2016) {imdb-tt1179933} [Bluray-1080p Proper][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv";

    #[test]
    fn the_scene_name_wins_then_the_original_path_then_the_library_name() {
        let scene =
            "10 Cloverfield Lane 2016 PROPER UHD BluRay 1080p DD Atmos 5 1 DoVi HDR10 x265-SM737";
        let original = "Some.Folder/10.Cloverfield.Lane.2016.1080p.BluRay.x265-SM737.mkv";
        assert_eq!(
            pick_name(&file(Some(scene), Some(original), LIBRARY)),
            (scene.to_string(), "scene_name")
        );
        assert_eq!(
            pick_name(&file(Some("  "), Some(original), LIBRARY)),
            (
                "10.Cloverfield.Lane.2016.1080p.BluRay.x265-SM737.mkv".to_string(),
                "original_path"
            )
        );
        assert_eq!(
            pick_name(&file(None, None, LIBRARY)),
            (LIBRARY.to_string(), "library_name")
        );
        assert_eq!(
            pick_name(&file(
                None,
                None,
                "Season 01/Show (2021) - S01E01 - Pilot [WEBDL-1080p][EAC3 5.1][h264]-NTb.mkv"
            ))
            .0,
            "Show (2021) - S01E01 - Pilot [WEBDL-1080p][EAC3 5.1][h264]-NTb.mkv"
        );
        assert_eq!(file_name(r"C:\media\Movie (2000)\file.mkv"), "file.mkv");
    }

    #[test]
    fn a_library_name_is_read_through_the_naming_module() {
        let parsed = parse_named(
            "Harry Potter and the Prisoner of Azkaban (2004) {imdb-tt0304141} {edition-Open Matte} [WEBDL-1080p][EAC3 5.1][HEVC]-PeruGuy.mkv",
            "library_name",
        );
        assert_eq!(parsed.group.as_deref(), Some("PeruGuy"));
        assert!(parsed.has_edition("Open Matte"));
        assert_eq!(
            parsed.quality().map(Quality::label).as_deref(),
            Some("WEBDL-1080p")
        );
    }

    #[test]
    fn the_arr_record_comes_first_and_the_name_fills_the_rest() {
        // A pipeline name: the group in the name is stale, the record has the real one.
        let mut f = file(
            None,
            None,
            "Movie (2010) {tmdb-1} [Bluray-1080p][Opus 5.1][HEVC HUSH]-[SHB].mkv",
        );
        f.arr_group = Some("QxR".to_string());
        f.arr_quality = Some("WEBDL-1080p".to_string());
        f.arr_edition = Some("Extended".to_string());
        f.arr_proper = true;
        let parsed = parse_named(file_name(&f.relative_path), "library_name");
        assert_eq!(parsed.group.as_deref(), Some("SHB"), "the name as written");
        let held = as_recorded(&parsed, &f);
        assert_eq!(held.group.as_deref(), Some("QxR"));
        assert_eq!(
            held.quality().map(Quality::label).as_deref(),
            Some("WEBDL-1080p")
        );
        assert!(held.proper && held.has_edition("Extended"));
        assert_eq!(
            held.audio_codec, parsed.audio_codec,
            "what the record does not say stays as parsed"
        );
        assert_eq!(held.title, parsed.title);

        // A record that says nothing changes nothing.
        f.arr_group = None;
        f.arr_quality = Some("Unknown".to_string());
        f.arr_edition = Some(String::new());
        f.arr_proper = false;
        assert_eq!(as_recorded(&parsed, &f), parsed);

        // The DirtyHippie spelling is passed through as recorded.
        f.arr_group = Some("60fps-DirtyHippie".to_string());
        assert_eq!(
            as_recorded(&parsed, &f).group.as_deref(),
            Some("60fps-DirtyHippie")
        );
    }

    #[test]
    fn arr_and_singularr_quality_names_compare() {
        assert!(same_quality("Bluray-1080p Remux", "Remux-1080p"));
        assert!(same_quality("Remux-2160p", "Remux-2160p"));
        assert!(same_quality("WEBDL-1080p", "webdl-1080p"));
        assert!(same_quality("DVD", "DVD-480p"));
        assert!(same_quality("SDTV", "HDTV-480p"));
        assert!(same_quality("BR-DISK", "br-disk"));
        assert!(!same_quality("WEBDL-1080p", "WEBRip-1080p"));
        assert!(!same_quality("Bluray-1080p", "Bluray-720p"));
        assert!(!same_quality("Bluray-1080p", "Remux-1080p"));
    }

    #[test]
    fn readings_differ_only_where_the_arr_recorded_something() {
        let arr = ArrReading {
            quality: Some("Bluray-1080p".into()),
            group: Some("SM737".into()),
            edition: Some(String::new()),
            custom_formats: vec![],
            score: Some(1550),
        };
        let same = SingularrReading {
            quality: Some("Bluray-1080p".into()),
            group: Some("sm737".into()),
            edition: None,
            score: Some(1550),
        };
        assert_eq!(compare(&arr, &same, None), (false, false, false, false));
        let other = SingularrReading {
            quality: Some("WEBDL-1080p".into()),
            group: None,
            edition: Some("Open Matte".into()),
            score: Some(1500),
        };
        assert_eq!(compare(&arr, &other, None), (true, true, true, true));
        assert_eq!(
            differs(compare(&arr, &other, None)),
            vec!["quality", "group", "edition", "score"]
        );
        // Sonarr records no edition and may record no score: neither is a difference.
        let sonarr = ArrReading {
            quality: Some("Unknown".into()),
            group: None,
            edition: None,
            custom_formats: vec![],
            score: None,
        };
        let no_group = SingularrReading {
            group: None,
            ..other.clone()
        };
        assert_eq!(
            compare(&sonarr, &no_group, None),
            (false, false, false, false)
        );
        // The arr kept the collective and dropped the encoder: the same group.
        let qxr = ArrReading {
            group: Some("QxR".into()),
            ..arr.clone()
        };
        let tigole = SingularrReading {
            group: Some("Tigole".into()),
            ..same.clone()
        };
        assert!(!compare(&qxr, &tigole, Some("QxR")).1);
        assert!(compare(&qxr, &tigole, None).1);
        // Edition wording does not count, the editions do.
        let cut = ArrReading {
            edition: Some("Director's Cut".into()),
            ..arr.clone()
        };
        let ours = SingularrReading {
            edition: Some("Director's Cut".into()),
            ..same.clone()
        };
        assert!(!compare(&cut, &ours, None).2);
        assert!(compare(&cut, &same, None).2);
    }

    #[test]
    fn an_assessment_scores_the_held_release_and_keeps_the_plain_parse() {
        let assessor = Assessor::new(Arc::new(ActivePolicy::builtin()), "test");
        assert!(assessor.stamp().ends_with(":test"));
        assert_eq!(
            assessor.lane_for("radarr", Some("HD-1080p")).as_deref(),
            Some("movies-hd")
        );
        assert_eq!(assessor.lane_for("radarr", Some("No Such Profile")), None);
        assert_eq!(assessor.lane_for("radarr", None), None);

        let mut f = file(
            Some("Movie.2004.Open.Matte.1080p.AMZN.WEB-DL.DDP5.1.H.265-PeruGuy"),
            None,
            LIBRARY,
        );
        f.arr_quality = Some("WEBDL-1080p".into());
        f.arr_group = Some("PeruGuy".into());
        f.arr_edition = Some("Open Matte".into());
        f.arr_custom_formats = vec!["Open Matte".into(), "Some Format Singularr Lacks".into()];
        f.arr_score = Some(9999);
        let a = assessor.assess(&f, &title(Some("movies-hd"), &[]));
        assert_eq!(a.name_source, "scene_name");
        assert_eq!(a.mq_quality.as_deref(), Some("WEBDL-1080p"));
        assert_eq!(a.mq_group.as_deref(), Some("PeruGuy"));
        assert_eq!(a.protections, vec!["open-matte"]);
        assert_eq!(
            (a.edition.key.as_str(), a.edition.marked),
            ("open-matte", true)
        );
        assert_eq!(assessor.default_edition(), "theatrical");
        // An edition only the arr's record names is the copy's edition too.
        let mut g = f.clone();
        g.scene_name = Some("Movie.2004.1080p.AMZN.WEB-DL.DDP5.1.H.265-PeruGuy".into());
        g.arr_edition = Some("Eagle Pictures Release".into());
        let b = assessor.assess(&g, &title(Some("movies-hd"), &[]));
        assert_eq!(b.edition.key, "other:eagle-pictures-release");
        g.arr_edition = Some(String::new());
        let c = assessor.assess(&g, &title(Some("movies-hd"), &[]));
        assert_eq!(
            (c.edition.key.as_str(), c.edition.marked),
            ("theatrical", false)
        );
        assert!(c.edition_rank > a.edition_rank);
        let verdict = a.verdict.as_ref().expect("the lane is in the policy");
        assert_eq!(Some(verdict.score), a.mq_score);
        assert_eq!(
            a.ladder_steps,
            Some(5),
            "movies-hd has five steps in the built-in policy"
        );
        let stored = serde_json::to_value(a.verdict_view()).unwrap();
        assert_eq!(
            (stored["rank"].as_u64(), stored["ladder_steps"].as_u64()),
            (Some(2), Some(5))
        );
        assert!(
            verdict
                .trace
                .iter()
                .any(|t| t.trait_id == "edition.open-matte")
        );
        assert_eq!(
            (a.diff_quality, a.diff_group, a.diff_edition),
            (false, false, false)
        );
        assert!(a.diff_score, "9999 is not what the policy gives");
        // The built-in policy maps radarr's Open Matte format to its trait, and has no
        // trait for the other format.
        assert_eq!(a.arr_only_formats, vec!["Some Format Singularr Lacks"]);
        assert!(a.singularr_only_traits.is_empty());
        assert_eq!(a.held, a.parsed, "the record agrees with the name here");

        // The record disagrees with the name: the score follows the record, the plain
        // parse keeps the name, and the shadow flags the difference.
        f.arr_group = Some("QxR".into());
        f.arr_edition = Some(String::new());
        f.arr_custom_formats = vec![];
        let b = assessor.assess(&f, &title(Some("movies-hd"), &[]));
        assert_eq!(b.parsed.group.as_deref(), Some("PeruGuy"));
        assert_eq!(b.held.group.as_deref(), Some("QxR"));
        assert!(b.diff_group);
        assert_eq!(
            b.singularr_only_traits,
            vec!["edition.open-matte"],
            "Singularr matched a trait whose format the arr did not"
        );

        // No lane, or a lane the policy does not have: parsed, but not scored.
        let unscored = assessor.assess(&f, &title(None, &[]));
        assert!(unscored.verdict.is_none() && unscored.mq_score.is_none() && !unscored.diff_score);
        assert!(
            assessor
                .assess(&f, &title(Some("no-such-lane"), &[]))
                .verdict
                .is_none()
        );
    }

    #[test]
    fn the_probe_of_the_file_scores_where_the_name_is_silent() {
        let assessor = Assessor::new(Arc::new(ActivePolicy::builtin()), "test");
        let mut f = file(Some("Movie.2004.1080p.BluRay-GRP"), None, LIBRARY);
        f.arr_group = Some("GRP".into());
        let silent = assessor.assess(&f, &title(Some("movies-hd"), &[]));
        assert_eq!(silent.mq_score, Some(0));
        f.media_info = Some(serde_json::json!({
            "videoCodec": "x265", "videoDynamicRangeType": "", "audioCodec": "EAC3 Atmos",
            "audioChannels": 5.1, "resolution": "1920x800"
        }));
        let probed = assessor.assess(&f, &title(Some("movies-hd"), &[]));
        let verdict = probed.verdict.unwrap();
        assert_eq!(verdict.score, 150, "HEVC and Atmos, both from the file");
        assert!(
            verdict
                .trace
                .iter()
                .all(|t| t.evidence.contains("from the file"))
        );
    }

    /// The OpenAPI descriptions in `core::schema` are written by hand. This keeps them in
    /// step with what the core serializes.
    #[test]
    fn the_api_descriptions_cover_every_field_the_core_serializes() {
        use utoipa::PartialSchema;
        fn described<T: PartialSchema>() -> Vec<String> {
            let schema = serde_json::to_value(T::schema()).unwrap();
            schema["properties"]
                .as_object()
                .map(|p| p.keys().cloned().collect())
                .unwrap_or_default()
        }
        fn serialized<T: serde::Serialize>(value: &T) -> Vec<String> {
            serde_json::to_value(value)
                .unwrap()
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default()
        }
        let assessor = Assessor::new(Arc::new(ActivePolicy::builtin()), "test");
        let a = assessor.assess(
            &file(
                Some("Movie.2004.Open.Matte.1080p.WEB-DL.HEVC-PeruGuy"),
                None,
                LIBRARY,
            ),
            &title(Some("movies-hd"), &[]),
        );
        let verdict = a.verdict.clone().unwrap();
        let policy = ActivePolicy::builtin();
        let lane = policy.policy.lane("movies-hd").unwrap();
        let decision = singularr_core::policy::decide(
            &policy.policy,
            lane,
            None,
            &a.parsed,
            &EvalContext::default(),
        );
        let cases: Vec<(&str, Vec<String>, Vec<String>)> = vec![
            (
                "ParsedRelease",
                serialized(&a.parsed),
                described::<crate::core::schema::ParsedReleaseDoc>(),
            ),
            (
                "ParseNote",
                serialized(&a.parsed.notes[0]),
                described::<crate::core::schema::ParseNoteDoc>(),
            ),
            (
                "Verdict",
                serialized(&verdict),
                described::<crate::core::schema::VerdictDoc>(),
            ),
            (
                "TraceItem",
                serialized(&verdict.trace[0]),
                described::<crate::core::schema::TraceItemDoc>(),
            ),
            (
                "Decision",
                serialized(&decision),
                described::<crate::core::schema::DecisionDoc>(),
            ),
        ];
        for (name, on_the_wire, in_the_docs) in cases {
            for field in &on_the_wire {
                assert!(
                    in_the_docs.contains(field),
                    "{name}.{field} is serialized by singularr-core but missing from core::schema"
                );
            }
        }
    }
}
