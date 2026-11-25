//! The requests asked for on Singularr Requests, as the full process handles them.
//!
//! The face only inserts an intent. This process picks it up (woken by `NOTIFY
//! request_intents`, and by a poll every 15 seconds in case a notice was missed), applies the
//! request rules (the season gate, the owner exemption by Seerr user id) and records what it
//! would send to Seerr, on the requester's behalf (Seerr's `userId`). R1 is shadow: the
//! intent's state becomes `shadow` with the outcome, and nothing reaches Seerr. HD only: what
//! would be sent always says `is4k: false`.

use super::model::RequestIntent;
use super::rules::{RuleInput, Rules, season_gate};
use super::service::rule_input;
use super::store;
use crate::core::error::{AppError, AppResult};
use crate::core::journal::{Journal, event};
use crate::core::model::{Page, Paging};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::time::Duration;
use tokio::task::JoinHandle;

/// The channel the intent trigger notifies.
pub const CHANNEL: &str = "request_intents";

/// The states an intent can be in.
pub const STATES: [&str; 3] = ["new", "shadow", "failed"];

/// What the full process makes of one intent: the season gate, and what it would send.
/// `others` are the stored Seerr requests for the same show (the gate counts the seasons the
/// requester already holds).
pub fn shadow_outcome(intent: &RequestIntent, rules: &Rules, others: &[RuleInput<'_>]) -> Value {
    let input = RuleInput {
        // An intent has no Seerr id yet; a negative one never matches a stored request.
        seerr_id: -intent.id,
        site: &intent.site,
        kind: &intent.kind,
        is_4k: false,
        request_status: "pending",
        media_status: "unknown",
        requested_by: &intent.requested_by,
        requested_by_id: Some(intent.seerr_user_id),
        tmdb_id: Some(intent.tmdb_id),
        created_at: intent.asked_at,
        seasons: &intent.seasons,
    };
    let gate = season_gate(rules, &input, others);
    let would_send = if gate.result == "decline" {
        Value::Null
    } else {
        let mut body = json!({
            "mediaType": if intent.kind == "show" { "tv" } else { "movie" },
            "mediaId": intent.tmdb_id,
            "is4k": false,
            "userId": intent.seerr_user_id,
        });
        if intent.kind == "show" {
            body["seasons"] = json!(gate.approved);
        }
        json!({ "method": "POST", "path": "/api/v1/request", "site": intent.site, "body": body })
    };
    json!({
        "stage": "R1",
        "sent": false,
        "note": "Shadow: what would be sent is recorded; nothing reaches Seerr.",
        "owner": rules.is_owner(Some(intent.seerr_user_id)),
        "gate": gate,
        "would_send": would_send,
    })
}

/// Handle every intent that is still `new`. Returns how many were handled.
pub async fn process_new(
    pool: &PgPool,
    journal: &Journal,
    rules: &Rules,
) -> Result<usize, sqlx::Error> {
    let mut handled = 0;
    loop {
        let batch = store::new_intents(pool, 50).await?;
        if batch.is_empty() {
            return Ok(handled);
        }
        for intent in batch {
            let stored = if intent.kind == "show" {
                store::show_requests(pool, &[intent.tmdb_id]).await?
            } else {
                Vec::new()
            };
            let others: Vec<RuleInput<'_>> = stored.iter().map(|s| rule_input(&s.facts)).collect();
            let outcome = shadow_outcome(&intent, rules, &others);
            let mut tx = journal.begin(pool).await?;
            if store::finish_intent(tx.conn(), intent.id, "shadow", &outcome).await? {
                tx.append_one(event(
                    "request_intent_shadowed",
                    "request_intent",
                    intent.id,
                    "requests:shadow",
                    json!({
                        "intent_id": intent.id,
                        "site": intent.site,
                        "requested_by": intent.requested_by,
                        "kind": intent.kind,
                        "tmdb_id": intent.tmdb_id,
                        "title": intent.title,
                        "seasons": intent.seasons,
                        "gate": outcome["gate"]["result"],
                        "would_send": outcome["would_send"],
                    }),
                ))
                .await?;
            }
            tx.commit().await?;
            handled += 1;
        }
    }
}

/// The intents loop of the full process: listen, poll, handle. Stop it by aborting the handle.
pub fn spawn(pool: PgPool, journal: Journal, rules: Rules) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut listener = match sqlx::postgres::PgListener::connect_with(&pool).await {
            Ok(mut l) => match l.listen(CHANNEL).await {
                Ok(()) => Some(l),
                Err(e) => {
                    tracing::warn!(error = %e, "LISTEN request_intents failed; polling only");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(error = %e, "no listener connection; polling intents only");
                None
            }
        };
        loop {
            match process_new(&pool, &journal, &rules).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(intents = n, "request intents recorded in shadow"),
                Err(e) => tracing::warn!(error = %e, "request intents were not handled"),
            }
            match listener.as_mut() {
                Some(l) => {
                    let _ = tokio::time::timeout(Duration::from_secs(15), l.recv()).await;
                }
                None => tokio::time::sleep(Duration::from_secs(15)).await,
            }
        }
    })
}

