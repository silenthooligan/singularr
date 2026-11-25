//! The release-name reader.
//!
//! Order of work:
//! 1. Blank what is not part of the release: site tags, the file extension, `{tmdb-..}` blocks.
//! 2. Split into tokens. Offsets always point into the raw name.
//! 3. Find where the title ends: the last year before the first strong technical token, an episode
//!    marker, or the first strong technical token. Strong tokens are resolutions, unambiguous
//!    sources and video codecs. Words such as Cam, TS, Screener, Proper, Extended and Web are never
//!    strong, so they stay title words until the title has ended.
//! 4. Find the group at the tail (`-GROUP`, ` - GROUP`, `-[GROUP]`).
//! 5. Read technical tokens after the title, left to right, longest reading first.
//! 6. With no dash group, take the group from the structure: the last word of a technical
//!    `( .. )` block, a single trailing word, a trailing `[TAG]`, a leading `[TAG]`.
//!
//! Every field that is set gets a [`ParseNote`] with the token as written and its byte offset.

use super::{
    AudioCodec, HdrFormat, MediaKind, ParseNote, ParsedRelease, Resolution, Source, VideoCodec,
};
use regex::Regex;
use std::sync::LazyLock;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a constant pattern")
}

static YEAR: LazyLock<Regex> = LazyLock::new(|| re(r"^(19\d\d|20\d\d)$"));
static SXXEYY: LazyLock<Regex> = LazyLock::new(|| re(r"^s(\d{1,4})((?:e\d{1,3})+)$"));
static SXX: LazyLock<Regex> = LazyLock::new(|| re(r"^s(\d{1,2})$"));
static EYY: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:e\d{1,3})+$"));
static NXNN: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{1,2})x(\d{2,3})$"));
static WXH: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{3,4}x(\d{3,4})$"));
/// A downscale written as one word: `4Kto1080p`.
static DOWNSCALED: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:4k|uhd|2160p)to(\d{3,4}p)$"));
static CRC: LazyLock<Regex> = LazyLock::new(|| re(r"^[0-9a-f]{8}$"));
static FPS: LazyLock<Regex> = LazyLock::new(|| re(r"^(\d{2,3})fps$"));
static ENCODER_SETTING: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:q|fs|s|crf)\d{2,3}$"));
static VERSION: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:\d{1,2}|\d{1,2}v\d|v\d|lite)$"));
static ORDINAL: LazyLock<Regex> = LazyLock::new(|| re(r"^\d{1,3}(?:st|nd|rd|th)$"));
static EAC3_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"^ddp(a)?([12578])?$"));
static AC3_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"^dd([12578])?$"));
static AAC_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"^(?:q|he)?aac([12578])?$"));
static FLAC_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"^flac([12578])?$"));
static LIB_ID: LazyLock<Regex> = LazyLock::new(|| re(r"\{(?:imdb|tmdb|tvdb|tmbd)-[A-Za-z0-9]+\}"));
static LIB_EDITION: LazyLock<Regex> = LazyLock::new(|| re(r"\{edition-([^}]+)\}"));
/// A site name in front of the release: `[Bitsearch.to] ` or `www.UIndex.org    -    `.
static SITE_HEAD: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)^\s*(?:\[\s*(?:[a-z0-9-]+\.)+(?:to|org|com|net|me|cc|io)\s*\]\s*|www\.[a-z0-9-]+\.[a-z]{2,4}\s+-\s+)",
    )
});
/// One thing that may trail the release: an extension, a copy number, a site tag, a hash, a dot.
static TRAILER: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)(?:\.(?:mkv|mp4|avi|m4v|m2ts|wmv|mov|iso|torrent)|\s(?:mkv|mp4|avi)|\s*\(\d\)|\[\s*(?:tgx|rartv|rarbg|eztv|ettv|glodls|[0-9a-f]{8}|(?:[a-z0-9-]+\.)+(?:to|org|com|net|me|cc|io))\s*\]|\.|\s+)$",
    )
});
static TS_EXT: LazyLock<Regex> = LazyLock::new(|| re(r"\.ts$"));
static GROUP_SITE_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\.(?:com|org|net|to|nu|me|cc|io)$"));
/// An absolute episode number after a spaced dash, as anime releases write it: `Show - 12 [1080p]`.
static ABSOLUTE: LazyLock<Regex> = LazyLock::new(|| re(r" - \d{1,4}(?:v\d)?(?:\s|$|\[|\()"));

/// Groups whose name holds a dash. The plain rule would cut them at the dash.
const HYPHENATED_GROUPS: &[&str] = &["d-z0n3", "fight-bb", "utr-hd", "blu-bits"];
/// Groups written as two words.
const TWO_WORD_GROUPS: &[&str] = &["anime land", "anime time", "project angel", "hakata ramen"];
/// Second halves of technical compounds (`WEB-DL`, `DTS-HD`, `Blu-Ray`, `10-bit`, `Open-Matte`).
const COMPOUND_TAILS: &[&str] = &[
    "dl", "rip", "ray", "hd", "ma", "x", "hra", "es", "bit", "matte", "plus", "vision", "audio",
    "enhanced", "cut", "edition", "version", "mux", "sub", "subs", "dub",
];
/// Words that are neither technical nor a group.
const NOISE: &[&str] = &[
    "extras",
    "commentary",
    "rifftrax",
    "hush",
    "sample",
    "subs",
    "sub",
    "esub",
    "esubs",
    "msubs",
    "hybrid",
    "ds4k",
    "regrade",
    "internal",
    "limited",
    "complete",
    "retail",
    "dubbed",
    "subbed",
    "real",
    "orarbg",
    "eztv",
    "eztvx",
    "tgx",
    "rartv",
    "ettv",
    "mkv",
    "mp4",
    "nfo",
    "hsbs",
    "3d",
    "hfr",
];

#[derive(Debug, Clone)]
struct Tok {
    at: usize,
    end: usize,
    lower: String,
}

fn is_separator(c: char) -> bool {
    matches!(
        c,
        '.' | ' '
            | '_'
            | '['
            | ']'
            | '('
            | ')'
            | '{'
            | '}'
            | ','
            | '+'
            | '-'
            | '~'
            | '\u{2014}'
            | '\u{2013}'
    )
}

fn is_dash(c: char) -> bool {
    matches!(c, '-' | '\u{2014}' | '\u{2013}')
}

fn tokens(text: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        if is_separator(c) {
            if let Some(s) = start.take() {
                out.push(Tok {
                    at: s,
                    end: i,
                    lower: text[s..i].to_ascii_lowercase(),
                });
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        out.push(Tok {
            at: s,
            end: text.len(),
            lower: text[s..].to_ascii_lowercase(),
        });
    }
    out
}

/// The name and its tokens. `work` has the same bytes as `raw` except that ignorable regions are
/// spaces, so an offset in one is an offset in the other.
struct Ctx<'a> {
    raw: &'a str,
    work: &'a str,
    toks: &'a [Tok],
}

