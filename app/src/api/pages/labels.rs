//! Pure pieces of the page answers: the words the client shows for what a file is, the status
//! words of its vocabulary, and the reading of the managers' lists. No database, no network.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::Deserialize;
use singularr_core::{
    AudioCodec, HdrFormat, ParsedRelease, Quality, Resolution, Source, VideoCodec,
};

// What a file is ---------------------------------------------------------------------------------

/// Lines of a resolution: 1080, 2160.
pub fn lines(r: Resolution) -> i32 {
    match r {
        Resolution::R480 => 480,
        Resolution::R576 => 576,
        Resolution::R720 => 720,
        Resolution::R1080 => 1080,
        Resolution::R1440 => 1440,
        Resolution::R2160 => 2160,
    }
}

/// A source in the client's words: `BluRay`, `WEB-DL`, `WEBRip`, `Remux`, `HDTV`, `DVD`.
pub fn source(s: Source) -> &'static str {
    match s {
        Source::Workprint => "Workprint",
        Source::Cam => "CAM",
        Source::Telesync => "Telesync",
        Source::Telecine => "Telecine",
        Source::Screener => "Screener",
        Source::Dvd => "DVD",
        Source::Tv => "HDTV",
        Source::WebRip => "WEBRip",
        Source::WebDl => "WEB-DL",
        Source::BluRay => "BluRay",
        Source::Remux => "Remux",
        Source::Disc => "BR-DISK",
    }
}

/// `1080p WEB-DL`, `2160p Remux`.
pub fn quality_text(resolution: Option<Resolution>, src: Option<Source>) -> Option<String> {
    let text = [resolution.map(Resolution::label), src.map(source)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    (!text.is_empty()).then_some(text)
}

/// The source of a manager's quality name: `WEBDL-1080p` is `WEB-DL`.
pub fn source_of_quality(name: &str) -> Option<&'static str> {
    Quality::from_label(name).map(|q| source(q.source))
}

pub fn codec(c: VideoCodec) -> &'static str {
    match c {
        VideoCodec::H264 => "H.264",
        VideoCodec::H265 => "HEVC",
        VideoCodec::Av1 => "AV1",
        VideoCodec::Vc1 => "VC-1",
        VideoCodec::Mpeg2 => "MPEG-2",
        VideoCodec::Xvid => "MPEG-4",
        VideoCodec::Vp9 => "VP9",
    }
}

/// A probe's codec name in the same words: `x265` is `HEVC`.
pub fn probe_codec(name: &str) -> Option<String> {
    let n = name.trim();
    if n.is_empty() {
        return None;
    }
    Some(
        match n.to_ascii_lowercase().replace('.', "").as_str() {
            "x265" | "h265" | "hevc" => "HEVC",
            "x264" | "h264" | "avc" => "H.264",
            "av1" => "AV1",
            "xvid" | "divx" | "mpeg4" => "MPEG-4",
            "vc1" | "vc-1" => "VC-1",
            "mpeg2" | "mpeg2video" => "MPEG-2",
            "vp9" => "VP9",
            _ => n,
        }
        .to_string(),
    )
}

