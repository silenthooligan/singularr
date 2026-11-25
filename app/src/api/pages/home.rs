//! Home: the state of the server on one page. Storage is left out: the server does not know
//! how much room its disks have.

use super::activity;
use super::calendar;
use super::cards::Cards;
use super::labels;
use super::managers;
use super::model::{
    Added, AddedEpisode, AddedMovie, EpisodeLine, Home, HomeAttention, Playing, PlayingUser,
    QueueCounts, Stat, TitleCard,
};
use super::requests;
use super::workspace;
use crate::api::state::AppState;
use crate::catalog::model::TitleKind;
use crate::catalog::service as catalog;
use crate::catalog::store as catalog_store;
use crate::core::error::AppResult;
use crate::engines::ServerSession;
use crate::engines::service as engines;
use chrono::{Duration, Utc};
use std::collections::HashMap;

/// Titles of each row of what was added.
const ADDED: i64 = 12;
/// Items of the attention list.
const ATTENTION: usize = 20;
/// How long Home waits for the managers' wanted lists.
const WANTED_WAIT: std::time::Duration = std::time::Duration::from_millis(1500);

fn num(n: i64) -> String {
    let digits = n.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// What plays, each with the card of its title when the catalog holds it.
async fn playing(state: &AppState, sessions: &[ServerSession]) -> AppResult<Vec<Playing>> {
    let keys: Vec<String> = sessions
        .iter()
        .filter_map(|s| match s.kind.as_str() {
            "episode" => s.show_key.clone(),
            _ => s.rating_key.clone(),
        })
        .collect();
    let titles: HashMap<String, i64> = catalog_store::server_item_titles(&state.pool, &keys)
        .await?
        .into_iter()
        .collect();
    let ids: Vec<i64> = titles.values().copied().collect();
    let cards = Cards::of_titles(&state.pool, &ids).await?;
    Ok(sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let is_episode = s.kind == "episode";
            let key = if is_episode {
                &s.show_key
            } else {
                &s.rating_key
            };
            let card = key
                .as_ref()
                .and_then(|k| titles.get(k))
                .and_then(|id| cards.card(*id));
            let title = card.unwrap_or_else(|| TitleCard {
                id: None,
                kind: match s.kind.as_str() {
                    "episode" => "show".to_string(),
                    other => other.to_string(),
                },
                title: if is_episode {
                    s.show.clone().unwrap_or_else(|| s.title.clone())
                } else {
                    s.title.clone()
                },
                year: None,
                poster: None,
                rating: None,
                state: None,
            });
            Playing {
                id: format!("s-{}", i + 1),
                title,
                episode: match (is_episode, s.season, s.episode) {
                    (true, Some(season), Some(number)) => Some(EpisodeLine {
                        season,
                        number,
                        title: Some(s.title.clone()),
                    }),
                    _ => None,
                },
                user: s.person.clone().map(|name| PlayingUser { name }),
                player: s.player.clone(),
                progress: s.progress.map(|p| (p * 10.0).round() / 10.0),
                stream: labels::stream(&s.decision),
            }
        })
        .collect())
}

/// What came in last: movies, and episodes (one per file).
async fn added(state: &AppState) -> AppResult<Added> {
    let pool = &state.pool;
    let movies = catalog_store::recent_movies(pool, ADDED).await?;
    let episodes = catalog_store::recent_episodes(pool, ADDED).await?;
    let ids: Vec<i64> = movies
        .iter()
        .map(|(id, _)| *id)
        .chain(episodes.iter().map(|e| e.title_id))
        .collect();
    let cards = Cards::of_titles(pool, &ids).await?;
    Ok(Added {
        movies: movies
            .iter()
            .filter_map(|(id, at)| {
                Some(AddedMovie {
                    card: cards.card(*id)?,
                    added: *at,
                })
            })
            .collect(),
        episodes: episodes
            .iter()
            .filter_map(|e| {
                Some(AddedEpisode {
                    added: e.added,
                    episode: EpisodeLine {
                        season: e.season_number,
                        number: e.episode_number,
                        title: e.name.clone(),
                    },
                    title: cards.card(e.title_id)?,
                })
            })
            .collect(),
    })
}

