use super::*;
use crate::release::parse;

const POLICY: &str = r#"
name = "test"
version = "1"

[[traits]]
id = "group.top"
label = "Top encode groups"
why = "The best 1080p encodes"
category = "group"
when = { any = [{ group = ["Tigole", "MONOLITH"] }, { collective = ["QxR"] }] }

[[traits]]
id = "group.web"
label = "Top web groups"
why = "Untouched streams from the groups that tag them right"
category = "group"
when = { group = ["NTb", "FLUX"], source = ["WebDl", "WebRip"] }

[[traits]]
id = "group.final"
label = "The library's master encode"
why = "Nothing better will come"
category = "group"
when = { group = ["SHR"] }

[[traits]]
id = "edition.open-matte"
label = "Open Matte"
why = "Tie-breaker only"
category = "edition"
when = { edition = ["Open Matte"] }

[[traits]]
id = "audio.surround"
label = "5.1 audio"
why = "Surround beats stereo"
category = "audio"
when = { channels = ["5.1"] }

[[lanes]]
id = "hd-1080p"
label = "HD-1080p"
ladder = [["Remux-1080p"], ["Bluray-1080p", "WEBDL-1080p", "WEBRip-1080p"]]
min_score = 50
upgrade_step = 75
cutoff_score = 3000
points = { "group.top" = 1500, "group.web" = 1000, "group.final" = 3000, "edition.open-matte" = 100, "audio.surround" = 50 }

[[lanes]]
id = "any"
label = "Any"
ladder = [["Bluray-1080p Remux"], ["Bluray-1080p"], ["WEBDL-1080p", "WEBRip-1080p"], ["DVD"]]
cutoff_quality = "Bluray-1080p"
cutoff_score = 10
points = { "group.top" = 30, "audio.surround" = 5 }

[[protections]]
id = "dirtyhippie"
label = "a DirtyHippie copy is replaced only by a DirtyHippie remux"
why = "The owner keeps the 60 fps version unless its own group releases a remux."
tags = ["dirtyhippie"]
current = { group = ["DirtyHippie"] }
allow = { group = ["DirtyHippie"], source = ["Remux"] }

[[protections]]
id = "open-matte"
label = "an open matte copy is replaced only by another open matte copy"
why = "The owner keeps the taller frame."
tags = ["open-matte"]
current = { edition = ["Open Matte"], not = { group = ["DirtyHippie"] } }
allow = { edition = ["Open Matte"] }

[[protections]]
id = "keep"
label = "this title is kept exactly as it is"
why = "The tag hush-keep says so."
tags = ["hush-keep"]
"#;

fn policy() -> Policy {
    Policy::from_toml(POLICY).unwrap()
}

fn ctx() -> EvalContext {
    EvalContext::default()
}

fn tagged(tag: &str) -> EvalContext {
    EvalContext {
        title_tags: vec![tag.to_string()],
        ..Default::default()
    }
}

/// Decide for `candidate` against `held` in the HD lane with plain contexts.
fn hd(held: Option<&str>, candidate: &str) -> Decision {
    let policy = policy();
    let lane = policy.lane("hd-1080p").unwrap();
    let c = ctx();
    let held = held.map(parse);
    decide(
        &policy,
        lane,
        held.as_ref().map(|h| (h, &c)),
        &parse(candidate),
        &c,
    )
}

#[test]
fn scores_with_a_trace() {
    let policy = policy();
    let lane = policy.lane("HD-1080p").unwrap();
    let v = evaluate(
        &policy,
        lane,
        &parse("Movie.2004.Open.Matte.1080p.AMZN.WEB-DL.x265-MONOLITH"),
        &ctx(),
    );
    assert_eq!(v.score, 1600);
    assert_eq!(v.rank, Some(1));
    assert_eq!(v.trace[0].trait_id, "group.top");
    assert_eq!(v.trace[0].evidence, "group MONOLITH");
    assert_eq!(v.quality.as_deref(), Some("WEBDL-1080p"));
    assert!(v.matched_unscored.is_empty());
}

#[test]
fn a_collective_scores_like_its_members() {
    let policy = policy();
    let lane = policy.lane("hd-1080p").unwrap();
    let v = evaluate(
        &policy,
        lane,
        &parse("The Time Machine (2002) (1080p BluRay x265 10bit EAC3 5.1 Celdra) [QxR]"),
        &ctx(),
    );
    assert_eq!(v.score, 1550);
    assert_eq!(v.trace[0].evidence, "collective QxR");
}