/// `DV HDR10`, `HDR10+`. None for SDR.
pub fn hdr(list: &[HdrFormat]) -> Option<String> {
    let words: Vec<&str> = list
        .iter()
        .map(|h| match h {
            HdrFormat::DolbyVision => "DV",
            HdrFormat::Hdr10 => "HDR10",
            HdrFormat::Hdr10Plus => "HDR10+",
            HdrFormat::Hlg => "HLG",
            HdrFormat::Pq => "PQ",
            HdrFormat::Hdr => "HDR",
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// A probe's dynamic range (`DV HDR10Plus`) in the same words (`DV HDR10+`).
pub fn probe_hdr(text: &str) -> Option<String> {
    let t = text.replace("HDR10Plus", "HDR10+").trim().to_string();
    (!t.is_empty()).then_some(t)
}

pub fn audio_codec(a: AudioCodec) -> &'static str {
    match a {
        AudioCodec::TrueHd => "TrueHD",
        AudioCodec::DtsX => "DTS:X",
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

/// Channels the way a layout is written everywhere: `5.1`, `7.1`, and `2.0` for stereo
/// whether the probe says `2` or `2.0`. A whole number over two is a count the probe gave
/// without a layout (`8`), and stays a count (`8 ch`): which of them is the low channel is
/// not known, so no layout is made up for it.
fn channels(text: &str) -> Option<String> {
    let n: f64 = text.trim().parse().ok()?;
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    Some(if n.fract() != 0.0 {
        format!("{n}")
    } else if n <= 2.0 {
        format!("{n:.1}")
    } else {
        format!("{n:.0} ch")
    })
}

/// `EAC3 Atmos 5.1`, from the probe's codec and channels.
pub fn probe_audio(codec: &str, chans: Option<&str>) -> Option<String> {
    let c = codec.trim();
    if c.is_empty() {
        return None;
    }
    Some(match chans.and_then(channels) {
        Some(ch) => format!("{c} {ch}"),
        None => c.to_string(),
    })
}

/// `EAC3 Atmos 5.1`, from a release name's reading.
pub fn release_audio(r: &HeldFacts) -> Option<String> {
    let mut text = audio_codec(r.audio_codec?).to_string();
    if r.atmos {
        text.push_str(" Atmos");
    }
    if let Some(ch) = r.audio_channels.as_deref().and_then(channels) {
        text.push(' ');
        text.push_str(&ch);
    }
    Some(text)
}

/// The fields of a release as held that the tables show. A field that does not read is
/// absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct HeldFacts {
    pub resolution: Option<Resolution>,
    pub source: Option<Source>,
    pub video_codec: Option<VideoCodec>,
    pub hdr: Vec<HdrFormat>,
    pub audio_codec: Option<AudioCodec>,
    pub audio_channels: Option<String>,
    pub atmos: bool,
    pub group: Option<String>,
}

impl HeldFacts {
    pub fn from_json(v: Option<&serde_json::Value>) -> HeldFacts {
        v.and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    pub fn from_release(r: &ParsedRelease) -> HeldFacts {
        HeldFacts {
            resolution: r.resolution,
            source: r.source,
            video_codec: r.video_codec,
            hdr: r.hdr.clone(),
            audio_codec: r.audio_codec,
            audio_channels: r.audio_channels.clone(),
            atmos: r.atmos,
            group: r.group.clone(),
        }
    }
}

/// The TMDB path of an image address the managers store (`.../t/p/original/abc.jpg` is
/// `/abc.jpg`), so the client asks the size it needs. Any other address is kept as it is.
pub fn poster(url: Option<&str>) -> Option<String> {
    let u = url?.trim();
    if u.is_empty() {
        return None;
    }
    let marker = "image.tmdb.org/t/p/";
    if let Some(at) = u.find(marker) {
        let rest = &u[at + marker.len()..];
        if let Some(i) = rest.find('/') {
            return Some(rest[i..].to_string());
        }
    }
    Some(u.to_string())
}

/// `movie-603`, `show-1396`.
pub fn title_id(kind: &str, tmdb: Option<i64>) -> Option<String> {
    tmdb.filter(|n| *n > 0).map(|n| format!("{kind}-{n}"))
}

// Status words ------------------------------------------------------------------------------------

/// Where a title's downloads are.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Motion {
    pub stuck: bool,
    pub downloading: bool,
    pub importing: bool,
}

impl Motion {
    /// A download of the title is stuck or on its way.
    pub fn any(self) -> bool {
        self.stuck || self.downloading || self.importing
    }
}

/// What a card of a title says: held (a show partly), else where its download is, else
/// requested, else missing. A stuck download needs attention whatever is held.
pub fn card_state(
    kind: &str,
    copies: i64,
    have: i32,
    episodes: i32,
    motion: Motion,
    requested: bool,
) -> &'static str {
    if motion.stuck {
        return "attention";
    }
    if copies > 0 {
        return if kind == "show" && episodes > 0 && have < episodes {
            "partial"
        } else {
            "available"
        };
    }
    if motion.importing {
        "importing"
    } else if motion.downloading {
        "downloading"
    } else if requested {
        "requested"
    } else {
        "missing"
    }
}

/// A movie the library does not hold yet is `unreleased` while the manager calls it announced
/// or in theaters (not out for a home library).
pub fn unreleased(status: Option<&str>) -> bool {
    matches!(
        status.map(str::to_ascii_lowercase).as_deref(),
        Some("tba" | "announced" | "incinemas")
    )
}

/// The stage of one download: `unassigned` (no manager owns it), `downloading`, `importing`
/// (complete and still in a manager's queue), or `seeding` (done).
pub fn queue_stage(orphan: bool, progress: f64, in_queue: bool) -> &'static str {
    if orphan {
        "unassigned"
    } else if progress < 1.0 {
        "downloading"
    } else if in_queue {
        "importing"
    } else {
        "seeding"
    }
}

/// A request's status in the client's words, from Seerr's words and where the title's download
/// is. A request still waiting for a decision is `requested`.
pub fn request_status(request: &str, media: &str, motion: Motion) -> &'static str {
    match request {
        "pending" => return "requested",
        "declined" => return "declined",
        "failed" => return "failed",
        "approved" | "completed" => {}
        _ => return "pending",
    }
    if media == "available" {
        return "available";
    }
    if motion.stuck {
        "attention"
    } else if motion.importing {
        "importing"
    } else if motion.downloading {
        "downloading"
    } else if media == "partially_available" {
        "partial"
    } else if request == "completed" && matches!(media, "deleted" | "unknown" | "blocklisted") {
        // It was in the library and has left it.
        "missing"
    } else {
        "approved"
    }
}

