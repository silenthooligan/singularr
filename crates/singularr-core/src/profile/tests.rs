use super::*;
use crate::policy::{Action, EvalContext, Lane, Policy, Verdict, decide, evaluate};
use crate::release::parse;

/// A real installation's rule set, copied for the tests.
const INSTALLATION: &str = "crates/singularr-core/tests/data/installation-policy.toml";

fn file_policy(path: &str) -> Policy {
    let text = std::fs::read_to_string(format!("{}/../../{path}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|e| panic!("{path}: {e}"));
    Policy::from_toml(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn list(id: &str, name: &str, members: &[&str]) -> List {
    List {
        id: id.to_string(),
        name: name.to_string(),
        members: members.iter().map(|m| m.to_string()).collect(),
        ..List::default()
    }
}

/// One movie profile at 1080p with the default answers, two ranked lists and a blocked one.
fn model() -> Model {
    let mut m = Model {
        lists: vec![
            list("tier-a", "Tier A", &["AAA"]),
            list("tier-b", "Tier B", &["BBB"]),
            list("bad", "Bad groups", &["YIFY"]),
        ],
        ..Model::default()
    };
    let mut p = Profile::new("movies", "Movies, 1080p", Target::default());
    p.manager_profiles = vec!["radarr:HD-1080p".to_string()];
    m.profiles.push(p);
    m.fill();
    m.profiles[0].lists.ranked = vec![
        Pick::new("tier-a", Stance::Prefer),
        Pick::new("tier-b", Stance::Prefer),
        Pick::new("bad", Stance::Block),
    ];
    m
}

fn score(policy: &Policy, lane: &str, name: &str) -> i32 {
    let lane = policy.lane(lane).unwrap();
    evaluate(policy, lane, &parse(name), &EvalContext::default()).score
}

#[test]
fn the_same_model_always_gives_the_same_policy() {
    let m = model();
    let a = compile(&m, "7").unwrap();
    assert_eq!(a, compile(&m.clone(), "7").unwrap());
    let text = export(&m, "7").unwrap();
    assert_eq!(text, export(&m, "7").unwrap());
    // The file form loads back into the very same policy.
    assert_eq!(Policy::from_toml(&text).unwrap(), a);
    assert_eq!((a.name.as_str(), a.version.as_str()), ("singularr", "7"));
    let lane = a.lane("movies").unwrap();
    assert_eq!(lane.arr_profiles, ["radarr:HD-1080p"]);
}

#[test]
fn ranked_lists_keep_their_order_against_any_mix_of_smaller_preferences() {
    let mut m = model();
    for (group, key) in [
        (Group::Codec, "h264"),
        (Group::Formats, "aac"),
        (Group::Release, "hardsub"),
    ] {
        let pick = m.profiles[0]
            .picks_mut(group)
            .iter_mut()
            .find(|x| x.key == key)
            .unwrap();
        pick.stance = Stance::Avoid;
    }
    let policy = compile(&m, "1").unwrap();
    let best_b = "Movie.2020.1080p.BluRay.DV.HDR10Plus.HDR10.TrueHD.Atmos.7.1.x265.REPACK-BBB";
    let worst_a = "Movie.2020.1080p.BluRay.HC.AAC.2.0.x264-AAA";
    let (a, b) = (
        score(&policy, "movies", worst_a),
        score(&policy, "movies", best_b),
    );
    assert!(a > b, "the higher list ranks first: {a} against {b}");
    // Releases from no list rank below the last preferred list.
    let worst_b = score(
        &policy,
        "movies",
        "Movie.2020.1080p.BluRay.HC.AAC.2.0.x264-BBB",
    );
    let best_none = score(
        &policy,
        "movies",
        "Movie.2020.1080p.BluRay.DV.HDR10.TrueHD.Atmos.7.1.x265-NOBODY",
    );
    assert!(worst_b > best_none, "{worst_b} against {best_none}");
    // A profile that lets a clearly better release cross ranks the better one first.
    m.profiles[0].lists.crossing = Crossing::ClearlyBetter;
    let policy = compile(&m, "1").unwrap();
    let (a, b) = (
        score(&policy, "movies", worst_a),
        score(&policy, "movies", best_b),
    );
    assert!(b > a, "the clearly better release crosses: {b} against {a}");
}

#[test]
fn block_rejects() {
    let m = model();
    let policy = compile(&m, "1").unwrap();
    let lane = policy.lane("movies").unwrap();
    let ctx = EvalContext::default();
    for name in [
        "Movie.2020.1080p.BluRay.x264-YIFY",
        "Movie.2020.1080p.WEB-DL.DDP5.1.H.264.3D.HSBS-AAA",
    ] {
        let d = decide(&policy, lane, None, &parse(name), &ctx);
        assert_eq!(d.action, Action::Hold, "{name}");
        assert!(d.candidate.score < lane.min_score, "{name}");
    }
    let d = decide(
        &policy,
        lane,
        None,
        &parse("Movie.2020.1080p.BluRay.x264-AAA"),
        &ctx,
    );
    assert_eq!(d.action, Action::Grab);

    // Releases from no list, when the profile blocks them.
    let mut m = model();
    m.profiles[0].lists.unlisted = Stance::Block;
    let policy = compile(&m, "1").unwrap();
    let lane = policy.lane("movies").unwrap();
    let nobody = parse("Movie.2020.1080p.BluRay.x264-NOBODY");
    assert_eq!(
        decide(&policy, lane, None, &nobody, &ctx).action,
        Action::Hold
    );
    let listed = parse("Movie.2020.1080p.BluRay.x264-BBB");
    assert_eq!(
        decide(&policy, lane, None, &listed, &ctx).action,
        Action::Grab
    );
}

#[test]
fn accept_from_and_stop_at_become_the_minimum_and_the_cutoff() {
    let m = model();
    let policy = compile(&m, "1").unwrap();
    let lane = policy.lane("movies").unwrap();
    let step = |q: &str| vec![q.to_string()];
    assert_eq!(
        lane.ladder,
        [
            step("Remux-1080p"),
            step("Bluray-1080p"),
            step("WEBDL-1080p")
        ]
    );
    assert_eq!(lane.cutoff_quality.as_deref(), Some("Bluray-1080p"));
    let ctx = EvalContext::default();
    // Below Accept from: never taken.
    let d = decide(
        &policy,
        lane,
        None,
        &parse("Movie.2020.1080p.WEBRip.x264-AAA"),
        &ctx,
    );
    assert_eq!(d.action, Action::Hold);
    assert!(d.candidate.rank.is_none());
    // At Stop at, from the top list, with the best of picture and sound: final.
    let held = parse("Movie.2020.1080p.BluRay.DV.TrueHD.Atmos.7.1.x265-AAA");
    let remux = parse("Movie.2020.1080p.BluRay.REMUX.DV.TrueHD.Atmos.7.1.HEVC-AAA");
    let d = decide(&policy, lane, Some((&held, &ctx)), &remux, &ctx);
    assert_eq!(d.action, Action::Hold, "{:?}", d.reasons);
    // Below Stop at, the better source replaces it.
    let web = parse("Movie.2020.1080p.WEB-DL.DV.TrueHD.Atmos.7.1.H.265-AAA");
    let d = decide(&policy, lane, Some((&web, &ctx)), &remux, &ctx);
    assert_eq!(d.action, Action::Upgrade, "{:?}", d.reasons);
    // A file from a lower list is not final yet: a release of the top list replaces it.
    let lower = parse("Movie.2020.1080p.BluRay.DV.TrueHD.Atmos.7.1.x265-BBB");
    let d = decide(&policy, lane, Some((&lower, &ctx)), &held, &ctx);
    assert_eq!(d.action, Action::Upgrade, "{:?}", d.reasons);
}

#[test]
fn a_verdict_names_the_answer_in_the_model_s_words() {
    let m = model();
    let policy = compile(&m, "1").unwrap();
    let lane = policy.lane("movies").unwrap();
    let v = evaluate(
        &policy,
        lane,
        &parse("Movie.2020.1080p.BluRay.DV.HDR10.x265-AAA"),
        &EvalContext::default(),
    );
    let labels: Vec<&str> = v.trace.iter().map(|t| t.label.as_str()).collect();
    assert!(labels.contains(&"Release groups: Tier A"), "{labels:?}");
    assert!(labels.contains(&"Picture: Dolby Vision"), "{labels:?}");
    let reasons = explain(&m, &m.profiles[0], &v.trace);
    let texts: Vec<&str> = reasons.iter().map(|r| r.text.as_str()).collect();
    assert!(texts.contains(&"Release groups: Tier A"), "{texts:?}");
    assert!(
        texts.contains(&"Picture: Dolby Vision preferred"),
        "{texts:?}"
    );
    assert_eq!(reasons[0].answer, "lists");
    let blocked = evaluate(
        &policy,
        lane,
        &parse("Movie.2020.1080p.BluRay.x264-YIFY"),
        &EvalContext::default(),
    );
    let texts: Vec<String> = explain(&m, &m.profiles[0], &blocked.trace)
        .into_iter()
        .map(|r| r.text)
        .collect();
    assert!(
        texts
            .iter()
            .any(|t| t == "Release groups: Bad groups blocked"),
        "{texts:?}"
    );
}

#[test]
fn a_list_dragged_between_fixed_numbers_lands_between_them() {
    let mut m = model();
    m.lists.push(list("tier-c", "Tier C", &["CCC"]));
    m.fill();
    m.profiles[0].lists.ranked = vec![
        Pick {
            points: Some(1500),
            ..Pick::new("tier-a", Stance::Prefer)
        },
        Pick::new("tier-c", Stance::Prefer),
        Pick {
            points: Some(500),
            ..Pick::new("tier-b", Stance::Prefer)
        },
        Pick::new("bad", Stance::Block),
    ];
    let policy = compile(&m, "1").unwrap();
    let points = &policy.lane("movies").unwrap().points;
    assert_eq!(
        (
            points["list.tier-a"],
            points["list.tier-c"],
            points["list.tier-b"]
        ),
        (1500, 1000, 500)
    );
}

#[test]
fn size_per_hour_and_the_smaller_file() {
    let mut m = model();
    m.profiles[0].size = Size {
        min_per_hour: Some(2.0),
        max_per_hour: Some(10.0),
        smaller_wins_tie: true,
    };
    let policy = compile(&m, "1").unwrap();
    let lane = policy.lane("movies").unwrap();
    let r = parse("Movie.2020.1080p.BluRay.x264-AAA");
    let at = |gb: u64| {
        let ctx = EvalContext {
            size_bytes: Some(gb << 30),
            ..EvalContext::default()
        };
        evaluate(&policy, lane, &r, &ctx).score
    };
    assert!(at(8) > at(12), "the smaller file wins a tie");
    assert!(at(12) > at(3), "below 2 GB per hour of a 2 h movie");
    assert!(at(12) > at(30), "above 10 GB per hour of a 2 h movie");
}

#[test]
fn a_profile_says_itself_in_one_line() {
    let mut m = model();
    m.profiles[0].size = Size {
        min_per_hour: Some(4.0),
        max_per_hour: Some(30.0),
        smaller_wins_tie: false,
    };
    assert_eq!(
        m.profiles[0].summary(&m),
        "1080p, Remux to WEB-DL, prefers Dolby Vision and Atmos, Tier A first, 4 to 30 GB per hour"
    );
}

#[test]
fn a_model_that_cannot_compile_says_why() {
    let mut m = model();
    m.profiles[0].target.stop_at = Some(Source::WebRip);
    let err = compile(&m, "1").unwrap_err().to_string();
    assert!(err.contains("Stop at"), "{err}");

    let mut m = model();
    m.profiles[0]
        .picture
        .range
        .push(Pick::new("nope", Stance::Prefer));
    assert!(compile(&m, "1").is_err());

    let mut m = model();
    m.lists.push(list("tier-a", "Again", &["XYZ"]));
    assert!(
        compile(&m, "1")
            .unwrap_err()
            .to_string()
            .contains("two lists")
    );

    let mut m = model();
    m.rules.push(Rule {
        id: "broken".to_string(),
        name: "Broken pattern".to_string(),
        when: Condition {
            name: Some("(".to_string()),
            ..Condition::default()
        },
        ..Rule::default()
    });
    m.fill();
    assert!(matches!(compile(&m, "1"), Err(ModelError::Policy(_))));
}

/// Release names across the four profiles of the two policy files: groups of every tier, the
/// collectives, services, HDR, audio, editions, penalties, episodes.
const RELEASES: &[&str] = &[
    "Dune.Part.Two.2024.1080p.BluRay.REMUX.AVC.TrueHD.Atmos.7.1-FraMeSToR",
    "Dune.Part.Two.2024.1080p.BluRay.x265.10bit.DDP5.1-SM737",
    "The.Matrix.1999.1080p.BluRay.x265-afm72",
    "The Matrix (1999) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]",
    "The Matrix (1999) (1080p BluRay x265 HEVC 10bit AAC 5.1 Joy) [UTR]",
    "Heat.1995.1080p.BluRay.x264.DTS-HD.MA.5.1-CiNEPHiLES",
    "Heat.1995.1080p.BluRay.x264-SPARKS",
    "Heat.1995.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX",
    "Heat.1995.1080p.ATVP.WEB-DL.DDP5.1.Atmos.H.264-CRUD",
    "Heat.1995.1080p.DSNP.WEB-DL.DDP5.1.H.264-NTb",
    "Heat.1995.1080p.WEBRip.x264.AAC-YIFY",
    "Heat.1995.1080p.BluRay.x265.10bit.AAC.5.1-PSA",
    "Heat.1995.IMAX.1080p.WEB-DL.DDP5.1.H.264-NTG",
    "Heat.1995.Open.Matte.1080p.WEB-DL.DDP5.1.H.264-PeruGuy",
    "Heat.1995.1080p.WEB-DL.DDP5.1.Hindi.H.264-GRP",
    "Heat.1995.1080p.BluRay.x264.REPACK-ETRG",
    "Heat.1995.1080p.BluRay.AV1.Opus.5.1-dAV1nci",
    "Heat.1995.HDCAM.x264-GRP",
    "Heat.1995.1080p.HDTV.x264-GRP",
    "Heat.1995.1080p.WEB-DL.DDP5.1.H.264.HC-GRP",
    "Heat.1995.1080p.WEB-DL.DDP5.1.H.264-SHR",
    "Heat.1995.1080p.BluRay.x264.3D.HSBS-GRP",
    "Dune.Part.Two.2024.2160p.UHD.BluRay.REMUX.DV.HDR10.HEVC.TrueHD.Atmos.7.1-FraMeSToR",
    "Dune.Part.Two.2024.2160p.UHD.BluRay.x265.10bit.HDR.TrueHD.Atmos.7.1-CtrlHD",
    "Dune.Part.Two.2024.2160p.MA.WEB-DL.DDP5.1.Atmos.DV.HDR10.H.265-FLUX",
    "Dune.Part.Two.2024.2160p.BluRay.x265.10bit.SDR.DTS-HD.MA.5.1-GRP",
    "Dune.Part.Two.2024.2160p.WEB-DL.DDP5.1.HDR10Plus.H.265-TRiToN",
    "Dune.Part.Two.2024.2160p.BluRay.REMUX.HDR.HEVC.DTS-HD.MA.7.1-DirtyHippie",
    "Show.Name.S01E01.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb",
    "Show.Name.S01E01.1080p.BluRay.x265-d3g",
    "Show.Name.S01E01.1080p.WEB.h264-ELiTE",
    "Show.Name.S01E01.1080p.BluRay.REMUX.AVC.DTS-HD.MA.5.1-BMF",
    "Show.Name.S01E01.2160p.DSNP.WEB-DL.DDP5.1.Atmos.DV.HDR.H.265-FLUX",
    "Show.Name.S01E01.2160p.WEB-DL.DDP5.1.H.265-MeGusta",
    "Show.Name.S01E01.1080p.WEB-DL.DDP5.1.AV1-GRP",
    "Show.Name.S01E01.1080p.WEB-DL.DDP5.1.H.264-RGzsRutracker",
    "Those.Who.Kill.US.S01E01.1080p.WEB-DL.DDP5.1.H.264-GRP",
    "Show.Name.S01E01.1080p.HDTV.x264-SHK",
    "Show.Name.S01E01.1080p.WEB-DL.DDP5.1.H.264.exe",
];

fn accepted(v: &Verdict, lane: &Lane) -> bool {
    v.rank.is_some() && v.score >= lane.min_score
}

/// The exact import scores every release of the set as the file does: the same releases are
/// taken, in the same order, with the same scores, and the same file replaces the same file.
fn imports_faithfully(path: &str) {
    let file = file_policy(path);
    let model = import_as(&file, guess_kind, ImportMode::Exact);
    model.check().unwrap_or_else(|e| panic!("{path}: {e}"));
    let ours = compile(&model, "1").unwrap_or_else(|e| panic!("{path}: {e}"));
    assert_eq!(ours.lanes.len(), file.lanes.len());
    let ctx = EvalContext::default();
    let releases: Vec<_> = RELEASES.iter().map(|n| parse(n)).collect();
    for lane in &file.lanes {
        let mine = ours.lane(&lane.id).unwrap();
        let mut theirs_order = Vec::new();
        let mut our_order = Vec::new();
        for r in &releases {
            let a = evaluate(&file, lane, r, &ctx);
            let b = evaluate(&ours, mine, r, &ctx);
            let name = &r.raw;
            assert_eq!(
                accepted(&a, lane),
                accepted(&b, mine),
                "{path} {}: {name}",
                lane.id
            );
            assert_eq!(a.rank, b.rank, "{path} {}: {name}", lane.id);
            if accepted(&a, lane) {
                assert_eq!(a.score, b.score, "{path} {}: {name}", lane.id);
                theirs_order.push((a.rank, std::cmp::Reverse(a.score), name.clone()));
                our_order.push((b.rank, std::cmp::Reverse(b.score), name.clone()));
            }
        }
        theirs_order.sort();
        our_order.sort();
        assert_eq!(theirs_order, our_order, "{path} {}", lane.id);
        for pair in releases.windows(2) {
            let (held, candidate) = (&pair[0], &pair[1]);
            let a = decide(&file, lane, Some((held, &ctx)), candidate, &ctx);
            let b = decide(&ours, mine, Some((held, &ctx)), candidate, &ctx);
            assert_eq!(
                a.action, b.action,
                "{path} {}: {} over {}",
                lane.id, candidate.raw, held.raw
            );
        }
    }
}

#[test]
fn the_product_policy_imports_into_a_model_that_scores_as_the_file() {
    imports_faithfully("rules/policy.toml");
}

#[test]
fn the_installation_policy_imports_into_a_model_that_scores_as_the_file() {
    imports_faithfully(INSTALLATION);
}

#[test]
fn import_groups_lists_by_equal_points_and_reads_options() {
    let file = file_policy(INSTALLATION);
    let m = import(&file, guess_kind);
    let together = m.lists.iter().find(|l| l.has("SM737")).unwrap();
    assert!(together.has("afm72"), "{:?}", together.members);
    let web = m.lists.iter().find(|l| l.has("CRUD")).unwrap();
    assert_eq!(
        web.only.as_ref().map(|c| c.source.clone()),
        Some(vec![Source::WebDl, Source::WebRip])
    );
    let qxr = m.lists.iter().find(|l| l.has("Tigole")).unwrap();
    assert!(qxr.has("QxR"), "{:?}", qxr.members);
    // Attribute traits become stances, with the file's points.
    let uhd = m.profile("shows-uhd").unwrap();
    let dv = uhd
        .picks(Group::Range)
        .iter()
        .find(|p| p.key == "dv")
        .unwrap();
    assert_eq!((dv.stance, dv.points), (Stance::Prefer, Some(100)));
    let other = uhd
        .picks(Group::Languages)
        .iter()
        .find(|p| p.key == "other")
        .unwrap();
    assert_eq!(other.stance, Stance::Block);
    // The rest are advanced rules.
    assert!(m.rules.iter().any(|r| r.name == "Hindi audio"));
    // The groups nobody wants are one blocked list.
    let hd = m.profile("movies-hd").unwrap();
    let unwanted = m.lists.iter().find(|l| l.has("YIFY")).unwrap();
    assert!(unwanted.has("V3SP4EV3R"), "{:?}", unwanted.members);
    let pick = hd
        .lists
        .ranked
        .iter()
        .find(|p| p.key == unwanted.id)
        .unwrap();
    assert_eq!(pick.stance, Stance::Block);
    // The ladder of one step is the target's strip.
    assert_eq!(hd.target.sources.len(), 1);
    assert_eq!(hd.target.resolution, Resolution::R1080);
    assert_eq!(hd.target.kind, TitleKind::Movie);
    assert!(hd.scores.ladder.is_none());
    assert_eq!(m.profile("shows-hd").unwrap().target.kind, TitleKind::Show);
    // The first preferred list is the one with the most points.
    let first = hd
        .lists
        .ranked
        .iter()
        .find(|p| p.stance == Stance::Prefer)
        .unwrap();
    assert_eq!(first.points, Some(3000));
    // The managers' formats stay with the traits they came from.
    assert!(
        m.formats
            .values()
            .any(|f| f.iter().any(|x| x == "radarr:WEB Tier 01"))
    );
}

#[test]
fn a_model_reads_back_as_it_was_written() {
    let mut m = model();
    m.profiles[0].target.stop_at = None;
    let imported = import(&file_policy(INSTALLATION), guess_kind);
    for model in [m, imported] {
        let text = serde_json::to_string(&model).unwrap();
        let back: Model = serde_json::from_str(&text).unwrap();
        assert_eq!(back, model);
    }
}

#[test]
fn a_tiered_import_gathers_the_owner_s_groups_and_takes_the_same_releases() {
    let file = file_policy(INSTALLATION);
    let m = import(&file, guess_kind);
    m.check().unwrap();
    let with = |name: &str| {
        m.lists
            .iter()
            .find(|l| l.has(name))
            .unwrap_or_else(|| panic!("no list has {name}"))
    };
    let together = |names: &[&str]| {
        let l = with(names[0]);
        for n in names {
            assert!(l.has(n), "{n} is not with {}: {:?}", names[0], l.members);
        }
        assert_eq!(l.members.len(), names.len(), "{:?}", l.members);
    };
    together(&["HeVK", "CiNEPHiLES", "FraMeSToR", "SARTRE"]);
    together(&["TRiToN", "FGT"]);
    together(&["SQS", "PeruGuy"]);
    together(&["SM737", "afm72"]);
    together(&[
        "NmCT",
        "The.PunisheR",
        "MgB",
        "KK650",
        "EDGE2020",
        "BHDStudio",
    ]);
    together(&["SHH", "SHA"]);
    assert!(with("YIFY").has("V3SP4EV3R"));
    assert_eq!(with("HeVK").name, "HeVK, CiNEPHiLES +2");
    let names: Vec<&str> = m.lists.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(m.lists.len(), 32, "{names:?}");
    // A tier's points are its members' mean.
    let hd = m.profile("movies-hd").unwrap();
    let top = hd
        .lists
        .ranked
        .iter()
        .find(|p| p.key == with("HeVK").id)
        .unwrap();
    assert_eq!((top.stance, top.points), (Stance::Prefer, Some(1625)));
    // The same releases are taken; the ranks that move are reported. Each profile is compared
    // on the releases of its kind, as the import's dry run compares it on the library's files.
    let ours = compile(&m, "1").unwrap();
    let of_kind = |p: &Policy, kind: TitleKind| Policy {
        lanes: p
            .lanes
            .iter()
            .filter(|l| guess_kind(l) == kind)
            .cloned()
            .collect(),
        ..p.clone()
    };
    let compare = |after: &Policy| {
        let mut all = RankReport::default();
        for kind in [TitleKind::Movie, TitleKind::Show] {
            let items: Vec<_> = RELEASES
                .iter()
                .map(|n| parse(n))
                .filter(|r| {
                    (r.season.is_some() || !r.episodes.is_empty()) == (kind == TitleKind::Show)
                })
                .map(|r| (r, EvalContext::default()))
                .collect();
            let r = rank_changes(&of_kind(&file, kind), &of_kind(after, kind), &items);
            all.releases += r.releases;
            all.moved += r.moved;
            all.accept_changed += r.accept_changed;
            all.moves.extend(r.moves);
        }
        all
    };
    let report = compare(&ours);
    // Two profiles of each kind: every release is compared twice.
    assert_eq!(report.releases, RELEASES.len() * 2);
    assert_eq!(report.accept_changed, 0, "{:?}", report.moves);
    eprintln!(
        "tiers: {} of {} places move; largest {:?}",
        report.moved,
        report.releases,
        report.moves.iter().take(5).collect::<Vec<_>>()
    );
    // The exact import keeps one list per distinct points vector.
    let exact = import_as(&file, guess_kind, ImportMode::Exact);
    assert_eq!(exact.lists.len(), 55);
    assert!(
        !exact
            .lists
            .iter()
            .find(|l| l.has("CiNEPHiLES"))
            .unwrap()
            .has("FraMeSToR")
    );
    let same = compare(&compile(&exact, "1").unwrap());
    assert_eq!(
        (same.moved, same.accept_changed),
        (0, 0),
        "{:?}",
        same.moves
    );
}
