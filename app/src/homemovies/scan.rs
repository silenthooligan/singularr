//! Read the home movies folder: one tape per video file, its size, and what its name says.
//! Plain parsing and a walk with `std::fs`, so it is tested on any machine.
//!
//! A name reads as `Years - Event - Event`:
//!
//! - Years: a leading `1986`, or a range `1986-1987`. None means the date is uncertain, and so
//!   does a range of more than two years (`1988-1991`).
//! - Events: the rest, split on ` - ` (or `;`, `|`). A name that uses bare hyphens only
//!   (`tennis-shaving papa-...`) is split on those. Each event becomes a chapter: spaces tidied,
//!   a few common shorthands written out (`xmas`, `bday`, `grad`), a two-digit year inside the
//!   tape's years written in full at the end (`easter 87` is `Easter 1987`), the first letter
//!   capitalized.
//!
//! No outside source is asked about a tape, and no audio or video is read.

use chrono::{DateTime, SubsecRound, Utc};
use std::fs;
use std::path::Path;

/// The video formats, by file extension.
pub const VIDEO: [&str; 16] = [
    "mp4", "m4v", "mkv", "avi", "mov", "mpg", "mpeg", "wmv", "ts", "m2ts", "mts", "webm", "3gp",
    "dv", "vob", "flv",
];
/// How deep the walk goes below the root.
const MAX_DEPTH: usize = 4;
/// Chapters read from one name, at most.
const MAX_CHAPTERS: usize = 50;

/// One video file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedTape {
    /// Relative to the root, with `/` between folders.
    pub path: String,
    pub size_bytes: i64,
    pub modified_at: Option<DateTime<Utc>>,
}

/// What a file name says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    pub title: String,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub uncertain: bool,
    pub chapters: Vec<String>,
}

/// A leading four-digit year that stands alone (followed by the end, a space or a dash).
fn year_at(s: &str) -> Option<(i32, &str)> {
    let digits = s.get(..4)?;
    if !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let rest = &s[4..];
    if !(rest.is_empty() || rest.starts_with([' ', '-'])) {
        return None;
    }
    let year: i32 = digits.parse().ok()?;
    (1900..=2100).contains(&year).then_some((year, rest))
}

fn strip_separators(s: &str) -> &str {
    s.trim_start_matches([' ', '-']).trim_start()
}

/// The years in front of a name, and the rest.
fn leading_years(text: &str) -> (Option<(i32, i32)>, &str) {
    let Some((from, rest)) = year_at(text) else {
        return (None, text);
    };
    let after = rest.trim_start();
    if let Some(dashed) = after.strip_prefix('-') {
        let dashed = dashed.trim_start();
        if let Some((to, rest)) = year_at(dashed)
            && to >= from
            && to - from <= 30
        {
            return (Some((from, to)), strip_separators(rest));
        }
        return (Some((from, from)), strip_separators(dashed));
    }
    (Some((from, from)), strip_separators(after))
}

fn split_events(rest: &str) -> Vec<String> {
    let rest = rest.replace([';', '|'], " - ");
    let tidy = |p: &str| p.split_whitespace().collect::<Vec<_>>().join(" ");
    let spaced: Vec<String> = rest
        .split(" - ")
        .map(tidy)
        .filter(|p| !p.is_empty() && p != "-")
        .collect();
    if spaced.len() <= 1 && rest.contains('-') {
        return rest
            .split('-')
            .map(tidy)
            .filter(|p| !p.is_empty())
            .collect();
    }
    spaced
}