impl Ctx<'_> {
    fn lower(&self, i: usize) -> &str {
        self.toks.get(i).map_or("", |t| t.lower.as_str())
    }

    fn text(&self, i: usize) -> &str {
        self.toks.get(i).map_or("", |t| &self.raw[t.at..t.end])
    }

    /// What stands between token `i` and the next one.
    fn gap(&self, i: usize) -> &str {
        match (self.toks.get(i), self.toks.get(i + 1)) {
            (Some(a), Some(b)) => &self.work[a.end..b.at],
            _ => "",
        }
    }

    /// The next token is part of the same compound: at most one plain separator between them.
    fn tight(&self, i: usize) -> bool {
        self.toks.get(i + 1).is_some() && matches!(self.gap(i), "" | "." | " " | "-")
    }

    /// The lower-case token after `i` when it is tight, else the empty string.
    fn next(&self, i: usize) -> &str {
        if self.tight(i) { self.lower(i + 1) } else { "" }
    }

    fn is_upper(&self, i: usize) -> bool {
        let t = self.text(i);
        !t.is_empty() && !t.chars().any(|c| c.is_ascii_lowercase())
    }
}

const UNREAD: u8 = 0;
const TECHNICAL: u8 = 1;
const NOISE_WORD: u8 = 2;

struct Out {
    p: ParsedRelease,
    /// Per token: unread, technical or noise.
    kind: Vec<u8>,
    /// A `4K` or `UHD` token: 2160p when no `NNNNp` token says otherwise.
    implied_resolution: Option<usize>,
    /// A DVD or SDTV source token: 480p when nothing else says.
    sd_source: Option<usize>,
}

impl Out {
    fn new(raw: &str, tokens: usize) -> Out {
        Out {
            p: ParsedRelease {
                raw: raw.to_string(),
                ..Default::default()
            },
            kind: vec![UNREAD; tokens],
            implied_resolution: None,
            sd_source: None,
        }
    }

    fn mark(&mut self, i: usize, k: usize, kind: u8) {
        for slot in self.kind.iter_mut().skip(i).take(k) {
            *slot = kind;
        }
    }

    /// Record evidence for `field` from the byte span `at..end`.
    fn note_span(&mut self, c: &Ctx, field: &str, at: usize, end: usize, read_as: &str) {
        self.p.notes.push(ParseNote {
            field: field.to_string(),
            token: c.raw[at..end].to_string(),
            at,
            read_as: read_as.to_string(),
        });
    }

    /// Record evidence for `field` from tokens `i..i + k` and mark them technical.
    fn note(&mut self, c: &Ctx, field: &str, i: usize, k: usize, read_as: &str) {
        let (at, end) = (c.toks[i].at, c.toks[i + k - 1].end);
        self.note_span(c, field, at, end, read_as);
        self.mark(i, k, TECHNICAL);
    }

    fn edition(&mut self, c: &Ctx, i: usize, k: usize, name: &str) {
        if !self.p.has_edition(name) {
            self.p.editions.push(name.to_string());
        }
        self.note(c, "edition", i, k, name);
    }

    fn hdr(&mut self, c: &Ctx, at: usize, end: usize, format: HdrFormat, read_as: &str) {
        if !self.p.hdr.contains(&format) {
            self.p.hdr.push(format);
        }
        self.note_span(c, "hdr", at, end, read_as);
    }

    fn language(&mut self, c: &Ctx, i: usize, name: &str) {
        if !self.p.languages.iter().any(|l| l == name) {
            self.p.languages.push(name.to_string());
        }
        self.note(c, "language", i, 1, name);
    }
}

fn resolution_of(lower: &str) -> Option<Resolution> {
    if let Some(c) = WXH.captures(lower) {
        return Resolution::from_label(&format!("{}p", &c[1]));
    }
    if let Some(c) = DOWNSCALED.captures(lower) {
        return Resolution::from_label(&c[1]);
    }
    if lower.len() < 4 || !lower.ends_with(['p', 'i']) {
        return None;
    }
    Resolution::from_label(lower)
}

/// A token that can only be technical: it ends the title wherever it stands.
fn strong(c: &Ctx, i: usize) -> bool {
    let l = c.lower(i);
    if resolution_of(l).is_some() {
        return true;
    }
    if matches!(
        l,
        "bluray"
            | "bdrip"
            | "brrip"
            | "bdremux"
            | "remux"
            | "webdl"
            | "webrip"
            | "hdtv"
            | "pdtv"
            | "sdtv"
            | "dvdrip"
            | "dvdscr"
            | "hdcam"
            | "camrip"
            | "hdts"
            | "telesync"
            | "hdtc"
            | "telecine"
            | "workprint"
            | "hdrip"
            | "uhd"
            | "x264"
            | "x265"
            | "h264"
            | "h265"
            | "hevc"
            | "xvid"
            | "divx"
            | "av1"
    ) {
        return true;
    }
    matches!(
        (l, c.next(i)),
        ("web", "dl" | "rip") | ("blu", "ray") | ("h" | "x", "264" | "265")
    )
}

fn source_at(c: &Ctx, i: usize) -> Option<(Source, usize)> {
    let l = c.lower(i);
    let two = match (l, c.next(i)) {
        ("web", "dl") => Some(Source::WebDl),
        ("web", "rip") => Some(Source::WebRip),
        ("blu", "ray") | ("bd" | "br", "rip") => Some(Source::BluRay),
        ("bd", "remux") => Some(Source::Remux),
        ("hd", "rip") => Some(Source::WebRip),
        ("hd", "tv") => Some(Source::Tv),
        ("dvd", "rip") => Some(Source::Dvd),
        ("hd", "cam") => Some(Source::Cam),
        ("hd", "ts") => Some(Source::Telesync),
        _ => None,
    };
    if let Some(s) = two {
        return Some((s, 2));
    }
    let one = match l {
        "webdl" | "web" | "webhd" => Source::WebDl,
        "webrip" | "webmux" | "hdrip" => Source::WebRip,
        "bluray" | "bdrip" | "brrip" | "bd" => Source::BluRay,
        "remux" | "bdremux" => Source::Remux,
        "hdtv" | "pdtv" | "sdtv" | "tvrip" | "hdtvrip" | "dsr" => Source::Tv,
        "dvdrip" | "dvd" => Source::Dvd,
        "dvdscr" | "screener" | "bdscr" => Source::Screener,
        "hdcam" | "cam" | "camrip" => Source::Cam,
        "telesync" | "hdts" => Source::Telesync,
        "telecine" | "hdtc" => Source::Telecine,
        "workprint" => Source::Workprint,
        // Two and three letter forms count only as written in release names: upper case.
        "scr" if c.is_upper(i) => Source::Screener,
        "ts" if c.is_upper(i) => Source::Telesync,
        "tc" if c.is_upper(i) => Source::Telecine,
        "wp" if c.is_upper(i) => Source::Workprint,
        _ => return None,
    };
    Some((one, 1))
}

fn video_at(c: &Ctx, i: usize) -> Option<(VideoCodec, usize)> {
    let l = c.lower(i);
    let two = match (l, c.next(i)) {
        ("h" | "x", "264") => Some(VideoCodec::H264),
        ("h" | "x", "265") => Some(VideoCodec::H265),
        ("vc", "1") => Some(VideoCodec::Vc1),
        ("mpeg", "2") => Some(VideoCodec::Mpeg2),
        _ => None,
    };
    if let Some(v) = two {
        return Some((v, 2));
    }
    let one = match l {
        "x264" | "h264" | "avc" => VideoCodec::H264,
        "x265" | "h265" | "hevc" | "x265m" => VideoCodec::H265,
        "av1" => VideoCodec::Av1,
        "vc1" => VideoCodec::Vc1,
        "mpeg2" => VideoCodec::Mpeg2,
        "xvid" | "divx" => VideoCodec::Xvid,
        "vp9" => VideoCodec::Vp9,
        _ => return None,
    };
    Some((one, 1))
}

