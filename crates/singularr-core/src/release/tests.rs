//! Parser tests. Most names are real: they come from a real library or from incidents
//! written up in its operations notes.

use super::*;

/// The token of every note stands at its offset, and every field that is set has a note.
fn check_notes(p: &ParsedRelease) {
    for n in &p.notes {
        assert_eq!(
            p.raw.get(n.at..n.at + n.token.len()),
            Some(n.token.as_str()),
            "note `{}` of {:?} does not point at its token",
            n.field,
            p.raw
        );
        assert!(!n.token.is_empty() && !n.read_as.is_empty());
    }
    let has = |field: &str| p.notes.iter().any(|n| n.field == field);
    let count = |field: &str| p.notes.iter().filter(|n| n.field == field).count();
    let set: [(&str, bool); 21] = [
        ("kind", p.kind != MediaKind::Unknown),
        ("title", p.title.is_some()),
        ("year", p.year.is_some()),
        ("season", p.season.is_some()),
        ("episode", !p.episodes.is_empty()),
        ("air_date", p.air_date.is_some()),
        ("resolution", p.resolution.is_some()),
        ("source", p.source.is_some()),
        ("video_codec", p.video_codec.is_some()),
        ("bit_depth", p.bit_depth.is_some()),
        ("audio_codec", p.audio_codec.is_some()),
        ("audio_channels", p.audio_channels.is_some()),
        ("atmos", p.atmos),
        ("service", p.service.is_some()),
        ("fps", p.fps.is_some()),
        ("ai_upscaled", p.ai_upscaled),
        ("proper", p.proper),
        ("repack", p.repack),
        ("group", p.group.is_some()),
        ("collective", p.collective.is_some()),
        ("sdr", p.sdr),
    ];
    for (field, is_set) in set {
        assert_eq!(
            has(field),
            is_set,
            "field `{field}` of {:?}: set {is_set}, note {}",
            p.raw,
            has(field)
        );
    }
    assert!(count("hdr") >= p.hdr.len(), "hdr notes of {:?}", p.raw);
    assert!(
        count("edition") >= p.editions.len(),
        "edition notes of {:?}",
        p.raw
    );
    assert!(
        count("language") >= p.languages.len(),
        "language notes of {:?}",
        p.raw
    );
}

fn read(name: &str) -> ParsedRelease {
    let p = parse(name);
    check_notes(&p);
    p
}

fn quality(p: &ParsedRelease) -> String {
    p.quality().map(Quality::label).unwrap_or_default()
}

fn note<'a>(p: &'a ParsedRelease, field: &str) -> &'a ParseNote {
    p.notes
        .iter()
        .find(|n| n.field == field)
        .unwrap_or_else(|| panic!("no `{field}` note for {:?}", p.raw))
}

#[test]
fn scene_movie() {
    let p = read(
        "Harry.Potter.and.the.Prisoner.of.Azkaban.2004.Open.Matte.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb",
    );
    assert_eq!(
        p.title.as_deref(),
        Some("Harry Potter and the Prisoner of Azkaban")
    );
    assert_eq!(p.year, Some(2004));
    assert_eq!(quality(&p), "WEBDL-1080p");
    assert_eq!(p.service.as_deref(), Some("AMZN"));
    assert!(p.has_edition("open matte"));
    assert_eq!(p.video_codec, Some(VideoCodec::H264));
    assert_eq!(p.audio_codec, Some(AudioCodec::Eac3));
    assert_eq!(p.audio_channels.as_deref(), Some("5.1"));
    assert_eq!(p.group.as_deref(), Some("NTb"));
    assert_eq!(p.kind, MediaKind::Movie);
}

#[test]
fn notes_carry_the_token_and_its_offset() {
    let name = "Cam.2018.1080p.NF.WEB-DL.DDP5.1.x264-NTG";
    let p = read(name);
    let at = |field: &str| {
        let n = note(&p, field);
        (n.token.as_str(), n.at, n.read_as.as_str())
    };
    assert_eq!(at("title"), ("Cam", 0, "Cam"));
    assert_eq!(at("year"), ("2018", 4, "2018"));
    assert_eq!(at("resolution"), ("1080p", 9, "1080p"));
    assert_eq!(at("service"), ("NF", 15, "NF"));
    assert_eq!(at("source"), ("WEB-DL", 18, "WEBDL"));
    assert_eq!(at("audio_codec"), ("DDP", 25, "Eac3"));
    assert_eq!(at("audio_channels"), ("5.1", 28, "5.1"));
    assert_eq!(at("video_codec"), ("x264", 32, "H264"));
    assert_eq!(at("group"), ("NTG", 37, "NTG"));
    assert_eq!(at("kind"), ("2018", 4, "Movie"));

    // Offsets are bytes, so a long dash (three bytes) moves what follows it.
    let p = read("Aquaman (2018) 1080p OpenMatte SDR 10bit HEVC \u{2014} PeruGuy");
    let n = note(&p, "group");
    assert_eq!((n.token.as_str(), n.at), ("PeruGuy", 50));
    assert_eq!(&p.raw[n.at..], "PeruGuy");

    // Compounds are noted as written.
    let p = read("Movie.2019.2160p.UHD.BluRay.REMUX.DV.HDR10+.DTS-HD.MA.7.1.H.265-GRP");
    assert_eq!(note(&p, "audio_codec").token, "DTS-HD.MA");
    assert_eq!(note(&p, "video_codec").token, "H.265");
    assert!(p.notes.iter().any(|n| n.token == "HDR10+"));
    let p = read("Movie.2019.Open.Matte.1080p.WEB-DL.DD+5.1.x264-GRP");
    assert_eq!(note(&p, "edition").token, "Open.Matte");
    assert_eq!(note(&p, "audio_codec").token, "DD+");
}