#[test]
fn a_lane_that_scores_an_unknown_trait_is_refused() {
    let bad = POLICY.replace("\"group.top\" = 1500", "\"group.nope\" = 1");
    assert!(matches!(
        Policy::from_toml(&bad),
        Err(PolicyError::UnknownTrait { .. })
    ));
    let bad = POLICY.replace(
        "cutoff_quality = \"Bluray-1080p\"",
        "cutoff_quality = \"HDTV-720p\"",
    );
    assert!(matches!(
        Policy::from_toml(&bad),
        Err(PolicyError::CutoffNotOnLadder { .. })
    ));
    let bad = POLICY.replace("id = \"keep\"", "id = \"open-matte\"");
    assert!(matches!(
        Policy::from_toml(&bad),
        Err(PolicyError::Duplicate(_))
    ));
}

#[test]
fn the_ladder_reads_the_arrs_quality_names() {
    let policy = policy();
    let lane = policy.lane("any").unwrap();
    let rank = |name: &str| parse(name).quality().and_then(|q| lane.rank(q));
    assert_eq!(rank("Show.S01E01.1080p.BluRay.REMUX.AVC-GRP"), Some(0));
    assert_eq!(rank("Show.S01E01.1080p.BluRay.x265-GRP"), Some(1));
    assert_eq!(rank("Show.S01E01.1080p.WEBRip.x265-GRP"), Some(2));
    assert_eq!(rank("Show.S01E01.DVDRip.x264-GRP"), Some(3));
    assert_eq!(rank("Show.S01E01.720p.HDTV.x264-GRP"), None);
}

#[test]
fn nothing_held_is_a_grab() {
    let d = hd(None, "Movie.2019.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb");
    assert_eq!(d.action, Action::Grab);
    assert_eq!(d.candidate.score, 1050);
    assert!(d.held.is_none() && d.blocked_by.is_none());
    assert_eq!(d.reasons, ["Nothing is held for this title in this lane."]);
}

#[test]
fn below_the_minimum_score_is_never_taken() {
    let d = hd(None, "Movie.2019.1080p.WEBRip.x264-NOBODY");
    assert_eq!(d.action, Action::Hold);
    assert_eq!(d.reasons, ["It scores 0, below the lane's minimum of 50."]);
}

#[test]
fn a_better_quality_step_is_an_upgrade() {
    let d = hd(
        Some("Movie.2019.1080p.BluRay.DDP5.1.x265-Tigole"),
        "Movie.2019.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR",
    );
    assert_eq!(d.action, Action::Upgrade);
    assert_eq!(
        d.reasons,
        ["Better quality: Remux-1080p over Bluray-1080p."]
    );
    // The other way round it is a lower step, whatever the score.
    let d = hd(
        Some("Movie.2019.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR"),
        "Movie.2019.1080p.BluRay.DDP5.1.x265-Tigole",
    );
    assert_eq!(d.action, Action::Hold);
    assert_eq!(
        d.reasons,
        ["Lower quality: Bluray-1080p against the held Remux-1080p."]
    );
}

#[test]
fn the_same_quality_needs_the_upgrade_step() {
    // 1050 against 1000: 50 more, the step is 75.
    let d = hd(
        Some("Movie.2019.1080p.NF.WEB-DL.DD2.0.H.264-FLUX"),
        "Movie.2019.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX",
    );
    assert_eq!(d.action, Action::Hold);
    assert_eq!(
        d.reasons,
        ["Same quality and 1050 against the held 1000: it must gain at least 75."]
    );
    // 1150 against 1050: 100 more.
    let d = hd(
        Some("Movie.2019.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX"),
        "Movie.2019.Open.Matte.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX",
    );
    assert_eq!(d.action, Action::Upgrade);
    assert_eq!(d.reasons, ["Same quality, better score: 1150 over 1050."]);
    assert_eq!(d.held.as_ref().map(|v| v.score), Some(1050));
    // Web and disc share a step in this lane, so the score decides between them too.
    let d = hd(
        Some("Movie.2019.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX"),
        "Movie.2019.1080p.BluRay.DDP5.1.x265-Tigole",
    );
    assert_eq!(d.action, Action::Upgrade);
}

#[test]
fn a_copy_at_the_cutoff_score_is_not_replaced_at_the_same_quality() {
    let d = hd(
        Some("Movie (2019) {tmdb-1} [BluRay-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv"),
        "Movie (2019) {tmdb-1} [Open Matte][BluRay-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv",
    );
    assert_eq!(d.action, Action::Hold);
    assert_eq!(
        d.reasons,
        ["The held copy already scores 3050, at or past the cutoff of 3000."]
    );
}

