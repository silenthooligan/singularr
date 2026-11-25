//! The request rules Singularr shows: Seerr's codes as words, the one-word status, the season
//! gate (seerr-bridge) and the friend-request cleanup (Maintainerr). Pure functions over plain
//! values, so they are tested without a database or an engine. Singularr applies none of these
//! rules in stage 1; it says what they do.
//!
//! The gate, as seerr-bridge's `decide` and `held_seasons` have it: a friend holds at most
//! `max_seasons` seasons of one show at a time, counted over their other requests for the show
//! (pending or approved, and completed ones whose title is still in the library). Seasons that
//! fit are approved, lowest first; a request with some that fit is trimmed to them; one with
//! none is declined. The owner, movies and 4K requests are not gated.
//!
//! The cleanup, as Maintainerr's three collections have it: a season a friend asked for leaves
//! 3 days after it is watched to the end, or after 120 days with no play (removed 7 days after
//! it enters that collection). Only TV seasons; the owner's requests never match.

use super::model::{Cleanup, SeasonGate};
use chrono::{DateTime, Duration, Utc};

pub const WATCHED_DAYS: i32 = 3;
pub const UNPLAYED_DAYS: i32 = 120;
pub const UNPLAYED_REMOVAL_DAYS: i32 = 7;

/// The words of the one-word status, in the order the Requests page shows them.
pub const STATUSES: [&str; 8] = [
    "pending",
    "approved",
    "processing",
    "available",
    "removed",
    "declined",
    "failed",
    "unknown",
];

/// Seerr's request status code as a word.
pub fn request_status(code: i32) -> &'static str {
    match code {
        1 => "pending",
        2 => "approved",
        3 => "declined",
        4 => "failed",
        5 => "completed",
        _ => "unknown",
    }
}

/// Seerr's media status code as a word.
pub fn media_status(code: i32) -> &'static str {
    match code {
        1 => "unknown",
        2 => "pending",
        3 => "processing",
        4 => "partially_available",
        5 => "available",
        6 => "blocklisted",
        7 => "deleted",
        _ => "unknown",
    }
}

/// The one word for a request, from Seerr's two codes.
///
/// - Pending, declined and failed requests say so.
/// - An approved request is `available` when its title is, `processing` while something of
///   it is on the way (a show partly in, or a title Seerr marked deleted and asked for
///   again), and `approved` before anything happened.
/// - A completed request is `available` while its title is in the library, `processing` when
///   it is being fetched again, and `removed` once the title has left.
pub fn status_word(request: i32, media: i32) -> &'static str {
    match request {
        1 => "pending",
        3 => "declined",
        4 => "failed",
        2 => match media {
            5 => "available",
            3 | 4 | 7 => "processing",
            _ => "approved",
        },
        5 => match media {
            4 | 5 => "available",
            2 | 3 => "processing",
            _ => "removed",
        },
        _ => "unknown",
    }
}

/// The rules' settings (from the configuration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    /// The owner's Seerr accounts, by Seerr user id.
    pub owners: Vec<i64>,
    pub max_seasons: usize,
    /// Whether the season gate and the cleanup act.
    pub season_gate: bool,
}

impl Rules {
    /// Whether this Seerr account is one of the owner's. By Seerr user id, as seerr-bridge
    /// does (`SEERR_EXEMPT_USER_IDS`): any user can change their own display name, so a name
    /// would let a friend take the owner's exemption.
    pub fn is_owner(&self, user_id: Option<i64>) -> bool {
        user_id.is_some_and(|id| self.owners.contains(&id))
    }

    /// The request sites the gate acts on, as the portal's settings state them: the one site
    /// when the gate is on, none when it is off.
    pub fn gated_sites(&self) -> Vec<String> {
        if self.season_gate {
            vec![crate::engines::model::SITE.to_string()]
        } else {
            Vec::new()
        }
    }
}

/// What the rules read of one request. Statuses are the words of [`request_status`] and
/// [`media_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInput<'a> {
    pub seerr_id: i64,
    pub site: &'a str,
    /// `movie` or `show`.
    pub kind: &'a str,
    pub is_4k: bool,
    pub request_status: &'a str,
    pub media_status: &'a str,
    pub requested_by: &'a str,
    pub requested_by_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub seasons: &'a [i32],
}

fn same_requester(a: &RuleInput<'_>, b: &RuleInput<'_>) -> bool {
    match (a.requested_by_id, b.requested_by_id) {
        (Some(x), Some(y)) => x == y,
        _ => a.requested_by.eq_ignore_ascii_case(b.requested_by),
    }
}