#[test]
fn a_title_word_is_never_a_quality() {
    // 12 Nov 2026: the arr imported Cam (2018) as a CAM rip.
    let p = read("Cam.2018.1080p.NF.WEB-DL.DDP5.1.x264-NTG");
    assert_eq!(p.title.as_deref(), Some("Cam"));
    assert_eq!(p.source, Some(Source::WebDl));
    assert_eq!(quality(&p), "WEBDL-1080p");

    for (name, title) in [
        ("TS.2021.1080p.WEB-DL.x264-GRP", "TS"),
        (
            "Screener.2023.1080p.AMZN.WEB-DL.DDP5.1.H.264-FLUX",
            "Screener",
        ),
        ("Proper.2020.1080p.WEB-DL.x264-GRP", "Proper"),
        ("Extended.2021.1080p.WEB-DL.x265-GRP", "Extended"),
        ("The.Web.2019.1080p.WEB-DL.x264-GRP", "The Web"),
        ("Cam (2018) [WEBDL-1080p][EAC3 5.1][h264]-NTG.mkv", "Cam"),
        // No year: the title still ends only at a token that cannot be a word.
        ("Cam.1080p.NF.WEB-DL.DDP5.1.x264-NTG", "Cam"),
    ] {
        let p = read(name);
        assert_eq!(p.title.as_deref(), Some(title), "{name}");
        assert_eq!(p.source, Some(Source::WebDl), "{name}");
        assert!(!p.proper && p.editions.is_empty(), "{name}");
    }
}

#[test]
fn prerelease_sources_only_after_the_title() {
    for (name, source) in [
        ("Movie.2024.CAM.x264-GRP", Source::Cam),
        ("Movie.2024.HDCAM.x264-GRP", Source::Cam),
        ("Movie.2024.TS.x264-GRP", Source::Telesync),
        ("Movie.2024.TELESYNC.x264-GRP", Source::Telesync),
        ("Movie.2024.1080p.HDTS.x264-GRP", Source::Telesync),
        ("Movie.2024.TC.x264-GRP", Source::Telecine),
        ("Movie.2024.SCR.x264-GRP", Source::Screener),
        ("Movie.2024.DVDSCR.x264-GRP", Source::Screener),
        ("Movie.2024.WORKPRINT.x264-GRP", Source::Workprint),
    ] {
        let p = read(name);
        assert_eq!(p.source, Some(source), "{name}");
        assert!(source.is_prerelease());
        assert_eq!(p.title.as_deref(), Some("Movie"), "{name}");
    }
    // The same words inside a title are words.
    let p = read("The.Cam.Girl.TS.Story.2020.1080p.BluRay.x264-GRP");
    assert_eq!(p.title.as_deref(), Some("The Cam Girl TS Story"));
    assert_eq!(p.source, Some(Source::BluRay));
}

#[test]
fn group_after_a_spaced_or_long_dash() {
    // Radarr read "BDRip" out of this one.
    let p = read("Aquaman (2018) 1080p OpenMatte SDR [HINDI-ENG-5.1] 10bit HEVC \u{2014} PeruGuy");
    assert_eq!(p.group.as_deref(), Some("PeruGuy"));
    assert!(p.has_edition("Open Matte"));
    assert_eq!(p.languages, ["Hindi", "English"]);
    assert_eq!(p.audio_channels.as_deref(), Some("5.1"));
    assert_eq!(
        (p.bit_depth, p.video_codec),
        (Some(10), Some(VideoCodec::H265))
    );

    for name in [
        "Aquaman (2018) 1080p OpenMatte SDR 10bit HEVC - PeruGuy",
        "Aquaman (2018) 1080p OpenMatte SDR 10bit HEVC \u{2013} PeruGuy.mkv",
        "Aquaman (2018) 1080p OpenMatte SDR 10bit HEVC &mdash; PeruGuy",
        "Aquaman (2018) 1080p OpenMatte SDR 10bit HEVC - PeruGuy (1).mkv",
        "Aquaman (2018) [SDR] DS4K 1080p BDRip DDP 5.1 PeruGuy.mkv",
    ] {
        assert_eq!(read(name).group.as_deref(), Some("PeruGuy"), "{name}");
    }
}

#[test]
fn groups_with_dots_dashes_and_digits() {
    for group in [
        "D-Z0N3",
        "E.N.D",
        "EVO",
        "4K4U",
        "SM737",
        "SARTRE",
        "W4NK3R",
        "d3g",
        "NTb",
        "FLUX",
        "FraMeSToR",
        "CiNEPHiLES",
        "TRiToN",
        "KONTRAST",
        "Tigole",
        "UTR-HD",
        "YTS.MX",
        "The.PunisheR",
        "GalaxyRG265",
        "EDGE2020",
    ] {
        for name in [
            format!("Some.Movie.2019.1080p.BluRay.DTS-HD.MA.5.1.x264-{group}"),
            format!("Some Movie 2019 1080p WEB-DL DDP5 1 H 264-{group}"),
            format!("Some.Movie.2019.2160p.UHD.BluRay.x265-{group}.mkv"),
            format!("Some.Show.S01E03.1080p.AMZN.WEB-DL.DDP5.1.H.264-{group}[TGx]"),
        ] {
            let p = read(&name);
            assert_eq!(p.group.as_deref(), Some(group), "{name}");
            assert_eq!(note(&p, "group").token, group);
        }
    }
    // Technical compounds that end in a dash part are not groups.
    for name in [
        "Some.Movie.2019.1080p.WEB-DL",
        "Some.Movie.2019.1080p.BluRay.x264.DTS-HD",
        "Some.Movie.2019.1080p.Blu-Ray",
        "Spider-Man.2002",
    ] {
        assert_eq!(read(name).group, None, "{name}");
    }
}

