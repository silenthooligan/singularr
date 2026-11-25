//! The model into the engine's policy: one trait per list, per scored option and per rule, one
//! lane per quality profile.
//!
//! Points come from positions. In a group of options the preferred ones get one unit per place
//! from the bottom (the first of three gets three units), the avoided ones minus one unit per
//! place, a blocked one [`BLOCK`]. The preferred lists are a step apart that is larger than
//! everything the smaller preferences can add up to, so a lower list never passes a higher one,
//! unless the profile lets a clearly better release cross. A number a pick fixes (`points`, kept
//! from an import) wins over its position.

use super::{
    CHOICES, Choice, Crossing, Group, ListKind, Model, ModelError, Pick, Profile, Stance,
    UpgradeWhen, condition_of,
};
use crate::policy::{Condition, Lane, Policy, Trait};
use crate::release::{Quality, Source};
use std::collections::{BTreeMap, BTreeSet};

/// The points of one place in a group of options.
pub const UNIT: i32 = 10;
/// The points of a blocked option: far below anything a release can make up for.
pub const BLOCK: i32 = -1_000_000;
/// The minimum score of a profile that fixes none: only something blocked falls below it.
pub const FLOOR: i32 = -500_000;
/// The trait of releases from no list of release groups.
pub const UNLISTED: &str = "list.none";

/// The trait id of a built-in option: `range.dv`.
pub fn option_trait(group: Group, key: &str) -> String {
    format!("{}.{key}", group.prefix())
}

/// The trait id of a list: `list.remux-tier-1`.
pub fn list_trait(id: &str) -> String {
    format!("list.{id}")
}

/// The trait id of an advanced rule: `rule.hindi-audio`.
pub fn rule_trait(id: &str) -> String {
    format!("rule.{id}")
}

/// What a reason calls a built-in option: `Picture: Dolby Vision`, `Edition: IMAX`, `AI upscale`.
pub fn option_label(c: &Choice) -> String {
    match c.group {
        Group::Release => c.label.to_string(),
        Group::Editions => format!("Edition: {}", c.label),
        group => format!("{}: {}", group.answer(), c.label),
    }
}

fn round_up(n: i32) -> i32 {
    let rest = n.rem_euclid(UNIT);
    if rest == 0 { n } else { n - rest + UNIT }
}

/// The points of each pick of one group, by position. Allowed picks get none.
fn weigh(picks: &[Pick]) -> Vec<i32> {
    let preferred = picks.iter().filter(|p| p.stance == Stance::Prefer).count() as i32;
    let mut prefer_place = 0;
    let mut avoid_place = 0;
    picks
        .iter()
        .map(|p| match p.stance {
            Stance::Prefer => {
                let w = UNIT * (preferred - prefer_place);
                prefer_place += 1;
                p.points.unwrap_or(w)
            }
            Stance::Avoid => {
                avoid_place += 1;
                p.points.unwrap_or(-UNIT * avoid_place)
            }
            Stance::Block => p.points.unwrap_or(BLOCK),
            Stance::Allow => 0,
        })
        .collect()
}

/// The points of the preferred lists, best first. A fixed number stays as it is; a list
/// without one is `step` above the next, or between the fixed numbers around it.
fn ranked_points(fixed: &[Option<i32>], step: i32) -> Vec<i32> {
    let n = fixed.len();
    let gap = |a: usize, b: usize| (b - a) as i32;
    (0..n)
        .map(|i| {
            if let Some(x) = fixed[i] {
                return x;
            }
            let above = (0..i).rev().find_map(|j| fixed[j].map(|x| (j, x)));
            let below = (i + 1..n).find_map(|k| fixed[k].map(|x| (k, x)));
            match (above, below) {
                (Some((j, a)), Some((k, b))) => a - (a - b) * gap(j, i) / gap(j, k),
                (Some((j, a)), None) => a - a * gap(j, i) / gap(j, n),
                (None, Some((k, b))) => b + step * gap(i, k),
                (None, None) => step * gap(i, n),
            }
        })
        .collect()
}

/// One scored pick of a profile: its trait, the pick, its points.
struct Scored<'a> {
    id: String,
    pick: &'a Pick,
    points: i32,
    group: Option<Group>,
}

