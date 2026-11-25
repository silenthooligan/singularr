//! Compare what a Seerr site lists now with what is stored for it. Pure, so the sync's
//! decisions are tested without a database.

use chrono::{DateTime, Utc};

/// One request as a row holds it: everything a pass writes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct RequestFacts {
    pub site: String,
    pub seerr_id: i64,
    /// `movie` or `show`.
    pub kind: String,
    pub tmdb_id: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub is_4k: bool,
    pub request_status: String,
    pub media_status: String,
    /// The one-word status.
    pub status: String,
    pub requested_by: String,
    pub requested_by_id: Option<i64>,
    pub requested_at: DateTime<Utc>,
    pub seerr_updated_at: Option<DateTime<Utc>>,
    pub seasons: Vec<i32>,
    pub title_id: Option<i64>,
}

/// A stored request: its row id and its facts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "ssr", derive(sqlx::FromRow))]
pub struct StoredRequest {
    pub id: i64,
    #[cfg_attr(feature = "ssr", sqlx(flatten))]
    pub facts: RequestFacts,
}

/// What to do with one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A request the site lists and Singularr does not hold. The index is into the incoming
    /// requests.
    Add(usize),
    /// The one-word status moved; `from` is the stored one.
    Status { id: i64, new: usize, from: String },
    /// Something else changed (seasons, the catalog link, Seerr's own update time).
    Update { id: i64, new: usize },
    /// A stored request the site no longer lists.
    Remove(i64),
}

/// The changes that turn `stored` into `incoming`, by Seerr id. Removals are left out when
/// `removals` is false (a pass that may have missed a request must not remove it).
pub fn diff(stored: &[StoredRequest], incoming: &[RequestFacts], removals: bool) -> Vec<Change> {
    use std::collections::{HashMap, HashSet};
    let by_id: HashMap<i64, &StoredRequest> =
        stored.iter().map(|s| (s.facts.seerr_id, s)).collect();
    let listed: HashSet<i64> = incoming.iter().map(|r| r.seerr_id).collect();
    let mut changes = Vec::new();
    for (index, new) in incoming.iter().enumerate() {
        match by_id.get(&new.seerr_id) {
            None => changes.push(Change::Add(index)),
            Some(old) if old.facts.status != new.status => changes.push(Change::Status {
                id: old.id,
                new: index,
                from: old.facts.status.clone(),
            }),
            Some(old) if old.facts != *new => changes.push(Change::Update {
                id: old.id,
                new: index,
            }),
            Some(_) => {}
        }
    }
    if removals {
        changes.extend(
            stored
                .iter()
                .filter(|s| !listed.contains(&s.facts.seerr_id))
                .map(|s| Change::Remove(s.id)),
        );
    }
    changes
}

/// A listing this much shorter than what is stored is refused: a site that answers with a
/// fraction of its requests must not read as a mass removal.
pub fn implausible(known: usize, listed: usize) -> bool {
    known >= 20 && listed * 2 < known
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn facts(id: i64, status: &str) -> RequestFacts {
        RequestFacts {
            site: "plex".into(),
            seerr_id: id,
            kind: "movie".into(),
            tmdb_id: Some(id * 10),
            tvdb_id: None,
            is_4k: false,
            request_status: "approved".into(),
            media_status: "processing".into(),
            status: status.into(),
            requested_by: "maple.reed".into(),
            requested_by_id: Some(4),
            requested_at: Utc.with_ymd_and_hms(2026, 11, 1, 10, 0, 0).unwrap(),
            seerr_updated_at: None,
            seasons: vec![],
            title_id: None,
        }
    }

    #[test]
    fn new_moved_changed_and_gone_requests_are_told_apart() {
        let stored = vec![
            StoredRequest {
                id: 1,
                facts: facts(10, "processing"),
            },
            StoredRequest {
                id: 2,
                facts: facts(11, "processing"),
            },
            StoredRequest {
                id: 3,
                facts: facts(12, "processing"),
            },
            StoredRequest {
                id: 4,
                facts: facts(13, "processing"),
            },
        ];
        let mut linked = facts(11, "processing");
        linked.title_id = Some(77);
        let incoming = vec![
            facts(10, "available"),
            linked,
            facts(12, "processing"),
            facts(14, "pending"),
        ];
        let changes = diff(&stored, &incoming, true);
        assert_eq!(
            changes,
            vec![
                Change::Status {
                    id: 1,
                    new: 0,
                    from: "processing".into()
                },
                Change::Update { id: 2, new: 1 },
                Change::Add(3),
                Change::Remove(4),
            ]
        );
        assert!(!diff(&stored, &incoming, false).contains(&Change::Remove(4)));
    }

    #[test]
    fn a_short_listing_is_implausible() {
        assert!(!implausible(10, 0), "a small site may empty");
        assert!(implausible(1500, 700));
        assert!(!implausible(1500, 800));
    }
}
