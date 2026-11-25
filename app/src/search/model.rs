//! What the search port takes and answers: one document per thing the library has, and the
//! status of the provider in force.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One thing the library has, as the search index holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SearchDoc {
    /// `movie-603`, `show-1396`, `collection-2344`, `album-<key>`, `artist-<key>`.
    pub id: String,
    /// `movie`, `show`, `collection`, `album` or `artist`.
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_title: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    /// Other names it is found by: a movie's collection and studio, a show's network, an
    /// album's artist and label.
    #[serde(default)]
    pub extra: Vec<String>,
    /// A TMDB image path (`/abc.jpg`), or Singularr's address of a cover or a photo.
    #[serde(default)]
    pub poster: Option<String>,
    #[serde(default)]
    pub rating: Option<f64>,
    /// An album's artist (`artist-<key>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// An album's artist by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
}

impl SearchDoc {
    /// Every searchable word in lower case: the title, the original title, the other names
    /// and the year.
    pub fn words(&self) -> String {
        let mut parts: Vec<&str> = vec![self.title.as_str()];
        if let Some(o) = &self.original_title {
            parts.push(o);
        }
        parts.extend(self.extra.iter().map(String::as_str));
        let mut text = parts.join(" ").to_lowercase();
        if let Some(y) = self.year {
            text.push(' ');
            text.push_str(&y.to_string());
        }
        text
    }
}

/// Which search is in force and how its index stands (`GET /api/v1/search/status`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "ssr", derive(utoipa::ToSchema))]
#[serde(rename_all = "camelCase")]
pub struct SearchStatus {
    /// The provider answering searches now: `postgresql` (built in) or `meilisearch`.
    pub provider: String,
    /// The provider the configuration asks for. When it differs from `provider`, Meilisearch
    /// is configured and failing, and built-in search answers.
    pub configured: String,
    /// Documents in the index of the provider in force.
    pub documents: i64,
    /// When that index was last written in full.
    pub last_indexed: Option<DateTime<Utc>>,
    /// `ok`, `warn` (falling back, or not indexed yet) or `down`.
    pub state: String,
    pub detail: Option<String>,
    /// Built-in search tolerates typos (the database has the trigram extension).
    pub typo_tolerant: bool,
}