/// Tags that are not words, matched in any case.
const SERVICES_ANY_CASE: &[&str] = &[
    "amzn", "atvp", "dsnp", "hmax", "hulu", "pcok", "pmtp", "crav", "roku", "bcore", "vudu",
    "amcp", "mgmp", "skst", "dsny", "aptv", "fdng", "itunes",
];
/// Tags that are also common words or very short: matched exactly as written.
const SERVICES_EXACT: &[&str] = &[
    "NF", "MA", "MAX", "NOW", "iT", "iP", "STAN", "STZ", "CR", "SHO", "HBO", "ITV", "ATV",
];

fn language_of(c: &Ctx, i: usize) -> Option<&'static str> {
    let by_name = match c.lower(i) {
        "english" => "English",
        "french" | "truefrench" | "vff" | "vfq" => "French",
        "german" => "German",
        "spanish" | "latino" | "castellano" => "Spanish",
        "italian" => "Italian",
        "hindi" => "Hindi",
        "korean" => "Korean",
        "japanese" => "Japanese",
        "russian" => "Russian",
        "portuguese" => "Portuguese",
        "dutch" => "Dutch",
        "swedish" => "Swedish",
        "norwegian" => "Norwegian",
        "danish" => "Danish",
        "finnish" => "Finnish",
        "polish" => "Polish",
        "chinese" | "mandarin" | "cantonese" => "Chinese",
        "tamil" => "Tamil",
        "telugu" => "Telugu",
        "turkish" => "Turkish",
        "arabic" => "Arabic",
        "hebrew" => "Hebrew",
        "thai" => "Thai",
        "vietnamese" => "Vietnamese",
        "persian" | "farsi" => "Persian",
        "ukrainian" => "Ukrainian",
        "czech" => "Czech",
        "hungarian" => "Hungarian",
        "greek" => "Greek",
        "romanian" => "Romanian",
        "indonesian" => "Indonesian",
        "icelandic" => "Icelandic",
        "bengali" => "Bengali",
        "malayalam" => "Malayalam",
        "kannada" => "Kannada",
        "punjabi" => "Punjabi",
        "urdu" => "Urdu",
        "tagalog" | "filipino" => "Filipino",
        "multi" => "Multi",
        _ => "",
    };
    if !by_name.is_empty() {
        return Some(by_name);
    }
    // Three letter codes are words too ("spa", "por"), so they count only as release names
    // write them: ENG, GER, or the scene's iTA.
    if !(c.is_upper(i) || c.text(i) == "iTA") {
        return None;
    }
    Some(match c.lower(i) {
        "eng" => "English",
        "ita" => "Italian",
        "fre" | "fra" => "French",
        "ger" | "deu" => "German",
        "spa" | "esp" => "Spanish",
        "hin" => "Hindi",
        "kor" => "Korean",
        "jpn" | "jap" => "Japanese",
        "rus" => "Russian",
        _ => return None,
    })
}

fn capitalized(word: &str) -> String {
    // Short upper-case words are abbreviations: "US Cut".
    if word.len() <= 3 && !word.chars().any(|c| c.is_ascii_lowercase()) {
        return word.to_string();
    }
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => {
            first.to_ascii_uppercase().to_string() + &chars.as_str().to_ascii_lowercase()
        }
        None => String::new(),
    }
}

/// Read an edition at token `i`. Returns the number of tokens used, 0 when it is not an edition.
fn read_edition(c: &Ctx, o: &mut Out, i: usize) -> usize {
    let l = c.lower(i);
    let n1 = c.next(i);
    let closes = |n: &str| matches!(n, "cut" | "edition" | "version");
    let with_close = |n: &str| if closes(n) { 2 } else { 1 };
    match l {
        "open" if n1 == "matte" => {
            o.edition(c, i, 2, "Open Matte");
            2
        }
        "openmatte" => {
            o.edition(c, i, 1, "Open Matte");
            1
        }
        // "OM" alone is open matte only between technical tokens: "1080p.OM.WEB-DL".
        "om" if c.text(i) == "OM"
            && i > 0
            && o.kind[i - 1] == TECHNICAL
            && (strong(c, i + 1) || source_at(c, i + 1).is_some()) =>
        {
            o.edition(c, i, 1, "Open Matte");
            1
        }
        "imax" => {
            if n1 == "enhanced" {
                o.edition(c, i, 2, "IMAX");
                o.edition(c, i, 2, "IMAX Enhanced");
                2
            } else {
                let k = if n1 == "edition" { 2 } else { 1 };
                o.edition(c, i, k, "IMAX");
                k
            }
        }
        "extended" | "unrated" | "uncut" | "uncensored" => {
            let k = with_close(n1);
            o.edition(c, i, k, &capitalized(l));
            k
        }
        "theatrical" => {
            let k = if closes(n1) || n1 == "release" { 2 } else { 1 };
            o.edition(c, i, k, "Theatrical");
            k
        }
        "directors" | "director's" => {
            let k = with_close(n1);
            o.edition(c, i, k, "Director's Cut");
            k
        }
        "director" if n1 == "s" => {
            let k = if closes(c.next(i + 1)) { 3 } else { 2 };
            o.edition(c, i, k, "Director's Cut");
            k
        }
        "dc" if c.text(i) == "DC" => {
            o.edition(c, i, 1, "Director's Cut");
            1
        }
        "final" if n1 == "cut" => {
            o.edition(c, i, 2, "Final Cut");
            2
        }
        // "Black and Chrome", "Black & Chrome Edition" (Mad Max: Fury Road).
        "black" if matches!(n1, "and" | "&") && c.next(i + 1) == "chrome" => {
            let k = if closes(c.next(i + 2)) { 4 } else { 3 };
            o.edition(c, i, k, "Black and Chrome");
            k
        }
        "special" if n1 == "edition" => {
            o.edition(c, i, 2, "Special Edition");
            2
        }
        "ultimate" => {
            let k = with_close(n1);
            o.edition(c, i, k, "Ultimate");
            k
        }
        "collectors" | "collector's" => {
            let k = with_close(n1);
            o.edition(c, i, k, "Collector's");
            k
        }
        "alternate" | "alternative" => {
            let k = with_close(n1);
            o.edition(c, i, k, "Alternate Cut");
            k
        }
        "criterion" => {
            let k = if n1 == "collection" { 2 } else { 1 };
            o.edition(c, i, k, "Criterion");
            k
        }
        "cc" if c.text(i) == "CC" => {
            o.edition(c, i, 1, "Criterion");
            1
        }
        "anniversary" => {
            let from = if i > 0 && o.kind[i - 1] == UNREAD && ORDINAL.is_match(c.lower(i - 1)) {
                i - 1
            } else {
                i
            };
            let k = i - from + with_close(n1);
            o.edition(c, from, k, "Anniversary");
            k - (i - from)
        }
        "rm4k" => {
            o.edition(c, i, 1, "Remastered");
            o.edition(c, i, 1, "4K Remaster");
            1
        }
        "rm" if c.text(i) == "RM" => {
            o.edition(c, i, 1, "Remastered");
            1
        }
        // "4K Remaster", "4K Remastered", "4K Restoration": an edition, not a resolution.
        "4k" if matches!(n1, "remaster" | "remastered" | "restoration" | "restored") => {
            o.edition(c, i, 2, "Remastered");
            o.edition(c, i, 2, "4K Remaster");
            2
        }
        "remastered" | "remaster" => {
            // "Remastered in 4K".
            let in_4k = n1 == "in" && c.next(i + 1) == "4k";
            let k = if in_4k { 3 } else { 1 };
            o.edition(c, i, k, "Remastered");
            if in_4k {
                o.edition(c, i, k, "4K Remaster");
            }
            k
        }
        "restored" | "restoration" => {
            o.edition(c, i, 1, "Restored");
            1
        }
        _ => 0,
    }
}

