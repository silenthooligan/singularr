//! A real installation's rule set (a copy in `tests/data/installation-policy.toml`), loaded the way
//! the server loads it, and
//! the product's default, `rules/policy.toml`.
//!
//! The scores asserted here are the ones the installation's operations notes quote for real
//! releases, so a change to the rules that moves them is seen at once.

use serde_json::Value;
use singularr_core::policy::{Action, EvalContext, Policy, decide, evaluate};
use singularr_core::release::{ParsedRelease, parse};

fn policy() -> Policy {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/installation-policy.toml"
    ))
    .expect("tests/data/installation-policy.toml");
    Policy::from_toml(&text).expect("tests/data/installation-policy.toml loads")
}

fn arr_config() -> Value {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/arr_config.json"
    ))
    .expect("fixtures/arr_config.json");
    serde_json::from_str(&text).expect("arr_config.json is JSON")
}

const LANES: [(&str, &str, &str); 4] = [
    ("movies-hd", "radarr", "HD-1080p"),
    ("movies-uhd", "radarr", "UHD-4K"),
    ("shows-hd", "sonarr", "HD-1080p"),
    ("shows-uhd", "sonarr", "UHD-4K"),
];

fn score(policy: &Policy, lane: &str, name: &str) -> i32 {
    let lane = policy.lane(lane).expect("lane");
    evaluate(policy, lane, &parse(name), &EvalContext::default()).score
}