#[test]
fn collective_tags() {
    for name in [
        "Practical Magic (1998) Open Matte (1080p AMZN WEB-DL x265 HEVC 10bit AAC 5.1 MONOLITH) [QxR]",
        "Practical Magic 1998 Open Matte 1080p AMZN WEB-DL x265 HEVC 10bit AAC 5.1 MONOLITH [QxR]",
        "Practical.Magic.1998.Open.Matte.1080p.AMZN.WEB-DL.x265.HEVC.10bit.AAC.5.1-MONOLITH [QxR]",
        "Practical Magic 1998 Open Matte 1080p AMZN WEB-DL x265 HEVC 10bit AAC 5 1 MONOLITH QxR",
    ] {
        let p = read(name);
        assert_eq!(p.group.as_deref(), Some("MONOLITH"), "{name}");
        assert_eq!(p.collective.as_deref(), Some("QxR"), "{name}");
        assert_eq!(p.title.as_deref(), Some("Practical Magic"), "{name}");
        assert_eq!(quality(&p), "WEBDL-1080p", "{name}");
        assert_eq!(p.audio_codec, Some(AudioCodec::Aac), "{name}");
    }
    let p = read(
        "10 Things I Hate About You (1999) (1080p BluRay x265 HEVC 10bit AAC 5.1 Silence) [QxR]",
    );
    assert_eq!(
        (p.group.as_deref(), p.collective.as_deref()),
        (Some("Silence"), Some("QxR"))
    );
    // Without a collective.
    let p = read("Elemental (2023) (1080p BluRay x265 HEVC 10bit AAC 7.1 Tigole)");
    assert_eq!(
        (p.group.as_deref(), p.collective.as_deref()),
        (Some("Tigole"), None)
    );
    // A bracket tag with no group before it is the group.
    let p = read(
        "Dan.Browns.The.Lost.Symbol.S01E01.As.Above.So.Below.1080p.10bit.DDP.5.1.x265.[HashMiner]",
    );
    assert_eq!(
        (p.group.as_deref(), p.collective.as_deref()),
        (Some("HashMiner"), None)
    );
    let p = read("Society Of The Snow (2023) [1080p] [WEBRip] [x265] [10bit] [5.1] [YTS.MX]");
    assert_eq!(p.group.as_deref(), Some("YTS.MX"));
    assert_eq!(quality(&p), "WEBRip-1080p");
    // A site tag is neither.
    let p = read("The.Retirement.Plan.2023.1080p.AMZN.WEBRip.DDP5.1.x265.10bit-GalaxyRG265[TGx]");
    assert_eq!(
        (p.group.as_deref(), p.collective.as_deref()),
        (Some("GalaxyRG265"), None)
    );
    let p =
        read("[Bitsearch.to] City Slickers (1991) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole)");
    assert_eq!(p.title.as_deref(), Some("City Slickers"));
    assert_eq!(p.group.as_deref(), Some("Tigole"));
    let p = read("Match Point 2005 1080p BluRay x265 RARBG ORARBG");
    assert_eq!(
        (p.group.as_deref(), p.collective.as_deref()),
        (Some("RARBG"), None)
    );
    // A cast list after the technical block holds no group.
    let p = read(
        "Predator 2 (1990) (Jaboody 1080p BluRay x265 HEVC 10bit AAC 5.1 Commentary) Stephen Hopkins Danny Glover Gary Busey",
    );
    assert_eq!(p.group, None);
    // The group's site and the words "release group" are not part of its name.
    let p =
        read("Equilibrium (2002) 1080p Open Matte 10bit BluRay x265 HEVC-KatmovieHD.nu (1).mkv");
    assert_eq!(p.group.as_deref(), Some("KatmovieHD"));
    let p = read(
        "R I P D 2 Rise Of The Damned 2022 1080p BluRay HEVC x265 10Bit AC-3 5.1-MSubs - KINGDOM RG",
    );
    assert_eq!(p.group.as_deref(), Some("KINGDOM"));
    // A name cut off in the middle of a word has no readable group either.
    for name in [
        "The Gift 2000 RM4k SF 1080p BluRay x265 HEVC 10bit AAC 5 1 Com",
        "The Adventures of Baron Munchausen (1988) Criterion (1080p BluRay x265 HEVC 10bi",
    ] {
        assert_eq!(read(name).group, None, "{name}");
    }
    // A name cut short has no readable group.
    let p = read(
        "Theres.Something.Wrong.With.The.Children.2023.1080p.AMZN.WEBRip.DDP5.1.x265.10bit-GalaxyRG...",
    );
    assert_eq!(p.group, None);
    assert_eq!(quality(&p), "WEBRip-1080p");
}

#[test]
fn interpolated_and_upscaled() {
    let p = read(
        "Back.to.the.Future.Part.II.1989.2160p.DV.HDR10Plus.Ai-Enhanced.HEVC.TrueHD.7.1.Atmos.MULTI-RIFE.4.25v2-60fps-DirtyHippie",
    );
    assert_eq!(p.group.as_deref(), Some("DirtyHippie"));
    assert_eq!(p.fps, Some(60));
    assert!(p.ai_upscaled);
    assert_eq!(p.hdr, [HdrFormat::DolbyVision, HdrFormat::Hdr10Plus]);
    assert_eq!(
        (p.audio_codec, p.audio_channels.as_deref(), p.atmos),
        (Some(AudioCodec::TrueHd), Some("7.1"), true)
    );
    assert_eq!(p.resolution, Some(Resolution::R2160));
    assert_eq!(p.title.as_deref(), Some("Back to the Future Part II"));

    // The library's own name for the same kind of file.
    let p = read(
        "Home Alone (1990) {tmdb-771} [WEB-DL-2160p PROPER][UHD][DV HDR10Plus][Opus 5.1][HEVC 10bit HUSH]-[60fps-DirtyHippie].mkv",
    );
    assert_eq!((p.group.as_deref(), p.fps), (Some("DirtyHippie"), Some(60)));
    assert_eq!(quality(&p), "WEBDL-2160p");
    assert!(p.proper);

    for name in [
        "Movie.1990.2160p.WEB-DL.RIFE.4.16.lite.v2.HEVC-GRP",
        "Movie.1990.2160p.Ai-Enhanced.HEVC-GRP",
        "Movie (1990) AI Upscaled 2160p HEVC-GRP",
        "The.Matrix.Reloaded.2003.Open.Matte.2160p.AI.WEB-DL.60FPS.AV1.Opus.5.1-Marjenbo",
        "Movie (2005) UpScaled 2160p H265 10 bit DV HDR10+ AC3 5.1 GRP",
    ] {
        let p = read(name);
        assert!(p.ai_upscaled, "{name}");
        assert_eq!(p.resolution, Some(Resolution::R2160), "{name}");
        assert_eq!(
            p.audio_channels.as_deref().unwrap_or("5.1"),
            "5.1",
            "{name}"
        );
    }
    let p = read("The.Matrix.Reloaded.2003.Open.Matte.2160p.AI.WEB-DL.60FPS.AV1.Opus.5.1-Marjenbo");
    assert_eq!((p.fps, p.source), (Some(60), Some(Source::WebDl)));
    // IMAX Enhanced is an edition, not an upscale.
    let p = read("Movie.2021.IMAX.Enhanced.1080p.DSNP.WEB-DL.DDP5.1.Atmos.H.264-GRP");
    assert!(!p.ai_upscaled);
    assert!(p.has_edition("IMAX") && p.has_edition("IMAX Enhanced"));
}

