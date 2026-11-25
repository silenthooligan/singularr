//! A policy file into the model.
//!
//! Traits that only name release groups or streaming services (with at most a restriction to
//! some sources or resolutions) become lists. By default they are clustered into tiers: traits
//! whose points are close in every profile share one list, with their mean points
//! ([`ImportMode::Tiers`]). [`ImportMode::Exact`] keeps one list per distinct points vector, so
//! the model scores each release exactly as the file did. Traits whose condition is a built-in
//! option become that option's stance. The rest become advanced rules. Every option and rule
//! keeps the file's points and every profile the file's thresholds. [`rank_changes`] says what
//! a tiered import moves on a set of releases.

use super::compile::{list_trait, option_trait, rule_trait};
use super::{
    CHOICES, Group, List, ListKind, Model, Pick, Profile, Rule, STRIP, Scores, Stance, Target,
    TitleKind, choices, condition_of, slug, unique_id,
};
use crate::policy::{Condition, EvalContext, Lane, Policy, Trait, Verdict, evaluate};
use crate::release::{ParsedRelease, Quality, Resolution, Source};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;

/// How an import gathers the traits that name release groups or services into lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    /// Tiers: traits whose points are within a tenth of the profile's largest list points of
    /// each other, in every profile, share one list with their mean points.
    #[default]
    Tiers,
    /// One list per distinct points vector: every release scores exactly as in the file.
    Exact,
}

/// Points at or below this block a release; a blocked list never shares a tier with one that
/// is not.
const BLOCKING: i32 = -10_000;

/// The kind of title a lane is for: from the managers its profiles belong to (`radarr:` is
/// movies, `sonarr:` shows), else from its id and label.
pub fn guess_kind(lane: &Lane) -> TitleKind {
    for p in &lane.arr_profiles {
        let p = p.to_ascii_lowercase();
        if p.starts_with("radarr") {
            return TitleKind::Movie;
        }
        if p.starts_with("sonarr") {
            return TitleKind::Show;
        }
    }
    let words = format!("{} {}", lane.id, lane.label).to_ascii_lowercase();
    if ["show", "tv", "series", "episode"]
        .iter()
        .any(|w| words.contains(w))
    {
        TitleKind::Show
    } else {
        TitleKind::Movie
    }
}

fn sorted<T: Clone + Debug>(v: &[T]) -> Vec<T> {
    let mut v = v.to_vec();
    v.sort_by_key(|x| format!("{x:?}"));
    v
}

/// The condition with every list in one order, so two that mean the same compare equal.
fn canon(c: &Condition) -> Condition {
    let mut any: Vec<Condition> = c.any.iter().map(canon).collect();
    any.sort_by_key(|x| format!("{x:?}"));
    Condition {
        any,
        not: c.not.as_ref().map(|n| Box::new(canon(n))),
        group: sorted(&c.group),
        collective: sorted(&c.collective),
        name: c.name.clone(),
        kind: sorted(&c.kind),
        source: sorted(&c.source),
        resolution: sorted(&c.resolution),
        video_codec: sorted(&c.video_codec),
        hdr: sorted(&c.hdr),
        sdr: c.sdr,
        audio_codec: sorted(&c.audio_codec),
        atmos: c.atmos,
        channels: sorted(&c.channels),
        service: sorted(&c.service),
        edition: sorted(&c.edition),
        language: sorted(&c.language),
        original_language: c.original_language,
        title_tag: sorted(&c.title_tag),
        min_fps: c.min_fps,
        ai_upscaled: c.ai_upscaled,
        prerelease: c.prerelease,
        proper_or_repack: c.proper_or_repack,
        min_size_gb: c.min_size_gb,
        max_size_gb: c.max_size_gb,
    }
}

fn push_member(members: &mut Vec<String>, name: &str) {
    if !members.iter().any(|m| m.eq_ignore_ascii_case(name)) {
        members.push(name.to_string());
    }
}