/// The intents, newest first, for the owner's Requests page and `GET /api/v1/requests/intents`.
pub async fn list(
    pool: &PgPool,
    state: Option<&str>,
    page: Option<u32>,
    per_page: Option<u32>,
) -> AppResult<Page<RequestIntent>> {
    let state = state.map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = state
        && !STATES.contains(&s)
    {
        return Err(AppError::bad_request(format!(
            "state is one of new, shadow, failed, not `{s}`"
        )));
    }
    let paging = Paging::new(page, per_page);
    let (items, total) = store::list_intents(pool, state, paging).await?;
    Ok(Page::new(items, paging, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn rules() -> Rules {
        Rules {
            owners: vec![1],
            max_seasons: 2,
            season_gate: true,
        }
    }

    fn intent(kind: &str, seasons: &[i32], user: i64) -> RequestIntent {
        RequestIntent {
            id: 7,
            site: "plex".to_string(),
            requested_by: "Friend".to_string(),
            seerr_user_id: user,
            kind: kind.to_string(),
            tmdb_id: 1396,
            seasons: seasons.to_vec(),
            title: Some("A Show".to_string()),
            year: Some(2008),
            asked_at: Utc.with_ymd_and_hms(2026, 11, 22, 12, 0, 0).unwrap(),
            state: "new".to_string(),
            processed_at: None,
            outcome: None,
        }
    }

    #[test]
    fn a_movie_would_go_out_as_the_requester_in_hd() {
        let o = shadow_outcome(&intent("movie", &[], 7), &rules(), &[]);
        assert_eq!(o["sent"], false);
        assert_eq!(o["gate"]["result"], "not_gated");
        let body = &o["would_send"]["body"];
        assert_eq!(
            (
                body["mediaType"].as_str(),
                body["mediaId"].as_i64(),
                body["userId"].as_i64()
            ),
            (Some("movie"), Some(1396), Some(7))
        );
        assert_eq!(body["is4k"], false);
        assert!(body.get("seasons").is_none());
    }

    #[test]
    fn the_season_gate_trims_a_friend_and_spares_the_owner() {
        let friend = shadow_outcome(&intent("show", &[1, 2, 3], 7), &rules(), &[]);
        assert_eq!(friend["gate"]["result"], "trim");
        assert_eq!(friend["would_send"]["body"]["seasons"], json!([1, 2]));
        assert_eq!(friend["owner"], false);
        let owner = shadow_outcome(&intent("show", &[1, 2, 3], 1), &rules(), &[]);
        assert_eq!(owner["gate"]["result"], "exempt");
        assert_eq!(owner["would_send"]["body"]["seasons"], json!([1, 2, 3]));
        assert_eq!(owner["owner"], true);
        // Seasons the friend already holds through Seerr count toward the limit.
        let held = [1, 2];
        let earlier = RuleInput {
            seerr_id: 40,
            site: "plex",
            kind: "show",
            is_4k: false,
            request_status: "approved",
            media_status: "processing",
            requested_by: "Friend",
            requested_by_id: Some(7),
            tmdb_id: Some(1396),
            created_at: Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap(),
            seasons: &held,
        };
        let full = shadow_outcome(&intent("show", &[3], 7), &rules(), &[earlier]);
        assert_eq!(full["gate"]["result"], "decline");
        assert!(full["would_send"].is_null(), "nothing would be sent");
    }
}