#[test]
fn open_matte_spellings() {
    for name in [
        "Movie.2004.Open.Matte.1080p.WEB-DL.x264-GRP",
        "Movie (2004) 1080p OpenMatte SDR 10bit HEVC - GRP",
        "Movie (2004) OPEN MATTE (1080p Web-DL x265 HEVC 10bit 5.1 GRP) [UTR]",
        "Movie (2004) Open-Matte [SDR] DS4K 1080p Ai BluRay DDP 5.1 \u{2014} GRP.mkv",
        "Movie.2004.1080p.OM.WEB-DL.x264-GRP",
        "Movie (2004) {imdb-tt0304141} {edition-Open Matte} [WEBDL-1080p][EAC3 5.1][HEVC]-GRP.mkv",
        "Movie (2004) {tmdb-673} [Open Matte][BluRay-1080p][Opus 5.1][HEVC HUSH]-[GRP].mkv",
    ] {
        let p = read(name);
        assert!(p.has_edition("Open Matte"), "{name}");
        assert_eq!(p.group.as_deref(), Some("GRP"), "{name}");
        assert_eq!(p.resolution, Some(Resolution::R1080), "{name}");
    }
    // "OM" in a title, or anywhere it is not between technical tokens, is not open matte.
    for name in [
        "OM.2023.1080p.WEB-DL.x264-GRP",
        "Shanti.OM.2007.1080p.BluRay.x264-GRP",
        "Movie.2004.OM.Shanti.1080p.BluRay.x264-GRP",
    ] {
        assert!(!read(name).has_edition("Open Matte"), "{name}");
    }
}

#[test]
fn a_4k_remaster_is_an_edition_not_a_resolution() {
    for name in [
        "Tremors (1990) RM4K (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]",
        "Desperado.1995.RM4k.Arrow.1080p.BluRay.x265.hevc.10bit.AAC.5.1.commentary-HeVK",
        "Movie.1990.4K.Remaster.1080p.BluRay.x265-GRP",
        "Movie.1990.4K.Remastered.1080p.BluRay.x265-GRP",
        "Movie 1990 Remastered in 4K 1080p BluRay x265-GRP",
        "Piranha (1978) (Shout! Factory 4k Remastered 1080p BluRay x265 HEVC 10bit AAC 2.0 Commentary)",
    ] {
        let p = read(name);
        assert_eq!(p.resolution, Some(Resolution::R1080), "{name}");
        assert!(p.has_edition("Remastered"), "{name}");
        assert!(p.has_edition("4K Remaster"), "{name}");
    }
    // The same edition with no resolution in the name leaves the resolution unknown.
    let p = read("Movie.1990.RM4K.BluRay.x265-GRP");
    assert_eq!(p.resolution, None);
    // A plain remaster.
    let p = read("Fletch 1985 Remastered 1080p BluRay HEVC x265 5.1 BONE");
    assert!(p.has_edition("Remastered") && !p.has_edition("4K Remaster"));
    assert_eq!(p.group.as_deref(), Some("BONE"));
    // A UHD disc is 2160p only when the name says so.
    let p = read("Movie.2019.UHD.BluRay.2160p.REMUX.HDR.HEVC.DTS-HD.MA.5.1-GRP");
    assert_eq!(quality(&p), "Remux-2160p");
    let p =
        read("10 Cloverfield Lane 2016 PROPER UHD BluRay 1080p DD Atmos 5 1 DoVi HDR10 x265-SM737");
    assert_eq!(quality(&p), "Bluray-1080p");
    assert_eq!(p.hdr, [HdrFormat::DolbyVision, HdrFormat::Hdr10]);
    assert!(p.proper && p.atmos);
    assert_eq!(note(&p, "resolution").token, "1080p");
    // Without a line count, 4K and UHD are the only word on resolution.
    let p = read("Movie.2019.4K.HDR.x265-GRP");
    assert_eq!(p.resolution, Some(Resolution::R2160));
    assert_eq!(note(&p, "resolution").token, "4K");
    let p = read(
        "Saving Private Ryan (1998) {imdb-tt0120815} [BluRay-2160p][UHD][HDR10][Opus 7.1][HEVC 10bit HUSH]-[SHR].mkv",
    );
    assert_eq!(quality(&p), "Bluray-2160p");
    assert_eq!(note(&p, "resolution").token, "2160p");
}

