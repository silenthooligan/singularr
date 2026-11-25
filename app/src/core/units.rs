//! Numbers, sizes and spans of time as the interface writes them (`docs/copy.md`, rule 11):
//! `1,019`, `4,470 titles`, `4.30 GB`, `8 d`. Every line the server writes for a page takes
//! its numbers from here, so a count reads the same in a job line, a check and the journal.

use chrono::Duration;

/// A whole number with its thousands apart: `1,019`.
pub fn count(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A count with its noun, one or many: `1 title`, `4,470 titles`.
pub fn counted(n: i64, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

/// Bytes as the web client writes them: steps of 1024, three figures from megabytes on.
/// `730 MB`, `41.9 GB`, `4.30 GB`.
pub fn size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = bytes.max(0) as f64;
    let mut unit = 0;
    while n >= 1024.0 && unit < UNITS.len() - 1 {
        n /= 1024.0;
        unit += 1;
    }
    let number = if n >= 100.0 || unit < 2 {
        format!("{n:.0}")
    } else if n >= 10.0 {
        format!("{n:.1}")
    } else {
        format!("{n:.2}")
    };
    format!("{number} {}", UNITS[unit])
}

/// A span of time as one number and its unit: minutes under two hours, hours under two days,
/// then days. `45 min`, `30 h`, `8 d`.
pub fn span(d: Duration) -> String {
    if d < Duration::hours(2) {
        format!("{} min", d.num_minutes().max(0))
    } else if d < Duration::days(2) {
        format!("{} h", d.num_hours())
    } else {
        format!("{} d", d.num_days())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_grouped_by_thousands_and_name_one_or_many() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1019), "1,019");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(count(-7491), "-7,491");
        assert_eq!(counted(1, "title", "titles"), "1 title");
        assert_eq!(counted(4470, "title", "titles"), "4,470 titles");
        assert_eq!(counted(0, "run", "runs"), "0 runs");
    }

    #[test]
    fn sizes_and_spans_read_as_the_client_writes_them() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(2048), "2 KB");
        assert_eq!(size(765_460_480), "730 MB");
        assert_eq!(size(4_617_089_843), "4.30 GB");
        assert_eq!(size(44_990_000_000), "41.9 GB");
        assert_eq!(size(-5), "0 B");
        assert_eq!(span(Duration::minutes(-5)), "0 min");
        assert_eq!(span(Duration::minutes(45)), "45 min");
        assert_eq!(span(Duration::hours(30)), "30 h");
        assert_eq!(span(Duration::hours(48)), "2 d");
        assert_eq!(span(Duration::days(8) + Duration::hours(5)), "8 d");
    }
}