#[test]
fn a_cutoff_quality_makes_a_copy_final() {
    let policy = policy();
    let lane = policy.lane("any").unwrap();
    let c = ctx();
    let go = |held: &str, candidate: &str| {
        let held = parse(held);
        decide(&policy, lane, Some((&held, &c)), &parse(candidate), &c)
    };
    let remux = "Show.S01E01.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-GRP";
    // Held at the cutoff quality with the cutoff score: a remux no longer replaces it.
    let d = go("Show.S01E01.1080p.BluRay.x265.AAC.5.1-Tigole", remux);
    assert_eq!(d.action, Action::Hold);
    assert!(d.reasons[0].contains("cutoff quality (Bluray-1080p) and cutoff score (10)"));
    // At the cutoff quality but under the cutoff score: quality still upgrades.
    let d = go("Show.S01E01.1080p.BluRay.x265.AAC.5.1-GRP", remux);
    assert_eq!(d.action, Action::Upgrade);
    // Under the cutoff quality: quality upgrades even with a full score.
    let d = go(
        "Show.S01E01.1080p.WEBRip.x265.AAC.5.1-Tigole",
        "Show.S01E01.1080p.BluRay.x265-GRP",
    );
    assert_eq!(d.action, Action::Upgrade);
}

#[test]
fn a_candidate_outside_the_lane_is_held() {
    let held = "Movie.2019.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX";
    for (candidate, reason) in [
        (
            "Movie.2019.2160p.UHD.BluRay.x265-Tigole",
            "HD-1080p does not take Bluray-2160p.",
        ),
        (
            "Movie.2019.720p.BluRay.x264-Tigole",
            "HD-1080p does not take Bluray-720p.",
        ),
        (
            "Movie.2019.1080p.x265-Tigole",
            "The name does not give a source and a resolution, so HD-1080p cannot place it.",
        ),
    ] {
        for held in [None, Some(held)] {
            let d = hd(held, candidate);
            assert_eq!(d.action, Action::Hold, "{candidate}");
            assert_eq!(d.reasons, [reason], "{candidate}");
            assert_eq!(d.candidate.rank, None);
        }
    }
    // A held copy from outside the lane gives way to one inside it.
    let d = hd(
        Some("Movie.2019.720p.BluRay.x264-Tigole"),
        "Movie.2019.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX",
    );
    assert_eq!(d.action, Action::Upgrade);
    assert_eq!(
        d.reasons,
        ["The held copy's quality is outside HD-1080p; this one is inside."]
    );
}

#[test]
fn a_prerelease_source_is_held() {
    for candidate in [
        "Movie.2024.1080p.TS.x264-Tigole",
        "Movie.2024.1080p.HDCAM.x264-Tigole",
        "Movie.2024.TELESYNC.x264-Tigole",
        "Movie.2024.1080p.DVDSCR.x264-Tigole",
    ] {
        let d = hd(None, candidate);
        assert_eq!(d.action, Action::Hold, "{candidate}");
        assert!(
            d.reasons[0].contains("is a pre-release source"),
            "{candidate}: {:?}",
            d.reasons
        );
    }
    // The movie called Cam is not one of them.
    let d = hd(None, "Cam.2018.1080p.NF.WEB-DL.DDP5.1.x264-NTb");
    assert_eq!(d.action, Action::Grab);
}

#[test]
fn protection_by_the_held_copy_holds_and_allows() {
    let held = "Movie.2004.Open.Matte.1080p.WEB-DL.HEVC-PeruGuy";
    let plain = hd(Some(held), "Movie.2004.1080p.BluRay.x265-Tigole");
    assert_eq!(
        (plain.action, plain.blocked_by.as_deref()),
        (Action::Hold, Some("open-matte"))
    );
    assert_eq!(
        plain.reasons,
        [
            "Protected: an open matte copy is replaced only by another open matte copy. The owner keeps the taller frame."
        ]
    );
    let open = hd(
        Some(held),
        "Movie.2004.Open.Matte.1080p.WEB-DL.x265-MONOLITH",
    );
    assert_eq!((open.action, open.blocked_by), (Action::Upgrade, None));
}

