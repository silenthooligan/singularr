//! Singularr's verdict on each release a search found: the name parsed by Singularr's parser,
//! scored in the title's quality profile and decided against the file held now, the way the
//! release lab decides against a held copy. Beside it the manager's own word. Pure.

use super::model::{
    Chip, HeldScore, HeldView, ManagerScore, ManagerVerdict, ReleaseCounts, ReleaseView,
    SingularrVerdict, TraceLine,
};
use crate::engines::FoundRelease;
use crate::policy::eval_context;
use singularr_core::EvalContext;
use singularr_core::policy::{Action, Decision, Lane, Policy, TraceItem, decide, evaluate};
use singularr_core::release::{self, ParsedRelease};
use std::cmp::Reverse;

/// The outcome of a decision, by its code, with the interface's label.
const OUTCOMES: [(&str, &str); 13] = [
    ("no_file", "No file held"),
    ("better_quality", "Better quality"),
    ("better_score", "Better score"),
    ("held_outside", "Held file outside profile"),
    ("prerelease", "Pre-release"),
    ("unknown_quality", "Quality unknown"),
    ("not_in_profile", "Quality not in profile"),
    ("below_minimum", "Below minimum score"),
    ("protected", "Protected"),
    ("lower_quality", "Lower quality"),
    ("cutoff_met", "Cutoff met"),
    ("not_better", "Not an upgrade"),
    ("other_episode", "Other episode"),
];
/// The rules that took points away, as chips after the outcome: at most this many.
const TRAIT_CHIPS: usize = 2;

fn chip(code: &str) -> Chip {
    let label = OUTCOMES
        .iter()
        .find(|(c, _)| *c == code)
        .map_or(code, |(_, label)| label);
    Chip {
        code: code.to_string(),
        label: label.to_string(),
    }
}

/// A file the title holds now, as the comparison needs it.
#[derive(Debug, Clone)]
pub struct Held {
    pub copy_id: i64,
    pub file: String,
    pub release: String,
    pub season: Option<i32>,
    pub episodes: Vec<i32>,
    pub size: i64,
    pub edition: String,
    /// The release as it is held (the manager's record first, the name for the rest).
    pub parsed: ParsedRelease,
    /// What is known of it beyond its name: its size, the title's tags and original language,
    /// the languages on record and the probe of the file.
    pub ctx: EvalContext,
    pub manager: ManagerScore,
}

/// What releases are judged with: the policy, the title's profile, its tags and original
/// language, the files it holds now for what was searched, and what was searched for (a
/// show's season and episode).
pub struct Judge<'a> {
    pub policy: &'a Policy,
    pub lane: &'a Lane,
    pub tags: &'a [String],
    pub original_language: Option<&'a str>,
    pub held: &'a [Held],
    pub season: Option<i32>,
    pub episode: Option<i32>,
}

fn trace(items: &[TraceItem]) -> Vec<TraceLine> {
    items
        .iter()
        .map(|t| TraceLine {
            label: t.label.clone(),
            points: t.points,
            why: t.why.clone(),
        })
        .collect()
}

/// Which way a decision went, by the code of its outcome, and whether the release is taken.
/// The order of the tests is the order `decide` takes them in.
pub fn outcome(d: &Decision, lane: &Lane, candidate: &ParsedRelease) -> (&'static str, bool) {
    let c = &d.candidate;
    let Some(c_rank) = c.rank else {
        let code = if candidate.source.is_some_and(|s| s.is_prerelease()) {
            "prerelease"
        } else if c.quality.is_none() {
            "unknown_quality"
        } else {
            "not_in_profile"
        };
        return (code, false);
    };
    if c.score < lane.min_score {
        return ("below_minimum", false);
    }
    if d.blocked_by.is_some() {
        return ("protected", false);
    }
    let held = d.held.as_ref();
    let h_rank = held.and_then(|h| h.rank);
    match d.action {
        Action::Grab => ("no_file", true),
        Action::Upgrade => match h_rank {
            None => ("held_outside", true),
            Some(h) if c_rank < h => ("better_quality", true),
            Some(_) => ("better_score", true),
        },
        Action::Hold => match h_rank {
            Some(h) if c_rank > h => ("lower_quality", false),
            Some(h) if c_rank < h => ("cutoff_met", false),
            _ if held.is_some_and(|h| h.score >= lane.cutoff_score) => ("cutoff_met", false),
            _ => ("not_better", false),
        },
    }
}