/// A gate result as a rule line's result: the seasons that fit pass, a trim or a decline
/// does not, and a request the gate does not look at is not applied.
pub fn gate_result(result: &str) -> &'static str {
    match result {
        "approve" => "pass",
        "trim" | "decline" => "fail",
        _ => "skip",
    }
}

// The managers' lists -----------------------------------------------------------------------------

/// A manager's history event in the client's words. Events the pages do not show (renames,
/// ignored downloads) are `None`.
pub fn history_event(event: &str) -> Option<&'static str> {
    match event {
        "grabbed" => Some("grabbed"),
        "downloadFolderImported" | "movieFolderImported" | "seriesFolderImported" => {
            Some("imported")
        }
        "downloadFailed" => Some("failed"),
        "movieFileDeleted" | "episodeFileDeleted" => Some("removed"),
        _ => None,
    }
}

/// One history record as the folding reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event<'a> {
    pub at: DateTime<Utc>,
    pub title: Option<i64>,
    pub episode: Option<(i32, i32)>,
    pub event: &'a str,
    pub reason: Option<&'a str>,
    pub quality: Option<&'a str>,
}

/// What each record becomes: its event word and, for an upgrade, the quality it replaced.
/// A file deleted because a better one came is folded into that import, which becomes
/// `upgraded`; the deletion itself is dropped. Records are given newest first.
pub fn fold_upgrades<'a>(records: &[Event<'a>]) -> Vec<Option<(&'static str, Option<&'a str>)>> {
    let mut out: Vec<Option<(&'static str, Option<&'a str>)>> = records
        .iter()
        .map(|r| history_event(r.event).map(|e| (e, None)))
        .collect();
    for (i, r) in records.iter().enumerate() {
        let is_upgrade_delete = history_event(r.event) == Some("removed")
            && r.reason.is_some_and(|x| x.eq_ignore_ascii_case("upgrade"));
        if !is_upgrade_delete {
            continue;
        }
        // The import that replaced it: same title and episode, at most ten minutes apart.
        let import = records.iter().enumerate().find(|(j, o)| {
            *j != i
                && out[*j].is_some_and(|(e, _)| e == "imported")
                && o.title == r.title
                && o.episode == r.episode
                && (o.at - r.at).num_seconds().abs() <= 600
        });
        if let Some((j, _)) = import {
            out[j] = Some(("upgraded", r.quality));
            out[i] = None;
        }
    }
    out
}

/// The last folder or file name of a path.
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

// Calendar ----------------------------------------------------------------------------------------

/// The days the calendar covers when none are asked: the week before this one through three
/// weeks ahead, four weeks from a Monday.
pub fn default_range(today: NaiveDate) -> (NaiveDate, NaiveDate) {
    let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let from = monday - Duration::days(7);
    (from, from + Duration::days(27))
}

/// The state of an episode on the calendar.
pub fn episode_state(
    has_file: bool,
    motion: Motion,
    airs_at: Option<DateTime<Utc>>,
    day: NaiveDate,
    now: DateTime<Utc>,
) -> &'static str {
    if has_file {
        "available"
    } else if motion.stuck {
        "attention"
    } else if motion.importing {
        "importing"
    } else if motion.downloading {
        "downloading"
    } else if airs_at.map_or(day > now.date_naive(), |a| a > now) {
        "unaired"
    } else {
        "missing"
    }
}