#[test]
fn protection_by_tag_holds_and_allows() {
    let policy = policy();
    let lane = policy.lane("hd-1080p").unwrap();
    // The title is tagged open-matte but the held file's name does not say it.
    let held = parse("Movie (2004) {tmdb-1} [WEBDL-1080p][AAC 5.1][x265]-PeruGuy.mkv");
    let c = tagged("open-matte");
    let go = |candidate: &str| decide(&policy, lane, Some((&held, &c)), &parse(candidate), &c);
    assert_eq!(
        go("Movie.2004.1080p.BluRay.x265-Tigole")
            .blocked_by
            .as_deref(),
        Some("open-matte")
    );
    assert_eq!(
        go("Movie.2004.Open.Matte.1080p.WEB-DL.x265-MONOLITH").action,
        Action::Upgrade
    );
    // The tag on the held copy's context counts as well.
    let d = decide(
        &policy,
        lane,
        Some((&held, &c)),
        &parse("Movie.2004.1080p.BluRay.x265-Tigole"),
        &ctx(),
    );
    assert_eq!(d.blocked_by.as_deref(), Some("open-matte"));
}

#[test]
fn dirtyhippie_is_replaced_only_by_its_own_remux() {
    let policy = policy();
    let lane = policy.lane("hd-1080p").unwrap();
    let c = ctx();
    let held = parse("Movie.1990.1080p.WEB-DL.60fps.HEVC.DDP5.1-DirtyHippie");
    let go = |held: &ParsedRelease, c: &EvalContext, candidate: &str| {
        decide(&policy, lane, Some((held, c)), &parse(candidate), c)
    };
    for candidate in [
        "Movie.1990.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR",
        "Movie.1990.1080p.BluRay.x265.AAC.5.1-Tigole",
        "Movie.1990.1080p.WEB-DL.60fps.HEVC.DDP5.1-DirtyHippie",
    ] {
        let d = go(&held, &c, candidate);
        assert_eq!(
            (d.action, d.blocked_by.as_deref()),
            (Action::Hold, Some("dirtyhippie")),
            "{candidate}"
        );
    }
    let d = go(
        &held,
        &c,
        "Movie.1990.1080p.BluRay.REMUX.60fps.HEVC.TrueHD.5.1-DirtyHippie",
    );
    assert_eq!((d.action, d.blocked_by), (Action::Upgrade, None));
    // A DirtyHippie file the pipeline renamed SHR is covered by the tag on the title.
    let renamed =
        parse("Movie (1990) {tmdb-1} [WEB-DL-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHT].mkv");
    let t = tagged("dirtyhippie");
    let d = go(&renamed, &t, "Movie.1990.1080p.BluRay.x265.AAC.5.1-Tigole");
    assert_eq!(d.blocked_by.as_deref(), Some("dirtyhippie"));
    // An open matte DirtyHippie file keeps only the DirtyHippie rule: its remux need not say
    // open matte.
    let both = parse("Movie.1990.Open.Matte.1080p.WEB-DL.60fps.HEVC.DDP5.1-DirtyHippie");
    let d = go(
        &both,
        &c,
        "Movie.1990.1080p.BluRay.REMUX.60fps.HEVC.TrueHD.5.1-DirtyHippie",
    );
    assert_eq!((d.action, d.blocked_by), (Action::Upgrade, None));
}

#[test]
fn a_kept_title_is_never_replaced() {
    let policy = policy();
    let lane = policy.lane("hd-1080p").unwrap();
    let c = tagged("hush-keep");
    let held = parse("Movie.2003.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-GRP");
    for candidate in [
        "Movie (2003) {tmdb-1} [Remux-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv",
        "Movie.2003.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR",
    ] {
        let d = decide(&policy, lane, Some((&held, &c)), &parse(candidate), &c);
        assert_eq!(
            (d.action, d.blocked_by.as_deref()),
            (Action::Hold, Some("keep")),
            "{candidate}"
        );
    }
    // With nothing held there is nothing to keep.
    let d = decide(
        &policy,
        lane,
        None,
        &parse("Movie.2003.1080p.BluRay.x265.AAC.5.1-Tigole"),
        &c,
    );
    assert_eq!(d.action, Action::Grab);
}

/// The test policy without its open matte protection: the edition rule alone.
fn without_open_matte_protection() -> Policy {
    let mut p = policy();
    p.protections.retain(|x| x.id != "open-matte");
    p
}