/// A trait that only names release groups or services: its kind, members and restriction.
fn as_list(c: &Condition) -> Option<(ListKind, Vec<String>, Option<Condition>)> {
    let mut members = Vec::new();
    let kind = if !c.group.is_empty() && c.collective.is_empty() && c.any.is_empty() {
        for g in &c.group {
            push_member(&mut members, g);
        }
        ListKind::Group
    } else if c.group.is_empty() && c.collective.is_empty() && !c.any.is_empty() {
        for entry in &c.any {
            let groups = Condition {
                group: entry.group.clone(),
                ..Condition::default()
            };
            let collectives = Condition {
                collective: entry.collective.clone(),
                ..Condition::default()
            };
            let plain = (*entry == groups && !entry.group.is_empty())
                || (*entry == collectives && !entry.collective.is_empty());
            if !plain {
                return None;
            }
            for m in entry.group.iter().chain(&entry.collective) {
                push_member(&mut members, m);
            }
        }
        if !c.service.is_empty() {
            return None;
        }
        ListKind::Group
    } else if !c.service.is_empty() && c.group.is_empty() && c.collective.is_empty() {
        if !c.any.is_empty() {
            return None;
        }
        for s in &c.service {
            push_member(&mut members, s);
        }
        ListKind::Service
    } else {
        return None;
    };
    if kind == ListKind::Group && !c.service.is_empty() {
        return None;
    }
    let scope = Condition {
        group: Vec::new(),
        collective: Vec::new(),
        service: Vec::new(),
        any: Vec::new(),
        ..c.clone()
    };
    let allowed = Condition {
        source: scope.source.clone(),
        resolution: scope.resolution.clone(),
        kind: scope.kind.clone(),
        not: scope.not.clone(),
        ..Condition::default()
    };
    if scope != allowed || members.iter().any(|m| m.trim().is_empty() || m.trim() != m) {
        return None;
    }
    Some((
        kind,
        members,
        (scope != Condition::default()).then_some(scope),
    ))
}

/// Where a lane's ladder puts the target, or the ladder as written when the target cannot say
/// it (several resolutions, a source the strip does not hold).
struct Shape {
    target: Target,
    ladder: Option<Vec<Vec<String>>>,
    cutoff_quality: Option<String>,
}

fn shape_of(lane: &Lane, kind: TitleKind) -> Shape {
    let steps: Vec<Vec<Option<Quality>>> = lane
        .ladder
        .iter()
        .map(|step| step.iter().map(|s| Quality::from_label(s)).collect())
        .collect();
    let all: Vec<Quality> = steps.iter().flatten().flatten().copied().collect();
    let mut counts: BTreeMap<Resolution, usize> = BTreeMap::new();
    for q in &all {
        *counts.entry(q.resolution).or_default() += 1;
    }
    let resolution = counts
        .iter()
        .max_by_key(|(r, n)| (**n, **r))
        .map(|(r, _)| *r)
        .unwrap_or(Resolution::R1080);
    let parsed = steps.iter().flatten().all(Option::is_some);
    let simple = parsed
        && !all.is_empty()
        && all
            .iter()
            .all(|q| q.resolution == resolution && STRIP.contains(&q.source));
    if simple {
        let mut seen: BTreeSet<Source> = BTreeSet::new();
        let mut sources: Vec<Vec<Source>> = Vec::new();
        for step in &steps {
            let fresh: Vec<Source> = step
                .iter()
                .flatten()
                .map(|q| q.source)
                .filter(|s| seen.insert(*s))
                .collect();
            if !fresh.is_empty() {
                sources.push(fresh);
            }
        }
        let accept_from = sources.last().and_then(|s| s.last()).copied();
        let stop = lane.cutoff_quality.as_deref().map(Quality::from_label);
        let stop_at = match stop {
            None => Some(None),
            Some(Some(q)) if q.resolution == resolution && seen.contains(&q.source) => {
                Some(Some(q.source))
            }
            Some(_) => None,
        };
        if let (Some(accept_from), Some(stop_at)) = (accept_from, stop_at) {
            for s in STRIP {
                if !seen.contains(&s) {
                    sources.push(vec![s]);
                }
            }
            return Shape {
                target: Target {
                    kind,
                    resolution,
                    sources,
                    accept_from,
                    stop_at,
                },
                ladder: None,
                cutoff_quality: None,
            };
        }
    }
    Shape {
        target: Target {
            kind,
            resolution,
            ..Target::default()
        },
        ladder: Some(lane.ladder.clone()),
        cutoff_quality: lane.cutoff_quality.clone(),
    }
}