/// One profile as a lane, and the size traits that are its own.
fn lane_of(p: &Profile, lists: &[&str], unlisted: bool) -> (Lane, Vec<Trait>) {
    let mut small: Vec<Scored<'_>> = Vec::new();
    for group in Group::ALL {
        let picks = p.picks(group);
        for (pick, points) in picks.iter().zip(weigh(picks)) {
            small.push(Scored {
                id: option_trait(group, &pick.key),
                pick,
                points,
                group: Some(group),
            });
        }
    }
    for (pick, points) in p.more.rules.iter().zip(weigh(&p.more.rules)) {
        small.push(Scored {
            id: rule_trait(&pick.key),
            pick,
            points,
            group: None,
        });
    }
    let counted = || {
        small
            .iter()
            .filter(|s| !matches!(s.pick.stance, Stance::Block | Stance::Allow))
    };
    let strongest = counted().map(|s| s.points.abs()).max().unwrap_or(0);
    let mut points: BTreeMap<String, i32> = BTreeMap::new();
    for s in &small {
        if s.pick.stance != Stance::Allow && s.points != 0 {
            points.insert(s.id.clone(), s.points);
        }
    }

    // Size: outside the range is avoided a little more than the strongest single preference.
    let mut traits = Vec::new();
    let kind = p.target.kind;
    let hours = kind.hours();
    let size_points = -round_up(strongest + UNIT);
    let mut swing: i32 = counted().map(|s| s.points.abs()).sum();
    let bounds = [
        ("small", p.size.min_per_hour, "Size: below range"),
        ("large", p.size.max_per_hour, "Size: above range"),
    ];
    let mut sized = false;
    for (end, gb, label) in bounds {
        let Some(gb) = gb.filter(|g| *g > 0.0) else {
            continue;
        };
        let id = format!("size.{}.{end}", p.id);
        let when = if end == "small" {
            Condition {
                kind: vec![kind.media()],
                max_size_gb: Some(gb * hours),
                ..Condition::default()
            }
        } else {
            Condition {
                kind: vec![kind.media()],
                min_size_gb: Some(gb * hours),
                ..Condition::default()
            }
        };
        traits.push(Trait {
            id: id.clone(),
            label: label.to_string(),
            why: format!("{} per hour, read for {hours} h", super::gb(gb)),
            category: "size".to_string(),
            when,
            arr_formats: Vec::new(),
        });
        points.insert(id, size_points);
        sized = true;
    }
    if sized {
        swing += size_points.abs();
    }
    if p.size.smaller_wins_tie {
        for n in [1, 2, 4, 8, 16, 32, 64] {
            let id = format!("size.{}.under-{n}", p.id);
            traits.push(Trait {
                id: id.clone(),
                label: "Size: smaller file".to_string(),
                why: format!("At most {n} GB per hour"),
                category: "size".to_string(),
                when: Condition {
                    kind: vec![kind.media()],
                    max_size_gb: Some(f64::from(n) * hours),
                    ..Condition::default()
                },
                arr_formats: Vec::new(),
            });
            points.insert(id, 1);
            swing += 1;
        }
    }

    // The lists: a step apart that the smaller preferences cannot make up, unless the profile
    // lets a clearly better release cross.
    let strict = round_up(swing + UNIT);
    let step = match p.lists.crossing {
        Crossing::Never => strict,
        Crossing::ClearlyBetter => strict.min(round_up((2 * strongest).max(UNIT))),
    };
    let ranked: Vec<&Pick> = p
        .lists
        .ranked
        .iter()
        .filter(|x| lists.contains(&x.key.as_str()))
        .collect();
    let preferred: Vec<&Pick> = ranked
        .iter()
        .copied()
        .filter(|x| x.stance == Stance::Prefer)
        .collect();
    let fixed: Vec<Option<i32>> = preferred.iter().map(|x| x.points).collect();
    let list_points = ranked_points(&fixed, step);
    for (x, w) in preferred.iter().zip(&list_points) {
        points.insert(list_trait(&x.key), *w);
    }
    let mut avoid_place = 0;
    for x in &ranked {
        let w = match x.stance {
            Stance::Avoid => {
                avoid_place += 1;
                x.points.unwrap_or(-step * avoid_place)
            }
            Stance::Block => x.points.unwrap_or(BLOCK),
            _ => continue,
        };
        points.insert(list_trait(&x.key), w);
    }
    if unlisted {
        match p.lists.unlisted {
            Stance::Avoid => {
                points.insert(UNLISTED.to_string(), -step);
            }
            Stance::Block => {
                points.insert(UNLISTED.to_string(), BLOCK);
            }
            _ => {}
        }
    }

    // Final: from the top list, with the required preferences, or with the best of each group
    // of picture and sound when none is required.
    let required: Vec<i32> = small
        .iter()
        .filter(|s| s.pick.stance == Stance::Prefer && s.pick.required)
        .map(|s| s.points)
        .collect();
    let ideal: i32 = if required.is_empty() {
        Group::ALL
            .into_iter()
            .filter(|g| g.ideal())
            .map(|g| {
                small
                    .iter()
                    .filter(|s| s.group == Some(g) && s.pick.stance == Stance::Prefer)
                    .map(|s| s.points)
                    .max()
                    .unwrap_or(0)
                    .max(0)
            })
            .sum()
    } else {
        required.iter().sum()
    };
    let top = list_points.first().copied().unwrap_or(0).max(0);
    let upgrade_step = match p.upgrades.when {
        UpgradeWhen::Any => 1,
        UpgradeWhen::ClearlyBetter => round_up(strongest.max(UNIT)),
    };

    let (ladder, cutoff_quality) = match &p.scores.ladder {
        Some(ladder) => (ladder.clone(), p.scores.cutoff_quality.clone()),
        None => {
            let t = &p.target;
            let last = t.step_of(t.accept_from).unwrap_or(0);
            let label = |source: Source| {
                Quality {
                    source,
                    resolution: t.resolution,
                }
                .label()
            };
            (
                t.sources
                    .iter()
                    .take(last + 1)
                    .map(|step| step.iter().map(|s| label(*s)).collect())
                    .collect(),
                t.stop_at.map(label),
            )
        }
    };
    let lane = Lane {
        id: p.id.clone(),
        label: p.name.clone(),
        ladder,
        points,
        min_score: p.scores.minimum.unwrap_or(FLOOR),
        upgrade_step: p.scores.upgrade_step.unwrap_or(upgrade_step),
        cutoff_score: p.scores.cutoff.unwrap_or(top + ideal),
        cutoff_quality,
        arr_profiles: p.manager_profiles.clone(),
    };
    (lane, traits)
}

