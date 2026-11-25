//! The home movies as the API and the web app see them. Nothing here leaves the server and no
//! outside metadata is used: names, dates and chapters come from the file names and from the
//! owner's own annotations.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};

/// One chapter of a tape: an event read from the file name, or set by the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Chapter {
    pub label: String,
    /// Where the chapter starts, in seconds from the start of the file. Absent when not set.
    #[serde(default)]
    pub start_seconds: Option<i32>,
}

/// What the file name says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TapeReading {
    pub title: String,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    /// No year in the name, or a range of more than two years.
    pub uncertain: bool,
    pub chapters: Vec<Chapter>,
}

/// What the owner set. A field that is absent keeps the reading.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TapeAnnotation {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_from: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year_to: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uncertain: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapters: Option<Vec<Chapter>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub people: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub places: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// One tape: one video file in the home movies folder, as the owner sees it. Every field
/// below `size_bytes` is the owner's annotation where there is one, else the reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct Tape {
    pub id: i64,
    /// Relative to the home movies folder.
    pub path: String,
    pub file_name: String,
    pub size_bytes: i64,
    pub modified_at: Option<DateTime<Utc>>,
    pub title: String,
    pub year_from: Option<i32>,
    pub year_to: Option<i32>,
    pub uncertain: bool,
    pub chapters: Vec<Chapter>,
    pub people: Vec<String>,
    pub places: Vec<String>,
    /// Kept out of every shared library.
    pub private: bool,
    pub notes: Option<String>,
    /// The owner has looked at it. A tape that was never annotated is in the inbox.
    pub annotated: bool,
    pub annotated_at: Option<DateTime<Utc>>,
    pub read: TapeReading,
    pub annotation: TapeAnnotation,
    /// A still made on this server. Always absent in this stage: no still is made yet.
    pub still_url: Option<String>,
    /// The file is gone from the folder; the tape and its annotation are kept.
    pub missing: bool,
    pub missing_since: Option<DateTime<Utc>>,
    pub first_seen_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Filters of `GET /api/v1/home-movies`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct TapeQuery {
    /// Tapes whose years include this one.
    pub year: Option<i32>,
    /// Tapes tagged with this person, any case.
    pub person: Option<String>,
    /// Tapes tagged with this place, any case.
    pub place: Option<String>,
    pub uncertain: Option<bool>,
    pub private: Option<bool>,
    /// `true`: only tapes never annotated. `false`: only annotated ones.
    pub inbox: Option<bool>,
    /// `true`: only tapes whose file is gone. Left out or `false`: only tapes that are there.
    pub missing: Option<bool>,
    /// Words of the title, the chapters, the tags, the notes or the file name.
    pub q: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// Paging of `GET /api/v1/home-movies/inbox`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "ssr", into_params(parameter_in = Query))]
pub struct InboxQuery {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct YearCount {
    pub year: i32,
    /// Tapes that start in this year.
    pub tapes: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TagCount {
    pub name: String,
    pub tapes: i64,
}

/// Totals over every tape that is there.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TapeTotals {
    pub tapes: i64,
    pub size_bytes: i64,
    pub first_year: Option<i32>,
    pub last_year: Option<i32>,
    pub inbox: i64,
    pub uncertain: i64,
    pub private: i64,
    /// Tapes whose file is gone.
    pub missing: i64,
}

/// The answer of `GET /api/v1/home-movies`. `years`, `people`, `places` and `totals` are over
/// every tape that is there, whatever the filters, so the filter lists stay whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
pub struct TapePage {
    pub items: Vec<Tape>,
    pub page: u32,
    pub per_page: u32,
    /// Tapes that match the filters, over all pages.
    pub total: i64,
    pub years: Vec<YearCount>,
    pub people: Vec<TagCount>,
    pub places: Vec<TagCount>,
    pub totals: TapeTotals,
}

/// A field of a patch: absent keeps the value, `null` clears the annotation (back to the
/// reading), a value sets it.
fn patch_field<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// The body of `PATCH /api/v1/home-movies/{id}`. Every field is optional: a field that is
/// absent keeps its value, `null` clears the annotation so the reading shows again, a value
/// sets it. Any patch, an empty one too, marks the tape annotated and takes it out of the
/// inbox.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct TapePatch {
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<String>))]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<i32>))]
    pub year_from: Option<Option<i32>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<i32>))]
    pub year_to: Option<Option<i32>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<bool>))]
    pub uncertain: Option<Option<bool>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<Vec<Chapter>>))]
    pub chapters: Option<Option<Vec<Chapter>>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<Vec<String>>))]
    pub people: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<Vec<String>>))]
    pub places: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<bool>))]
    pub private: Option<Option<bool>>,
    #[serde(default, deserialize_with = "patch_field")]
    #[cfg_attr(feature = "ssr", schema(value_type = Option<String>))]
    pub notes: Option<Option<String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_tells_absent_from_null() {
        let p: TapePatch =
            serde_json::from_str(r#"{"title": "Easter 1987", "notes": null}"#).unwrap();
        assert_eq!(p.title, Some(Some("Easter 1987".to_string())));
        assert_eq!(p.notes, Some(None));
        assert_eq!(p.people, None);
        assert!(serde_json::from_str::<TapePatch>(r#"{"titel": "x"}"#).is_err());
        let empty: TapePatch = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, TapePatch::default());
    }
}
