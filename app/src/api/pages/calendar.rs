//! The calendar: episodes airing and movies releasing, from the managers' own calendars, each
//! with the title's card and its state.

use super::cards::Cards;
use super::labels;
use super::managers;
use super::model::{Calendar, CalendarItem, EpisodeLine};
use crate::api::state::AppState;
use crate::core::error::{AppError, AppResult};
use chrono::{Duration, NaiveDate, Utc};
use serde::Deserialize;
use utoipa::IntoParams;

/// The longest span one answer covers.
pub const MAX_DAYS: i64 = 92;

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CalendarQuery {
    /// The first day, `2026-11-16`. Default: the Monday of last week.
    pub from: Option<String>,
    /// The last day, included. Default: four weeks after `from`, less a day.
    pub to: Option<String>,
}

fn parse_day(name: &str, text: Option<&str>) -> AppResult<Option<NaiveDate>> {
    match text.map(str::trim).filter(|t| !t.is_empty()) {
        None => Ok(None),
        Some(t) => NaiveDate::parse_from_str(t, "%Y-%m-%d")
            .map(Some)
            .map_err(|_| {
                AppError::bad_request(format!("`{name}` is a day written 2026-11-23, not `{t}`"))
            }),
    }
}

/// The days asked for, checked: `from` before `to`, at most [`MAX_DAYS`] days.
pub fn span(q: &CalendarQuery, today: NaiveDate) -> AppResult<(NaiveDate, NaiveDate)> {
    let (default_from, default_to) = labels::default_range(today);
    let from = parse_day("from", q.from.as_deref())?;
    let to = parse_day("to", q.to.as_deref())?;
    let (from, to) = match (from, to) {
        (None, None) => (default_from, default_to),
        (Some(f), None) => (f, f + Duration::days(27)),
        (None, Some(t)) => (t - Duration::days(27), t),
        (Some(f), Some(t)) => (f, t),
    };
    if to < from {
        return Err(AppError::bad_request("`to` is before `from`"));
    }
    if (to - from).num_days() >= MAX_DAYS {
        return Err(AppError::bad_request(format!(
            "one answer covers at most {MAX_DAYS} days"
        )));
    }
    Ok((from, to))
}

/// The managers' calendars over these days, day by day.
pub async fn items(
    state: &AppState,
    from: NaiveDate,
    to: NaiveDate,
) -> AppResult<Vec<CalendarItem>> {
    let lists = managers::calendars(state, from, to).await;
    let wanted: Vec<(String, Vec<i64>)> = lists
        .iter()
        .map(|l| {
            (
                l.engine.clone(),
                l.entries.iter().map(|e| e.engine_title_id).collect(),
            )
        })
        .collect();
    let cards = Cards::of_engine_titles(&state.pool, &wanted).await?;
    let now = Utc::now();
    let today = now.date_naive();
    let mut out = Vec::new();
    for list in lists.iter() {
        let kind = list.kind.as_str();
        for e in &list.entries {
            let title_id = cards.title_of(&list.engine, e.engine_title_id);
            let motion = title_id.map(|t| cards.motion(t)).unwrap_or_default();
            let state = match (&e.episode, e.release) {
                (Some(_), _) => labels::episode_state(e.has_file, motion, e.airs_at, e.date, now),
                (None, Some(release)) => {
                    labels::release_state(e.has_file, motion, release, e.date, today)
                }
                (None, None) => continue,
            };
            out.push(CalendarItem {
                date: e.date,
                time: e.airs_at,
                kind: kind.to_string(),
                title: cards.card_for(&list.engine, e.engine_title_id, kind, "Unknown title"),
                episode: e.episode.as_ref().map(|x| EpisodeLine {
                    season: x.season,
                    number: x.number,
                    title: x.title.clone(),
                }),
                release: e.release.map(str::to_string),
                state: state.to_string(),
            });
        }
    }
    out.sort_by(|a, b| {
        a.date
            .cmp(&b.date)
            .then_with(|| a.time.cmp(&b.time))
            .then_with(|| a.title.title.cmp(&b.title.title))
    });
    Ok(out)
}

pub async fn calendar(state: &AppState, q: &CalendarQuery) -> AppResult<Calendar> {
    let today = Utc::now().date_naive();
    let (from, to) = span(q, today)?;
    Ok(Calendar {
        from,
        to,
        today,
        items: items(state, from, to).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_span_is_checked() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 23).unwrap();
        let q = |from: Option<&str>, to: Option<&str>| CalendarQuery {
            from: from.map(str::to_string),
            to: to.map(str::to_string),
        };
        assert_eq!(
            span(&q(None, None), today).unwrap(),
            labels::default_range(today)
        );
        let (f, t) = span(&q(Some("2026-12-01"), None), today).unwrap();
        assert_eq!((t - f).num_days(), 27);
        assert!(span(&q(Some("2026-12-01"), Some("2026-11-01")), today).is_err());
        assert!(span(&q(Some("2026-01-01"), Some("2026-12-31")), today).is_err());
        assert!(span(&q(Some("23 Nov"), None), today).is_err());
    }
}