/// Name an edition from free words in front of `Cut` or `Edition` ("Encore Cut", "Taco Bell Cut").
/// Only tokens in `from..to` that nothing else read are used.
fn read_named_editions(c: &Ctx, o: &mut Out, from: usize, to: usize) {
    for i in from..to.min(c.toks.len()) {
        if o.kind[i] != UNREAD || !matches!(c.lower(i), "cut" | "edition") {
            continue;
        }
        let mut j = i;
        while j > from
            && i - j < 3
            && o.kind[j - 1] == UNREAD
            && c.lower(j - 1)
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '\'')
        {
            j -= 1;
        }
        if j == i {
            continue;
        }
        let name = (j..=i)
            .map(|x| capitalized(c.text(x)))
            .collect::<Vec<_>>()
            .join(" ");
        o.edition(c, j, i - j + 1, &name);
    }
}

/// The audio channel layout that follows a codec token ending in a digit: `DDP5.1`, `AAC2 0`.
/// Returns 1 when the next token completed it.
fn channels_after_digit(c: &Ctx, o: &mut Out, i: usize, digit: Option<&str>) -> usize {
    let Some(d) = digit.filter(|_| o.p.audio_channels.is_none()) else {
        return 0;
    };
    let n = c.next(i);
    if !matches!(c.gap(i), "." | " ") || !matches!(n, "0" | "1") {
        return 0;
    }
    let value = format!("{d}.{n}");
    let at = c.toks[i].end - d.len();
    o.note_span(c, "audio_channels", at, c.toks[i + 1].end, &value);
    o.p.audio_channels = Some(value);
    o.mark(i + 1, 1, TECHNICAL);
    1
}

fn set_audio(c: &Ctx, o: &mut Out, at: usize, end: usize, codec: AudioCodec) {
    if o.p.audio_codec.is_none() {
        o.p.audio_codec = Some(codec);
        o.note_span(c, "audio_codec", at, end, &format!("{codec:?}"));
    }
}

/// Read audio at token `i`. Returns the number of tokens used, 0 when it is not audio.
fn read_audio(c: &Ctx, o: &mut Out, i: usize) -> usize {
    let l = c.lower(i);
    let (at, end) = (c.toks[i].at, c.toks[i].end);
    let n1 = c.next(i);
    let n2 = if c.tight(i) { c.next(i + 1) } else { "" };
    let codec_span = |k: usize| (at, c.toks[i + k - 1].end);
    let (codec, k) = match l {
        "dts" => match (n1, n2) {
            ("hd", "ma") => (AudioCodec::DtsHdMa, 3),
            ("hd", "hra" | "hr") => (AudioCodec::DtsHdHra, 3),
            ("hd" | "ma", _) => (AudioCodec::DtsHdMa, 2),
            ("hra" | "hr", _) => (AudioCodec::DtsHdHra, 2),
            ("x", _) => (AudioCodec::DtsX, 2),
            ("es", _) => (AudioCodec::Dts, 2),
            _ => (AudioCodec::Dts, 1),
        },
        "dtsx" | "dts:x" => (AudioCodec::DtsX, 1),
        "dtshd" | "dtsma" | "dtshdma" => (AudioCodec::DtsHdMa, 1),
        "truehd" => (AudioCodec::TrueHd, 1),
        "true" if n1 == "hd" => (AudioCodec::TrueHd, 2),
        "eac3" | "ec3" => (AudioCodec::Eac3, 1),
        "e" if n1 == "ac3" => (AudioCodec::Eac3, 2),
        "e" if n1 == "ac" && n2 == "3" => (AudioCodec::Eac3, 3),
        "ac3" => (AudioCodec::Ac3, 1),
        "ac" if n1 == "3" => (AudioCodec::Ac3, 2),
        "lpcm" | "pcm" => (AudioCodec::Pcm, 1),
        "opus" => (AudioCodec::Opus, 1),
        "mp3" => (AudioCodec::Mp3, 1),
        "atmos" => {
            o.p.atmos = true;
            o.note(c, "atmos", i, 1, "Atmos");
            return 1;
        }
        _ => {
            // Codec tokens that may carry the first channel digit: DDP5, DD2, AAC5, FLAC2.
            let (codec, atmos, digit) = if let Some(m) = EAC3_TOKEN.captures(l) {
                (AudioCodec::Eac3, m.get(1).is_some(), m.get(2))
            } else if let Some(m) = AC3_TOKEN.captures(l) {
                // "DD+" is Dolby Digital Plus. The plus sign is a separator, so look at the gap.
                let plus = c.work[end..].starts_with('+');
                let codec = if plus {
                    AudioCodec::Eac3
                } else {
                    AudioCodec::Ac3
                };
                (codec, false, m.get(1))
            } else if let Some(m) = AAC_TOKEN.captures(l) {
                (AudioCodec::Aac, false, m.get(1))
            } else if let Some(m) = FLAC_TOKEN.captures(l) {
                (AudioCodec::Flac, false, m.get(1))
            } else {
                return 0;
            };
            let digit = digit.map(|d| d.as_str());
            let name_end = end - digit.map_or(0, str::len);
            // "DD+" keeps its plus sign in the evidence.
            let plus = digit.is_none() && c.work[end..].starts_with('+');
            set_audio(c, o, at, name_end + usize::from(plus), codec);
            o.mark(i, 1, TECHNICAL);
            if atmos {
                o.p.atmos = true;
                o.note_span(c, "atmos", at, name_end, "Atmos");
            }
            return 1 + channels_after_digit(c, o, i, digit);
        }
    };
    let (a, b) = codec_span(k);
    set_audio(c, o, a, b, codec);
    o.mark(i, k, TECHNICAL);
    k
}