#[test]
fn the_last_year_before_the_technical_tokens_is_the_year() {
    for (name, title, year) in [
        (
            "2001.A.Space.Odyssey.1968.1080p.BluRay.x264-GRP",
            "2001 A Space Odyssey",
            1968,
        ),
        (
            "Blade.Runner.2049.2017.2160p.UHD.BluRay.x265-GRP",
            "Blade Runner 2049",
            2017,
        ),
        ("1917.2019.1080p.BluRay.x264-GRP", "1917", 2019),
        (
            "Wonder.Woman.1984.2020.1080p.HMAX.WEB-DL.DDP5.1.Atmos.H.264-GRP",
            "Wonder Woman 1984",
            2020,
        ),
        (
            "Death Race 2050 2017 1080p BluRay x265 RARBG ORARBG",
            "Death Race 2050",
            2017,
        ),
        (
            "2012 (2009) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]",
            "2012",
            2009,
        ),
        (
            "Never Say Never Again-1983-REPACK 1080p BluRay OPUS 5 1 AV1-WhiskeyJack",
            "Never Say Never Again",
            1983,
        ),
        (
            "Ginger Snaps 2: Unleashed (2004) (1080p BluRay x265 HEVC 10bit EAC3 5.1 Ghost) [QxR]",
            "Ginger Snaps 2: Unleashed",
            2004,
        ),
        (
            "I, Robot (2004) Open-Matte [SDR] DS4K 1080p Ai BluRay DDP 5.1 \u{2014} PeruGuy.mkv",
            "I, Robot",
            2004,
        ),
        (
            "Your Sister&#039;s Sister (2011) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]",
            "Your Sister's Sister",
            2011,
        ),
        (
            "www.UIndex.org    -    Daredevil 2003 Directors Cut 1080p BluRay 10Bit X265 DD 5 1-Chivaman",
            "Daredevil",
            2003,
        ),
    ] {
        let p = read(name);
        assert_eq!(p.title.as_deref(), Some(title), "{name}");
        assert_eq!(p.year, Some(year), "{name}");
        assert_eq!(p.kind, MediaKind::Movie, "{name}");
    }
    // A year that opens the name is a title when nothing else is.
    let p = read("2012.1080p.BluRay.x264-GRP");
    assert_eq!((p.title.as_deref(), p.year), (Some("2012"), None));
}

#[test]
fn audio() {
    use AudioCodec::*;
    for (text, codec, channels, atmos) in [
        ("DDP5.1", Eac3, Some("5.1"), false),
        ("DDP5 1", Eac3, Some("5.1"), false),
        ("DDP 5 1", Eac3, Some("5.1"), false),
        ("DD+ 5.1", Eac3, Some("5.1"), false),
        ("DD+5.1", Eac3, Some("5.1"), false),
        ("DDPA5.1", Eac3, Some("5.1"), true),
        ("DDP5.1.Atmos", Eac3, Some("5.1"), true),
        ("EAC3", Eac3, None, false),
        ("EAC3.5.1", Eac3, Some("5.1"), false),
        ("E-AC-3.5.1", Eac3, Some("5.1"), false),
        ("DD5.1", Ac3, Some("5.1"), false),
        ("DD.2.0", Ac3, Some("2.0"), false),
        ("AC3.5.1", Ac3, Some("5.1"), false),
        ("TrueHD.Atmos.7.1", TrueHd, Some("7.1"), true),
        ("TrueHD.7.1.Atmos", TrueHd, Some("7.1"), true),
        ("DTS-HD.MA.5.1", DtsHdMa, Some("5.1"), false),
        ("DTS-HD MA 7.1", DtsHdMa, Some("7.1"), false),
        ("DTS-HD.HRA.5.1", DtsHdHra, Some("5.1"), false),
        ("DTS-X.7.1", DtsX, Some("7.1"), false),
        ("DTS.5.1", Dts, Some("5.1"), false),
        ("AAC2.0", Aac, Some("2.0"), false),
        ("AAC 5.1", Aac, Some("5.1"), false),
        ("Opus 5.1", Opus, Some("5.1"), false),
        ("OPUS.7.1", Opus, Some("7.1"), false),
        ("FLAC2.0", Flac, Some("2.0"), false),
        ("AAC.6CH", Aac, Some("5.1"), false),
    ] {
        for name in [
            format!("Movie.2019.1080p.BluRay.{text}.x264-GRP"),
            format!("Movie 2019 1080p BluRay x264 {text}-GRP"),
            format!("Show.S02E05.Some.Title.1080p.WEB-DL.{text}.H.264-GRP"),
        ] {
            let p = read(&name);
            assert_eq!(p.audio_codec, Some(codec), "{name}");
            assert_eq!(p.audio_channels.as_deref(), channels, "{name}");
            assert_eq!(p.atmos, atmos, "{name}");
            assert_eq!(p.video_codec, Some(VideoCodec::H264), "{name}");
            assert_eq!(p.group.as_deref(), Some("GRP"), "{name}");
            assert_eq!(p.service, None, "{name}");
        }
    }
    // Channels with no codec named.
    let p = read("8.Mile.(2002).DS4K.1080p.10bit.x265.BDRip.5.1-PeruGuy.mkv");
    assert_eq!(
        (p.audio_codec, p.audio_channels.as_deref()),
        (None, Some("5.1"))
    );
    assert_eq!(p.title.as_deref(), Some("8 Mile"));
}

#[test]
fn video_and_hdr() {
    use HdrFormat::*;
    for (text, codec) in [
        ("H.264", VideoCodec::H264),
        ("H 264", VideoCodec::H264),
        ("H264", VideoCodec::H264),
        ("x264", VideoCodec::H264),
        ("x265", VideoCodec::H265),
        ("H.265", VideoCodec::H265),
        ("HEVC", VideoCodec::H265),
        ("AV1", VideoCodec::Av1),
    ] {
        let name = format!("Movie 2019 1080p WEB-DL DDP5 1 {text}-GRP");
        let p = read(&name);
        assert_eq!(p.video_codec, Some(codec), "{name}");
        assert_eq!(note(&p, "video_codec").token, text);
        assert_eq!(p.audio_channels.as_deref(), Some("5.1"), "{name}");
    }
    assert_eq!(
        read("Movie.2019.1080p.BluRay.10bit.x265-GRP").bit_depth,
        Some(10)
    );
    assert_eq!(
        read("Movie (2019) 2160p H265 10 bit DV-GRP").bit_depth,
        Some(10)
    );
    assert_eq!(read("Movie.2019.1080p.BluRay.x264-GRP").bit_depth, None);

    for (text, hdr) in [
        ("DV", vec![DolbyVision]),
        ("DoVi", vec![DolbyVision]),
        ("Dolby.Vision", vec![DolbyVision]),
        ("HDR10Plus", vec![Hdr10Plus]),
        ("HDR10+", vec![Hdr10Plus]),
        ("HDR10", vec![Hdr10]),
        ("HDR", vec![Hdr]),
        ("HLG", vec![Hlg]),
        ("DV.HDR10Plus", vec![DolbyVision, Hdr10Plus]),
        ("DV.HDR10+", vec![DolbyVision, Hdr10Plus]),
        ("DoVi HDR10", vec![DolbyVision, Hdr10]),
        ("DV HDR", vec![DolbyVision, Hdr]),
        ("SDR", vec![]),
    ] {
        let name = format!("Movie.2019.2160p.WEB-DL.{text}.DDP5.1.H.265-GRP");
        let p = read(&name);
        assert_eq!(p.hdr, hdr, "{name}");
        assert_eq!(p.audio_codec, Some(AudioCodec::Eac3), "{name}");
    }
}

