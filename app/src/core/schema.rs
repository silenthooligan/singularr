//! OpenAPI descriptions of the `singularr-core` types the API returns as they are.
//!
//! The core crate has no OpenAPI dependency, so its types are described here, field for
//! field, and referenced with `#[schema(value_type = ...)]`. They are documentation only:
//! the JSON on the wire is serialized from the core types themselves. A test in
//! `releases::assess` checks that every field the core serializes is described here.

#![allow(dead_code)]

use utoipa::ToSchema;

/// One piece of evidence: which token of the name set which field.
#[derive(ToSchema)]
#[schema(as = ParseNote)]
pub struct ParseNoteDoc {
    /// The field that was set, for example `source` or `group`.
    pub field: String,
    /// The text in the name that set it, as written.
    pub token: String,
    /// Byte offset of the token in the raw name.
    pub at: usize,
    /// What it was read as.
    pub read_as: String,
}

/// A release name read into fields. Every field that is set has a note naming its token.
#[derive(ToSchema)]
#[schema(as = ParsedRelease)]
pub struct ParsedReleaseDoc {
    pub raw: String,
    /// `Movie`, `Episode`, `SeasonPack` or `Unknown`.
    pub kind: String,
    pub title: Option<String>,
    pub year: Option<u16>,
    pub season: Option<u16>,
    pub episodes: Vec<u16>,
    /// `480p`, `576p`, `720p`, `1080p`, `1440p` or `2160p`.
    pub resolution: Option<String>,
    /// `WebDl`, `WebRip`, `BluRay`, `Remux`, `Tv`, `Dvd`, `Disc`, or a pre-release source.
    pub source: Option<String>,
    pub video_codec: Option<String>,
    pub bit_depth: Option<u8>,
    pub hdr: Vec<String>,
    pub audio_codec: Option<String>,
    /// As written, for example `5.1`.
    pub audio_channels: Option<String>,
    pub atmos: bool,
    /// Streaming service tag, upper case: `AMZN`, `NF`.
    pub service: Option<String>,
    /// Normalized edition names: `Open Matte`, `Extended`, `IMAX`.
    pub editions: Vec<String>,
    pub fps: Option<u16>,
    pub ai_upscaled: bool,
    pub proper: bool,
    pub repack: bool,
    pub group: Option<String>,
    pub languages: Vec<String>,
    pub notes: Vec<ParseNoteDoc>,
    /// The collective a group releases under, from a trailing tag: `QxR`.
    pub collective: Option<String>,
    /// The air date of a daily show episode, `2026-11-20`.
    pub air_date: Option<String>,
    /// The name says SDR outright.
    pub sdr: bool,
}

/// One rule that scored, and why.
#[derive(ToSchema)]
#[schema(as = TraceItem)]
pub struct TraceItemDoc {
    pub trait_id: String,
    pub label: String,
    pub category: String,
    pub points: i32,
    /// The reason the rule exists.
    pub why: String,
    /// What in the release made the rule match.
    pub evidence: String,
}

/// The score of one release in one lane, with the rules that produced it.
#[derive(ToSchema)]
#[schema(as = Verdict)]
pub struct VerdictDoc {
    pub lane: String,
    pub score: i32,
    /// For example `WEBDL-1080p`.
    pub quality: Option<String>,
    /// Ladder position, 0 is best. Absent: the lane does not take this quality.
    pub rank: Option<usize>,
    /// The number of steps in the lane's ladder, added by the API next to `rank`. The
    /// verdicts inside a `Decision` do not repeat it.
    pub ladder_steps: Option<usize>,
    pub trace: Vec<TraceItemDoc>,
    /// Traits that matched but carry no points in this lane.
    pub matched_unscored: Vec<String>,
}

/// Whether a candidate would be taken, given what is held.
#[derive(ToSchema)]
#[schema(as = Decision)]
pub struct DecisionDoc {
    /// `Grab`, `Upgrade` or `Hold`.
    pub action: String,
    /// Plain sentences, most important first.
    pub reasons: Vec<String>,
    pub candidate: VerdictDoc,
    pub held: Option<VerdictDoc>,
    /// The protection that stopped it, if one did.
    pub blocked_by: Option<String>,
}

/// An error, as RFC 9457 problem details (`application/problem+json`).
#[derive(ToSchema)]
#[schema(as = Problem)]
pub struct ProblemDoc {
    /// Always `about:blank`.
    #[schema(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: u16,
    /// What went wrong, in a sentence.
    pub detail: Option<String>,
    /// A stable code, for example `singularr.not_found`.
    pub code: String,
    /// The act it is about, when there is one: `GET /api/v1/acts/{id}` reads it.
    pub act: Option<i64>,
}