#[test]
fn open_matte_is_kept_by_the_edition_rule_alone() {
    let policy = without_open_matte_protection();
    let editions = crate::edition::EditionList::shipped();
    let lane = policy.lane("hd-1080p").unwrap();
    let c = ctx();
    let held = parse("Movie.2004.Open.Matte.1080p.WEB-DL.HEVC-PeruGuy");
    let go = |candidate: &str| {
        decide_edition(
            &policy,
            editions,
            lane,
            &[(&held, &c)],
            None,
            &parse(candidate),
            &c,
        )
    };
    // A standard copy from a better tier is another edition, which the title does not want.
    let plain = go("Movie.2004.1080p.BluRay.x265-Tigole");
    assert_eq!(plain.edition.key, "theatrical");
    assert_eq!(
        (
            plain.decision.action,
            plain.decision.blocked_by.as_deref(),
            plain.against
        ),
        (Action::Hold, None, None)
    );
    assert_eq!(
        plain.decision.reasons,
        [
            "Theatrical is not an edition this title wants: it holds Open Matte. A copy competes only with the held copy of its own edition, so it replaces none of them."
        ]
    );
    // Another open matte copy competes with the held one, as the protection allowed.
    let open = go("Movie.2004.Open.Matte.1080p.WEB-DL.x265-MONOLITH");
    assert_eq!(
        (
            open.decision.action,
            open.against,
            open.edition.key.as_str()
        ),
        (Action::Upgrade, Some(0), "open-matte")
    );
    // The protection in the policy file still says the same through `decide`.
    let with = self::policy();
    let lane = with.lane("hd-1080p").unwrap();
    let d = decide(
        &with,
        lane,
        Some((&held, &c)),
        &parse("Movie.2004.1080p.BluRay.x265-Tigole"),
        &c,
    );
    assert_eq!(d.blocked_by.as_deref(), Some("open-matte"));
}

#[test]
fn each_edition_is_upgraded_on_its_own() {
    let policy = policy();
    let editions = crate::edition::EditionList::shipped();
    let lane = policy.lane("hd-1080p").unwrap();
    let c = ctx();
    let dc = parse("Movie.2019.Directors.Cut.1080p.BluRay.DDP5.1.x265-Tigole");
    let theatrical = parse("Movie.2019.1080p.NF.WEB-DL.DDP5.1.H.264-FLUX");
    let held = [(&dc, &c), (&theatrical, &c)];
    let go = |candidate: &str, wanted: Option<&[String]>| {
        decide_edition(
            &policy,
            editions,
            lane,
            &held,
            wanted,
            &parse(candidate),
            &c,
        )
    };
    // A better theatrical replaces the theatrical copy, never the director's cut.
    let d = go(
        "Movie.2019.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR",
        None,
    );
    assert_eq!((d.decision.action, d.against), (Action::Upgrade, Some(1)));
    assert_eq!(d.decision.held.as_ref().map(|v| v.score), Some(1050));
    // A director's cut remux replaces the director's cut.
    let d = go(
        "Movie.2019.Directors.Cut.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR",
        None,
    );
    assert_eq!(
        (d.decision.action, d.against, d.edition.label.as_str()),
        (Action::Upgrade, Some(0), "Director's Cut")
    );
    // An extended cut is not wanted until the owner says so; then it is a first grab.
    let ext = "Movie.2019.Extended.1080p.BluRay.DDP5.1.x265-Tigole";
    assert_eq!(go(ext, None).decision.action, Action::Hold);
    let wanted = ["extended".to_string(), "theatrical".to_string()];
    let d = go(ext, Some(&wanted));
    assert_eq!((d.decision.action, d.against), (Action::Grab, None));
    assert_eq!(
        d.decision.reasons,
        ["Nothing of the Extended edition is held in this lane."]
    );
    // A wanted list without the director's cut leaves it alone.
    let only_theatrical = ["theatrical".to_string()];
    let d = go(
        "Movie.2019.DC.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR",
        Some(&only_theatrical),
    );
    assert_eq!(d.decision.action, Action::Hold);
    // With nothing held, any edition may be the first.
    let d = decide_edition(&policy, editions, lane, &[], None, &parse(ext), &c);
    assert_eq!(d.decision.action, Action::Grab);
    assert_eq!(
        d.decision.reasons,
        ["Nothing is held for this title in this lane."]
    );
    // Protections still apply inside an edition.
    let kept = tagged("hush-keep");
    let held = [(&theatrical, &kept)];
    let d = decide_edition(
        &policy,
        editions,
        lane,
        &held,
        None,
        &parse("Movie.2019.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-FraMeSToR"),
        &kept,
    );
    assert_eq!(d.decision.blocked_by.as_deref(), Some("keep"));
}

fn when(toml: &str) -> Condition {
    #[derive(Deserialize)]
    struct Holder {
        when: Condition,
    }
    toml::from_str::<Holder>(&format!("when = {toml}"))
        .unwrap()
        .when
}