/// A shorthand written out, or the word as it is.
fn expand(word: &str) -> String {
    match word.to_lowercase().as_str() {
        "xmas" | "christmas" => "Christmas",
        "bday" | "birthday" => "birthday",
        "grad" => "graduation",
        "easter" => "Easter",
        "thanksgiving" => "Thanksgiving",
        "halloween" => "Halloween",
        _ => return word.to_string(),
    }
    .to_string()
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// One event as a chapter label.
fn chapter(raw: &str, years: Option<(i32, i32)>) -> String {
    let mut words: Vec<&str> = raw.split_whitespace().collect();
    let mut year = None;
    if let Some((from, to)) = years {
        let in_span = |w: &str| -> Option<i32> {
            if w.len() != 2 || !w.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let yy: i32 = w.parse().ok()?;
            (from..=to).find(|y| y % 100 == yy)
        };
        if let Some(pos) = words.iter().position(|w| in_span(w).is_some()) {
            year = in_span(words[pos]);
            words.remove(pos);
        }
    }
    let text = capitalized(&words.into_iter().map(expand).collect::<Vec<_>>().join(" "));
    match (year, text.is_empty()) {
        (Some(y), true) => y.to_string(),
        (Some(y), false) => format!("{text} {y}"),
        (None, _) => text,
    }
}

/// A title from the chapters: the first three, and how many more.
fn title_of(chapters: &[String], fallback: &str) -> String {
    match chapters.len() {
        0 => capitalized(fallback),
        1..=3 => chapters.join(", "),
        n => format!("{} and {} more", chapters[..3].join(", "), n - 3),
    }
}

/// Read a file name (or a path: the file name is read).
pub fn read_name(file_name: &str) -> Reading {
    let leaf = file_name.rsplit('/').next().unwrap_or(file_name);
    let stem = match leaf.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 5 => stem,
        _ => leaf,
    };
    // Dashes of other widths read as the plain one, underscores as spaces.
    let text: String = stem
        .chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' => '-',
            '_' => ' ',
            c => c,
        })
        .collect();
    let text = text.trim();
    let (years, rest) = leading_years(text);
    let chapters: Vec<String> = split_events(rest)
        .iter()
        .map(|e| chapter(e, years))
        .filter(|c| !c.is_empty())
        .take(MAX_CHAPTERS)
        .collect();
    let uncertain = match years {
        None => true,
        Some((from, to)) => to - from >= 2,
    };
    Reading {
        title: title_of(&chapters, text),
        year_from: years.map(|(from, _)| from),
        year_to: years.map(|(_, to)| to),
        uncertain,
        chapters,
    }
}

fn skip_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('@') || name.starts_with('#')
}

fn is_video(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && VIDEO.contains(&ext.to_ascii_lowercase().as_str())
    })
}

fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<ScannedTape>, skipped: &mut usize) {
    let Ok(entries) = fs::read_dir(dir) else {
        *skipped += 1;
        return;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            *skipped += 1;
            continue;
        };
        let Ok(name) = entry.file_name().into_string() else {
            *skipped += 1;
            continue;
        };
        if skip_name(&name) {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            *skipped += 1;
            continue;
        };
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        // Links are not followed, so the walk never leaves the folder.
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            if depth < MAX_DEPTH {
                walk(&entry.path(), &path, depth + 1, out, skipped);
            }
            continue;
        }
        if !kind.is_file() || !is_video(&name) {
            continue;
        }
        let meta = entry.metadata().ok();
        out.push(ScannedTape {
            path,
            size_bytes: meta
                .as_ref()
                .and_then(|m| i64::try_from(m.len()).ok())
                .unwrap_or(0),
            modified_at: meta
                .and_then(|m| m.modified().ok())
                .map(|t| DateTime::<Utc>::from(t).trunc_subsecs(6)),
        });
    }
}

