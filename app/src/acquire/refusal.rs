//! What a manager's refusal to import a download says, read into Singularr's terms: who refused,
//! why as a label, and the two scores and custom formats when the message gives them. The
//! manager's own sentence stays beside it. Pure.

use super::model::{Refusal, RefusalSide};

/// The managers' refusals Singularr knows, by a part of the message, each with its label. The
/// first that matches is taken.
const REASONS: [(&str, &str); 8] = [
    ("not a custom format upgrade", "Not a custom format upgrade"),
    ("not a quality revision upgrade", "Not a revision upgrade"),
    ("not a revision upgrade", "Not a revision upgrade"),
    ("not an upgrade", "Not an upgrade"),
    ("sample", "Sample"),
    ("no files found are eligible", "No eligible files"),
    ("manual import required", "Manual import required"),
    ("unable to parse", "Not recognized"),
];
/// The label of a refusal Singularr does not know.
const REFUSED: &str = "Import refused";

/// A queue state in which the manager holds a finished download back from its library.
pub fn is_refused(state: Option<&str>) -> bool {
    state.is_some_and(|s| {
        s.eq_ignore_ascii_case("importBlocked") || s.eq_ignore_ascii_case("importPending")
    })
}

/// The list after `marker` up to its `]`, and the number in the brackets after it:
/// `New: [a, b] (300)`.
fn side(message: &str, marker: &str) -> Option<RefusalSide> {
    let lower = message.to_ascii_lowercase();
    let at = lower.find(&marker.to_ascii_lowercase())? + marker.len();
    let rest = &message[at..];
    let close = rest.find(']')?;
    let formats: Vec<String> = rest[..close]
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .collect();
    let after = rest[close + 1..].trim_start();
    let score = after
        .strip_prefix('(')
        .and_then(|s| s.split(')').next())
        .and_then(|n| n.trim().parse::<i32>().ok());
    Some(RefusalSide { score, formats })
}

/// What `by` (the manager's name) said when it refused to import: the first message that
/// reads as a known refusal, else the first message.
pub fn read(by: &str, messages: &[String]) -> Refusal {
    let known = |m: &String| {
        let lower = m.to_ascii_lowercase();
        REASONS
            .iter()
            .find(|(part, _)| lower.contains(part))
            .map(|(_, label)| *label)
    };
    let picked = messages.iter().find(|m| known(m).is_some());
    let reason = picked.and_then(known).unwrap_or(REFUSED);
    let message = picked
        .or(messages.first())
        .map(String::as_str)
        .unwrap_or("");
    Refusal {
        by: by.to_string(),
        reason: reason.to_string(),
        new: side(message, "New: ["),
        held: side(message, "Existing: ["),
    }
}

/// The refusal in the interface's words: `Radarr refused the import · 300 against 600`, or
/// with the reason where the message gives no scores.
pub fn words(r: &Refusal) -> String {
    let scores = (
        r.new.as_ref().and_then(|s| s.score),
        r.held.as_ref().and_then(|s| s.score),
    );
    match scores {
        (Some(new), Some(held)) => format!("{} refused the import · {new} against {held}", r.by),
        _ if r.reason != REFUSED => format!("{} refused the import · {}", r.by, r.reason),
        _ => format!("{} refused the import", r.by),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusal of the case that showed the gap (The Devil Wears Prada, 24 Nov), as Radarr's
    /// queue reports it.
    #[test]
    fn a_custom_format_refusal_reads_as_two_scores() {
        let queue: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/radarr_queue_import_refused.json"
        ))
        .unwrap();
        let messages: Vec<String> = queue[0]["statusMessages"][0]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_str().unwrap().to_string())
            .collect();
        let r = read("Radarr", &messages);
        assert_eq!(
            r,
            Refusal {
                by: "Radarr".to_string(),
                reason: "Not a custom format upgrade".to_string(),
                new: Some(RefusalSide {
                    score: Some(300),
                    formats: ["5.1 Surround", "Bluray", "DD+", "Open Matte", "x264"]
                        .map(String::from)
                        .to_vec(),
                }),
                held: Some(RefusalSide {
                    score: Some(600),
                    formats: ["5.1 Surround", "HUSH SHH", "Opus", "x265 (HD)"]
                        .map(String::from)
                        .to_vec(),
                }),
            }
        );
        assert_eq!(words(&r), "Radarr refused the import · 300 against 600");
        assert!(is_refused(queue[0]["trackedDownloadState"].as_str()));
        assert!(!is_refused(Some("downloading")) && !is_refused(None));
    }

    #[test]
    fn other_refusals_read_as_their_reason() {
        let r = read(
            "Radarr",
            &["Not an upgrade for existing movie file(s)".to_string()],
        );
        assert_eq!(
            (r.reason.as_str(), &r.new, &r.held),
            ("Not an upgrade", &None, &None)
        );
        assert_eq!(words(&r), "Radarr refused the import · Not an upgrade");
        let r = read(
            "Sonarr",
            &[
                "One or more episodes expected in this release were not imported or missing"
                    .to_string(),
                "Sample".to_string(),
            ],
        );
        assert_eq!(r.reason, "Sample");
        let r = read("Sonarr", &["Something new".to_string()]);
        assert_eq!(words(&r), "Sonarr refused the import");
        let r = read("Sonarr", &[]);
        assert_eq!((r.reason.as_str(), r.new), ("Import refused", None));
        // Sonarr's words for an episode, and a message cut short.
        let r = read(
            "Sonarr",
            &["Not a Custom Format upgrade for existing episode file(s). New: [DD+] (50) do not improve on Existing: [DD+, x265] (".to_string()],
        );
        assert_eq!(r.new.unwrap().score, Some(50));
        assert_eq!(r.held.unwrap().score, None);
    }
}