/// The built-in option a trait's condition is, in the lanes that score it. A resolution that
/// every such lane targets anyway says nothing there and is left out of the comparison.
fn as_option(t: &Trait, points: &[Option<i32>], shapes: &[Shape]) -> Option<(Group, &'static str)> {
    let mut when = t.when.clone();
    let scoring: Vec<&Shape> = points
        .iter()
        .zip(shapes)
        .filter(|(p, _)| p.is_some())
        .map(|(_, s)| s)
        .collect();
    let targets: BTreeSet<Resolution> = scoring.iter().map(|s| s.target.resolution).collect();
    if !scoring.is_empty()
        && scoring.iter().all(|s| s.ladder.is_none())
        && targets.len() == 1
        && when.resolution.len() == 1
        && targets.contains(&when.resolution[0])
    {
        when.resolution.clear();
    }
    let want = canon(&when);
    CHOICES
        .iter()
        .find(|c| condition_of(c.group, c.key).is_some_and(|x| canon(&x) == want))
        .map(|c| (c.group, c.key))
}

fn imported_pick(key: &str, points: Option<i32>) -> Pick {
    Pick {
        key: key.to_string(),
        stance: points.map_or(Stance::Allow, Stance::of_points),
        required: false,
        points,
    }
}

/// Preferred picks by their points, best first; then the allowed ones; then the avoided ones,
/// mildest first; then the blocked. Ties keep their order.
fn order(picks: &mut [Pick]) {
    picks.sort_by_key(|p| {
        let place = match p.stance {
            Stance::Prefer => 0,
            Stance::Allow => 1,
            Stance::Avoid => 2,
            Stance::Block => 3,
        };
        (place, std::cmp::Reverse(p.points.unwrap_or(0)))
    });
}

fn add_formats(into: &mut Vec<String>, from: &[String]) {
    for f in from {
        if !into.contains(f) {
            into.push(f.clone());
        }
    }
}

struct ListDraft {
    list: List,
    points: Vec<Option<i32>>,
    formats: Vec<String>,
}

/// A trait that only names release groups or services, before lists are made of it.
struct Candidate {
    trait_id: String,
    label: String,
    kind: ListKind,
    members: Vec<String>,
    only: Option<Condition>,
    /// Kind and restriction: candidates with different scopes never share a list.
    scope: String,
    points: Vec<Option<i32>>,
    formats: Vec<String>,
}

impl Candidate {
    fn at(&self, lane: usize) -> i32 {
        self.points[lane].unwrap_or(0)
    }

    /// What orders the tiers: the largest points in any profile, then their sum.
    fn strength(&self) -> (i32, i64) {
        let max = self
            .points
            .iter()
            .map(|p| p.unwrap_or(0))
            .max()
            .unwrap_or(0);
        let sum = self.points.iter().map(|p| i64::from(p.unwrap_or(0))).sum();
        (max, sum)
    }

    fn shares_member(&self, other: &Candidate) -> bool {
        self.members
            .iter()
            .any(|m| other.members.iter().any(|n| n.eq_ignore_ascii_case(m)))
    }
}

/// A tier's name: its members' names, the first two and a count past three.
fn tier_name(labels: &[&str]) -> String {
    if labels.len() <= 3 {
        labels.join(", ")
    } else {
        format!("{} +{}", labels[..2].join(", "), labels.len() - 2)
    }
}