#[test]
fn services_only_after_the_title_and_as_written() {
    for tag in [
        "AMZN", "NF", "ATVP", "DSNP", "HMAX", "MAX", "HULU", "PCOK", "PMTP", "MA", "iT", "STAN",
        "CRAV", "NOW", "ROKU",
    ] {
        let name = format!("Movie.2019.1080p.{tag}.WEB-DL.DDP5.1.H.264-GRP");
        let p = read(&name);
        assert_eq!(
            p.service.as_deref(),
            Some(tag.to_uppercase().as_str()),
            "{name}"
        );
        assert_eq!(note(&p, "service").token, tag);
    }
    // Title words that spell a tag.
    for (name, title) in [
        ("It.2017.1080p.BluRay.x264-GRP", "It"),
        (
            "Now.You.See.Me.2013.1080p.BluRay.x264-GRP",
            "Now You See Me",
        ),
        ("Max.2015.1080p.BluRay.x264-GRP", "Max"),
        ("Ma.2019.1080p.BluRay.x264-GRP", "Ma"),
        (
            "Stan.and.Ollie.2018.1080p.BluRay.x264-GRP",
            "Stan and Ollie",
        ),
        ("NOW.2020.1080p.BluRay.x264-GRP", "NOW"),
    ] {
        let p = read(name);
        assert_eq!(
            (p.title.as_deref(), p.service.as_deref()),
            (Some(title), None),
            "{name}"
        );
    }
    // After the title, the common words count only in the tag's own spelling.
    for name in [
        "Movie.2019.1080p.BluRay.DTS-HD.MA.5.1.x264-GRP",
        "Movie.2019.1080p.WEB-DL.it.x264-GRP",
        "Movie.2019.1080p.WEB-DL.Now.x264-GRP",
        "Movie.2019.1080p.WEB-DL.Max.x264-GRP",
    ] {
        assert_eq!(read(name).service, None, "{name}");
    }
    // An episode title is not searched for tags.
    let p = read("Show.S01E03.Now.or.Never.1080p.HULU.WEB-DL.DDP5.1.H.264-NTb");
    assert_eq!(p.service.as_deref(), Some("HULU"));
    // A channel behind a platform is the platform.
    let p = read("Movie.2023.1080p.AMZN.HWDY.WEB-DL.DDP5.1.H.264-GRP");
    assert_eq!(p.service.as_deref(), Some("AMZN"));
    // A service with no source named is a web download, and the note says so.
    let p = read("Doctor Strange (2016) IMAX [SDR] DS4K 1080p DSNP DDP 7.1 \u{2014} PeruGuy.mkv");
    assert_eq!(quality(&p), "WEBDL-1080p");
    assert!(
        p.notes
            .iter()
            .any(|n| n.field == "source" && n.read_as.contains("implied"))
    );
}

#[test]
fn sources() {
    for (text, label) in [
        ("BluRay.REMUX", "Remux-1080p"),
        ("BDRemux", "Remux-1080p"),
        ("Bluray.Remux.AVC", "Remux-1080p"),
        ("BluRay", "Bluray-1080p"),
        ("Blu-ray", "Bluray-1080p"),
        ("BDRip", "Bluray-1080p"),
        ("WEB-DL", "WEBDL-1080p"),
        ("WEB.DL", "WEBDL-1080p"),
        ("WEB DL", "WEBDL-1080p"),
        ("WEBDL", "WEBDL-1080p"),
        ("WEB", "WEBDL-1080p"),
        ("WEBRip", "WEBRip-1080p"),
        ("WEB-Rip", "WEBRip-1080p"),
        ("HDTV", "HDTV-1080p"),
    ] {
        let name = format!("Movie 2019 1080p {text} x264-GRP");
        assert_eq!(quality(&read(&name)), label, "{name}");
    }
    assert_eq!(
        quality(&read("Movie.2019.UHD.BluRay.2160p.REMUX.HEVC-GRP")),
        "Remux-2160p"
    );
    assert_eq!(quality(&read("Movie.2019.720p.HDTV.x264-GRP")), "HDTV-720p");
    // DVD and SDTV say 480p without a line count. The note names the token that implied it.
    let p = read("Show.S01E01.DVDRip.x264-GRP");
    assert_eq!(quality(&p), "DVD-480p");
    assert!(note(&p, "resolution").read_as.contains("implied"));
    // A resolution with no source stays without one. The arrs assume HDTV here.
    let p = read("Bad.Monkey.S01E02.1080p.x265-ELiTE");
    assert_eq!((p.source, p.resolution), (None, Some(Resolution::R1080)));
    assert_eq!(p.quality(), None);
}