impl Judge<'_> {
    /// Singularr's score of a held file in the profile.
    fn held_score(&self, h: &Held) -> HeldScore {
        let v = evaluate(self.policy, self.lane, &h.parsed, &h.ctx);
        HeldScore {
            score: v.score,
            quality: v.quality,
            trace: trace(&v.trace),
        }
    }

    /// A held file as the answer shows it.
    pub fn held_view(&self, h: &Held) -> HeldView {
        HeldView {
            copy_id: h.copy_id,
            file: h.file.clone(),
            release: h.release.clone(),
            season: h.season,
            episodes: h.episodes.clone(),
            size: h.size,
            edition: h.edition.clone(),
            singularr: self.held_score(h),
            manager: h.manager.clone(),
        }
    }

    /// The held file a release competes with: for an episode, the file that holds it; for a
    /// season pack, the weakest file of its season (it would replace them all); for a movie,
    /// the file held. `None`: nothing is held for it.
    fn against(&self, r: &FoundRelease) -> Option<&Held> {
        if self.held.iter().all(|h| h.season.is_none()) {
            return self.held.first();
        }
        let season = r.season?;
        match r.episodes.first() {
            Some(first) => self
                .held
                .iter()
                .find(|h| h.season == Some(season) && h.episodes.contains(first)),
            None => self
                .held
                .iter()
                .filter(|h| h.season == Some(season))
                .min_by_key(|h| self.held_score(h).score),
        }
    }

    /// One release with Singularr's verdict beside the manager's.
    pub fn release(&self, r: &FoundRelease) -> ReleaseView {
        let parsed = release::parse(&r.name);
        let mut ctx = eval_context(
            r.size.and_then(|s| u64::try_from(s).ok()),
            self.tags.to_vec(),
            self.original_language.map(str::to_string),
        );
        // The languages the manager read are known beyond the name, as for a held file.
        ctx.languages = r.languages.clone();
        let against = self.against(r);
        let decision = decide(
            self.policy,
            self.lane,
            against.map(|h| (&h.parsed, &h.ctx)),
            &parsed,
            &ctx,
        );
        // A release of another season or another episode than the one searched for is not
        // what was asked, whatever it scores.
        let other = self
            .season
            .is_some_and(|s| r.season.is_some_and(|rs| rs != s))
            || self
                .episode
                .is_some_and(|e| !r.episodes.is_empty() && !r.episodes.contains(&e));
        let (code, accepted) = if other {
            ("other_episode", false)
        } else {
            outcome(&decision, self.lane, &parsed)
        };
        let mut chips = vec![chip(code)];
        let mut taken: Vec<&TraceItem> = decision
            .candidate
            .trace
            .iter()
            .filter(|t| t.points < 0)
            .collect();
        taken.sort_by_key(|t| t.points);
        chips.extend(taken.into_iter().take(TRAIT_CHIPS).map(|t| Chip {
            code: "trait".to_string(),
            label: t.label.clone(),
        }));
        let action = match decision.action {
            _ if other => "hold",
            Action::Grab => "grab",
            Action::Upgrade => "upgrade",
            Action::Hold => "hold",
        };
        ReleaseView {
            guid: r.guid.clone(),
            indexer_id: r.indexer_id,
            indexer: r.indexer.clone(),
            name: r.name.clone(),
            protocol: r.protocol.clone(),
            quality: r.quality.clone(),
            size: r.size,
            age: r.age_days,
            published: r.published,
            seeders: r.seeders,
            leechers: r.leechers,
            languages: r.languages.clone(),
            season: r.season,
            episodes: r.episodes.clone(),
            full_season: r.full_season,
            singularr: SingularrVerdict {
                accepted,
                action: action.to_string(),
                score: decision.candidate.score,
                quality: decision.candidate.quality.clone(),
                group: parsed.group.clone(),
                edition: parsed.editions.first().cloned(),
                rank: decision.candidate.rank,
                chips,
                reasons: decision.reasons.clone(),
                trace: trace(&decision.candidate.trace),
                against: against.map(|h| h.copy_id),
            },
            manager: ManagerVerdict {
                accepted: r.approved,
                score: r.score,
                formats: r.formats.clone(),
                rejections: r.rejections.clone(),
                temporary: r.temporarily_rejected,
            },
        }
    }

    /// Every release, the ones Singularr would take first, then by Singularr's score, the
    /// manager's score and the seeders.
    pub fn all(&self, found: &[FoundRelease]) -> Vec<ReleaseView> {
        let mut views: Vec<ReleaseView> = found.iter().map(|r| self.release(r)).collect();
        views.sort_by_key(|v| {
            (
                Reverse(v.singularr.accepted),
                Reverse(v.singularr.score),
                Reverse(v.manager.score.unwrap_or(i32::MIN)),
                Reverse(v.seeders.unwrap_or(0)),
            )
        });
        views
    }
}

