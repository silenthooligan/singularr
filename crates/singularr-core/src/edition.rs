//! Editions: the closed list in `rules/editions.toml`, and which edition a name belongs to.
//!
//! The release parser ([`crate::release::parse`]) finds the edition words of a name, after the
//! title only, so a title word is never an edition ("Midnight Special", "The Final Cut"). This
//! module decides which edition of the list those words are. Every copy belongs to exactly one
//! edition: the first one of the list its name names, else an `other:` edition for a name
//! the list does not know, else the default edition (Theatrical) when nothing names one.

use crate::release::{self, ParsedRelease};
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// The list as shipped in `rules/editions.toml`, compiled in. The server reads the file at
/// run time and falls back to this text.
pub const SHIPPED: &str = include_str!("../../../rules/editions.toml");

/// The prefix of an edition the list does not know: `other:taco-bell-cut`.
pub const OTHER: &str = "other:";

static SHIPPED_LIST: LazyLock<EditionList> = LazyLock::new(|| {
    EditionList::from_toml(SHIPPED).expect("the shipped list is checked by a test")
});

#[derive(Debug, thiserror::Error)]
pub enum EditionError {
    #[error("the edition list is not valid TOML: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("edition key `{0}` must be lower case letters, digits and dashes")]
    BadKey(String),
    #[error("`{0}` appears twice in the edition list")]
    Duplicate(String),
    #[error("the edition list needs exactly one default edition; it has {0}")]
    Default(usize),
    #[error("others_before names `{0}`, which is not a key of the list")]
    UnknownKey(String),
}

/// One edition of the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Edition {
    /// `directors-cut`, `open-matte`, `theatrical`.
    pub key: String,
    /// What people read and what library file names carry: `Director's Cut`.
    pub label: String,
    /// Other ways names write it: `DC`, `Directors.Cut`, `Extended Edition`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// The edition of a copy whose name and record name none.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

/// The closed list of editions, in order of precedence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditionList {
    /// A named cut the list does not know (an `other:` edition) takes precedence just before
    /// this edition: after the cuts of the list, before the presentations and transfers. Left
    /// out, it comes after every edition of the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub others_before: Option<String>,
    pub editions: Vec<Edition>,
}

/// Which edition a release or a copy belongs to, and on what evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditionRead {
    /// A key of the list, or `other:<slug>`.
    pub key: String,
    pub label: String,
    /// Something names the edition. False: nothing does, so the copy is the default edition.
    pub marked: bool,
    /// The other editions the name also names, by key, in the list's order: `remastered` for
    /// "4K Remastered - Director's Cut".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<String>,
    /// The words that named it, as written: `DC`, `Open Matte`, the arr's edition text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
}

/// Text as the list compares it: lower case, words of letters and digits split on anything
/// else, apostrophes dropped, `&` read as "and". `Director's.Cut` gives `directors cut`.
pub fn normalize(text: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    let word = |out: &mut String, gap: &mut bool, piece: &str| {
        if *gap && !out.is_empty() {
            out.push(' ');
        }
        *gap = false;
        out.push_str(piece);
    };
    for ch in text.chars() {
        match ch {
            '\'' | '\u{2019}' | '\u{2018}' | '`' => {}
            '&' => {
                gap = true;
                word(&mut out, &mut gap, "and");
                gap = true;
            }
            c if c.is_alphanumeric() => {
                let lower: String = c.to_lowercase().collect();
                word(&mut out, &mut gap, &lower);
            }
            _ => gap = true,
        }
    }
    out
}

/// The key of an edition the list does not know: `other:taco-bell-cut`. `None` when the label
/// has no letter or digit.
pub fn other_key(label: &str) -> Option<String> {
    let slug = normalize(label).replace(' ', "-");
    (!slug.is_empty()).then(|| format!("{OTHER}{slug}"))
}

