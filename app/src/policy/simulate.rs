//! Replay the stored copies under a proposed policy. Pure: nothing is read or written here,
//! the caller supplies the copies. The comparison itself is `singularr_core::policy::simulate`;
//! this file feeds it, spreads the work over the machine's cores and counts.

use super::model::{LaneCount, ScoreChange, TraitChange};
use singularr_core::policy::simulate as core_simulate;
use singularr_core::{EvalContext, ParsedRelease, Policy, SimulatedChange};
use std::collections::BTreeMap;

/// One stored copy: the release as it is scored, and what is known beyond its name.
#[derive(Debug, Clone)]
pub struct SimInput {
    pub copy_id: i64,
    pub title_id: i64,
    pub title: String,
    pub library: String,
    pub lane: Option<String>,
    pub release: ParsedRelease,
    pub ctx: EvalContext,
}

#[derive(Debug, Clone, Default)]
pub struct SimOutcome {
    pub copies_evaluated: i64,
    pub copies_without_lane: i64,
    pub changed: i64,
    pub raised: i64,
    pub lowered: i64,
    pub rank_changed: i64,
    pub by_lane: Vec<LaneCount>,
    /// Copies whose score changes, largest change first.
    pub changes: Vec<ScoreChange>,
}

/// Score every input under both policies and report what differs. The work is spread over
/// the machine's cores; call it from a blocking thread.
pub fn simulate(active: &Policy, proposed: &Policy, inputs: &[SimInput]) -> SimOutcome {
    let mut out = SimOutcome {
        copies_evaluated: inputs.len() as i64,
        ..SimOutcome::default()
    };
    let mut lanes: BTreeMap<String, LaneCount> = BTreeMap::new();
    // The core takes (release, context, lane); `origin` leads each item back to its copy.
    let mut origin: Vec<usize> = Vec::new();
    let mut items: Vec<(ParsedRelease, EvalContext, String)> = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let Some(lane) = &input.lane else {
            out.copies_without_lane += 1;
            continue;
        };
        lanes
            .entry(lane.clone())
            .or_insert_with(|| LaneCount {
                lane: lane.clone(),
                ..LaneCount::default()
            })
            .copies += 1;
        origin.push(index);
        items.push((input.release.clone(), input.ctx.clone(), lane.clone()));
    }

    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .clamp(1, 8);
    let chunk = items.len().div_ceil(workers).max(1);
    let found: Vec<SimulatedChange> = std::thread::scope(|scope| {
        let handles: Vec<_> = items
            .chunks(chunk)
            .enumerate()
            .map(|(n, part)| {
                scope.spawn(move || {
                    let mut changes = core_simulate(active, proposed, part);
                    for c in &mut changes {
                        c.index += n * chunk;
                    }
                    changes
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a simulation worker panicked"))
            .collect()
    });

    for change in found {
        let input = &inputs[origin[change.index]];
        if change.rank_before != change.rank_after {
            out.rank_changed += 1;
        }
        if change.after.is_none() {
            // The proposed policy has no such lane: the copy could not be scored at all.
            out.copies_without_lane += 1;
        }
        if change.before == change.after {
            continue;
        }
        let count = lanes.entry(change.lane.clone()).or_default();
        out.changed += 1;
        count.changed += 1;
        if change.delta > 0 {
            out.raised += 1;
            count.raised += 1;
        } else if change.delta < 0 {
            out.lowered += 1;
            count.lowered += 1;
        }
        out.changes.push(ScoreChange {
            copy_id: input.copy_id,
            title_id: input.title_id,
            title: input.title.clone(),
            library: input.library.clone(),
            lane: change.lane,
            release_name: change.name,
            old_score: change.before,
            new_score: change.after,
            delta: change.delta,
            traits: change
                .causes
                .into_iter()
                .map(|c| TraitChange {
                    trait_id: c.trait_id,
                    label: c.label,
                    before: c.before,
                    after: c.after,
                    what: c.what,
                })
                .collect(),
            old_rank: change.rank_before,
            new_rank: change.rank_after,
        });
    }
    out.by_lane = lanes.into_values().collect();
    out.changes.sort_by(|a, b| {
        b.delta
            .abs()
            .cmp(&a.delta.abs())
            .then(a.copy_id.cmp(&b.copy_id))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ActivePolicy, BUILTIN};
    use singularr_core::release::parse;

    fn input(id: i64, lane: &str, name: &str) -> SimInput {
        SimInput {
            copy_id: id,
            title_id: id,
            title: format!("Title {id}"),
            library: "plex".to_string(),
            lane: Some(lane.to_string()),
            release: parse(name),
            ctx: EvalContext::default(),
        }
    }

    #[test]
    fn the_same_policy_changes_nothing() {
        let active = ActivePolicy::builtin().policy;
        let inputs = vec![
            input(1, "movies-hd", "Movie.2004.1080p.BluRay.x265-QxR"),
            input(
                2,
                "movies-uhd",
                "Movie.2019.2160p.UHD.BluRay.REMUX.DV.HDR.TrueHD.Atmos-GRP",
            ),
        ];
        let out = simulate(&active, &active, &inputs);
        assert_eq!(out.copies_evaluated, 2);
        assert_eq!((out.changed, out.rank_changed), (0, 0));
        assert!(out.changes.is_empty());
    }

    #[test]
    fn a_new_weight_shows_up_with_the_trait_that_moved() {
        let active = ActivePolicy::builtin().policy;
        let proposed_text = BUILTIN.replacen(
            "\"video.hevc\" = 100, \"audio.atmos\" = 50",
            "\"video.hevc\" = 400, \"audio.atmos\" = 50",
            1,
        );
        assert_ne!(proposed_text, BUILTIN, "the test edits the movies-hd lane");
        let proposed = Policy::from_toml(&proposed_text).unwrap();
        let mut unplaced = input(5, "x", "Show.S01E03.1080p.WEB-DL.x265-NTb");
        unplaced.lane = None;
        let inputs = vec![
            input(1, "movies-hd", "Movie.2004.1080p.BluRay.x265-QxR"),
            input(2, "movies-hd", "Movie.2005.1080p.BluRay.x264-GRP"),
            input(3, "shows-hd", "Show.S01E01.1080p.WEB-DL.x265-NTb"),
            unplaced,
        ];
        let out = simulate(&active, &proposed, &inputs);
        assert_eq!(out.copies_evaluated, 4);
        assert_eq!(out.copies_without_lane, 1, "the copy with no lane");
        assert_eq!((out.changed, out.raised, out.lowered), (1, 1, 0));
        let change = &out.changes[0];
        assert_eq!(change.copy_id, 1);
        assert_eq!(
            (change.old_score, change.new_score, change.delta),
            (Some(100), Some(400), 300)
        );
        assert_eq!(change.traits.len(), 1);
        assert_eq!(change.traits[0].trait_id, "video.hevc");
        assert_eq!(
            (change.traits[0].before, change.traits[0].after),
            (100, 400)
        );
        assert_eq!(change.traits[0].what, "points 100 to 400");
        let hd = out.by_lane.iter().find(|l| l.lane == "movies-hd").unwrap();
        assert_eq!((hd.copies, hd.changed, hd.raised), (2, 1, 1));
    }

    #[test]
    fn many_copies_keep_their_identity_across_the_worker_split() {
        let active = ActivePolicy::builtin().policy;
        let proposed = Policy::from_toml(&BUILTIN.replacen(
            "\"video.hevc\" = 100, \"audio.atmos\" = 50",
            "\"video.hevc\" = 250, \"audio.atmos\" = 50",
            1,
        ))
        .unwrap();
        let inputs: Vec<SimInput> = (0..997)
            .map(|i| {
                let codec = if i % 3 == 0 { "x265" } else { "x264" };
                input(
                    i,
                    "movies-hd",
                    &format!("Movie.{i}.2004.1080p.BluRay.{codec}-GRP"),
                )
            })
            .collect();
        let out = simulate(&active, &proposed, &inputs);
        assert_eq!(out.changed, 333);
        assert!(
            out.changes
                .iter()
                .all(|c| c.copy_id % 3 == 0 && c.delta == 150)
        );
        assert!(
            out.changes.iter().all(
                |c| c.release_name == format!("Movie.{}.2004.1080p.BluRay.x265-GRP", c.copy_id)
            )
        );
    }
}
