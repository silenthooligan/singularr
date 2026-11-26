//! Types shared by every layer and by the browser: pages, scopes, journal events, API keys.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The largest page a list route returns.
pub const MAX_PER_PAGE: u32 = 200;
pub const DEFAULT_PER_PAGE: u32 = 50;

/// One page of a list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    /// 1-based.
    pub page: u32,
    pub per_page: u32,
    /// Rows that match the filters, over all pages.
    pub total: i64,
}

impl<T> Page<T> {
    pub fn new(items: Vec<T>, paging: Paging, total: i64) -> Self {
        Page {
            items,
            page: paging.page,
            per_page: paging.per_page,
            total,
        }
    }

    pub fn pages(&self) -> u32 {
        if self.per_page == 0 {
            return 1;
        }
        ((self.total.max(0) as u64).div_ceil(self.per_page as u64)).max(1) as u32
    }
}

/// A whole list that is short enough to need no paging: the libraries, the API keys, the
/// metadata providers. It has the same `items` and `total` as a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Listing<T> {
    pub items: Vec<T>,
    /// The number of items.
    pub total: i64,
}

impl<T> Listing<T> {
    pub fn new(items: Vec<T>) -> Self {
        let total = items.len() as i64;
        Listing { items, total }
    }
}

/// A checked page request: `page` from 1, `per_page` from 1 to [`MAX_PER_PAGE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paging {
    pub page: u32,
    pub per_page: u32,
}

impl Paging {
    pub fn new(page: Option<u32>, per_page: Option<u32>) -> Self {
        Paging {
            page: page.unwrap_or(1).max(1),
            per_page: per_page.unwrap_or(DEFAULT_PER_PAGE).clamp(1, MAX_PER_PAGE),
        }
    }

    pub fn limit(self) -> i64 {
        self.per_page as i64
    }

    pub fn offset(self) -> i64 {
        (self.page as i64 - 1) * self.per_page as i64
    }
}

/// What an API key may do. Stage 1 routes need `read`; key management needs `admin`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Read,
    Library,
    Acquire,
    Settings,
    Admin,
}

impl Scope {
    pub const ALL: [Scope; 5] = [
        Scope::Read,
        Scope::Library,
        Scope::Acquire,
        Scope::Settings,
        Scope::Admin,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Read => "read",
            Scope::Library => "library",
            Scope::Acquire => "acquire",
            Scope::Settings => "settings",
            Scope::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Scope> {
        Scope::ALL
            .into_iter()
            .find(|x| x.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

/// One journal entry. Every change Singularr sees or makes is one of these.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema, sqlx::FromRow)]
pub struct Event {
    /// Monotonic: a larger id was committed later.
    pub id: i64,
    pub at: DateTime<Utc>,
    /// For example `copy_replaced` or `download_stuck`.
    pub kind: String,
    /// `title`, `copy`, `download`, `engine`, `policy`, `key`, `request`, `album` or `tape`.
    pub subject_type: String,
    pub subject_id: String,
    /// Who did it: `sync:radarr`, `policy`, `user:<email>`, `key:<name>`.
    pub actor: String,
    /// The event that led to this one, when there is one.
    pub cause: Option<i64>,
    #[schema(value_type = Object)]
    pub body: serde_json::Value,
}

/// Filters of `GET /api/v1/events`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EventQuery {
    /// One kind, or several separated by commas.
    pub kind: Option<String>,
    pub subject_type: Option<String>,
    pub subject_id: Option<String>,
    /// Shorthand for a subject: `copy:12`, `download:<hash>`, `engine:radarr`. `title:7`
    /// means the same as `title=7`.
    pub subject: Option<String>,
    /// Events about this title: the title itself, its copies and its downloads.
    pub title: Option<i64>,
    pub actor: Option<String>,
    /// Only events with an id above this one.
    pub after: Option<i64>,
    /// Only events with an id below this one.
    pub before: Option<i64>,
    /// `asc` for oldest first. The default is newest first.
    pub order: Option<String>,
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ApiKeyInfo {
    pub id: i64,
    pub name: String,
    /// The first characters of the key, enough to tell keys apart. Never the secret.
    pub prefix: String,
    pub scopes: Vec<Scope>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct NewApiKey {
    pub name: String,
    pub scopes: Vec<Scope>,
}

/// The answer to creating a key. `secret` is shown this once and is not stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreatedApiKey {
    pub key: ApiKeyInfo,
    pub secret: String,
}

/// Who the current caller is, for the web app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Whoami {
    /// `owner` or `key`.
    pub kind: String,
    pub name: String,
    pub scopes: Vec<Scope>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_is_clamped() {
        let p = Paging::new(Some(0), Some(5000));
        assert_eq!((p.page, p.per_page), (1, MAX_PER_PAGE));
        let p = Paging::new(Some(3), Some(25));
        assert_eq!((p.limit(), p.offset()), (25, 50));
        let page: Page<u8> = Page::new(vec![], p, 51);
        assert_eq!(page.pages(), 3);
    }

    #[test]
    fn scopes_read_back() {
        assert_eq!(Scope::parse(" Admin "), Some(Scope::Admin));
        assert_eq!(Scope::parse("nope"), None);
        assert_eq!(serde_json::to_string(&Scope::Read).unwrap(), "\"read\"");
    }
}