/// Lists from candidates. Exact: one list per scope and points vector, members of candidates
/// that share no member. Tiers: complete linkage, strongest first; a candidate joins a tier when,
/// in every profile, its points and every member's have no opposite signs, are both blocking or
/// both not, and differ by at most a tenth of that profile's largest points of any list that
/// does not block. A tier's points are its members' mean.
fn gather(cands: Vec<Candidate>, mode: ImportMode, lanes: usize) -> Vec<ListDraft> {
    let groups: Vec<Vec<usize>> = match mode {
        ImportMode::Exact => {
            let mut groups: Vec<Vec<usize>> = Vec::new();
            for (i, c) in cands.iter().enumerate() {
                let fits = |g: &&mut Vec<usize>| {
                    let first = &cands[g[0]];
                    first.scope == c.scope
                        && first.points == c.points
                        && !g.iter().any(|j| cands[*j].shares_member(c))
                };
                match groups.iter_mut().find(fits) {
                    Some(g) => g.push(i),
                    None => groups.push(vec![i]),
                }
            }
            groups
        }
        ImportMode::Tiers => {
            // A tenth of the largest points a list of the same kind has in each profile, lists
            // that block left out: a small bonus in one profile does not split a tier.
            let kind_index = |k: ListKind| usize::from(k == ListKind::Service);
            let mut tolerance = vec![[0.0_f64; 2]; lanes];
            for (l, tol) in tolerance.iter_mut().enumerate() {
                for c in &cands {
                    let p = c.at(l);
                    if p > BLOCKING {
                        let k = kind_index(c.kind);
                        tol[k] = tol[k].max(f64::from(p.abs()) / 10.0);
                    }
                }
            }
            let close = |a: &Candidate, b: &Candidate| {
                a.scope == b.scope
                    && !a.shares_member(b)
                    && (0..lanes).all(|l| {
                        let (x, y) = (a.at(l), b.at(l));
                        (x <= BLOCKING) == (y <= BLOCKING)
                            && x.signum() * y.signum() >= 0
                            && f64::from(x.abs_diff(y)) <= tolerance[l][kind_index(a.kind)]
                    })
            };
            let mut order: Vec<usize> = (0..cands.len()).collect();
            order.sort_by_key(|i| std::cmp::Reverse(cands[*i].strength()));
            let mut tiers: Vec<Vec<usize>> = Vec::new();
            for i in order {
                match tiers
                    .iter_mut()
                    .find(|t| t.iter().all(|j| close(&cands[*j], &cands[i])))
                {
                    Some(t) => t.push(i),
                    None => tiers.push(vec![i]),
                }
            }
            tiers
        }
    };
    let mut ids: BTreeSet<String> = BTreeSet::from(["none".to_string()]);
    let mut drafts = Vec::new();
    for g in &groups {
        let first = &cands[g[0]];
        let base = slug(
            first
                .trait_id
                .trim_start_matches("group.")
                .trim_start_matches("service."),
        );
        let id = unique_id(&base, &ids);
        ids.insert(id.clone());
        let mut members: Vec<String> = Vec::new();
        let mut formats: Vec<String> = Vec::new();
        for j in g {
            for m in &cands[*j].members {
                push_member(&mut members, m);
            }
            add_formats(&mut formats, &cands[*j].formats);
        }
        let labels: Vec<&str> = g.iter().map(|j| cands[*j].label.as_str()).collect();
        let points = match mode {
            ImportMode::Exact => first.points.clone(),
            ImportMode::Tiers => (0..lanes)
                .map(|l| {
                    let sum: i64 = g.iter().map(|j| i64::from(cands[*j].at(l))).sum();
                    let mean = (sum as f64 / g.len() as f64).round() as i32;
                    (mean != 0).then_some(mean)
                })
                .collect(),
        };
        drafts.push(ListDraft {
            list: List {
                id,
                name: match mode {
                    ImportMode::Exact => labels.join(", "),
                    ImportMode::Tiers => tier_name(&labels),
                },
                kind: first.kind,
                members,
                only: first.only.clone(),
                rev: 1,
            },
            points,
            formats,
        });
    }
    drafts
}