#[test]
fn every_lane_exists_with_the_arr_profile_thresholds() {
    let policy = policy();
    let config = arr_config();
    for (id, arr, profile_name) in LANES {
        let lane = policy.lane(id).unwrap_or_else(|| panic!("lane {id}"));
        assert_eq!(
            policy
                .lane_for_arr(arr, profile_name)
                .map(|l| l.id.as_str()),
            Some(id)
        );
        let profile = config[arr]["qualityProfiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == profile_name)
            .unwrap_or_else(|| panic!("{arr} profile {profile_name}"));
        assert_eq!(i64::from(lane.min_score), profile["minFormatScore"], "{id}");
        assert_eq!(
            i64::from(lane.upgrade_step),
            profile["minUpgradeFormatScore"],
            "{id}"
        );
        assert_eq!(
            i64::from(lane.cutoff_score),
            profile["cutoffFormatScore"],
            "{id}"
        );
        assert!(!lane.ladder.is_empty(), "{id}");
    }
    assert_eq!(policy.lanes.len(), LANES.len());
    assert_eq!(
        (
            policy.lane("movies-hd").unwrap().min_score,
            policy.lane("movies-hd").unwrap().upgrade_step,
            policy.lane("movies-hd").unwrap().cutoff_score
        ),
        (100, 75, 3000)
    );
}

/// Every scored custom format of every profile is carried by a trait with the same points, and
/// no lane scores anything the arr does not (the release-profile guards aside).
#[test]
fn lane_points_are_the_arr_scores() {
    let policy = policy();
    let config = arr_config();
    for (id, arr, profile_name) in LANES {
        let lane = policy.lane(id).unwrap();
        let profile = config[arr]["qualityProfiles"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == profile_name)
            .unwrap();
        let scores = profile["scores"].as_object().unwrap();
        for (format, points) in scores {
            let points = points.as_i64().unwrap();
            let traits = policy.traits_for_arr_format(arr, format);
            assert!(!traits.is_empty(), "{arr}:{format} has no trait");
            if points == 0 {
                continue;
            }
            // RM4K is merged into the 4K remaster trait, which takes the points once.
            for t in traits {
                assert_eq!(
                    lane.points.get(&t.id).copied().map(i64::from),
                    Some(points),
                    "{id}: {} should carry {arr}:{format}",
                    t.id
                );
            }
        }
        for (trait_id, points) in &lane.points {
            let t = policy.traits.iter().find(|t| &t.id == trait_id).unwrap();
            if t.arr_formats.is_empty() {
                assert!(*points <= -10000, "{id}: {trait_id} is a guard");
                continue;
            }
            let from_arr = t.arr_formats.iter().any(|f| {
                f.split_once(':').is_some_and(|(a, name)| {
                    a == arr && scores.get(name).and_then(Value::as_i64) == Some(i64::from(*points))
                })
            });
            assert!(from_arr, "{id}: {trait_id} = {points} is not an arr score");
        }
    }
    // Every custom format of every arr has a trait, scored or not.
    for arr in ["radarr", "sonarr"] {
        for format in config[arr]["customFormats"].as_array().unwrap() {
            let name = format["name"].as_str().unwrap();
            assert!(
                !policy.traits_for_arr_format(arr, name).is_empty(),
                "{arr}:{name} has no trait"
            );
        }
    }
}

#[test]
fn traits_are_written_for_people() {
    let policy = policy();
    for t in &policy.traits {
        for text in [&t.label, &t.why] {
            assert!(!text.contains('\u{2014}'), "{}: no long dashes", t.id);
            assert!(!text.trim().is_empty(), "{}", t.id);
        }
        assert!(t.why.ends_with('.'), "{}: `why` is a sentence", t.id);
        assert!(
            [
                "group", "service", "edition", "video", "audio", "penalty", "other"
            ]
            .contains(&t.category.as_str()),
            "{}: category {}",
            t.id,
            t.category
        );
        assert!(t.id.starts_with(&format!("{}.", t.category)), "{}", t.id);
    }
    for p in &policy.protections {
        assert!(!p.label.contains('\u{2014}') && !p.why.contains('\u{2014}'));
        assert!(p.why.ends_with('.'));
    }
}

#[test]
fn known_scores_in_the_1080p_movie_lane() {
    let policy = policy();
    let hd = |name: &str| score(&policy, "movies-hd", name);
    // Top web groups by service: NF 1100, PCOK 1150, AMZN 1200.
    assert_eq!(hd("Movie.2023.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX"), 1100);
    assert_eq!(hd("Movie.2023.1080p.PCOK.WEB-DL.DDP5.1.H.264-FLUX"), 1150);
    assert_eq!(hd("Movie.2023.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX"), 1200);
    assert_eq!(hd("Movie.2023.1080p.NF.WEB-DL.DDP5.1.H.264-NTb"), 1100);
    // The library's HEVC conversion: 600 from a web source, 700 from a disc.
    assert_eq!(
        hd("Movie (2012) {tmdb-1} [WEB-DL-1080p][Opus 5.1][HEVC HUSH]-[SHH].mkv"),
        600
    );
    assert_eq!(
        hd("Movie (2012) {tmdb-1} [BluRay-1080p][Opus 5.1][HEVC HUSH]-[SHH].mkv"),
        700
    );
    // The master encode sits at the cutoff.
    assert!(
        hd("Movie (2012) {tmdb-1} [BluRay-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv") >= 3000
    );
    // 48 Hrs: QxR 1500, 4K remaster 900 counted once, disc 100, x265 50, 5.1 50.
    assert_eq!(
        hd("48 Hrs 1982 RM4K 1080p BluRay x265 HEVC 10bit AAC 5 1 Tigole QxR"),
        2600
    );
    assert_eq!(
        hd("48 Hrs (1982) RM4K (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]"),
        2600
    );
    // Men in Black II: the SQS file 1800, Tigole's 1700.
    assert_eq!(
        hd("Men in Black II 2002 1080p UHD BluRay DD+ 5 1 DV HDR x265-SQS"),
        1800
    );
    assert_eq!(
        hd("Men in Black II (2002) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]"),
        1700
    );
    // A typical SM737 release: 1900, 2100 as a proper.
    assert_eq!(
        hd("Until Dawn 2025 UHD BluRay 1080p DD Atmos 5 1 DoVi HDR10 x265-SM737"),
        1900
    );
    assert_eq!(
        hd("10 Cloverfield Lane 2016 PROPER UHD BluRay 1080p DD Atmos 5 1 DoVi HDR10 x265-SM737"),
        2100
    );
    // The open matte MONOLITH web download that replaced the PeruGuy file: 1800.
    assert_eq!(
        hd(
            "Harry Potter and the Prisoner of Azkaban (2004) Open Matte (1080p AMZN WEB-DL x265 HEVC 10bit AAC 5.1 MONOLITH) [QxR]"
        ),
        1800
    );
    // Guards.
    assert!(hd("Movie.2023.1080p.BluRay.x264.AAC5.1-YTS.MX") < -10000);
    assert!(hd("Movie (2018) 1080p BluRay [HINDI-ENG-5.1] 10bit HEVC - PeruGuy") < 0);
    assert!(hd("Movie.2023.1080p.WEB-DL.DDP5.1.H.264-FLUX.exe") < -5000);
    // Cam (2018) is a web download and clears the minimum.
    assert_eq!(hd("Cam.2018.1080p.NF.WEB-DL.DDP5.1.x264-NTG"), 1100);
}

#[test]
fn known_scores_in_the_other_lanes() {
    let policy = policy();
    // UHD movies: a DirtyHippie web download 1210, its remux 1300.
    assert_eq!(
        score(
            &policy,
            "movies-uhd",
            "Home.Alone.1990.2160p.DV.HDR10Plus.Ai-Enhanced.HEVC.DDP5.1.MULTI-RIFE.4.25v2-60fps-DirtyHippie"
        ),
        1210
    );
    assert_eq!(
        score(
            &policy,
            "movies-uhd",
            "Home.Alone.1990.2160p.UHD.BluRay.REMUX.DV.HDR10Plus.HEVC.TrueHD.7.1.Atmos-60fps-DirtyHippie"
        ),
        1300
    );
    // TV at 1080p: a top web download with 5.1 is 1050, the library's conversion 550.
    let tv = |name: &str| score(&policy, "shows-hd", name);
    assert_eq!(tv("Show.S01E01.1080p.NF.WEB-DL.DDP5.1.H.264-NTb"), 1050);
    assert_eq!(
        tv("Game.of.Thrones.2011.S08E01.1080p.NF.WEB-DL.Opus.5.1.HEVC-SHH"),
        550
    );
    // Scores Sonarr recorded for these names.
    assert_eq!(
        tv("Billions.S07E01.Tower.of.London.1080p.10bit.AMZN.WEB-DL.DDP5.1.HEVC-Vyndros"),
        450
    );
    assert_eq!(
        tv("The.West.Wing.1999.S05E19.Proper.1080p.AMZN.WEB-DL.EAC3.2.0.x265-ImE"),
        1605
    );
    assert_eq!(
        tv("Outlander.2014.S08E07.1080p.AMZN.WEB-DL.EAC3.5.1.x265-Silence"),
        1650
    );
    assert_eq!(
        tv("Ghosts 2021 S02E03 1080p 10bit WEBRip 6CH x265 HEVC-PSA"),
        250
    );
    assert_eq!(
        tv("The Expanse S06E02 Azure Dragon 1080p WEBRip HDR DDP5 1 H265-d3g"),
        1050
    );
    assert!(tv("Those.Who.Kill.US.S01E01.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb") < 0);
}

fn tagged(tag: &str) -> EvalContext {
    EvalContext {
        title_tags: vec![tag.to_string()],
        ..Default::default()
    }
}

fn outcome(
    policy: &Policy,
    lane: &str,
    held: (&ParsedRelease, &EvalContext),
    candidate: &str,
) -> (Action, Option<String>) {
    let lane = policy.lane(lane).unwrap();
    let d = decide(policy, lane, Some(held), &parse(candidate), held.1);
    (d.action, d.blocked_by)
}

#[test]
fn protections() {
    let policy = policy();
    let plain = EvalContext::default();

    // (a) DirtyHippie: only its own remux.
    let held = parse(
        "Home.Alone.1990.2160p.DV.HDR10Plus.Ai-Enhanced.HEVC.DDP5.1.MULTI-RIFE.4.25v2-60fps-DirtyHippie",
    );
    let blocked = (Action::Hold, Some("dirtyhippie".to_string()));
    assert_eq!(
        outcome(
            &policy,
            "movies-uhd",
            (&held, &plain),
            "Home.Alone.1990.2160p.UHD.BluRay.REMUX.DV.HDR.HEVC.DTS-HD.MA.5.1-FraMeSToR"
        ),
        blocked
    );
    assert_eq!(
        outcome(
            &policy,
            "movies-uhd",
            (&held, &plain),
            "Home.Alone.1990.2160p.WEB-DL.DV.HDR10Plus.Ai-Enhanced.HEVC.TrueHD.7.1.Atmos-60fps-DirtyHippie"
        ),
        blocked
    );
    assert_eq!(
        outcome(
            &policy,
            "movies-uhd",
            (&held, &plain),
            "Home.Alone.1990.2160p.UHD.BluRay.REMUX.DV.HDR10Plus.HEVC.TrueHD.7.1.Atmos-60fps-DirtyHippie"
        ),
        (Action::Upgrade, None)
    );
    // A tagged title whose file the pipeline renamed.
    let renamed = parse(
        "Home Alone (1990) {tmdb-771} [WEB-DL-2160p][UHD][DV HDR10Plus][Opus 5.1][HEVC 10bit HUSH]-[SHT].mkv",
    );
    assert_eq!(
        outcome(
            &policy,
            "movies-uhd",
            (&renamed, &tagged("dirtyhippie")),
            "Home Alone (1990) {tmdb-771} [Remux-2160p][UHD][DV HDR10Plus][Opus 7.1][HEVC 10bit HUSH]-[SHR].mkv"
        ),
        blocked
    );

    // (b) Open matte: only another open matte release, by the held copy or by the tag.
    let held = parse(
        "Harry Potter and the Prisoner of Azkaban (2004) 1080p OpenMatte SDR 10bit HEVC WEB-DL DDP 5.1 - PeruGuy",
    );
    let blocked = (Action::Hold, Some("open-matte".to_string()));
    assert_eq!(
        outcome(
            &policy,
            "movies-hd",
            (&held, &plain),
            "Harry Potter and the Prisoner of Azkaban (2004) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]"
        ),
        blocked
    );
    assert_eq!(
        outcome(
            &policy,
            "movies-hd",
            (&held, &plain),
            "Harry Potter and the Prisoner of Azkaban (2004) Open Matte (1080p AMZN WEB-DL x265 HEVC 10bit AAC 5.1 MONOLITH) [QxR]"
        ),
        (Action::Upgrade, None)
    );
    let unlabeled =
        parse("The Ugly Truth (2009) {tmdb-1} [WEBDL-1080p][AAC 5.1][x265]-KONTRAST.mkv");
    assert_eq!(
        outcome(
            &policy,
            "movies-hd",
            (&unlabeled, &tagged("open-matte")),
            "The Ugly Truth (2009) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]"
        ),
        blocked
    );

    // (c) hush-keep: nothing replaces it, not even the library's own master encode.
    let held = parse(
        "Master.and.Commander.The.Far.Side.of.the.World.2003.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR",
    );
    assert_eq!(
        outcome(
            &policy,
            "movies-hd",
            (&held, &tagged("hush-keep")),
            "Master and Commander (2003) {tmdb-8619} [Remux-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv"
        ),
        (Action::Hold, Some("hush-keep".to_string()))
    );
    // The same pair with no tag: the master encode replaces the remux on score.
    assert_eq!(
        outcome(
            &policy,
            "movies-hd",
            (&held, &plain),
            "Master and Commander (2003) {tmdb-8619} [Remux-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv"
        ),
        (Action::Upgrade, None)
    );
}

/// (d) The tag-scoped release profile with ignored terms: a guard, so it also stops a first grab.
#[test]
fn release_profile_guards() {
    let policy = policy();
    let lane = policy.lane("movies-hd").unwrap();
    let mockbuster = parse("The Odyssey 2026 1080p WEB-DL DDP5 1 H 264-Kitsune");
    let grab = |ctx: &EvalContext| decide(&policy, lane, None, &mockbuster, ctx);
    let d = grab(&tagged("odyssey-mockbusters"));
    assert_eq!(d.action, Action::Hold);
    assert!(d.candidate.score <= -8000);
    assert_eq!(d.candidate.trace[0].trait_id, "penalty.odyssey-mockbusters");
    // Any other title takes a Kitsune release as the tier 1 web group it is.
    let d = grab(&EvalContext::default());
    assert_eq!((d.action, d.candidate.score), (Action::Grab, 1100));
}

/// The product's default policy: neutral, so it names no release group, no house rule and no
/// installation's arr.
#[test]
fn the_default_policy_is_neutral() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../rules/policy.toml"
    ))
    .expect("rules/policy.toml");
    let policy = Policy::from_toml(&text).expect("rules/policy.toml loads");
    for lane in ["movies-hd", "movies-uhd", "shows-hd", "shows-uhd"] {
        assert!(policy.lane(lane).is_some(), "lane {lane}");
    }
    assert!(policy.protections.is_empty(), "no house protections");
    for t in &policy.traits {
        assert!(
            !t.id.starts_with("group."),
            "{}: no release group tiers",
            t.id
        );
        assert!(t.when.title_tag.is_empty(), "{}: no title tags", t.id);
        assert!(t.why.ends_with('.'), "{}: `why` is a sentence", t.id);
    }
    // Every trait a lane scores exists, and the lanes score what a person expects.
    let movie = |name: &str| score(&policy, "movies-hd", name);
    assert!(
        movie("Movie.2010.1080p.BluRay.REMUX.AVC.TrueHD.7.1.Atmos-GRP")
            > movie("Movie.2010.1080p.WEB-DL.DDP5.1.H.264-GRP")
    );
    assert!(movie("Movie.2010.HDCAM.x264-GRP") < 0);
    let uhd = |name: &str| score(&policy, "movies-uhd", name);
    assert!(
        uhd("Movie.2010.2160p.UHD.BluRay.REMUX.DV.HDR10.HEVC.TrueHD.7.1.Atmos-GRP")
            > uhd("Movie.2010.2160p.WEB-DL.SDR.DDP5.1.H.265-GRP")
    );
}