/// How many releases each side would take.
pub fn counts(views: &[ReleaseView]) -> ReleaseCounts {
    ReleaseCounts {
        total: views.len(),
        accepted: views.iter().filter(|v| v.singularr.accepted).count(),
        manager_accepted: views.iter().filter(|v| v.manager.accepted).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{ActivePolicy, held_context};

    fn found(name: &str) -> FoundRelease {
        FoundRelease {
            guid: format!("g-{name}"),
            indexer_id: 1,
            indexer: Some("Indexer".to_string()),
            name: name.to_string(),
            protocol: Some("torrent".to_string()),
            quality: None,
            size: Some(8_000_000_000),
            age_days: Some(3),
            published: None,
            seeders: Some(10),
            leechers: Some(0),
            languages: vec!["English".to_string()],
            group: None,
            edition: None,
            season: None,
            episodes: Vec::new(),
            full_season: false,
            formats: Vec::new(),
            score: Some(0),
            approved: false,
            rejections: vec!["Not wanted".to_string()],
            temporarily_rejected: false,
        }
    }

    fn held(name: &str, season: Option<i32>, episodes: Vec<i32>) -> Held {
        let parsed = release::parse(name);
        Held {
            copy_id: 7,
            file: format!("{name}.mkv"),
            release: name.to_string(),
            season,
            episodes,
            size: 6_000_000_000,
            edition: "Theatrical".to_string(),
            parsed,
            ctx: held_context(6_000_000_000, &[], Some("English"), &[], None),
            manager: ManagerScore {
                quality: Some("WEBDL-1080p".to_string()),
                score: Some(600),
                formats: vec!["x265 (HD)".to_string()],
            },
        }
    }

    /// Every outcome has its label, and a decision reads as the branch it took.
    #[test]
    fn a_release_is_judged_against_the_file_held_now() {
        let active = ActivePolicy::builtin();
        let policy = &active.policy;
        let lane = policy
            .lane("movies-hd")
            .expect("the built-in policy has movies-hd");
        let h = [held("Movie.2004.720p.WEB-DL.x264-GRP", None, Vec::new())];
        let judge = Judge {
            policy,
            lane,
            tags: &[],
            original_language: Some("English"),
            held: &h,
            season: None,
            episode: None,
        };
        let better = judge.release(&found("Movie.2004.1080p.BluRay.x264-GRP"));
        assert!(better.singularr.accepted, "{:?}", better.singularr);
        assert_eq!(better.singularr.action, "upgrade");
        assert_eq!(better.singularr.chips[0].code, "better_quality");
        assert_eq!(better.singularr.chips[0].label, "Better quality");
        assert_eq!(better.singularr.against, Some(7));
        assert_eq!(better.singularr.quality.as_deref(), Some("Bluray-1080p"));
        assert!(!better.manager.accepted);
        assert_eq!(better.manager.rejections, ["Not wanted"]);
        let worse = judge.release(&found("Movie.2004.480p.DVDRip.x264-GRP"));
        assert!(!worse.singularr.accepted);
        assert_ne!(worse.singularr.chips[0].code, "better_quality");
        assert!(!worse.singularr.reasons.is_empty());
        let unknown = judge.release(&found("Movie 2004"));
        assert_eq!(unknown.singularr.chips[0].code, "unknown_quality");
        // Nothing held: any release the profile takes is taken.
        let none = Judge { held: &[], ..judge };
        let first = none.release(&found("Movie.2004.1080p.BluRay.x264-GRP"));
        assert_eq!(
            (
                first.singularr.chips[0].code.as_str(),
                first.singularr.accepted
            ),
            ("no_file", true)
        );
        // Every outcome code has a label of its own.
        for (code, label) in OUTCOMES {
            assert_eq!(chip(code).label, label);
            assert!(!label.ends_with('.') && label.len() < 30, "{label}");
        }
        let views = judge.all(&[
            found("Movie.2004.480p.DVDRip.x264-GRP"),
            found("Movie.2004.1080p.BluRay.x264-GRP"),
        ]);
        assert!(views[0].singularr.accepted && !views[1].singularr.accepted);
        assert_eq!(counts(&views).accepted, 1);
    }

    /// A show: an episode competes with the file of that episode, a season pack with the
    /// weakest file of its season.
    #[test]
    fn an_episode_competes_with_its_own_file() {
        let active = ActivePolicy::builtin();
        let policy = &active.policy;
        let lane = policy
            .lane("shows-hd")
            .or_else(|| policy.lanes.first())
            .unwrap();
        let mut one = held("Show.S02E01.720p.WEB-DL.x264-GRP", Some(2), vec![1]);
        one.copy_id = 1;
        let mut two = held("Show.S02E02.1080p.WEB-DL.x264-GRP", Some(2), vec![2]);
        two.copy_id = 2;
        let h = [one, two];
        let judge = Judge {
            policy,
            lane,
            tags: &[],
            original_language: None,
            held: &h,
            season: None,
            episode: None,
        };
        let mut episode = found("Show.S02E02.1080p.WEB-DL.x264-GRP");
        episode.season = Some(2);
        episode.episodes = vec![2];
        assert_eq!(judge.release(&episode).singularr.against, Some(2));
        let mut pack = found("Show.S02.1080p.WEB-DL.x264-GRP");
        pack.season = Some(2);
        pack.full_season = true;
        assert_eq!(judge.release(&pack).singularr.against, Some(1));
        let mut other = found("Show.S03E01.1080p.WEB-DL.x264-GRP");
        other.season = Some(3);
        other.episodes = vec![1];
        let v = judge.release(&other);
        assert_eq!(v.singularr.against, None);
        assert_eq!(v.singularr.action, "grab");
        // Searched for S02E01: S02E02 and season 3 are other episodes; a pack of season 2 is
        // not.
        let asked = Judge {
            season: Some(2),
            episode: Some(1),
            ..judge
        };
        for r in [&episode, &other] {
            let v = asked.release(r);
            assert_eq!(
                (
                    v.singularr.chips[0].code.as_str(),
                    v.singularr.accepted,
                    v.singularr.action.as_str()
                ),
                ("other_episode", false, "hold"),
                "{}",
                r.name
            );
        }
        assert_ne!(
            asked.release(&pack).singularr.chips[0].code,
            "other_episode"
        );
    }

    /// The manager's own fields of a file.
    fn manager(quality: &str, score: i32, formats: &[&str]) -> ManagerScore {
        ManagerScore {
            quality: Some(quality.to_string()),
            score: Some(score),
            formats: formats.iter().map(|f| f.to_string()).collect(),
        }
    }

    /// Radarr's interactive search for The Devil Wears Prada (24 Nov): 205 releases, every one
    /// refused by Radarr (the title wants an open matte release). Each gets Singularr's verdict
    /// against the file held now, a WEB-DL 1080p that Radarr scores 600.
    #[test]
    fn radarr_s_search_reads_with_both_verdicts() {
        let found = crate::engines::arr::found_releases(include_bytes!(
            "../../tests/fixtures/radarr_release.json"
        ))
        .unwrap();
        assert_eq!(found.len(), 205);
        let active = ActivePolicy::builtin();
        let policy = &active.policy;
        let lane = policy.lane("movies-hd").unwrap();
        let mut file = held(
            "The.Devil.Wears.Prada.2006.1080p.WEB-DL.Opus.5.1.HEVC-SHH",
            None,
            Vec::new(),
        );
        file.manager = manager(
            "WEBDL-1080p",
            600,
            &["5.1 Surround", "HUSH SHH", "Opus", "x265 (HD)"],
        );
        let h = [file];
        let tags = ["open-matte".to_string()];
        let judge = Judge {
            policy,
            lane,
            tags: &tags,
            original_language: Some("English"),
            held: &h,
            season: None,
            episode: None,
        };
        let held_view = judge.held_view(&h[0]);
        assert_eq!(
            (
                held_view.manager.score,
                held_view.singularr.quality.as_deref()
            ),
            (Some(600), Some("WEBDL-1080p"))
        );
        let views = judge.all(&found);
        let n = counts(&views);
        assert_eq!((n.total, n.manager_accepted), (205, 0));
        assert!(
            n.accepted > 0,
            "a better quality is an upgrade in the built-in profile"
        );
        for v in &views {
            assert!(
                !v.manager.accepted && !v.manager.rejections.is_empty(),
                "{}",
                v.name
            );
            assert!(
                !v.singularr.chips.is_empty() && !v.singularr.reasons.is_empty(),
                "{}",
                v.name
            );
            assert!(
                OUTCOMES
                    .iter()
                    .any(|(code, _)| *code == v.singularr.chips[0].code)
            );
            assert_eq!(v.singularr.against, Some(7), "one file is held");
        }
        // The ones Singularr would take come first.
        let first_refused = views.iter().position(|v| !v.singularr.accepted).unwrap();
        assert!(views[first_refused..].iter().all(|v| !v.singularr.accepted));
        // A remux beats the held WEB-DL in Singularr's ladder, whatever Radarr's terms said.
        let remux = views
            .iter()
            .find(|v| {
                v.name
                    == "The Devil Wears Prada 2006 BluRay 1080p DTS-HD MA 5 1 AVC REMUX-FraMeSToR"
            })
            .unwrap();
        assert_eq!(
            (
                remux.singularr.accepted,
                remux.singularr.chips[0].code.as_str()
            ),
            (true, "better_quality")
        );
        assert_eq!(remux.manager.score, Some(1750));
        assert!(remux.manager.rejections[0].contains("required terms"));
        // A 4K release is outside the HD profile.
        let uhd = views
            .iter()
            .find(|v| {
                v.singularr
                    .quality
                    .as_deref()
                    .is_some_and(|q| q.ends_with("2160p"))
            })
            .unwrap();
        assert_eq!(uhd.singularr.chips[0].code, "not_in_profile");
    }

    /// Sonarr's interactive search for one episode (Tires S02E01): 110 releases, 20 of them
    /// taken by Sonarr. Releases of other episodes are other episodes for Singularr too.
    #[test]
    fn sonarr_s_search_reads_with_both_verdicts() {
        let found = crate::engines::arr::found_releases(include_bytes!(
            "../../tests/fixtures/sonarr_release.json"
        ))
        .unwrap();
        assert_eq!(found.len(), 110);
        let active = ActivePolicy::builtin();
        let policy = &active.policy;
        let lane = policy.lane("shows-hd").unwrap();
        let mut file = held("Tires.S02E01.1080p.HDTV.x264-GRP", Some(2), vec![1]);
        file.manager = manager("HDTV-1080p", 150, &["x264"]);
        let h = [file];
        let judge = Judge {
            policy,
            lane,
            tags: &[],
            original_language: Some("English"),
            held: &h,
            season: Some(2),
            episode: Some(1),
        };
        let views = judge.all(&found);
        let n = counts(&views);
        assert_eq!((n.total, n.manager_accepted), (110, 20));
        assert!(n.accepted > 0);
        for v in &views {
            let ours = v.season == Some(2) && (v.episodes.is_empty() || v.episodes.contains(&1));
            if ours {
                assert_eq!(v.singularr.against, Some(7), "{}", v.name);
            } else {
                assert_eq!(v.singularr.chips[0].code, "other_episode", "{}", v.name);
                assert!(!v.singularr.accepted);
            }
        }
        // Sonarr's first: a WEB-DL of the episode, which beats the held HDTV in both.
        let flux = views
            .iter()
            .find(|v| v.name == "Tires S02E01 Bonus Money 1080p NF WEB-DL DDP5 1 H 264-FLUX")
            .unwrap();
        assert!(flux.manager.accepted && flux.singularr.accepted);
        assert_eq!(flux.singularr.chips[0].code, "better_quality");
        assert_eq!(flux.quality.as_deref(), Some("WEBDL-1080p"));
        assert_eq!(flux.episodes, vec![1]);
    }
}