struct OptionDraft {
    group: Group,
    key: &'static str,
    points: Vec<Option<i32>>,
    formats: Vec<String>,
}

struct RuleDraft {
    rule: Rule,
    points: Vec<Option<i32>>,
    formats: Vec<String>,
}

/// A policy into the model, its lists as tiers. `kind_of` says whether a lane is for movies or
/// shows; see [`guess_kind`].
pub fn import(policy: &Policy, kind_of: impl Fn(&Lane) -> TitleKind) -> Model {
    import_as(policy, kind_of, ImportMode::Tiers)
}

/// A policy into the model, its lists gathered as `mode` says.
pub fn import_as(policy: &Policy, kind_of: impl Fn(&Lane) -> TitleKind, mode: ImportMode) -> Model {
    let lanes = &policy.lanes;
    let shapes: Vec<Shape> = lanes.iter().map(|l| shape_of(l, kind_of(l))).collect();
    let points_of = |id: &str| -> Vec<Option<i32>> {
        lanes
            .iter()
            .map(|l| l.points.get(id).copied().filter(|p| *p != 0))
            .collect()
    };
    let mut cands: Vec<Candidate> = Vec::new();
    let mut options: Vec<OptionDraft> = Vec::new();
    let mut rules: Vec<RuleDraft> = Vec::new();
    let mut rule_ids: BTreeSet<String> = BTreeSet::new();
    for t in &policy.traits {
        let points = points_of(&t.id);
        if let Some((kind, members, only)) = as_list(&t.when) {
            let scope = format!(
                "{kind:?} {}",
                serde_json::to_string(&only.as_ref().map(canon)).unwrap_or_default()
            );
            cands.push(Candidate {
                trait_id: t.id.clone(),
                label: t.label.clone(),
                kind,
                members,
                only,
                scope,
                points,
                formats: t.arr_formats.clone(),
            });
            continue;
        }
        if let Some((group, key)) = as_option(t, &points, &shapes)
            && !options.iter().any(|o| o.group == group && o.key == key)
        {
            options.push(OptionDraft {
                group,
                key,
                points,
                formats: t.arr_formats.clone(),
            });
            continue;
        }
        let id = unique_id(&slug(&t.id), &rule_ids);
        rule_ids.insert(id.clone());
        rules.push(RuleDraft {
            rule: Rule {
                id,
                name: t.label.clone(),
                when: t.when.clone(),
                why: t.why.clone(),
                category: t.category.clone(),
                rev: 1,
            },
            points,
            formats: t.arr_formats.clone(),
        });
    }

    let drafts = gather(cands, mode, lanes.len());
    let mut profiles = Vec::new();
    for (i, (lane, shape)) in lanes.iter().zip(&shapes).enumerate() {
        let mut p = Profile {
            id: lane.id.clone(),
            name: lane.label.clone(),
            rev: 1,
            manager_profiles: lane.arr_profiles.clone(),
            target: shape.target.clone(),
            scores: Scores {
                minimum: Some(lane.min_score),
                upgrade_step: Some(lane.upgrade_step),
                cutoff: Some(lane.cutoff_score),
                ladder: shape.ladder.clone(),
                cutoff_quality: shape.cutoff_quality.clone(),
            },
            ..Profile::default()
        };
        for group in Group::ALL {
            let mut picks: Vec<Pick> = choices(group)
                .map(|c| {
                    let found = options
                        .iter()
                        .find(|o| o.group == group && o.key == c.key)
                        .and_then(|o| o.points[i]);
                    imported_pick(c.key, found)
                })
                .collect();
            order(&mut picks);
            *p.picks_mut(group) = picks;
        }
        p.lists.ranked = drafts
            .iter()
            .map(|d| imported_pick(&d.list.id, d.points[i]))
            .collect();
        order(&mut p.lists.ranked);
        p.more.rules = rules
            .iter()
            .map(|r| imported_pick(&r.rule.id, r.points[i]))
            .collect();
        order(&mut p.more.rules);
        profiles.push(p);
    }

    let mut formats: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut keep = |id: String, from: &[String]| {
        if !from.is_empty() {
            add_formats(formats.entry(id).or_default(), from);
        }
    };
    for d in &drafts {
        keep(list_trait(&d.list.id), &d.formats);
    }
    for o in &options {
        keep(option_trait(o.group, o.key), &o.formats);
    }
    for r in &rules {
        keep(rule_trait(&r.rule.id), &r.formats);
    }
    Model {
        name: policy.name.clone(),
        lists: drafts.into_iter().map(|d| d.list).collect(),
        rules: rules.into_iter().map(|r| r.rule).collect(),
        profiles,
        protections: policy.protections.clone(),
        formats,
        naming: Vec::new(),
    }
}