#[test]
fn editions() {
    for (name, editions) in [
        (
            "Stargate (1994) DC (1080p BluRay x265 HEVC 10bit AAC 7.1 Tigole) [QxR]",
            vec!["Director's Cut"],
        ),
        (
            "Revolver.2005.Directors.Cut.1080p.BluRay.Opus.5.1.HEVC-BHDStudio",
            vec!["Director's Cut"],
        ),
        (
            "Kingpin (1996) Extended (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole)",
            vec!["Extended"],
        ),
        (
            "Lords.of.Dogtown.2005.Extended.Cut.1080p.BluRay.Opus.5.1.HEVC-BHDStudio",
            vec!["Extended"],
        ),
        (
            "Saw II (2005) Unrated (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole)",
            vec!["Unrated"],
        ),
        (
            "Aquaman.2018.IMAX.DS4K.1080p.BluRay.HDR10.Opus.7.1.x265-PeruGuy",
            vec!["IMAX"],
        ),
        (
            "The.Player.1992.Criterion.1080p.BluRay.Opus.2.0.HEVC-SARTRE",
            vec!["Criterion"],
        ),
        (
            "The.Silence.of.the.Lambs.1991.RM4k.CC.1080p.BluRay.x265.hevc.10bit.AAC.5.1.commentary-HeVK",
            vec!["Remastered", "4K Remaster", "Criterion"],
        ),
        (
            "Crocodile Dundee 1986 Encore Cut 1080p UHD BluRay DD 5 1 SDR x265-DON",
            vec!["Encore Cut"],
        ),
        (
            "Horrible Bosses (2011) {imdb-tt1499658} [Extended Cut][BluRay-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv",
            vec!["Extended"],
        ),
        (
            "Memento (2000) RM (1080p BluRay x265 afm72).mkv",
            vec!["Remastered"],
        ),
    ] {
        assert_eq!(read(name).editions, editions, "{name}");
    }
    assert_eq!(
        editions_from_text("4K Remastered - Director's Cut"),
        ["Remastered", "4K Remaster", "Director's Cut"]
    );
    assert_eq!(editions_from_text("Criterion Collection"), ["Criterion"]);
    assert_eq!(
        editions_from_text("Theatrical Open Matte"),
        ["Theatrical", "Open Matte"]
    );
    assert_eq!(editions_from_text("Taco Bell Cut"), ["Taco Bell Cut"]);
    assert_eq!(editions_from_text(""), Vec::<String>::new());
    for name in EDITIONS {
        // A 4K remaster is also a remaster, IMAX Enhanced is also IMAX.
        assert!(
            editions_from_text(name).iter().any(|e| e == name),
            "{name} does not read back"
        );
    }
}

#[test]
fn episodes() {
    let p = read("Shoresy.S01E03.Know.Your.Role.1080p.HULU.WEB-DL.DDP5.1.H.264-NTb.mkv");
    assert_eq!(
        (p.kind, p.season, p.episodes.clone()),
        (MediaKind::Episode, Some(1), vec![3])
    );
    assert_eq!(p.title.as_deref(), Some("Shoresy"));
    assert_eq!(p.group.as_deref(), Some("NTb"));
    assert_eq!(quality(&p), "WEBDL-1080p");
    assert_eq!(
        (note(&p, "season").token.as_str(), note(&p, "season").at),
        ("S01", 8)
    );
    assert_eq!(
        (note(&p, "episode").token.as_str(), note(&p, "episode").at),
        ("E03", 11)
    );

    for (name, season, episodes) in [
        ("Show.S01E03E04.1080p.WEB-DL.x264-GRP", 1, vec![3, 4]),
        ("Show.S01E03-E04.1080p.WEB-DL.x264-GRP", 1, vec![3, 4]),
        ("Show.S01E03-E06.1080p.WEB-DL.x264-GRP", 1, vec![3, 4, 5, 6]),
        ("Show.S01E03-04.1080p.WEB-DL.x264-GRP", 1, vec![3, 4]),
        ("Show.S01.E03.1080p.WEB-DL.x264-GRP", 1, vec![3]),
        ("Show.1x03.1080p.WEB-DL.x264-GRP", 1, vec![3]),
        ("Show.2019.S12E110.720p.HDTV.x264-GRP", 12, vec![110]),
        (
            "Show (2022) - S01E03 - Know Your Role [HDTV-1080p][EAC3 5.1][h264]-GRP.mkv",
            1,
            vec![3],
        ),
    ] {
        let p = read(name);
        assert_eq!(p.kind, MediaKind::Episode, "{name}");
        assert_eq!(
            (p.season, p.episodes.clone()),
            (Some(season), episodes),
            "{name}"
        );
        assert_eq!(p.title.as_deref(), Some("Show"), "{name}");
        assert_eq!(p.group.as_deref(), Some("GRP"), "{name}");
    }
    assert_eq!(
        read("Show.2019.S12E110.720p.HDTV.x264-GRP").year,
        Some(2019)
    );
    // Flags written right before the technical tokens are read, the episode title is not.
    let p = read("The.West.Wing.1999.S05E19.Proper.1080p.AMZN.WEB-DL.EAC3.2.0.x265-ImE");
    assert!(p.proper);
    assert_eq!(
        (p.title.as_deref(), p.year),
        (Some("The West Wing"), Some(1999))
    );
    let p = read("Show.S01E03.The.Proper.Cam.Extended.Cut.TS.1080p.WEB-DL.x264-GRP");
    assert!(!p.proper && p.editions.is_empty());
    assert_eq!(p.source, Some(Source::WebDl));

    for (name, season) in [
        ("Show.S02.1080p.BluRay.x265-GRP", 2),
        ("Show.Season.2.1080p.BluRay.x265-GRP", 2),
        ("Show Season 2 Complete 1080p BluRay x265-GRP", 2),
    ] {
        let p = read(name);
        assert_eq!(
            (p.kind, p.season),
            (MediaKind::SeasonPack, Some(season)),
            "{name}"
        );
        assert!(p.episodes.is_empty(), "{name}");
        assert_eq!(p.title.as_deref(), Some("Show"), "{name}");
        assert_eq!(quality(&p), "Bluray-1080p", "{name}");
    }
    // "Season 2" in front of a year is a title.
    let p = read("Open Season 2 (2008) (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole) [QxR]");
    assert_eq!(
        (p.kind, p.title.as_deref()),
        (MediaKind::Movie, Some("Open Season 2"))
    );

    let p = read("The.Daily.Show.2026.11.20.Some.Guest.1080p.WEB.h264-GRP");
    assert_eq!(p.kind, MediaKind::Episode);
    assert_eq!(p.air_date.as_deref(), Some("2026-11-20"));
    assert_eq!((p.season, p.episodes.is_empty()), (None, true));
    assert_eq!(p.title.as_deref(), Some("The Daily Show"));
    assert_eq!(note(&p, "air_date").token, "2026.11.20");

    // Anime absolute numbering is out of scope: the kind stays unknown, nothing is guessed.
    for name in [
        "[SubsPlease] Sousou no Frieren - 12 (1080p) [ABCD1234].mkv",
        "[Group] Some Show (2023) - 05 [1080p HEVC]",
    ] {
        let p = read(name);
        assert_eq!(p.kind, MediaKind::Unknown, "{name}");
        assert!(p.season.is_none() && p.episodes.is_empty(), "{name}");
        assert_eq!(p.resolution, Some(Resolution::R1080), "{name}");
    }
    assert_eq!(
        read("[SubsPlease] Sousou no Frieren - 12 (1080p) [ABCD1234].mkv")
            .group
            .as_deref(),
        Some("SubsPlease")
    );
}

