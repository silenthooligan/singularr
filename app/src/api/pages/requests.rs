//! The Requests page: every request the request site holds (synced by `requests`), what was
//! asked, who asked and where it is, in the client's words; and the people who ask.

use super::cards::Cards;
use super::labels;
use super::model::{By, Person, RequestItem, RequestsPage, RuleLine, SeasonCount, TitleCard, What};
use crate::api::state::AppState;
use crate::core::error::AppResult;
use crate::engines::model::SITE;
use crate::lifecycle::store as downloads_store;
use crate::requests::model::SeasonGate;
use crate::requests::rules::{self as request_rules, RuleInput, Rules};
use crate::requests::service::rule_input;
use crate::requests::store::{self as requests_store, RequestRow, SiteAccount};
use std::collections::{BTreeMap, HashMap};

/// Requests the page gets, newest first.
pub const REQUEST_ROWS: i64 = 5000;

/// The season gate as a rule line; `None` for a request it does not look at (a movie).
fn gate_line(gate: &SeasonGate, kind: &str) -> Option<RuleLine> {
    (kind == "show").then(|| RuleLine {
        label: "Season limit".to_string(),
        result: labels::gate_result(&gate.result).to_string(),
        detail: gate.reason.clone(),
    })
}

/// What was asked for: the movie, or the seasons.
pub fn what(kind: &str, seasons: &[i32]) -> What {
    if kind == "movie" {
        What {
            kind: "movie".to_string(),
            seasons: None,
        }
    } else {
        let mut s = seasons.to_vec();
        s.sort_unstable();
        s.dedup();
        What {
            kind: "seasons".to_string(),
            seasons: Some(s),
        }
    }
}

/// The people who can ask (the site's accounts) and who did, with their numbers of requests,
/// most requests first.
fn people(rows: &[RequestRow], accounts: &[SiteAccount], rules: &Rules) -> Vec<Person> {
    let mut by_id: BTreeMap<i64, (String, i64)> = accounts
        .iter()
        .map(|a| (a.seerr_user_id, (a.display_name.clone(), 0)))
        .collect();
    for r in rows {
        if let Some(id) = r.facts.requested_by_id {
            by_id
                .entry(id)
                .or_insert_with(|| (r.facts.requested_by.clone(), 0))
                .1 += 1;
        }
    }
    let mut out: Vec<Person> = by_id
        .into_iter()
        .map(|(id, (name, requests))| {
            let owner = rules.is_owner(Some(id))
                || accounts.iter().any(|a| a.seerr_user_id == id && a.is_owner);
            Person {
                id: format!("u-{id}"),
                name,
                role: if owner { "owner" } else { "member" }.to_string(),
                requests,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.requests
            .cmp(&a.requests)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

pub async fn requests(state: &AppState) -> AppResult<RequestsPage> {
    let pool = &state.pool;
    let gate_rules = &state.requests.rules;
    let rows = requests_store::newest(pool, REQUEST_ROWS).await?;
    let accounts = requests_store::site_accounts(pool, SITE).await?;
    let title_ids: Vec<i64> = rows.iter().filter_map(|r| r.facts.title_id).collect();
    let cards = Cards::of_titles(pool, &title_ids).await?;
    // The downloads in motion of each requested title.
    let mut queue: HashMap<i64, Vec<(String, f64)>> = HashMap::new();
    for d in downloads_store::all(pool).await? {
        if let Some(t) = d.title_id
            && (d.progress < 1.0 || d.queue_state.is_some() || d.stuck)
        {
            queue
                .entry(t)
                .or_default()
                .push((format!("q-{}", d.hash.to_ascii_lowercase()), d.progress));
        }
    }
    // The season gate counts the requests for the same show, as the request service does.
    let inputs: Vec<RuleInput<'_>> = rows.iter().map(|r| rule_input(&r.facts)).collect();
    let mut shows: HashMap<i64, Vec<RuleInput<'_>>> = HashMap::new();
    for i in inputs.iter().filter(|i| i.kind == "show") {
        if let Some(t) = i.tmdb_id {
            shows.entry(t).or_default().push(i.clone());
        }
    }
    let mut out = Vec::with_capacity(rows.len());
    for (r, me) in rows.iter().zip(&inputs) {
        let f = &r.facts;
        let others: &[RuleInput<'_>] = match f.tmdb_id.and_then(|t| shows.get(&t)) {
            Some(list) if f.kind == "show" => list.as_slice(),
            _ => &[],
        };
        let gate = request_rules::season_gate(gate_rules, me, others);
        let cleanup = request_rules::cleanup(gate_rules, me);
        let motion = f.title_id.map(|t| cards.motion(t)).unwrap_or_default();
        let status = labels::request_status(&f.request_status, &f.media_status, motion);
        let catalog = f.title_id.and_then(|t| cards.card(t));
        let open = matches!(status, "requested" | "pending" | "approved");
        let title = TitleCard {
            id: labels::title_id(&f.kind, f.tmdb_id),
            kind: f.kind.clone(),
            title: r
                .title
                .clone()
                .or_else(|| catalog.as_ref().map(|c| c.title.clone()))
                .unwrap_or_else(|| "Unknown title".to_string()),
            year: r.year.or_else(|| catalog.as_ref().and_then(|c| c.year)),
            poster: r
                .poster_path
                .clone()
                .or_else(|| catalog.as_ref().and_then(|c| c.poster.clone())),
            rating: catalog.as_ref().and_then(|c| c.rating),
            state: catalog
                .as_ref()
                .and_then(|c| c.state.clone())
                .or_else(|| open.then(|| "requested".to_string())),
        };
        let downloads = f
            .title_id
            .and_then(|t| queue.get(&t))
            .cloned()
            .unwrap_or_default();
        let progress = (status == "downloading")
            .then(|| downloads.iter().map(|(_, p)| *p).reduce(f64::min))
            .flatten()
            .map(|p| (p * 1000.0).round() / 10.0);
        let note = if matches!(gate.result.as_str(), "trim" | "decline") {
            Some(gate.reason.clone())
        } else if cleanup.applies {
            Some(cleanup.reason.clone())
        } else {
            None
        };
        out.push(RequestItem {
            id: format!("r-{}", r.id),
            title,
            what: what(&f.kind, &f.seasons),
            target: if f.is_4k { "4K" } else { "HD" }.to_string(),
            by: By {
                id: f.requested_by_id.map(|id| format!("u-{id}")),
                name: f.requested_by.clone(),
            },
            at: f.requested_at,
            status: status.to_string(),
            progress,
            note,
            rules: gate_line(&gate, &f.kind).into_iter().collect(),
            counts: (f.kind == "show").then_some(SeasonCount {
                seasons: f.seasons.len() as i64,
            }),
            queue: downloads.into_iter().map(|(id, _)| id).collect(),
        });
    }
    Ok(RequestsPage {
        people: people(&rows, &accounts, gate_rules),
        requests: out,
    })
}

/// Requests still waiting for a decision, newest first.
pub fn waiting(page: &RequestsPage) -> impl Iterator<Item = &RequestItem> {
    page.requests.iter().filter(|r| r.status == "requested")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_asked_is_a_movie_or_seasons() {
        assert_eq!(
            serde_json::to_value(what("movie", &[])).unwrap(),
            serde_json::json!({"type": "movie"})
        );
        assert_eq!(
            serde_json::to_value(what("show", &[3, 1, 1])).unwrap(),
            serde_json::json!({"type": "seasons", "seasons": [1, 3]})
        );
    }
}