/// A release whose place among the releases a profile takes moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankMove {
    pub name: String,
    pub profile: String,
    /// Its place among the releases taken, the best first (1); none when it is not taken.
    pub from: Option<usize>,
    pub to: Option<usize>,
}

/// What a change of policy does to a set of releases: how many move, how many are taken or
/// refused differently, and the moves, the largest first.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RankReport {
    /// Releases compared, each once per profile.
    pub releases: usize,
    pub moved: usize,
    pub accept_changed: usize,
    pub moves: Vec<RankMove>,
}

fn taken(v: &Verdict, lane: &Lane) -> bool {
    v.rank.is_some() && v.score >= lane.min_score
}

/// The places of releases in one lane: the taken ones ordered by quality step, then score.
fn places(
    policy: &Policy,
    lane: &Lane,
    items: &[&(ParsedRelease, EvalContext)],
) -> Vec<Option<usize>> {
    let verdicts: Vec<Verdict> = items
        .iter()
        .map(|(r, ctx)| evaluate(policy, lane, r, ctx))
        .collect();
    let mut order: Vec<usize> = (0..items.len())
        .filter(|i| taken(&verdicts[*i], lane))
        .collect();
    order.sort_by_key(|i| (verdicts[*i].rank, std::cmp::Reverse(verdicts[*i].score)));
    let mut out = vec![None; items.len()];
    for (place, i) in order.into_iter().enumerate() {
        out[i] = Some(place + 1);
    }
    out
}

/// Compare two policies on releases, profile by profile (each profile both policies have):
/// which releases change place among those taken, and which are taken by one and not the other.
pub fn rank_changes(
    before: &Policy,
    after: &Policy,
    items: &[(ParsedRelease, EvalContext)],
) -> RankReport {
    let mut report = RankReport::default();
    let all: Vec<&(ParsedRelease, EvalContext)> = items.iter().collect();
    for lane in &before.lanes {
        let Some(other) = after.lane(&lane.id) else {
            continue;
        };
        let was = places(before, lane, &all);
        let now = places(after, other, &all);
        for (i, (from, to)) in was.iter().zip(&now).enumerate() {
            report.releases += 1;
            if from.is_some() != to.is_some() {
                report.accept_changed += 1;
            }
            if from != to {
                report.moved += 1;
                report.moves.push(RankMove {
                    name: items[i].0.raw.clone(),
                    profile: lane.id.clone(),
                    from: *from,
                    to: *to,
                });
            }
        }
    }
    let size = |m: &RankMove| match (m.from, m.to) {
        (Some(a), Some(b)) => a.abs_diff(b),
        _ => usize::MAX,
    };
    report.moves.sort_by_key(|m| std::cmp::Reverse(size(m)));
    report
}