/// The state of a movie release on the calendar. A release in theaters is not out for a home
/// library.
pub fn release_state(
    has_file: bool,
    motion: Motion,
    release: &str,
    day: NaiveDate,
    today: NaiveDate,
) -> &'static str {
    if has_file {
        "available"
    } else if motion.stuck {
        "attention"
    } else if motion.importing {
        "importing"
    } else if motion.downloading {
        "downloading"
    } else if release == "theaters" || day > today {
        "unreleased"
    } else {
        "missing"
    }
}

/// A Plex session's decision as the client words it: `Direct play`.
pub fn stream(decision: &str) -> String {
    let mut c = decision.chars();
    match c.next() {
        Some(first) => first.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn a_file_reads_in_the_words_of_the_client() {
        assert_eq!(
            quality_text(Some(Resolution::R1080), Some(Source::WebDl)).as_deref(),
            Some("1080p WEB-DL")
        );
        assert_eq!(source_of_quality("Bluray-2160p"), Some("BluRay"));
        assert_eq!(probe_codec("x265").as_deref(), Some("HEVC"));
        assert_eq!(probe_codec("h264").as_deref(), Some("H.264"));
        assert_eq!(probe_hdr("DV HDR10Plus").as_deref(), Some("DV HDR10+"));
        assert_eq!(probe_hdr(" "), None);
        assert_eq!(
            probe_audio("EAC3 Atmos", Some("5.1")).as_deref(),
            Some("EAC3 Atmos 5.1")
        );
        assert_eq!(probe_audio("Opus", Some("2")).as_deref(), Some("Opus 2.0"));
        assert_eq!(
            probe_audio("Opus", Some("2.0")).as_deref(),
            Some("Opus 2.0")
        );
        assert_eq!(probe_audio("AAC", Some("7.1")).as_deref(), Some("AAC 7.1"));
        assert_eq!(probe_audio("AAC", Some("1")).as_deref(), Some("AAC 1.0"));
        assert_eq!(
            probe_audio("EAC3 Atmos", Some("8")).as_deref(),
            Some("EAC3 Atmos 8 ch")
        );
        assert_eq!(probe_audio("AAC", Some("0")).as_deref(), Some("AAC"));
        assert_eq!(
            hdr(&[HdrFormat::DolbyVision, HdrFormat::Hdr10]).as_deref(),
            Some("DV HDR10")
        );
        let held = HeldFacts::from_json(Some(&serde_json::json!({
            "resolution": "2160p", "source": "WebDl", "video_codec": "H265",
            "hdr": ["DolbyVision"], "audio_codec": "Eac3", "audio_channels": "5.1",
            "atmos": true, "group": "GRP", "editions": ["Open Matte"]
        })));
        assert_eq!(held.resolution.map(lines), Some(2160));
        assert_eq!(release_audio(&held).as_deref(), Some("EAC3 Atmos 5.1"));
        assert_eq!(
            HeldFacts::from_json(Some(&serde_json::json!("x"))),
            HeldFacts::default()
        );
    }

    #[test]
    fn a_tmdb_address_becomes_a_path_and_any_other_stays() {
        assert_eq!(
            poster(Some("https://image.tmdb.org/t/p/original/poster1.jpg")).as_deref(),
            Some("/poster1.jpg")
        );
        assert_eq!(
            poster(Some("https://artworks.thetvdb.com/banners/posters/p1.jpg")).as_deref(),
            Some("https://artworks.thetvdb.com/banners/posters/p1.jpg")
        );
        assert_eq!(poster(Some("  ")), None);
        assert_eq!(title_id("movie", Some(603)).as_deref(), Some("movie-603"));
        assert_eq!(title_id("show", None), None);
    }

    #[test]
    fn cards_and_requests_speak_the_status_vocabulary() {
        let none = Motion::default();
        let busy = Motion {
            downloading: true,
            ..Motion::default()
        };
        let stuck = Motion {
            stuck: true,
            ..Motion::default()
        };
        assert_eq!(card_state("movie", 1, 0, 0, none, false), "available");
        assert_eq!(card_state("show", 2, 2, 10, none, false), "partial");
        assert_eq!(card_state("show", 10, 10, 10, none, false), "available");
        assert_eq!(card_state("movie", 0, 0, 0, busy, true), "downloading");
        assert_eq!(card_state("movie", 0, 0, 0, none, true), "requested");
        assert_eq!(card_state("movie", 0, 0, 0, none, false), "missing");
        assert_eq!(card_state("movie", 1, 0, 0, stuck, false), "attention");
        assert!(unreleased(Some("announced")) && unreleased(Some("inCinemas")));
        assert!(!unreleased(Some("released")) && !unreleased(None));

        assert_eq!(request_status("pending", "pending", busy), "requested");
        assert_eq!(request_status("approved", "processing", none), "approved");
        assert_eq!(
            request_status("approved", "processing", busy),
            "downloading"
        );
        assert_eq!(request_status("approved", "processing", stuck), "attention");
        assert_eq!(request_status("completed", "available", busy), "available");
        assert_eq!(
            request_status("approved", "partially_available", none),
            "partial"
        );
        assert_eq!(request_status("completed", "deleted", none), "missing");
        assert_eq!(request_status("declined", "unknown", none), "declined");
        assert_eq!(request_status("failed", "unknown", none), "failed");
        assert_eq!(request_status("unknown", "unknown", none), "pending");
        assert_eq!(gate_result("approve"), "pass");
        assert_eq!(gate_result("trim"), "fail");
        assert_eq!(gate_result("exempt"), "skip");

        assert_eq!(queue_stage(true, 0.5, false), "unassigned");
        assert_eq!(queue_stage(false, 0.5, true), "downloading");
        assert_eq!(queue_stage(false, 1.0, true), "importing");
        assert_eq!(queue_stage(false, 1.0, false), "seeding");
        assert_eq!(stream("direct play"), "Direct play");
    }

    #[test]
    fn a_deleted_file_and_the_import_that_replaced_it_are_one_upgrade() {
        let at =
            |m: i64| Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap() + Duration::minutes(m);
        let records = [
            Event {
                at: at(1),
                title: Some(7),
                episode: Some((1, 4)),
                event: "downloadFolderImported",
                reason: None,
                quality: Some("Bluray-1080p"),
            },
            Event {
                at: at(0),
                title: Some(7),
                episode: Some((1, 4)),
                event: "episodeFileDeleted",
                reason: Some("Upgrade"),
                quality: Some("HDTV-1080p"),
            },
            Event {
                at: at(-30),
                title: Some(7),
                episode: Some((1, 4)),
                event: "grabbed",
                reason: None,
                quality: Some("Bluray-1080p"),
            },
            Event {
                at: at(-60),
                title: Some(9),
                episode: None,
                event: "movieFileDeleted",
                reason: Some("MissingFromDisk"),
                quality: None,
            },
            Event {
                at: at(-90),
                title: Some(9),
                episode: None,
                event: "movieFileRenamed",
                reason: None,
                quality: None,
            },
        ];
        let folded = fold_upgrades(&records);
        assert_eq!(folded[0], Some(("upgraded", Some("HDTV-1080p"))));
        assert_eq!(folded[1], None, "the deletion is part of the upgrade");
        assert_eq!(folded[2], Some(("grabbed", None)));
        assert_eq!(folded[3], Some(("removed", None)));
        assert_eq!(folded[4], None, "a rename is not shown");
    }

    #[test]
    fn the_calendar_covers_four_weeks_from_the_monday_before_last() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        let (from, to) = default_range(today);
        assert_eq!(from, NaiveDate::from_ymd_opt(2026, 11, 16).unwrap());
        assert_eq!(to, NaiveDate::from_ymd_opt(2026, 12, 13).unwrap());
        let wednesday = NaiveDate::from_ymd_opt(2026, 11, 25).unwrap();
        assert_eq!(default_range(wednesday).0, from);

        let now = Utc.with_ymd_and_hms(2026, 11, 23, 19, 0, 0).unwrap();
        let none = Motion::default();
        let later = Some(Utc.with_ymd_and_hms(2026, 11, 24, 2, 0, 0).unwrap());
        assert_eq!(episode_state(false, none, later, today, now), "unaired");
        assert_eq!(episode_state(true, none, later, today, now), "available");
        assert_eq!(
            episode_state(false, none, None, wednesday - Duration::days(5), now),
            "missing"
        );
        assert_eq!(
            release_state(false, none, "theaters", today, today),
            "unreleased"
        );
        assert_eq!(
            release_state(false, none, "digital", today, today),
            "missing"
        );
        assert_eq!(
            release_state(false, none, "digital", wednesday, today),
            "unreleased"
        );
    }
}