#[test]
fn condition_fields() {
    let c = ctx();
    let hit = |cond: &str, name: &str| when(cond).matches(&parse(name), &c);
    let scene = "Movie.2019.Encore.Cut.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb";
    assert_eq!(
        hit(r#"{ channels = ["5.1"] }"#, scene).as_deref(),
        Some("5.1 audio")
    );
    assert_eq!(hit(r#"{ channels = ["7.1"] }"#, scene), None);
    assert_eq!(
        hit(r#"{ edition = ["* Cut"] }"#, scene).as_deref(),
        Some("edition Encore Cut")
    );
    assert_eq!(hit(r#"{ edition = ["Extended"] }"#, scene), None);
    assert_eq!(
        hit(
            r#"{ collective = ["qxr"] }"#,
            "Movie (2019) (1080p BluRay x265 Tigole) [QxR]"
        )
        .as_deref(),
        Some("collective QxR")
    );
    assert_eq!(hit(r#"{ collective = ["QxR"] }"#, scene), None);
    assert_eq!(
        hit(
            r#"{ language = ["Hindi"] }"#,
            "Movie (2018) 1080p [HINDI-ENG-5.1] 10bit HEVC - GRP"
        )
        .as_deref(),
        Some("language Hindi")
    );
    assert_eq!(hit(r#"{ language = ["Hindi"] }"#, scene), None);
    assert_eq!(
        hit(
            r#"{ proper_or_repack = true }"#,
            "Movie.2019.REPACK.1080p.WEB-DL.x264-GRP"
        )
        .as_deref(),
        Some("repack")
    );
    assert_eq!(
        hit(
            r#"{ service = ["AMZN"], source = ["WebDl", "WebRip"] }"#,
            scene
        )
        .as_deref(),
        Some("source WEBDL, service AMZN")
    );
    // "not" and "any" nest.
    let cond = r#"{ hdr = ["DolbyVision"], any = [{ hdr = ["Hdr10"] }, { hdr = ["Hdr"] }], not = { hdr = ["Hdr10Plus"] } }"#;
    assert!(hit(cond, "Movie.2019.2160p.WEB-DL.DV.HDR10.H.265-GRP").is_some());
    assert!(hit(cond, "Movie.2019.2160p.WEB-DL.DV.HDR.H.265-GRP").is_some());
    assert!(hit(cond, "Movie.2019.2160p.WEB-DL.DV.HDR10Plus.H.265-GRP").is_none());
    assert!(hit(cond, "Movie.2019.2160p.WEB-DL.HDR10.H.265-GRP").is_none());
}

#[test]
fn title_tags_and_languages_come_from_the_context() {
    let cond = when(r#"{ title_tag = ["odyssey-mockbusters"], group = ["BONE"] }"#);
    let r = parse("The Odyssey 2026 1080p WEB-DL HEVC x265 5.1 BONE");
    assert_eq!(cond.matches(&r, &ctx()), None);
    assert_eq!(
        cond.matches(&r, &tagged("Odyssey-Mockbusters")).as_deref(),
        Some("group BONE, title tag Odyssey-Mockbusters")
    );

    let not_original = when("{ original_language = false }");
    let korean = EvalContext {
        original_language: Some("Korean".into()),
        ..Default::default()
    };
    let dubbed = parse("Movie.2023.1080p.BluRay.English.DDP5.1.x265-GRP");
    let native = parse("Movie.2023.1080p.BluRay.KOREAN.DDP5.1.x265-GRP");
    let multi = parse("Movie.2023.1080p.BluRay.MULTI.DDP5.1.x265-GRP");
    let silent = parse("Movie.2023.1080p.BluRay.DDP5.1.x265-GRP");
    assert_eq!(
        not_original.matches(&dubbed, &korean).as_deref(),
        Some("languages English, original Korean")
    );
    assert_eq!(not_original.matches(&native, &korean), None);
    assert_eq!(not_original.matches(&multi, &korean), None);
    // A name that states no language is not held against the release.
    assert_eq!(not_original.matches(&silent, &korean), None);
    // Nor is a release for a title whose original language is unknown.
    assert_eq!(not_original.matches(&dubbed, &ctx()), None);
    // For a held file the arr's language record stands in for the silent name.
    let held = EvalContext {
        languages: vec!["English".into()],
        ..korean.clone()
    };
    assert_eq!(
        not_original.matches(&silent, &held).as_deref(),
        Some("languages English (from the file), original Korean")
    );
    assert!(
        when("{ original_language = true }")
            .matches(&native, &korean)
            .is_some()
    );
}

#[test]
fn media_info_fills_in_where_the_name_is_silent() {
    let probed = EvalContext {
        media: Some(MediaInfo::from_arr(
            "x265",
            "DV HDR10",
            "EAC3 Atmos",
            Some(5.1),
            "1920x800",
        )),
        ..Default::default()
    };
    assert_eq!(
        probed.media,
        Some(MediaInfo {
            resolution: Some(Resolution::R1080),
            video_codec: Some(VideoCodec::H265),
            hdr: Some(vec![HdrFormat::DolbyVision, HdrFormat::Hdr10]),
            audio_codec: Some(AudioCodec::Eac3),
            audio_channels: Some("5.1".into()),
            atmos: Some(true),
        })
    );
    let silent = parse("Bad.Monkey.S01E02.WEB-DL-ELiTE");
    let loud = parse("Bad.Monkey.S01E02.720p.WEB-DL.SDR.AAC2.0.H.264-ELiTE");
    for (cond, from_file, evidence) in [
        (
            r#"{ channels = ["5.1"] }"#,
            true,
            "5.1 audio (from the file)",
        ),
        (
            r#"{ audio_codec = ["Eac3"], atmos = true }"#,
            true,
            "Eac3 (from the file), Atmos (from the file)",
        ),
        (
            r#"{ video_codec = ["H265"] }"#,
            true,
            "H265 (from the file)",
        ),
        (
            r#"{ hdr = ["DolbyVision"] }"#,
            true,
            "DolbyVision (from the file)",
        ),
        (
            r#"{ resolution = ["1080p"] }"#,
            true,
            "1080p (from the file)",
        ),
        (r#"{ sdr = false }"#, true, "always"),
    ] {
        let cond = when(cond);
        // Without a probe the silent name matches nothing.
        assert_eq!(cond.matches(&silent, &ctx()), None, "{cond:?}");
        assert_eq!(
            cond.matches(&silent, &probed).as_deref(),
            from_file.then_some(evidence),
            "{cond:?}"
        );
        // A name that speaks is believed over the probe.
        assert_eq!(cond.matches(&loud, &probed), None, "{cond:?}");
    }
    assert!(when("{ sdr = true }").matches(&silent, &ctx()).is_some());
    assert!(when("{ sdr = true }").matches(&silent, &probed).is_none());

    // HDR formats add up: the name says Dolby Vision with HDR10, the file also carries HDR10+.
    let plus = EvalContext {
        media: Some(MediaInfo::from_arr(
            "x265",
            "DV HDR10Plus",
            "EAC3",
            Some(5.1),
            "1920x1080",
        )),
        ..Default::default()
    };
    let named =
        parse("A River Runs Through It 1992 UHD BluRay 1080p DDP 5 1 DoVi HDR10 x265-SM737");
    let dv_hdr10 = when(
        r#"{ hdr = ["DolbyVision"], any = [{ hdr = ["Hdr10"] }, { hdr = ["Hdr"] }], not = { hdr = ["Hdr10Plus"] } }"#,
    );
    assert!(dv_hdr10.matches(&named, &ctx()).is_some());
    assert!(dv_hdr10.matches(&named, &plus).is_none());
    assert_eq!(
        when(r#"{ hdr = ["Hdr10Plus"] }"#)
            .matches(&named, &plus)
            .as_deref(),
        Some("Hdr10Plus (from the file)")
    );
    assert_eq!(
        when(r#"{ hdr = ["DolbyVision"] }"#)
            .matches(&named, &plus)
            .as_deref(),
        Some("DolbyVision")
    );
    // The language a name states is a fact about the name. The file's record does not stand in.
    let dual = EvalContext {
        languages: vec!["English".into(), "Hindi".into()],
        ..Default::default()
    };
    assert_eq!(
        when(r#"{ language = ["Hindi"] }"#).matches(&named, &dual),
        None
    );

    // The arr's spellings.
    let m = MediaInfo::from_arr("h264", "", "DTS-HD MA", Some(2.0), "3840x1600");
    assert_eq!(
        (
            m.video_codec,
            m.hdr.as_deref(),
            m.audio_codec,
            m.audio_channels.as_deref(),
            m.atmos
        ),
        (
            Some(VideoCodec::H264),
            Some(&[][..]),
            Some(AudioCodec::DtsHdMa),
            Some("2.0"),
            Some(false)
        )
    );
    assert_eq!(m.resolution, Some(Resolution::R2160));
    let m = MediaInfo::from_arr("", "", "", None, "");
    assert_eq!(m, MediaInfo::default());
}

#[test]
fn simulate_reports_what_would_change_and_why() {
    let before = policy();
    // Open matte becomes a real bonus, surround loses its points, the web tier gains a group.
    let edited = POLICY
        .replace(
            "\"edition.open-matte\" = 100",
            "\"edition.open-matte\" = 1100",
        )
        .replace(", \"audio.surround\" = 50 }", " }")
        .replace(
            "group = [\"NTb\", \"FLUX\"]",
            "group = [\"NTb\", \"FLUX\", \"CMRG\"]",
        );
    let after = Policy::from_toml(&edited).unwrap();
    let item = |name: &str, lane: &str| (parse(name), ctx(), lane.to_string());
    let items = vec![
        item(
            "Movie.2004.Open.Matte.1080p.AMZN.WEB-DL.DDP5.1.x265-MONOLITH",
            "hd-1080p",
        ),
        item("Movie.2019.1080p.NF.WEB-DL.DDP5.1.H.264-CMRG", "hd-1080p"),
        item("Movie.2019.1080p.NF.WEB-DL.DD2.0.H.264-FLUX", "hd-1080p"),
        item("Show.S01E01.1080p.BluRay.x265.AAC.5.1-Tigole", "any"),
        item("Movie.2019.1080p.BluRay.x265-Tigole", "no-such-lane"),
    ];
    let changes = simulate(&before, &after, &items);
    // The stereo FLUX file, the "any" lane and the unknown lane do not change.
    assert_eq!(changes.iter().map(|c| c.index).collect::<Vec<_>>(), [0, 1]);

    let open = &changes[0];
    assert_eq!(
        (open.before, open.after, open.delta),
        (Some(1650), Some(2600), 950)
    );
    assert_eq!(open.lane, "hd-1080p");
    assert_eq!(
        open.name,
        "Movie.2004.Open.Matte.1080p.AMZN.WEB-DL.DDP5.1.x265-MONOLITH"
    );
    let causes: Vec<(&str, i32, i32, &str)> = open
        .causes
        .iter()
        .map(|c| (c.trait_id.as_str(), c.before, c.after, c.what.as_str()))
        .collect();
    assert_eq!(
        causes,
        [
            ("edition.open-matte", 100, 1100, "points 100 to 1100"),
            ("audio.surround", 50, 0, "points 50 to 0"),
        ]
    );

    let cmrg = &changes[1];
    assert_eq!(
        (cmrg.before, cmrg.after, cmrg.delta),
        (Some(50), Some(1000), 950)
    );
    assert_eq!(cmrg.causes[0].trait_id, "group.web");
    assert_eq!(cmrg.causes[0].what, "now matches");
    assert_eq!(cmrg.causes[1].what, "points 50 to 0");

    // Nothing changes between a policy and itself.
    assert!(simulate(&before, &before, &items).is_empty());

    // A ladder change shows as a rank change even when the score stays.
    let narrower = POLICY.replace(
        "\"WEBDL-1080p\", \"WEBRip-1080p\"]]\nmin_score",
        "\"WEBDL-1080p\"]]\nmin_score",
    );
    let narrower = Policy::from_toml(&narrower).unwrap();
    let rip = vec![item(
        "Movie.2019.1080p.WEBRip.DDP5.1.x265-Tigole",
        "hd-1080p",
    )];
    let changes = simulate(&before, &narrower, &rip);
    assert_eq!(changes.len(), 1);
    assert_eq!(
        (
            changes[0].delta,
            changes[0].rank_before,
            changes[0].rank_after
        ),
        (0, Some(1), None)
    );
    assert!(changes[0].causes.is_empty());

    // A lane that only one policy has.
    let without = POLICY
        .replace("id = \"any\"", "id = \"gone\"")
        .replace("label = \"Any\"", "label = \"Gone\"");
    let without = Policy::from_toml(&without).unwrap();
    let changes = simulate(&before, &without, &items);
    assert_eq!(changes.len(), 1);
    assert_eq!(
        (
            changes[0].index,
            changes[0].before,
            changes[0].after,
            changes[0].delta
        ),
        (3, Some(35), None, -35)
    );
}

/// A protection's condition on the file the library has is written `current`. Policy files
/// that still say `held` read the same, both at once are refused, and the policy is written
/// back with `current`.
#[test]
fn a_protection_reads_current_and_the_older_held() {
    let older = POLICY.replace("current = {", "held = {");
    assert_ne!(older, POLICY);
    assert_eq!(Policy::from_toml(&older).unwrap(), policy());
    let both = POLICY.replace(
        "current = { group = [\"DirtyHippie\"] }",
        "current = { group = [\"DirtyHippie\"] }\nheld = { group = [\"DirtyHippie\"] }",
    );
    assert!(
        Policy::from_toml(&both).is_err(),
        "one of the two, not both"
    );
    let written = serde_json::to_value(&policy().protections[0]).unwrap();
    assert_eq!(written["current"]["group"][0], "DirtyHippie");
    assert!(written.get("held").is_none());
}