/// Seasons of the same show the same requester held when this request was made: their
/// earlier requests on the same site, pending or approved, or completed while the title is
/// still in the library. Declined and failed requests hold nothing.
pub fn held_seasons(req: &RuleInput<'_>, others: &[RuleInput<'_>]) -> Vec<i32> {
    let Some(tmdb) = req.tmdb_id else {
        return Vec::new();
    };
    let mut held: Vec<i32> = others
        .iter()
        .filter(|o| {
            o.seerr_id != req.seerr_id
                && o.site == req.site
                && o.kind == "show"
                && !o.is_4k
                && o.tmdb_id == Some(tmdb)
                && o.created_at < req.created_at
                && same_requester(o, req)
                && match o.request_status {
                    "pending" | "approved" => true,
                    "completed" => matches!(
                        o.media_status,
                        "pending" | "processing" | "partially_available" | "available"
                    ),
                    _ => false,
                }
        })
        .flat_map(|o| o.seasons.iter().copied())
        .collect();
    held.sort_unstable();
    held.dedup();
    held
}

/// `season 3`, `seasons 1 and 2`, `seasons 1, 2 and 4`.
pub fn seasons_text(seasons: &[i32]) -> String {
    let n: Vec<String> = seasons.iter().map(|s| s.to_string()).collect();
    match n.as_slice() {
        [] => "no season".to_string(),
        [one] => format!("season {one}"),
        [rest @ .., last] => format!("seasons {} and {last}", rest.join(", ")),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// What the season gate makes of a request.
pub fn season_gate(rules: &Rules, req: &RuleInput<'_>, others: &[RuleInput<'_>]) -> SeasonGate {
    let mut asked = req.seasons.to_vec();
    asked.sort_unstable();
    asked.dedup();
    let limit = rules.max_seasons as i32;
    let not = |result: &str, reason: String| SeasonGate {
        result: result.to_string(),
        reason,
        limit,
        asked: asked.clone(),
        held: Vec::new(),
        approved: asked.clone(),
        declined: Vec::new(),
    };
    if req.kind != "show" {
        return not("not_gated", "Movies are not limited".to_string());
    }
    if req.is_4k {
        return not("not_gated", "4K requests are not limited".to_string());
    }
    if !rules.season_gate {
        return not("not_gated", "Season limit off".to_string());
    }
    if rules.is_owner(req.requested_by_id) {
        return not("exempt", "Owner account, exempt".to_string());
    }
    let held = held_seasons(req, others);
    let room = rules.max_seasons.saturating_sub(held.len());
    let approved: Vec<i32> = asked
        .iter()
        .filter(|n| !held.contains(n))
        .take(room)
        .copied()
        .collect();
    let declined: Vec<i32> = asked
        .iter()
        .filter(|n| !approved.contains(n))
        .copied()
        .collect();
    let held_part = if held.is_empty() {
        String::new()
    } else {
        format!(", {} already held", plural(held.len(), "season", "seasons"))
    };
    let (result, reason) = if declined.is_empty() {
        (
            "approve",
            format!(
                "{} requested{held_part}, within the limit of {limit}",
                plural(asked.len(), "season", "seasons")
            ),
        )
    } else if !approved.is_empty() {
        (
            "trim",
            format!(
                "{} requested{held_part}, limit {limit}: {} approved, {} declined",
                plural(asked.len(), "season", "seasons"),
                seasons_text(&approved),
                seasons_text(&declined),
            ),
        )
    } else {
        (
            "decline",
            format!(
                "{} already held ({}), limit {limit}: {} declined",
                plural(held.len(), "season", "seasons"),
                seasons_text(&held),
                seasons_text(&declined),
            ),
        )
    };
    SeasonGate {
        result: result.to_string(),
        reason,
        limit,
        asked,
        held,
        approved,
        declined,
    }
}

/// Whether the friend-request cleanup applies to a request, and the dates it gives.
pub fn cleanup(rules: &Rules, req: &RuleInput<'_>) -> Cleanup {
    let base = |applies: bool, reason: &str| Cleanup {
        applies,
        reason: reason.to_string(),
        watched_days: WATCHED_DAYS,
        unplayed_days: UNPLAYED_DAYS,
        unplayed_removal_days: UNPLAYED_REMOVAL_DAYS,
        plays_known: false,
        counts_from: None,
        unplayed_from: None,
        leaves_if_unplayed: None,
    };
    if req.kind != "show" {
        return base(false, "Retention applies to seasons only");
    }
    if !rules.season_gate {
        return base(false, "Retention off with the season limit");
    }
    if rules.is_owner(req.requested_by_id) {
        return base(false, "Owner requests are kept");
    }
    match req.request_status {
        "approved" | "completed" => {}
        "pending" => return base(false, "Nothing in the library"),
        _ => {
            return base(false, "Request not approved");
        }
    }
    if req.request_status == "completed"
        && matches!(req.media_status, "deleted" | "unknown" | "blocklisted")
    {
        return base(false, "Seasons left the library");
    }
    let mut c = base(
        true,
        &format!(
            "Leaves {WATCHED_DAYS} d after watched, or {UNPLAYED_DAYS} d unplayed plus {UNPLAYED_REMOVAL_DAYS} d; counted from the request"
        ),
    );
    let from = req.created_at;
    let unplayed = from + Duration::days(UNPLAYED_DAYS as i64);
    c.counts_from = Some(from);
    c.unplayed_from = Some(unplayed);
    c.leaves_if_unplayed = Some(unplayed + Duration::days(UNPLAYED_REMOVAL_DAYS as i64));
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn rules() -> Rules {
        Rules {
            owners: vec![1],
            max_seasons: 2,
            season_gate: true,
        }
    }

    fn at(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, 12, 0, 0).unwrap()
    }

    fn tv<'a>(
        id: i64,
        who: &'a str,
        seasons: &'a [i32],
        status: &'a str,
        day: u32,
    ) -> RuleInput<'a> {
        RuleInput {
            seerr_id: id,
            site: "plex",
            kind: "show",
            is_4k: false,
            request_status: status,
            media_status: "processing",
            requested_by: who,
            // The owner is user 1 on the site; friends get an id from their name.
            requested_by_id: Some(if who.eq_ignore_ascii_case("homeowner") {
                1
            } else {
                10 + who.len() as i64
            }),
            tmdb_id: Some(42009),
            created_at: at(day),
            seasons,
        }
    }

    #[test]
    fn seerr_codes_read_as_words() {
        assert_eq!(request_status(5), "completed");
        assert_eq!(media_status(4), "partially_available");
        assert_eq!(media_status(99), "unknown");
        assert_eq!(status_word(1, 2), "pending");
        assert_eq!(status_word(2, 4), "processing");
        assert_eq!(status_word(2, 1), "approved");
        assert_eq!(status_word(2, 5), "available");
        assert_eq!(status_word(5, 5), "available");
        assert_eq!(status_word(5, 7), "removed");
        assert_eq!(status_word(5, 1), "removed");
        assert_eq!(status_word(3, 1), "declined");
        assert_eq!(status_word(0, 1), "unknown");
        assert!(STATUSES.contains(&status_word(4, 1)));
    }

    #[test]
    fn a_friend_asking_four_seasons_gets_the_lowest_two() {
        let r = tv(1, "maple.reed", &[4, 1, 3, 2], "pending", 22);
        let g = season_gate(&rules(), &r, &[]);
        assert_eq!(g.result, "trim");
        assert_eq!(
            (g.approved.clone(), g.declined.clone()),
            (vec![1, 2], vec![3, 4])
        );
        assert_eq!(
            g.reason,
            "4 seasons requested, limit 2: seasons 1 and 2 approved, seasons 3 and 4 declined"
        );
        let two = tv(2, "maple.reed", &[1, 2], "pending", 22);
        assert_eq!(season_gate(&rules(), &two, &[]).result, "approve");
    }

    #[test]
    fn earlier_requests_use_up_the_allowance() {
        let earlier = tv(1, "maple.reed", &[1, 2], "approved", 1);
        let later = tv(2, "maple.reed", &[3], "pending", 20);
        let g = season_gate(&rules(), &later, &[earlier.clone(), later.clone()]);
        assert_eq!(g.result, "decline");
        assert_eq!(g.held, vec![1, 2]);
        assert_eq!(g.declined, vec![3]);
        assert!(
            g.reason
                .starts_with("2 seasons already held (seasons 1 and 2), limit 2")
        );

        // One held: one more fits. A held season asked again is declined, as the bridge does.
        let one = tv(3, "maple.reed", &[1], "completed", 1);
        let next = tv(4, "maple.reed", &[1, 2, 3], "pending", 20);
        let g = season_gate(&rules(), &next, &[one]);
        assert_eq!((g.result.as_str(), g.approved.clone()), ("trim", vec![2]));
        assert_eq!(g.declined, vec![1, 3]);

        // Declined requests, other people, later requests and a title that left hold nothing.
        let declined = tv(5, "maple.reed", &[1, 2], "declined", 1);
        let other = tv(6, "jun.park", &[1, 2], "approved", 1);
        let after = tv(7, "maple.reed", &[1, 2], "approved", 25);
        let mut gone = tv(8, "maple.reed", &[1, 2], "completed", 1);
        gone.media_status = "deleted";
        let g = season_gate(&rules(), &next, &[declined, other, after, gone]);
        assert!(g.held.is_empty());
        assert_eq!(g.approved, vec![1, 2]);
    }

    #[test]
    fn the_owner_movies_4k_and_a_gate_that_is_off_do_not_gate() {
        let owner = tv(1, "HomeOwner", &[1, 2, 3, 4, 5], "pending", 22);
        let g = season_gate(&rules(), &owner, &[]);
        assert_eq!((g.result.as_str(), g.approved.len()), ("exempt", 5));
        // A friend who names themselves after the owner is still a friend: the id decides.
        let mut namesake = tv(9, "homeowner", &[1, 2, 3], "pending", 22);
        namesake.requested_by_id = Some(7);
        assert_eq!(season_gate(&rules(), &namesake, &[]).result, "trim");
        let mut namesake_in = namesake.clone();
        namesake_in.request_status = "completed";
        namesake_in.media_status = "available";
        assert!(cleanup(&rules(), &namesake_in).applies);
        let mut no_id = tv(10, "homeowner", &[1, 2, 3], "pending", 22);
        no_id.requested_by_id = None;
        assert_eq!(season_gate(&rules(), &no_id, &[]).result, "trim");
        let mut movie = tv(2, "maple.reed", &[], "pending", 22);
        movie.kind = "movie";
        assert_eq!(season_gate(&rules(), &movie, &[]).result, "not_gated");
        let mut uhd = tv(3, "maple.reed", &[1, 2, 3], "pending", 22);
        uhd.is_4k = true;
        assert_eq!(season_gate(&rules(), &uhd, &[]).result, "not_gated");
        let off = Rules {
            season_gate: false,
            ..rules()
        };
        let friend = tv(4, "maple.reed", &[1, 2, 3], "pending", 22);
        let g = season_gate(&off, &friend, &[]);
        assert_eq!(g.result, "not_gated");
        assert!(g.reason.contains("off"));
        assert!(off.gated_sites().is_empty() && rules().gated_sites() == vec!["plex"]);
    }

    #[test]
    fn the_cleanup_gives_dates_from_the_request() {
        let r = tv(1, "maple.reed", &[1], "completed", 1);
        let mut available = r.clone();
        available.media_status = "available";
        let c = cleanup(&rules(), &available);
        assert!(c.applies && !c.plays_known);
        assert_eq!(c.counts_from, Some(at(1)));
        assert_eq!(
            c.unplayed_from,
            Some(Utc.with_ymd_and_hms(2027, 3, 1, 12, 0, 0).unwrap())
        );
        assert_eq!(
            c.leaves_if_unplayed,
            Some(Utc.with_ymd_and_hms(2027, 3, 8, 12, 0, 0).unwrap())
        );
        assert_eq!((c.watched_days, c.unplayed_days), (3, 120));

        let owner = tv(2, "homeowner", &[1], "completed", 1);
        assert!(!cleanup(&rules(), &owner).applies);
        let pending = tv(3, "maple.reed", &[1], "pending", 1);
        assert!(!cleanup(&rules(), &pending).applies);
        let mut gone = r.clone();
        gone.media_status = "deleted";
        assert_eq!(cleanup(&rules(), &gone).reason, "Seasons left the library");
        let mut movie = r.clone();
        movie.kind = "movie";
        assert!(!cleanup(&rules(), &movie).applies);
        assert_eq!(cleanup(&rules(), &movie).leaves_if_unplayed, None);
    }

    #[test]
    fn season_lists_read_as_text() {
        assert_eq!(seasons_text(&[]), "no season");
        assert_eq!(seasons_text(&[3]), "season 3");
        assert_eq!(seasons_text(&[1, 2]), "seasons 1 and 2");
        assert_eq!(seasons_text(&[1, 2, 4]), "seasons 1, 2 and 4");
    }
}