/// Read one technical token (or compound) at `i`. Returns how many tokens to step over.
fn read(c: &Ctx, o: &mut Out, i: usize) -> usize {
    let l = c.lower(i);
    let n1 = c.next(i);
    let (at, end) = (c.toks[i].at, c.toks[i].end);

    let k = read_edition(c, o, i);
    if k > 0 {
        return k;
    }

    if let Some(r) = resolution_of(l) {
        if o.p.resolution.is_none() {
            o.p.resolution = Some(r);
            o.note(c, "resolution", i, 1, r.label());
        }
        o.mark(i, 1, TECHNICAL);
        return 1;
    }
    if matches!(l, "4k" | "uhd") {
        o.implied_resolution.get_or_insert(i);
        o.mark(i, 1, TECHNICAL);
        return 1;
    }

    if let Some((s, k)) = source_at(c, i) {
        // A remux names its disc too ("BluRay REMUX"): the remux wins. A re-encode of a web
        // download names both ("WEB-DL ... WEBRip"): the rip wins.
        let take = match o.p.source {
            None => true,
            Some(Source::Remux) => false,
            Some(held) => s == Source::Remux || (held == Source::WebDl && s == Source::WebRip),
        };
        if take {
            o.p.source = Some(s);
            o.note(c, "source", i, k, s.label());
        }
        if matches!(l, "dvd" | "dvdrip" | "sdtv") {
            o.sd_source.get_or_insert(i);
        }
        o.mark(i, k, TECHNICAL);
        return k;
    }

    if let Some((v, k)) = video_at(c, i) {
        if o.p.video_codec.is_none() {
            o.p.video_codec = Some(v);
            o.note(c, "video_codec", i, k, &format!("{v:?}"));
        }
        o.mark(i, k, TECHNICAL);
        return k;
    }

    let depth = match (l, n1) {
        ("10bit" | "hi10p" | "hi10", _) => Some((10, 1)),
        ("8bit", _) => Some((8, 1)),
        ("12bit", _) => Some((12, 1)),
        ("10", "bit") => Some((10, 2)),
        ("8", "bit") => Some((8, 2)),
        ("12", "bit") => Some((12, 2)),
        _ => None,
    };
    if let Some((bits, k)) = depth {
        if o.p.bit_depth.is_none() {
            o.p.bit_depth = Some(bits);
            o.note(c, "bit_depth", i, k, &format!("{bits} bit"));
        }
        o.mark(i, k, TECHNICAL);
        return k;
    }

    match l {
        "dv" | "dovi" | "dolbyvision" => {
            o.hdr(c, at, end, HdrFormat::DolbyVision, "Dolby Vision");
            o.mark(i, 1, TECHNICAL);
            return 1;
        }
        "dolby" if n1 == "vision" => {
            o.hdr(
                c,
                at,
                c.toks[i + 1].end,
                HdrFormat::DolbyVision,
                "Dolby Vision",
            );
            o.mark(i, 2, TECHNICAL);
            return 2;
        }
        "hdr10plus" | "hdr10p" => {
            o.hdr(c, at, end, HdrFormat::Hdr10Plus, "HDR10+");
            o.mark(i, 1, TECHNICAL);
            return 1;
        }
        "hdr10" if c.work[end..].starts_with('+') => {
            o.hdr(c, at, end + 1, HdrFormat::Hdr10Plus, "HDR10+");
            o.mark(i, 1, TECHNICAL);
            return 1;
        }
        "hdr10" if n1 == "plus" => {
            o.hdr(c, at, c.toks[i + 1].end, HdrFormat::Hdr10Plus, "HDR10+");
            o.mark(i, 2, TECHNICAL);
            return 2;
        }
        "hdr10" => {
            o.hdr(c, at, end, HdrFormat::Hdr10, "HDR10");
            o.mark(i, 1, TECHNICAL);
            return 1;
        }
        "hdr" => {
            o.hdr(c, at, end, HdrFormat::Hdr, "HDR");
            o.mark(i, 1, TECHNICAL);
            return 1;
        }
        "hlg" => {
            o.hdr(c, at, end, HdrFormat::Hlg, "HLG");
            o.mark(i, 1, TECHNICAL);
            return 1;
        }
        "sdr" => {
            o.p.sdr = true;
            o.note(c, "sdr", i, 1, "SDR");
            return 1;
        }
        "pq" if c.is_upper(i) => {
            o.hdr(c, at, end, HdrFormat::Pq, "PQ");
            o.mark(i, 1, TECHNICAL);
            return 1;
        }
        _ => {}
    }

    let k = read_audio(c, o, i);
    if k > 0 {
        return k;
    }

    // Channels on their own: "5.1", "5 1", "7.1", "2.0", and "6CH".
    if l.len() == 1
        && matches!(l, "1" | "2" | "5" | "6" | "7")
        && matches!(c.gap(i), "." | " ")
        && matches!(n1, "0" | "1")
    {
        if o.p.audio_channels.is_none() {
            let value = format!("{l}.{n1}");
            o.note(c, "audio_channels", i, 2, &value);
            o.p.audio_channels = Some(value);
        }
        o.mark(i, 2, TECHNICAL);
        return 2;
    }
    let by_count = match l {
        "2ch" => Some("2.0"),
        "6ch" => Some("5.1"),
        "8ch" => Some("7.1"),
        _ => None,
    };
    if let Some(value) = by_count {
        if o.p.audio_channels.is_none() {
            o.p.audio_channels = Some(value.to_string());
            o.note(c, "audio_channels", i, 1, value);
        }
        o.mark(i, 1, TECHNICAL);
        return 1;
    }

    if SERVICES_ANY_CASE.contains(&l) || SERVICES_EXACT.contains(&c.text(i)) {
        if o.p.service.is_none() {
            let tag = match l {
                "itunes" => "IT".to_string(),
                _ => l.to_ascii_uppercase(),
            };
            o.note(c, "service", i, 1, &tag);
            o.p.service = Some(tag);
        }
        o.mark(i, 1, TECHNICAL);
        return 1;
    }

    if let Some(m) = FPS.captures(l) {
        o.p.fps = m[1].parse().ok();
        o.note(c, "fps", i, 1, &m[1]);
        return 1;
    }
    match (l, n1) {
        ("ai", "enhanced" | "upscaled" | "upscale" | "upscaling") => {
            o.p.ai_upscaled = true;
            o.note(c, "ai_upscaled", i, 2, "AI upscaled");
            return 2;
        }
        ("ai" | "upscaled" | "upscale" | "upscaling", _) => {
            o.p.ai_upscaled = true;
            o.note(c, "ai_upscaled", i, 1, "AI upscaled");
            return 1;
        }
        ("rife", _) => {
            // The interpolation model and its version: "RIFE.4.16.lite.v2", "RIFE.4.25v2".
            let mut k = 1;
            while VERSION.is_match(c.lower(i + k)) {
                k += 1;
            }
            o.p.ai_upscaled = true;
            o.note(c, "ai_upscaled", i, k, "AI frame interpolation");
            return k;
        }
        ("proper" | "proper2", _) => {
            o.p.proper = true;
            o.note(c, "proper", i, 1, "Proper");
            return 1;
        }
        ("repack" | "rerip" | "repack2", _) => {
            o.p.repack = true;
            o.note(c, "repack", i, 1, "Repack");
            return 1;
        }
        _ => {}
    }

    if let Some(language) = language_of(c, i) {
        o.language(c, i, language);
        return 1;
    }
    if l == "dual" && n1 == "audio" {
        o.mark(i, 2, NOISE_WORD);
        return 2;
    }

    if NOISE.contains(&l)
        || ENCODER_SETTING.is_match(l)
        || (CRC.is_match(l) && l.chars().any(|ch| ch.is_ascii_digit()) && c.is_upper(i))
    {
        o.mark(i, 1, NOISE_WORD);
    }
    1
}

