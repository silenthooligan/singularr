//! Readers an adapter reads a service's answer with: lenient where a field may have another
//! shape, and silent where a value means nothing.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

/// A usenet job's name as it is stored. A name that holds an address or a magnet link (a job
/// named after the link its NZB comes from) loses the link's query and fragment, and the user
/// and password before its host. An indexer's link carries its key there.
pub fn safe_name(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    if !(lower.contains("://") || lower.contains("magnet:")) {
        return name.to_string();
    }
    let mut out = name
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    if let Some(at) = out.find("://") {
        let host = at + 3;
        let host_end = out[host..].find('/').map_or(out.len(), |i| host + i);
        if let Some(user) = out[host..host_end].rfind('@') {
            out.replace_range(host..=host + user, "");
        }
    }
    out.trim().to_string()
}

/// A Unix time as a client writes it. Zero, negative and the 32-bit "never" are no date.
pub fn unix(seconds: Option<i64>) -> Option<DateTime<Utc>> {
    let s = seconds.filter(|s| *s > 0 && *s < 4_000_000_000)?;
    DateTime::from_timestamp(s, 0)
}

/// A count or a rate as a client reports it: negative is none.
pub fn count(v: Option<i64>) -> Option<i32> {
    v.filter(|n| *n >= 0)
        .map(|n| i32::try_from(n).unwrap_or(i32::MAX))
}

/// The host of a tracker's announce address: `tracker.example.org`. A value that is no
/// address is taken as a host already, as Deluge gives it.
pub fn tracker_host(announce: &str) -> Option<String> {
    let text = announce.trim();
    if text.is_empty() {
        return None;
    }
    match Url::parse(text) {
        Ok(url) => url.host_str().map(str::to_ascii_lowercase),
        Err(_) if !text.contains(['/', ' ']) => Some(text.to_ascii_lowercase()),
        Err(_) => None,
    }
}

/// A number a JSON answer holds as a number or as text: `"4.39"`, `4.39`, `"7"`.
pub fn number(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// Text that is not empty, trimmed.
pub fn text(v: Option<&serde_json::Value>) -> Option<String> {
    v?.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A field that is missing or `null` reads as its default. The arrs send `null` where a
/// list is empty in some answers (a movie's `customFormats` in the movie list).
pub fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A descriptive field, read leniently: a value of an unexpected shape is no value. What a
/// title is called or rated must never fail the listing that carries its files.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: DeserializeOwned,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).ok())
}