pub async fn home(state: &AppState) -> AppResult<Home> {
    let pool = &state.pool;
    let today = Utc::now().date_naive();
    let movies = catalog::kind_totals(pool, TitleKind::Movie).await?;
    let shows = catalog::kind_totals(pool, TitleKind::Show).await?;
    let queue = activity::queue(state, false).await?;
    let page = requests::requests(state).await?;
    let integrations = engines::integrations(pool, &state.config, &state.engines.access).await?;
    let sessions = managers::sessions(state).await;
    let wanted = managers::wanted_total(state, WANTED_WAIT).await;
    let upcoming = calendar::items(state, today, today + Duration::days(6)).await?;

    let in_motion: Vec<_> = queue.iter().filter(|q| q.in_motion()).collect();
    let by_kind = |k: &str| in_motion.iter().filter(|q| q.row.kind == k).count() as i64;
    let waiting: Vec<_> = requests::waiting(&page).collect();

    let mut stats = vec![
        Stat {
            id: "movies".to_string(),
            label: "Movies".to_string(),
            value: movies.titles,
            detail: format!("{} with a file", num(movies.with_file)),
        },
        Stat {
            id: "shows".to_string(),
            label: "Shows".to_string(),
            value: shows.titles,
            detail: format!(
                "{} of {} episodes",
                num(shows.episode_files),
                num(shows.episodes)
            ),
        },
    ];
    let bell = workspace::attention(
        &queue,
        &integrations,
        &workspace::down_since(state).await?,
        workspace::run_attention(state).await?,
    );
    let stuck = bell.iter().filter(|a| a.kind == "queue").count();
    stats.push(Stat {
        id: "queue".to_string(),
        label: "Queue".to_string(),
        value: in_motion.len() as i64,
        detail: match stuck {
            0 => "No issues".to_string(),
            1 => "1 issue".to_string(),
            n => format!("{} issues", num(n as i64)),
        },
    });
    stats.push(Stat {
        id: "requests".to_string(),
        label: "Requests".to_string(),
        value: waiting.len() as i64,
        detail: "Pending".to_string(),
    });
    if let Some(w) = wanted {
        stats.push(Stat {
            id: "wanted".to_string(),
            label: "Wanted".to_string(),
            value: w,
            detail: "Missing or below cutoff".to_string(),
        });
    }
    if let Some(list) = sessions.as_ref() {
        stats.push(Stat {
            id: "playing".to_string(),
            label: "Playing now".to_string(),
            value: list.len() as i64,
            detail: "Media server".to_string(),
        });
    }

    let mut attention: Vec<HomeAttention> = waiting
        .iter()
        .map(|r| HomeAttention {
            id: r.id.clone(),
            kind: "request".to_string(),
            title: r.title.title.clone(),
            reason: "Pending approval".to_string(),
            since: Some(r.at),
        })
        .chain(bell.into_iter().map(|a| HomeAttention {
            id: a.id,
            kind: a.kind,
            title: a.title,
            reason: a.detail,
            since: a.since,
        }))
        .collect();
    attention.sort_by(|a, b| b.since.cmp(&a.since).then_with(|| a.id.cmp(&b.id)));
    attention.truncate(ATTENTION);

    Ok(Home {
        stats,
        attention,
        added: added(state).await?,
        upcoming,
        queue: QueueCounts {
            movies: by_kind("movie"),
            shows: by_kind("show"),
        },
        requests: waiting.iter().map(|r| r.id.clone()).collect(),
        now_playing: match sessions.as_ref() {
            Some(list) => playing(state, list).await?,
            None => Vec::new(),
        },
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(super::num(0), "0");
        assert_eq!(super::num(999), "999");
        assert_eq!(super::num(7491), "7,491");
        assert_eq!(super::num(1_234_567), "1,234,567");
    }
}