/// Would the reader take any part of this text as technical? Used to tell a group from a tail
/// such as `DL` in `WEB-DL`.
fn is_technical_text(text: &str) -> bool {
    let toks = tokens(text);
    let c = Ctx {
        raw: text,
        work: text,
        toks: &toks,
    };
    let mut o = Out::new(text, toks.len());
    let mut i = 0;
    while i < toks.len() {
        let l = c.lower(i);
        if COMPOUND_TAILS.contains(&l)
            || l.chars().all(|ch| ch.is_ascii_digit())
            || YEAR.is_match(l)
        {
            return true;
        }
        i += read(&c, &mut o, i);
    }
    o.kind.contains(&TECHNICAL) || o.kind.contains(&NOISE_WORD)
}

/// The beginning of a technical word: what is left when a name is cut off in the middle of
/// one ("... AAC 5 1 Com", "... HEVC 10bi"). Such a tail is not a group.
fn is_cut_word(lower: &str) -> bool {
    const WORDS: &[&str] = &[
        "10bit", "bluray", "remux", "webrip", "hevc", "x265", "x264", "atmos", "truehd",
    ];
    lower.len() >= 3
        && NOISE
            .iter()
            .chain(WORDS)
            .any(|w| w.len() > lower.len() && w.starts_with(lower))
}

fn plausible_group(text: &str) -> bool {
    text.len() >= 2
        && text.len() <= 24
        && text.chars().any(|c| c.is_ascii_alphabetic())
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '!' | '@'))
        && !is_technical_text(text)
}

/// Editions named in free text, such as the arr's edition string or a `{edition-..}` block.
pub(super) fn editions(text: &str) -> Vec<String> {
    let toks = tokens(text);
    let c = Ctx {
        raw: text,
        work: text,
        toks: &toks,
    };
    let mut o = Out::new(text, toks.len());
    let mut i = 0;
    while i < toks.len() {
        i += read_edition(&c, &mut o, i).max(1);
    }
    read_named_editions(&c, &mut o, 0, toks.len());
    o.p.editions
}

fn blank(bytes: &mut [u8], a: usize, b: usize) {
    for x in &mut bytes[a..b] {
        *x = b' ';
    }
}

/// Blank everything that is not part of the release. Returns the working copy and the inner
/// spans of `{edition-..}` blocks.
fn prepare(raw: &str) -> (String, Vec<(usize, usize)>) {
    let mut bytes = raw.as_bytes().to_vec();
    let mut editions = Vec::new();
    for m in LIB_ID.find_iter(raw) {
        blank(&mut bytes, m.start(), m.end());
    }
    for m in LIB_EDITION.captures_iter(raw) {
        let (all, inner) = (m.get(0).expect("group 0"), m.get(1).expect("group 1"));
        blank(&mut bytes, all.start(), all.end());
        editions.push((inner.start(), inner.end()));
    }
    // An HTML dash entity is a dash. Padded to the entity's seven bytes, so offsets hold.
    for (entity, dash) in [("&mdash;", "  \u{2014}  "), ("&ndash;", "  \u{2013}  ")] {
        for (at, _) in raw.match_indices(entity) {
            bytes[at..at + entity.len()].copy_from_slice(dash.as_bytes());
        }
    }
    let mut start = 0;
    while let Some(m) = SITE_HEAD.find(&raw[start..]) {
        if m.end() == 0 {
            break;
        }
        blank(&mut bytes, start, start + m.end());
        start += m.end();
    }
    let mut end = raw.len();
    loop {
        let head = &raw[..end];
        // An ellipsis means the name was cut short. The word it cuts is not a whole token, so it
        // goes too: "x265.10bit-GalaxyRG..." has no readable group.
        if let Some(cut) = head
            .strip_suffix("...")
            .or_else(|| head.strip_suffix('\u{2026}'))
        {
            let word = cut
                .char_indices()
                .rfind(|(_, ch)| is_separator(*ch))
                .map_or(0, |(i, ch)| i + ch.len_utf8());
            let opener = cut[..word].trim_end_matches(['[', '(']).len();
            let at = if cut.len() > word { opener } else { cut.len() };
            if at >= start {
                blank(&mut bytes, at, end);
                end = at;
                continue;
            }
        }
        let cut = TRAILER
            .find(head)
            .or_else(|| TS_EXT.find(head))
            .map(|m| m.start());
        match cut {
            Some(at) if at >= start && at < end => {
                blank(&mut bytes, at, end);
                end = at;
            }
            _ => break,
        }
    }
    for x in &mut bytes {
        if *x == b'_' {
            *x = b' ';
        }
    }
    // Blanked ranges come from regex matches, so they sit on character boundaries.
    let work = String::from_utf8(bytes).unwrap_or_else(|_| raw.to_string());
    (work, editions)
}

/// An episode, season or air-date marker at one token.
struct Marker {
    at: usize,
    len: usize,
    kind: MediaKind,
}