/// Spaces collapsed and trimmed: the label of an `other:` edition as written.
fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl EditionList {
    pub fn from_toml(text: &str) -> Result<EditionList, EditionError> {
        let list: EditionList = toml::from_str(text)?;
        list.check()?;
        Ok(list)
    }

    /// The list as shipped in `rules/editions.toml`.
    pub fn shipped() -> &'static EditionList {
        &SHIPPED_LIST
    }

    fn check(&self) -> Result<(), EditionError> {
        // Every way of writing an edition, normalized, and the edition it names. An edition
        // may list spellings that read the same (`Directors Cut`, `Directors.Cut`); two
        // editions may not share one.
        let mut seen: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
        for e in &self.editions {
            let key_ok = !e.key.is_empty()
                && !e.key.starts_with("other")
                && e.key
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
            if !key_ok {
                return Err(EditionError::BadKey(e.key.clone()));
            }
            if seen.values().any(|k| *k == e.key) {
                return Err(EditionError::Duplicate(e.key.clone()));
            }
            for name in std::iter::once(&e.key.replace('-', " "))
                .chain(std::iter::once(&e.label))
                .chain(e.aliases.iter())
            {
                match seen.get(&normalize(name)) {
                    Some(owner) if *owner != e.key => {
                        return Err(EditionError::Duplicate(name.clone()));
                    }
                    _ => {
                        seen.insert(normalize(name), &e.key);
                    }
                }
            }
        }
        let defaults = self.editions.iter().filter(|e| e.default).count();
        if defaults != 1 {
            return Err(EditionError::Default(defaults));
        }
        if let Some(key) = &self.others_before
            && self.get(key).is_none()
        {
            return Err(EditionError::UnknownKey(key.clone()));
        }
        Ok(())
    }

    /// The edition of a copy that names none.
    pub fn default_edition(&self) -> &Edition {
        self.editions
            .iter()
            .find(|e| e.default)
            .expect("a checked list has one default")
    }

    pub fn get(&self, key: &str) -> Option<&Edition> {
        self.editions.iter().find(|e| e.key == key)
    }

    /// The edition a key, a label or an alias names, compared as [`normalize`] does.
    pub fn lookup(&self, text: &str) -> Option<&Edition> {
        let n = normalize(text);
        if n.is_empty() {
            return None;
        }
        self.editions.iter().find(|e| {
            normalize(&e.key) == n
                || normalize(&e.label) == n
                || e.aliases.iter().any(|a| normalize(a) == n)
        })
    }

    /// The order of precedence, lowest first: an edition of the list by its place, an `other:`
    /// edition just before `others_before`. For sorting; the numbers mean nothing else.
    pub fn rank(&self, key: &str) -> usize {
        match self.editions.iter().position(|e| e.key == key) {
            Some(i) => 2 * i + 1,
            None => {
                2 * self
                    .others_before
                    .as_deref()
                    .and_then(|k| self.editions.iter().position(|e| e.key == k))
                    .unwrap_or(self.editions.len())
            }
        }
    }

    /// The label of a key of the list. `None` for an `other:` key, whose label is kept with it.
    pub fn label(&self, key: &str) -> Option<&str> {
        self.get(key).map(|e| e.label.as_str())
    }

    /// A key the list knows, an `other:` key, or a label or alias the list knows: the key it
    /// stands for. For filters, where a person may type `Director's Cut` or `directors-cut`.
    pub fn key_for(&self, text: &str) -> Option<String> {
        let t = text.trim();
        if t.starts_with(OTHER) {
            return Some(t.to_string());
        }
        self.lookup(t).map(|e| e.key.clone())
    }

    /// Pick the edition from names found in order, each with the text that named it: the one
    /// of highest precedence, the others in `also`.
    fn pick(&self, names: Vec<(String, Option<String>)>) -> Option<EditionRead> {
        // (rank, order found, key, label, evidence), one entry per key.
        let mut found: Vec<(usize, usize, String, String, Option<String>)> = Vec::new();
        for (at, (name, evidence)) in names.into_iter().enumerate() {
            let (key, label) = match self.lookup(&name) {
                Some(e) => (e.key.clone(), e.label.clone()),
                None => match other_key(&name) {
                    Some(key) => (key, tidy(&name)),
                    None => continue,
                },
            };
            if !found.iter().any(|f| f.2 == key) {
                found.push((self.rank(&key), at, key, label, evidence));
            }
        }
        found.sort_by_key(|f| (f.0, f.1));
        let mut rest = found.into_iter();
        let (_, _, key, label, evidence) = rest.next()?;
        Some(EditionRead {
            key,
            label,
            marked: true,
            also: rest.map(|f| f.2).collect(),
            evidence,
        })
    }

    /// The default edition, named by nothing.
    pub fn unmarked(&self) -> EditionRead {
        let d = self.default_edition();
        EditionRead {
            key: d.key.clone(),
            label: d.label.clone(),
            marked: false,
            also: Vec::new(),
            evidence: None,
        }
    }

    /// The edition a parsed release belongs to: the first of its edition names in the list's
    /// order, else an `other:` edition, else the default.
    pub fn read_release(&self, r: &ParsedRelease) -> EditionRead {
        let names = r
            .editions
            .iter()
            .map(|name| {
                let evidence = r
                    .notes
                    .iter()
                    .find(|n| n.field == "edition" && n.read_as == *name)
                    .map(|n| n.token.clone());
                (name.clone(), evidence)
            })
            .collect();
        self.pick(names).unwrap_or_else(|| self.unmarked())
    }

    /// The edition free text names, such as Radarr's edition field or the label of a
    /// `{edition-...}` block. `None` for empty text. Text the list and the parser do not know
    /// is an `other:` edition with the text as its label ("Eagle Pictures Release").
    pub fn read_text(&self, text: &str) -> Option<EditionRead> {
        let t = text.trim();
        if t.is_empty() {
            return None;
        }
        if let Some(e) = self.lookup(t) {
            return Some(EditionRead {
                key: e.key.clone(),
                label: e.label.clone(),
                marked: true,
                also: Vec::new(),
                evidence: Some(t.to_string()),
            });
        }
        let names = release::editions_from_text(t)
            .into_iter()
            .map(|name| (name, Some(t.to_string())))
            .collect();
        if let Some(found) = self.pick(names) {
            return Some(found);
        }
        Some(EditionRead {
            key: other_key(t)?,
            label: tidy(t),
            marked: true,
            also: Vec::new(),
            evidence: Some(t.to_string()),
        })
    }

    /// The edition of a held copy: its release as held (the arr's record and the name
    /// together), and the arr's edition text where that names something the reader does not
    /// turn into an edition name.
    pub fn read_copy(&self, held: &ParsedRelease, arr_edition: Option<&str>) -> EditionRead {
        let read = self.read_release(held);
        if read.marked {
            return read;
        }
        arr_edition.and_then(|t| self.read_text(t)).unwrap_or(read)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::naming::parse_library_name;
    use crate::release::{EDITIONS, parse};

    fn list() -> &'static EditionList {
        EditionList::shipped()
    }

    fn key(name: &str) -> String {
        list().read_release(&parse(name)).key
    }

    #[test]
    fn the_shipped_list_is_valid_and_knows_every_name_the_parser_gives() {
        let l = list();
        assert_eq!(l.default_edition().key, "theatrical");
        for name in EDITIONS {
            assert!(
                l.lookup(name).is_some(),
                "the parser names `{name}` and rules/editions.toml has no key for it"
            );
        }
        for key in [
            "theatrical",
            "directors-cut",
            "extended",
            "unrated",
            "final-cut",
            "imax",
            "open-matte",
            "remastered",
            "special-edition",
            "ultimate",
            "anniversary",
            "black-and-chrome",
        ] {
            assert!(l.get(key).is_some(), "{key} is missing");
        }
    }

    #[test]
    fn a_broken_list_is_refused() {
        let two_defaults = r#"
[[editions]]
key = "a"
label = "A"
default = true
[[editions]]
key = "b"
label = "B"
default = true
"#;
        assert!(matches!(
            EditionList::from_toml(two_defaults),
            Err(EditionError::Default(2))
        ));
        let clash = r#"
[[editions]]
key = "a"
label = "Director's Cut"
default = true
[[editions]]
key = "b"
label = "B"
aliases = ["Directors.Cut"]
"#;
        assert!(matches!(
            EditionList::from_toml(clash),
            Err(EditionError::Duplicate(_))
        ));
        let bad = "[[editions]]\nkey = \"Other Cut\"\nlabel = \"X\"\ndefault = true\n";
        assert!(matches!(
            EditionList::from_toml(bad),
            Err(EditionError::BadKey(_))
        ));
    }

    #[test]
    fn aliases_match_without_case_dots_or_apostrophes() {
        let l = list();
        for (text, want) in [
            ("DC", "directors-cut"),
            ("Directors.Cut", "directors-cut"),
            ("DIRECTOR'S CUT", "directors-cut"),
            ("director\u{2019}s cut", "directors-cut"),
            ("Extended.Edition", "extended"),
            ("Ultimate.Cut", "ultimate"),
            ("Black & Chrome", "black-and-chrome"),
            ("OPEN-MATTE", "open-matte"),
            ("4K Remastered", "remastered"),
            ("Criterion Collection ", "criterion"),
            ("theatrical", "theatrical"),
            ("directors-cut", "directors-cut"),
        ] {
            assert_eq!(l.lookup(text).map(|e| e.key.as_str()), Some(want), "{text}");
        }
        assert!(l.lookup("Midnight").is_none());
        assert!(l.lookup("").is_none());
        assert_eq!(
            normalize("Black & Chrome.Edition"),
            "black and chrome edition"
        );
    }

    #[test]
    fn release_names_read_into_one_edition() {
        for (name, want) in [
            (
                "Blade.Runner.1982.Final.Cut.1080p.BluRay.x264-GRP",
                "final-cut",
            ),
            (
                "Aliens.1986.Special.Edition.1080p.BluRay.x265-GRP",
                "special-edition",
            ),
            (
                "Alien.3.1992.DC.1080p.BluRay.x265.hevc.10bit.AAC.5.1-HeVK",
                "directors-cut",
            ),
            (
                "Thief (1981) DC RM4K (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole)",
                "directors-cut",
            ),
            (
                "Ted.2.2015.Unrated.Extended.1080p.BluRay.DDP.5.1.x265-edge2020",
                "extended",
            ),
            (
                "Movie.2004.Open.Matte.1080p.AMZN.WEB-DL.x265-MONOLITH",
                "open-matte",
            ),
            (
                "Mad.Max.Fury.Road.2015.Black.and.Chrome.Edition.1080p.BluRay.x264-GRP",
                "black-and-chrome",
            ),
            (
                "Mad Max Fury Road (2015) Black & Chrome (1080p BluRay x265 10bit Tigole)",
                "black-and-chrome",
            ),
            (
                "A Christmas Story (1983) RM4K (1080p BluRay x265 HEVC 10bit AAC 2.0 Tigole)",
                "remastered",
            ),
            (
                "Pinocchio (1940) 70th Anniversary Platinum Edition (1080p BDRip x265 10bit EAC3 5.1 - Goki)",
                "anniversary",
            ),
            (
                "Dune.2021.IMAX.Enhanced.2160p.WEB-DL.DDP5.1.Atmos.HDR.HEVC-GRP",
                "imax",
            ),
            (
                "Demolition Man 1993 US Taco Bell Cut UHD BluRay 1080p DD Atmos 5 1 DoVi HDR10 x265-SM737",
                "other:us-taco-bell-cut",
            ),
            ("Movie.2019.1080p.BluRay.x264-GRP", "theatrical"),
        ] {
            assert_eq!(key(name), want, "{name}");
        }
        // The other editions the name names stay with it.
        let r = list().read_release(&parse(
            "Thief (1981) DC RM4K (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole)",
        ));
        assert_eq!(
            (r.label.as_str(), r.also.clone()),
            ("Director's Cut", vec!["remastered".to_string()])
        );
        assert_eq!(r.evidence.as_deref(), Some("DC"));
        assert!(r.marked);
        let plain = list().read_release(&parse("Movie.2019.1080p.BluRay.x264-GRP"));
        assert!(!plain.marked && plain.evidence.is_none());
        // A named theatrical cut with a presentation is the presentation.
        let r = list().read_text("Theatrical Open Matte").unwrap();
        assert_eq!(
            (r.key.as_str(), r.also.clone()),
            ("open-matte", vec!["theatrical".to_string()])
        );
    }

    #[test]
    fn title_words_are_never_editions() {
        for name in [
            "Midnight.Special.2016.1080p.BluRay.x264-SPARKS",
            "Midnight Special (2016) {imdb-tt2649554} [Bluray-1080p][AAC 5.1][x265]-Tigole.mkv",
            "Midnight Special 1080p BluRay x264-SPARKS",
            "The.Final.Cut.2004.1080p.WEB-DL.DD5.1.H264-FGT",
            "Directors.Cut.2016.1080p.WEB-DL.H264-GRP",
            "Extended.Family.2023.S01E01.1080p.WEB.h264-GRP",
            "The.Ultimate.Gift.2006.1080p.BluRay.x264-GRP",
            "Climax.2018.1080p.BluRay.x264-GRP",
            "DC.League.of.Super-Pets.2022.1080p.WEB-DL.DDP5.1.Atmos.H.264-GRP",
            "Unrated.II.Scary.Movie.2011.1080p.WEB-DL.H264-GRP",
            "The.Extendables.2014.1080p.WEB-DL.H264-GRP",
            "Open.Season.2006.1080p.BluRay.x264-GRP",
            "Black.Panther.2018.1080p.BluRay.x264-GRP",
            "Imaxination.2020.1080p.WEB-DL.H264-GRP",
        ] {
            assert_eq!(key(name), "theatrical", "{name}");
        }
        // "Special" after the year without "Edition" is not one either.
        assert_eq!(
            key("Midnight.Special.2016.Special.1080p.BluRay.x264-SPARKS"),
            "theatrical"
        );
        // The same title with a real edition after the year.
        assert_eq!(
            key("Midnight.Special.2016.Special.Edition.1080p.BluRay.x264-SPARKS"),
            "special-edition"
        );
        assert_eq!(
            key("The.Final.Cut.2004.Extended.1080p.WEB-DL.H264-FGT"),
            "extended"
        );
    }

    #[test]
    fn library_names_carry_the_edition_in_the_plex_naming() {
        let read = |name: &str| list().read_release(&parse_library_name(name).to_release());
        let plex = read(
            "1408 (2007) {imdb-tt0450385} {edition-Directors Cut} [Bluray-1080p][DV HDR10][EAC3 5.1][x265]-SM737.mkv",
        );
        assert_eq!(plex.key, "directors-cut");
        let plain = read("1408 (2007) {imdb-tt0450385} [Bluray-1080p][EAC3 5.1][x265]-SM737.mkv");
        assert_eq!(plain.key, "theatrical");
        let named =
            read("Demolition Man (1993) {edition-Taco Bell Cut} [Bluray-1080p][x265]-GRP.mkv");
        assert_eq!(
            (named.key.as_str(), named.label.as_str()),
            ("other:taco-bell-cut", "Taco Bell Cut")
        );
    }

    #[test]
    fn the_arr_field_reads_like_a_name() {
        let l = list();
        let t = |text: &str| l.read_text(text).map(|r| r.key);
        assert_eq!(
            t("4K Remastered - Director's Cut").as_deref(),
            Some("directors-cut")
        );
        assert_eq!(t("Extended UNRATED").as_deref(), Some("extended"));
        assert_eq!(t("Coppola Restoration").as_deref(), Some("restored"));
        assert_eq!(
            t("Eagle Pictures Release").as_deref(),
            Some("other:eagle-pictures-release")
        );
        assert_eq!(t("  ").as_deref(), None);
        let r = l.read_text("D+77 V2.4").unwrap();
        assert_eq!(
            (r.key.as_str(), r.label.as_str()),
            ("other:d-77-v2-4", "D+77 V2.4")
        );
        // A held copy: the name first, the arr's text where the name reads to nothing.
        let held = parse("Movie (2019) {tmdb-1} [Bluray-1080p][x265]-[SHR].mkv");
        assert_eq!(
            l.read_copy(&held, Some("Eagle Pictures Release")).key,
            "other:eagle-pictures-release"
        );
        assert_eq!(l.read_copy(&held, Some("")).key, "theatrical");
        assert_eq!(l.read_copy(&held, None).key, "theatrical");
        assert_eq!(
            l.key_for("Director's Cut").as_deref(),
            Some("directors-cut")
        );
        assert_eq!(
            l.key_for("other:taco-bell-cut").as_deref(),
            Some("other:taco-bell-cut")
        );
        assert_eq!(l.key_for("nonsense"), None);
        assert!(l.rank("directors-cut") < l.rank("theatrical"));
        // A cut the list does not know comes after its cuts and before the presentations.
        assert!(l.rank("anniversary") < l.rank("other:x"));
        assert!(l.rank("other:x") < l.rank("imax"));
        let r = l.read_release(&parse(
            "Waterworld (1995) Ulysses Cut RM4K (1080p BluRay x265 HEVC 10bit AAC 5.1 Tigole)",
        ));
        assert_eq!(
            (r.key.as_str(), r.also.clone()),
            ("other:ulysses-cut", vec!["remastered".to_string()])
        );
    }
}