#[test]
fn a_librarys_own_names() {
    let p = read(
        "10 Cloverfield Lane (2016) {imdb-tt1179933} [Bluray-1080p Proper][DV HDR10][EAC3 Atmos 5.1][x265]-SM737.mkv",
    );
    assert_eq!(p.title.as_deref(), Some("10 Cloverfield Lane"));
    assert_eq!(quality(&p), "Bluray-1080p");
    assert_eq!(p.hdr, [HdrFormat::DolbyVision, HdrFormat::Hdr10]);
    assert_eq!(
        (
            p.audio_codec,
            p.audio_channels.as_deref(),
            p.atmos,
            p.proper
        ),
        (Some(AudioCodec::Eac3), Some("5.1"), true, true)
    );
    assert_eq!(p.group.as_deref(), Some("SM737"));

    // Names the encode pipeline writes, with the group in brackets.
    for (name, label, group) in [
        (
            "#1 Cheerleader Camp (2010) {imdb-tt1637976} [BluRay-1080p][Opus 5.1][HEVC 10bit HUSH]-[SHR].mkv",
            "Bluray-1080p",
            "SHR",
        ),
        (
            "The Burial (2023) {tmdb-763165} [BluRay-1080p][Opus 5.1][HEVC HUSH]-[SHB].mkv",
            "Bluray-1080p",
            "SHB",
        ),
        (
            "Rise of the Guardians (2012) {tmdb-81188} [HMAX WEB-DL-1080p][Opus 5.1][HEVC HUSH]-[SHH].mkv",
            "WEBDL-1080p",
            "SHH",
        ),
        (
            "Saving Private Ryan (1998) {imdb-tt0120815} [BluRay-2160p][UHD][HDR10][Opus 7.1][HEVC 10bit HUSH]-[SHR].mkv",
            "Bluray-2160p",
            "SHR",
        ),
        (
            "Game.of.Thrones.2011.S08E01.1080p.AMZN.WEB-DL.Opus.5.1.HEVC-SHH",
            "WEBDL-1080p",
            "SHH",
        ),
        (
            "Halo.S02E01.1080p.WEB-DL.Opus.5.1.HEVC-SHT",
            "WEBDL-1080p",
            "SHT",
        ),
        (
            "Show.S01E01.1080p.WEB-DL.Opus.5.1.AV1-SHA",
            "WEBDL-1080p",
            "SHA",
        ),
        (
            "Show.S01E01.1080p.WEB-DL.Opus.2.0.HEVC-SHK",
            "WEBDL-1080p",
            "SHK",
        ),
    ] {
        let p = read(name);
        assert_eq!(quality(&p), label, "{name}");
        assert_eq!(p.group.as_deref(), Some(group), "{name}");
        assert_eq!(p.audio_codec, Some(AudioCodec::Opus), "{name}");
        assert_eq!(p.collective, None, "{name}");
    }
    let p = read(
        "Rise of the Guardians (2012) {tmdb-81188} [HMAX WEB-DL-1080p][Opus 5.1][HEVC HUSH]-[SHH].mkv",
    );
    assert_eq!(p.service.as_deref(), Some("HMAX"));
    assert_eq!(p.title.as_deref(), Some("Rise of the Guardians"));
    // A pipeline name with no group suffix has no group: HUSH is the pipeline's mark.
    let p = read("Wreck-It Ralph (2012) [BluRay-1080p][Opus 5.1][AV1 10bit HUSH].mkv");
    assert_eq!((p.group, p.video_codec), (None, Some(VideoCodec::Av1)));
    assert_eq!(p.title.as_deref(), Some("Wreck-It Ralph"));
}

#[test]
fn quality_labels_the_arrs_use() {
    for (label, source, resolution) in [
        ("WEBDL-1080p", Source::WebDl, Resolution::R1080),
        ("Remux-2160p", Source::Remux, Resolution::R2160),
        ("Bluray-1080p Remux", Source::Remux, Resolution::R1080),
        ("HDTV-720p", Source::Tv, Resolution::R720),
        ("DVD", Source::Dvd, Resolution::R480),
        ("SDTV", Source::Tv, Resolution::R480),
        ("Bluray-576p", Source::BluRay, Resolution::R576),
        ("WEBRip-480p", Source::WebRip, Resolution::R480),
    ] {
        assert_eq!(
            Quality::from_label(label),
            Some(Quality { source, resolution }),
            "{label}"
        );
    }
    assert_eq!(Quality::from_label("Unknown"), None);
    // Every label the parser can produce reads back.
    let q = Quality {
        source: Source::Dvd,
        resolution: Resolution::R480,
    };
    assert_eq!(Quality::from_label(&q.label()), Some(q));
}

#[test]
fn nothing_panics_on_odd_input() {
    for name in [
        "",
        " ",
        "-",
        "[",
        "]]]",
        "((((",
        "....",
        "S01E01",
        "2019",
        "1080p",
        "-GRP",
        "[]",
        "{edition-}",
        "\u{2014}\u{2014}",
        "Movie (2019) - ",
        "Movie 2019 1080p WEB-DL -",
        "Кино.2019.1080p.WEB-DL.x264-ГРУППА",
        "映画 (2019) [1080p] [日本語]",
        "a.mkv",
        "&mdash;&mdash;&mdash;",
        "Movie.2019.1080p...",
        "...",
    ] {
        let p = parse(name);
        for n in &p.notes {
            assert_eq!(
                p.raw.get(n.at..n.at + n.token.len()),
                Some(n.token.as_str()),
                "{name:?}"
            );
        }
    }
}