/// Walk the folder. Fails when the folder itself cannot be read; a sub-folder that cannot be
/// read is counted in the second value.
pub fn scan(root: &Path) -> Result<(Vec<ScannedTape>, usize), String> {
    let meta = fs::metadata(root).map_err(|e| format!("{} cannot be read: {e}", root.display()))?;
    if !meta.is_dir() {
        return Err(format!("{} is not a folder", root.display()));
    }
    let _listing =
        fs::read_dir(root).map_err(|e| format!("{} cannot be listed: {e}", root.display()))?;
    let mut out = Vec::new();
    let mut skipped = 0;
    walk(root, "", 0, &mut out, &mut skipped);
    out.sort_by_key(|t| t.path.clone());
    Ok((out, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::music::scan::tests::TempDir;

    fn read(name: &str) -> (Option<i32>, Option<i32>, bool, Vec<String>) {
        let r = read_name(name);
        (r.year_from, r.year_to, r.uncertain, r.chapters)
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn names_give_years_and_chapters() {
        assert_eq!(
            read("1986 - montana - reptile gardens - bear country.mp4"),
            (
                Some(1986),
                Some(1986),
                false,
                v(&["Montana", "Reptile gardens", "Bear country"])
            )
        );
        assert_eq!(
            read("1986 Casey memories.mp4"),
            (Some(1986), Some(1986), false, v(&["Casey memories"]))
        );
        assert_eq!(
            read("1986 xmas 86 - preschool xmas 86.mp4").3,
            v(&["Christmas 1986", "Preschool Christmas 1986"])
        );
        assert_eq!(
            read("1986-1987 - riley jordan 86 - easter 87 - riley bday 87.mp4"),
            (
                Some(1986),
                Some(1987),
                false,
                v(&["Riley jordan 1986", "Easter 1987", "Riley birthday 1987"])
            )
        );
        assert_eq!(
            read("1988-1989 riley's 5th bday - daycare grad - learning to ride bike .mp4").3,
            v(&[
                "Riley's 5th birthday",
                "Daycare graduation",
                "Learning to ride bike"
            ])
        );
        // A range of more than two years is uncertain.
        assert!(read("1988-1991 - easter 88 - easter 91.mp4").2);
        assert_eq!(
            read("1988-1991 - easter 88 - easter 91.mp4").3,
            v(&["Easter 1988", "Easter 1991"])
        );
        // Bare hyphens split when the name has no spaced ones; a number outside the years stays.
        assert_eq!(
            read(
                "1993 jordan tisch tennis-shaving papa-riley 10 bday-jordan 9 bday-papa soccer.mp4"
            )
            .3,
            v(&[
                "Jordan tisch tennis",
                "Shaving papa",
                "Riley 10 birthday",
                "Jordan 9 birthday",
                "Papa soccer"
            ])
        );
        let r = read_name(
            "1994-1995 94 xmas-95 austria skiing-buddy-disney paris-rouen-spain easter-legoland denmark-lubeck-school talent show.mp4",
        );
        assert_eq!(r.chapters.len(), 9);
        assert_eq!(r.chapters[0], "Christmas 1994");
        assert_eq!(r.chapters[1], "Austria skiing 1995");
        assert_eq!(
            r.title,
            "Christmas 1994, Austria skiing 1995, Buddy and 6 more"
        );
        // No year: uncertain.
        assert_eq!(
            read("montana - san jose - learning to roller skate - xmas.mp4"),
            (
                None,
                None,
                true,
                v(&[
                    "Montana",
                    "San jose",
                    "Learning to roller skate",
                    "Christmas"
                ])
            )
        );
        let r = read_name("Untitled video - Made with Clipchamp.mp4");
        assert!(r.uncertain && r.year_from.is_none());
        assert_eq!(r.title, "Untitled video, Made with Clipchamp");
        assert_eq!(read_name("1986.mp4").title, "1986");
        assert_eq!(
            read_name("folder/1991 - xmas san carlos.mp4").chapters,
            v(&["Christmas san carlos"])
        );
    }

    #[test]
    fn a_walk_finds_the_video_files() {
        let d = TempDir::new("tapes");
        d.file("1986 - montana.mp4", 12);
        d.file("1988 - xmas.MKV", 7);
        d.file("notes.txt", 3);
        d.file(".hidden.mp4", 3);
        d.file("Archive/1991 - xmas san carlos.mp4", 5);
        d.file("@eaDir/thumb.mp4", 1);
        let (tapes, skipped) = scan(&d.0).unwrap();
        let paths: Vec<&str> = tapes.iter().map(|t| t.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "1986 - montana.mp4",
                "1988 - xmas.MKV",
                "Archive/1991 - xmas san carlos.mp4"
            ]
        );
        assert_eq!(tapes[0].size_bytes, 12);
        assert!(tapes[0].modified_at.is_some());
        assert_eq!(skipped, 0);
        assert!(scan(&d.0.join("missing")).is_err());
    }
}