fn marker_at(c: &Ctx, o: &mut Out, i: usize, first: usize) -> Option<Marker> {
    let l = c.lower(i);
    let t = &c.toks[i];
    let numbers = |list: &str| -> Vec<u16> {
        list.split('e')
            .filter_map(|e| e.parse().ok())
            .collect::<Vec<u16>>()
    };
    // S01E03, S01E03E04, then an optional range end: S01E03-E05, S01E03-05.
    let single = SXXEYY.captures(l).map(|m| {
        let season_len = 1 + m[1].len();
        (
            m[1].to_string(),
            m[2].to_string(),
            t.at,
            t.at + season_len,
            t.end,
            1,
        )
    });
    // "S01 E03" and "S01.E03".
    let split = || {
        let m = SXX.captures(l)?;
        let n = c.next(i);
        EYY.is_match(n).then(|| {
            (
                m[1].to_string(),
                n.to_string(),
                t.at,
                t.end,
                c.toks[i + 1].end,
                2,
            )
        })
    };
    let is_single = single.is_some();
    if let Some((season, list, at, season_end, mut end, mut len)) = single.or_else(split) {
        let mut eps = numbers(&list);
        let last = i + len - 1;
        let follow = c.lower(last + 1);
        let range_end: Option<u16> = if c.gap(last) == "-" {
            follow
                .strip_prefix('e')
                .unwrap_or(follow)
                .parse()
                .ok()
                .filter(|_| follow.len() <= 4)
        } else {
            None
        };
        if let (Some(to), Some(from)) = (range_end, eps.last().copied())
            && to > from
            && to - from < 100
        {
            eps.extend(from + 1..=to);
            end = c.toks[last + 1].end;
            len += 1;
        }
        o.p.season = season.parse().ok();
        o.note_span(c, "season", at, season_end, &season);
        let ep_at = if is_single {
            season_end
        } else {
            c.toks[i + 1].at
        };
        let read_as = eps
            .iter()
            .map(u16::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        o.note_span(c, "episode", ep_at, end, &read_as);
        o.p.episodes = eps;
        o.mark(i, len, TECHNICAL);
        return Some(Marker {
            at: i,
            len,
            kind: MediaKind::Episode,
        });
    }
    if i <= first {
        return None;
    }
    if let Some(m) = NXNN.captures(l) {
        o.p.season = m[1].parse().ok();
        o.p.episodes = m[2].parse().ok().into_iter().collect();
        o.note_span(c, "season", t.at, t.at + m[1].len(), &m[1]);
        o.note_span(c, "episode", t.end - m[2].len(), t.end, &m[2]);
        o.mark(i, 1, TECHNICAL);
        return Some(Marker {
            at: i,
            len: 1,
            kind: MediaKind::Episode,
        });
    }
    if let Some(m) = SXX.captures(l) {
        o.p.season = m[1].parse().ok();
        o.note(c, "season", i, 1, &m[1]);
        return Some(Marker {
            at: i,
            len: 1,
            kind: MediaKind::SeasonPack,
        });
    }
    // "Season 2". A year after it means it was a title ("Open Season 2 (2008)").
    let n = c.next(i);
    if l == "season"
        && !n.is_empty()
        && n.len() <= 2
        && n.chars().all(|ch| ch.is_ascii_digit())
        && !(i + 2..c.toks.len())
            .take_while(|&j| !strong(c, j))
            .any(|j| YEAR.is_match(c.lower(j)))
    {
        o.p.season = n.parse().ok();
        o.note(c, "season", i, 2, n);
        return Some(Marker {
            at: i,
            len: 2,
            kind: MediaKind::SeasonPack,
        });
    }
    // A daily show: 2026.11.20.
    if YEAR.is_match(l) && c.tight(i) && c.tight(i + 1) {
        let (month, day) = (c.lower(i + 1), c.lower(i + 2));
        let valid = |text: &str, max: u8| {
            text.len() == 2 && text.parse::<u8>().is_ok_and(|v| (1..=max).contains(&v))
        };
        if valid(month, 12) && valid(day, 31) {
            let date = format!("{l}-{month}-{day}");
            o.note(c, "air_date", i, 3, &date);
            o.p.air_date = Some(date);
            return Some(Marker {
                at: i,
                len: 3,
                kind: MediaKind::Episode,
            });
        }
    }
    None
}

fn decode_entities(text: &str) -> String {
    text.replace("&#039;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
}

/// The title as people write it: dots and underscores become spaces, punctuation is kept.
fn title_text(slice: &str) -> String {
    let spaced = if slice.contains(' ') {
        slice.to_string()
    } else {
        slice.replace(['.', '_'], " ")
    };
    let joined = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    decode_entities(joined.trim_matches(|c: char| is_separator(c) && c != ')' && c != ']'))
}

pub(super) fn parse(name: &str) -> ParsedRelease {
    let raw = name.trim();
    let (work, lib_editions) = prepare(raw);
    let mut stem_end = work.trim_end().len();
    let toks = tokens(&work[..stem_end]);
    let c = Ctx {
        raw,
        work: &work,
        toks: &toks,
    };
    let n = toks.len();
    let mut o = Out::new(raw, n);

    // A leading "[Tag]" is a group candidate, not part of the title.
    let lead = work
        .trim_start()
        .starts_with('[')
        .then(|| work.find(']'))
        .flatten()
        .map(|close| (work.find('[').unwrap_or(0) + 1, close))
        .filter(|(a, b)| a < b && !is_technical_text(&work[*a..*b]));
    let first = lead.map_or(0, |(_, close)| {
        toks.iter().position(|t| t.at > close).unwrap_or(n)
    });

    // Where the title ends.
    let mut marker: Option<Marker> = None;
    let mut first_strong: Option<usize> = None;
    for i in first..n {
        if strong(&c, i) {
            first_strong = Some(i);
            break;
        }
        if let Some(m) = marker_at(&c, &mut o, i, first) {
            marker = Some(m);
            break;
        }
    }
    let boundary = marker.as_ref().map(|m| m.at).or(first_strong).unwrap_or(n);
    let year_at = match &marker {
        // A series carries its year right before the marker: "The.West.Wing.1999.S05E19".
        Some(m) => (m.at > first + 1 && YEAR.is_match(c.lower(m.at - 1))).then(|| m.at - 1),
        None => (first + 1..boundary)
            .rev()
            .find(|&i| YEAR.is_match(c.lower(i))),
    };
    if let Some(y) = year_at {
        o.p.year = c.lower(y).parse().ok();
        o.note(&c, "year", y, 1, c.lower(y));
    }
    let title_end = year_at.unwrap_or(boundary);
    if title_end > first {
        let (a, b) = (toks[first].at, toks[title_end - 1].end);
        let title = title_text(&raw[a..b]);
        if !title.is_empty() {
            o.note_span(&c, "title", a, b, &title);
            o.p.title = Some(title);
        }
    }

    // Where technical reading starts.
    let read_from = match (&marker, year_at) {
        (Some(m), _) => {
            // What follows the marker is the episode title, up to the first strong token. Flags
            // written right before that token ("S05E19.Proper.1080p") are read.
            let after = m.at + m.len;
            let mut from = (after..n).find(|&i| strong(&c, i)).unwrap_or(n);
            while from > after
                && matches!(
                    c.lower(from - 1),
                    "proper" | "repack" | "rerip" | "real" | "extended" | "uncut" | "unrated"
                )
            {
                from -= 1;
            }
            from
        }
        (None, Some(y)) => y + 1,
        (None, None) => boundary,
    };
    // Group rules look only past the title: from the end of the last token before the
    // technical ones (the year, the episode marker, the last title word).
    let tail_from = match (read_from.checked_sub(1).and_then(|i| toks.get(i)), &marker) {
        (_, Some(m)) if read_from >= n => toks[m.at + m.len - 1].end,
        (Some(t), _) if read_from < n || year_at.is_some() => t.end,
        (None, _) if read_from < n => 0,
        _ => stem_end,
    };

    // A trailing "[TAG]": the group itself after a dash ("-[SHR]"), else kept for later.
    let mut tail_tag: Option<(usize, usize)> = None;
    let mut group: Option<(usize, usize)> = None;
    if work[..stem_end].ends_with(']')
        && let Some(open) = work[..stem_end].rfind('[')
        && open >= tail_from
    {
        let inner = &work[open + 1..stem_end - 1];
        let a = open + 1 + (inner.len() - inner.trim_start().len());
        let b = a + inner.trim().len();
        let before = work[..open].trim_end();
        let dashed = before.ends_with(is_dash);
        // "-[60fps-DirtyHippie]": what stands before the inner dash is technical.
        let inner_cut = work[a..b]
            .rfind('-')
            .filter(|d| is_technical_text(&work[a..a + d]))
            .map_or(a, |d| a + d + 1);
        if dashed && plausible_group(&work[inner_cut..b]) {
            group = Some((inner_cut, b));
            stem_end = before.len() - before.chars().next_back().map_or(0, char::len_utf8);
        } else if plausible_group(&work[a..b]) && !CRC.is_match(&work[a..b].to_ascii_lowercase()) {
            tail_tag = Some((a, b));
            stem_end = work[..open].trim_end().len();
        }
    }

    // "-GROUP", " - GROUP", the same after a long dash, and "- GROUP)" inside a block.
    if group.is_none()
        && stem_end > tail_from
        && let Some((d, dash)) = work[tail_from..stem_end]
            .char_indices()
            .rfind(|(_, ch)| is_dash(*ch))
    {
        let d = tail_from + d;
        let after = d + dash.len_utf8();
        let tail = &work[after..stem_end];
        let a = after + (tail.len() - tail.trim_start().len());
        let trimmed = tail.trim().trim_end_matches(')').trim_end();
        // "- KINGDOM RG": the words "release group" after the group.
        let trimmed = trimmed.strip_suffix(" RG").map_or(trimmed, str::trim_end);
        // "-KatmovieHD.nu": the group's site, not part of its name. "YTS.MX" stays whole.
        let trimmed = GROUP_SITE_SUFFIX
            .find(trimmed)
            .map_or(trimmed, |m| &trimmed[..m.start()]);
        let b = a + trimmed.len();
        // "x264-D-Z0N3", "x265-UTR-HD": a known group with a dash of its own.
        let joined = work[..d]
            .char_indices()
            .rfind(|(_, ch)| is_separator(*ch))
            .map(|(s, ch)| s + ch.len_utf8())
            .filter(|s| *s >= tail_from && work[..*s].ends_with(is_dash))
            .filter(|s| {
                let whole = work[*s..b].to_ascii_lowercase();
                HYPHENATED_GROUPS.contains(&whole.as_str())
                    || (d - *s == 1 && work[*s..d].chars().all(|ch| ch.is_ascii_alphabetic()))
            });
        if let Some(s) = joined.filter(|_| !trimmed.is_empty() && !trimmed.contains(' ')) {
            group = Some((s, b));
        } else if plausible_group(trimmed) {
            group = Some((a, b));
        }
    }
    let in_group = |t: &Tok| group.is_some_and(|(a, b)| t.at >= a && t.end <= b);

    // Technical tokens, left to right.
    let mut i = read_from;
    while i < n {
        if in_group(&toks[i]) || o.kind[i] != UNREAD {
            i += 1;
            continue;
        }
        i += read(&c, &mut o, i);
    }
    if let Some(y) = year_at.filter(|_| marker.is_none()) {
        let to = (y + 1..n).find(|&j| strong(&c, j)).unwrap_or(n);
        read_named_editions(&c, &mut o, y + 1, to);
    }
    for (a, b) in lib_editions {
        for name in editions(&raw[a..b]) {
            if !o.p.has_edition(&name) {
                o.p.editions.push(name.clone());
            }
            o.note_span(&c, "edition", a, b, &name);
        }
    }

    // What the name implies without saying it.
    if o.p.resolution.is_none() {
        if let Some(i) = o.implied_resolution {
            o.p.resolution = Some(Resolution::R2160);
            o.note(&c, "resolution", i, 1, "2160p");
        } else if let Some(i) = o.sd_source {
            o.p.resolution = Some(Resolution::R480);
            o.note(&c, "resolution", i, 1, "480p, implied by the source");
        }
    }
    if o.p.source.is_none()
        && let Some(note) = o.p.notes.iter().find(|x| x.field == "service").cloned()
    {
        o.p.source = Some(Source::WebDl);
        o.note_span(
            &c,
            "source",
            note.at,
            note.at + note.token.len(),
            "WEBDL, implied by the service tag",
        );
    }

    // No dash group: take it from the structure of the name.
    let in_stem = |t: &Tok| t.end <= stem_end;
    let last = toks.iter().rposition(in_stem);
    if group.is_none()
        && let Some(last) = last
        && last >= read_from
    {
        // The last word of the last technical block: "(1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole)".
        let block = work[tail_from..stem_end]
            .rfind(['(', '['])
            .map(|open| tail_from + open)
            .map(|open| {
                let close = work[open..stem_end]
                    .find([')', ']'])
                    .map_or(stem_end, |x| open + x);
                (open, close)
            });
        if let Some((open, close)) = block {
            let inside: Vec<usize> = (read_from..=last)
                .filter(|&j| toks[j].at > open && toks[j].end <= close)
                .collect();
            let after_is_read = (read_from..=last)
                .filter(|&j| toks[j].at > close)
                .all(|j| o.kind[j] != UNREAD);
            if let Some(&j) = inside.last()
                && o.kind[j] == UNREAD
                && after_is_read
                && inside.iter().any(|&x| o.kind[x] == TECHNICAL)
                && plausible_group(c.text(j))
                && !is_cut_word(c.lower(j))
            {
                group = Some((toks[j].at, toks[j].end));
            }
        }
        // A single trailing word after technical tokens: "... HEVC x265 5.1 BONE".
        if group.is_none() {
            let j = (read_from..=last).rev().find(|&j| o.kind[j] != NOISE_WORD);
            if let Some(j) = j.filter(|&j| j > read_from && o.kind[j] == UNREAD) {
                let two =
                    j > read_from + 1 && o.kind[j - 1] == UNREAD && o.kind[j - 2] == TECHNICAL;
                let pair = format!("{} {}", c.lower(j.saturating_sub(1)), c.lower(j));
                if two && TWO_WORD_GROUPS.contains(&pair.as_str()) {
                    group = Some((toks[j - 1].at, toks[j].end));
                } else if two
                    && tail_tag.is_none()
                    && plausible_group(c.text(j - 1))
                    && plausible_group(c.text(j))
                {
                    // Brackets lost on the way: "... AAC 5 1 Tigole QxR" was "Tigole) [QxR]".
                    group = Some((toks[j - 1].at, toks[j - 1].end));
                    tail_tag = Some((toks[j].at, toks[j].end));
                } else if o.kind[j - 1] == TECHNICAL
                    && plausible_group(c.text(j))
                    && !is_cut_word(c.lower(j))
                {
                    group = Some((toks[j].at, toks[j].end));
                }
            }
        }
    }
    match (group, tail_tag) {
        (Some(_), Some((a, b))) => {
            o.p.collective = Some(raw[a..b].to_string());
            o.note_span(&c, "collective", a, b, &raw[a..b]);
        }
        (None, Some(tag)) => group = Some(tag),
        _ => {}
    }
    if group.is_none()
        && let Some((a, b)) = lead
    {
        let inner = &work[a..b];
        let a = a + (inner.len() - inner.trim_start().len());
        group = Some((a, a + inner.trim().len()));
    }
    if let Some((a, b)) = group {
        o.p.group = Some(raw[a..b].to_string());
        o.note_span(&c, "group", a, b, &raw[a..b]);
    }

    // What kind of release this is, and on what evidence.
    match (&marker, year_at) {
        (Some(m), _) => {
            o.p.kind = m.kind;
            let (a, b) = (toks[m.at].at, toks[m.at + m.len - 1].end);
            o.note_span(&c, "kind", a, b, &format!("{:?}", m.kind));
        }
        // "Show (2023) - 05 [1080p]" is an absolute episode number: out of scope, left unknown.
        (None, Some(y)) if !ABSOLUTE.is_match(&work[toks[y].end.min(stem_end)..stem_end]) => {
            o.p.kind = MediaKind::Movie;
            o.note_span(&c, "kind", toks[y].at, toks[y].end, "Movie");
        }
        _ => {}
    }
    o.p
}