fn members_line(n: usize) -> String {
    if n == 1 {
        "1 member".to_string()
    } else {
        format!("{n} members")
    }
}

/// The policy the model stands for. The same model always gives the same policy. `version`
/// names it (the saved model's version).
pub fn compile(model: &Model, version: &str) -> Result<Policy, ModelError> {
    model.check()?;
    let lists: Vec<&str> = model
        .lists
        .iter()
        .filter(|l| !l.members.is_empty())
        .map(|l| l.id.as_str())
        .collect();
    let group_lists: Vec<Condition> = model
        .lists
        .iter()
        .filter(|l| l.kind == ListKind::Group && !l.members.is_empty())
        .map(|l| l.condition())
        .collect();
    let mut lanes = Vec::new();
    let mut own = Vec::new();
    for p in &model.profiles {
        let (lane, traits) = lane_of(p, &lists, !group_lists.is_empty());
        lanes.push(lane);
        own.extend(traits);
    }
    let formats = |id: &str| model.formats.get(id).cloned().unwrap_or_default();
    let mut traits = Vec::new();
    for l in model.lists.iter().filter(|l| !l.members.is_empty()) {
        let id = list_trait(&l.id);
        traits.push(Trait {
            label: format!("{}: {}", l.kind.answer(), l.name),
            why: members_line(l.members.len()),
            category: l.kind.category().to_string(),
            when: l.condition(),
            arr_formats: formats(&id),
            id,
        });
    }
    if !group_lists.is_empty() {
        traits.push(Trait {
            id: UNLISTED.to_string(),
            label: "Release groups: no list".to_string(),
            why: "A release group in no list".to_string(),
            category: "group".to_string(),
            when: Condition {
                not: Some(Box::new(Condition {
                    any: group_lists,
                    ..Condition::default()
                })),
                ..Condition::default()
            },
            arr_formats: Vec::new(),
        });
    }
    let scored: BTreeSet<String> = lanes
        .iter()
        .flat_map(|l| l.points.keys().cloned())
        .collect();
    for c in CHOICES {
        let id = option_trait(c.group, c.key);
        if !scored.contains(&id) {
            continue;
        }
        let Some(when) = condition_of(c.group, c.key) else {
            continue;
        };
        traits.push(Trait {
            label: option_label(c),
            why: c.group.label().to_string(),
            category: c.group.category().to_string(),
            when,
            arr_formats: formats(&id),
            id,
        });
    }
    for r in &model.rules {
        let id = rule_trait(&r.id);
        traits.push(Trait {
            label: r.name.clone(),
            why: if r.why.is_empty() {
                "Advanced rule".to_string()
            } else {
                r.why.clone()
            },
            category: if r.category.is_empty() {
                "other".to_string()
            } else {
                r.category.clone()
            },
            when: r.when.clone(),
            arr_formats: formats(&id),
            id,
        });
    }
    traits.extend(own);
    let policy = Policy {
        name: model.name.clone(),
        version: version.to_string(),
        traits,
        lanes,
        protections: model.protections.clone(),
    };
    // What the engine loads from the file form is what it scores with: the same check.
    let text = toml::to_string(&policy).map_err(|e| ModelError::Policy(e.to_string()))?;
    Policy::from_toml(&text).map_err(|e| ModelError::Policy(e.to_string()))?;
    Ok(policy)
}

/// The model in the policy file form, ready for `SINGULARR_POLICY` or another import.
pub fn export(model: &Model, version: &str) -> Result<String, ModelError> {
    let policy = compile(model, version)?;
    let body = toml::to_string(&policy).map_err(|e| ModelError::Policy(e.to_string()))?;
    Ok(format!(
        "# Quality profiles, lists and rules as Singularr compiles them, version {version}.\n\
         # A policy file: name it with SINGULARR_POLICY, or import it.\n\n{body}"
    ))
}
