//! Parse, score, decide, compare with the arrs, and simulate a policy over the catalog.

use super::assess::{compare, differs, singularr_reading};
use super::model::{
    CopyVerdict, EngineDiffCounts, HeldCopy, ParseRequest, ParseResponse, ScoreRequest,
    ScoreResponse, ShadowQuery, ShadowResponse, ShadowRow,
};
use crate::catalog::model::{ArrReading, CopyView, SingularrReading, VerdictView};
use crate::catalog::store::{self as catalog_store, DiffFilter};
use crate::core::error::{AppError, AppResult};
use crate::core::model::{Page, Paging};
use crate::policy::model::{ScoreChange, SimulateRequest, SimulateResponse};
use crate::policy::simulate::{SimInput, simulate as run_simulation};
use crate::policy::{ActivePolicy, eval_context, hash_text, held_context};
use singularr_core::policy::{decide, evaluate};
use singularr_core::release::{self, Quality};
use singularr_core::{EvalContext, ParsedRelease, Policy};
use sqlx::PgPool;

/// Longer than any release name; a guard against pasting a file into the field.
const MAX_NAME: usize = 1000;

fn checked_name(name: &str) -> AppResult<&str> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::bad_request("`name` is empty."));
    }
    if name.len() > MAX_NAME {
        return Err(AppError::bad_request(format!(
            "`name` is {} bytes long, at most {MAX_NAME}.",
            name.len()
        )));
    }
    Ok(name)
}

pub fn parse(req: &ParseRequest) -> AppResult<ParseResponse> {
    let parsed = release::parse(checked_name(&req.name)?);
    Ok(ParseResponse {
        quality: parsed.quality().map(Quality::label),
        parsed,
    })
}

/// Score a candidate name in a lane. With a held copy, the candidate is judged against the
/// copy as it is held: the release the catalog scored (the arr's record first), with the
/// title's tags and original language and the probe of the file.
pub async fn score(
    pool: &PgPool,
    active: &ActivePolicy,
    req: &ScoreRequest,
) -> AppResult<ScoreResponse> {
    let name = checked_name(&req.name)?;
    let policy = &active.policy;
    let held = match req.held_copy {
        Some(id) => Some(catalog_store::copy(pool, id).await?.ok_or_else(|| {
            AppError::not_found(format!("There is no copy {id} to compare with."))
        })?),
        None => None,
    };
    let lane_id = req
        .lane
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .or_else(|| held.as_ref().and_then(|h| h.lane.clone()))
        .ok_or_else(|| AppError::bad_request("`lane` is missing: a quality profile id."))?;
    let lane = policy.lane(&lane_id).ok_or_else(|| {
        let known: Vec<&str> = policy.lanes.iter().map(|l| l.id.as_str()).collect();
        AppError::bad_request(format!(
            "Unknown quality profile `{lane_id}`; known: {}.",
            known.join(", ")
        ))
    })?;

    let held_tags = match &held {
        Some(h) => catalog_store::title_engines(pool, h.title_id)
            .await?
            .into_iter()
            .find(|e| e.engine == h.engine)
            .map(|e| e.tags)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let original_language = held.as_ref().and_then(|h| h.original_language.clone());
    let ctx = eval_context(
        req.size_bytes,
        req.title_tags.clone().unwrap_or_else(|| held_tags.clone()),
        original_language,
    );
    let candidate = release::parse(name);
    let held_side: Option<(ParsedRelease, EvalContext)> = held.as_ref().map(|h| {
        (
            h.held(),
            held_context(
                h.size_bytes,
                &held_tags,
                h.original_language.as_deref(),
                &h.languages,
                h.media_info.as_ref(),
            ),
        )
    });
    let decision = decide(
        policy,
        lane,
        held_side.as_ref().map(|(p, c)| (p, c)),
        &candidate,
        &ctx,
    );
    Ok(ScoreResponse {
        lane: lane.id.clone(),
        verdict: VerdictView {
            verdict: decision.candidate.clone(),
            ladder_steps: Some(lane.ladder.len()),
        },
        decision,
        parsed: candidate,
        held: held.map(|h| HeldCopy {
            copy_id: h.id,
            title_id: h.title_id,
            title: h.title.clone(),
            library: h.library_id.clone(),
            lane: h.lane.clone(),
            release_name: h.release_name.clone(),
            protections: h.protections.clone(),
        }),
        policy_hash: active.hash.clone(),
    })
}

pub fn copy_verdict(copy: CopyView) -> CopyVerdict {
    CopyVerdict {
        copy_id: copy.id,
        title_id: copy.title_id,
        title: copy.title,
        library: copy.library,
        lane: copy.lane,
        release_name: copy.release_name,
        name_source: copy.name_source,
        verdict: copy.verdict,
        protections: copy.protections,
        arr: copy.arr,
        singularr: copy.singularr,
        differs: copy.differs,
        arr_only_formats: copy.arr_only_formats,
        singularr_only_traits: copy.singularr_only_traits,
    }
}

fn shadow_row(c: CopyView) -> ShadowRow {
    ShadowRow {
        copy_id: c.id,
        title_id: c.title_id,
        title: c.title,
        library: c.library,
        engine: c.engine,
        lane: c.lane,
        season_number: c.season_number,
        episode_numbers: c.episode_numbers,
        release_name: c.release_name,
        name_source: c.name_source,
        arr: c.arr,
        singularr: c.singularr,
        differs: c.differs,
        arr_only_formats: c.arr_only_formats,
        singularr_only_traits: c.singularr_only_traits,
    }
}

pub const SHADOW_KINDS: [&str; 6] = ["any", "all", "quality", "group", "edition", "score"];

pub async fn shadow(
    pool: &PgPool,
    engines: &[String],
    q: &ShadowQuery,
) -> AppResult<ShadowResponse> {
    let kind = q
        .disagree
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .unwrap_or("any")
        .to_ascii_lowercase();
    // `disagree=true` and `disagree=false` read as "any difference" and "every copy".
    let kind = match kind.as_str() {
        "true" => "any".to_string(),
        "false" => "all".to_string(),
        _ => kind,
    };
    if !SHADOW_KINDS.contains(&kind.as_str()) {
        return Err(AppError::bad_request(format!(
            "`disagree` is one of {}, not `{kind}`.",
            SHADOW_KINDS.join(", ")
        )));
    }
    let engine = q.arr.as_deref().map(str::trim).filter(|a| !a.is_empty());
    if let Some(engine) = engine
        && !engines.iter().any(|e| e == engine)
    {
        return Err(AppError::bad_request(format!(
            "`arr` is one of {}, not `{engine}`.",
            engines.join(", ")
        )));
    }
    let filter = DiffFilter {
        engine: engine.map(str::to_string),
        differs: (kind != "all").then_some(kind),
        lane: q.lane.clone(),
        q: q.q.clone(),
    };
    let paging = Paging::new(q.page, q.per_page);
    let (rows, total) = catalog_store::list_differing(pool, &filter, paging).await?;
    let counts = catalog_store::diff_counts(pool, engine).await?;
    let mut by_engine = Vec::new();
    for e in engines {
        by_engine.push(EngineDiffCounts {
            engine: e.clone(),
            counts: catalog_store::diff_counts(pool, Some(e)).await?,
        });
    }
    Ok(ShadowResponse {
        counts,
        by_engine,
        rows: Page::new(
            rows.into_iter().map(|r| shadow_row(r.view())).collect(),
            paging,
            total,
        ),
    })
}

/// Replay every stored copy under a proposed policy and report what would change. The
/// copies are scored as they are held, with the same context the sync scores them with.
pub async fn simulate(
    pool: &PgPool,
    active: &ActivePolicy,
    req: &SimulateRequest,
) -> AppResult<SimulateResponse> {
    if req.policy.trim().is_empty() {
        return Err(AppError::bad_request("`policy` is empty."));
    }
    let proposed = Policy::from_toml(&req.policy)
        .map_err(|e| AppError::bad_request(format!("The proposed policy is not valid: {e}")))?;
    let rows = catalog_store::sim_rows(pool).await?;
    let inputs: Vec<SimInput> = rows
        .into_iter()
        .map(|r| {
            let ctx = held_context(
                r.size_bytes,
                &r.tags,
                r.original_language.as_deref(),
                &r.languages,
                r.media_info.as_ref(),
            );
            SimInput {
                copy_id: r.id,
                title_id: r.title_id,
                title: r.title,
                library: r.library_id,
                lane: r.lane,
                release: serde_json::from_value(r.held)
                    .unwrap_or_else(|_| release::parse(&r.release_name)),
                ctx,
            }
        })
        .collect();
    let (before, after) = (active.policy.clone(), proposed.clone());
    let outcome = tokio::task::spawn_blocking(move || run_simulation(&before, &after, &inputs))
        .await
        .map_err(|e| AppError::Internal(format!("the simulation task failed: {e}")))?;

    let lane = req.lane.as_deref().map(str::trim).filter(|l| !l.is_empty());
    let listed: Vec<ScoreChange> = outcome
        .changes
        .into_iter()
        .filter(|c| lane.is_none_or(|l| c.lane.eq_ignore_ascii_case(l)))
        .collect();
    let paging = Paging::new(req.page, req.per_page);
    let total = listed.len() as i64;
    let items: Vec<ScoreChange> = listed
        .into_iter()
        .skip(paging.offset() as usize)
        .take(paging.limit() as usize)
        .collect();
    Ok(SimulateResponse {
        name: proposed.name.clone(),
        version: proposed.version.clone(),
        hash: hash_text(&req.policy),
        active_hash: active.hash.clone(),
        copies_evaluated: outcome.copies_evaluated,
        copies_without_lane: outcome.copies_without_lane,
        changed: outcome.changed,
        raised: outcome.raised,
        lowered: outcome.lowered,
        rank_changed: outcome.rank_changed,
        by_lane: outcome.by_lane,
        changes: Page::new(items, paging, total),
    })
}

/// Singularr's reading of a bare name in a lane, for the side by side with an arr's parse.
pub fn reading_of(
    active: &ActivePolicy,
    name: &str,
    lane: Option<&str>,
) -> (ParsedRelease, SingularrReading) {
    let parsed = release::parse(name);
    let verdict = lane
        .and_then(|id| active.policy.lane(id))
        .map(|l| evaluate(&active.policy, l, &parsed, &EvalContext::default()));
    let reading = singularr_reading(&parsed, verdict.as_ref());
    (parsed, reading)
}

/// The names of the differences between an arr's reading of a name and Singularr's.
pub fn differences(
    arr: &ArrReading,
    singularr: &SingularrReading,
    parsed: &ParsedRelease,
) -> Vec<String> {
    differs(compare(arr, singularr, parsed.collective.as_deref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_refuses_an_empty_or_huge_name() {
        assert!(matches!(
            parse(&ParseRequest { name: "  ".into() }),
            Err(AppError::BadRequest(_))
        ));
        assert!(matches!(
            parse(&ParseRequest {
                name: "x".repeat(MAX_NAME + 1)
            }),
            Err(AppError::BadRequest(_))
        ));
        let ok = parse(&ParseRequest {
            name: "Movie.2004.1080p.BluRay.x265-QxR".into(),
        })
        .unwrap();
        assert_eq!(ok.quality.as_deref(), Some("Bluray-1080p"));
        assert_eq!(ok.parsed.group.as_deref(), Some("QxR"));
        assert!(!ok.parsed.notes.is_empty(), "every field has its evidence");
    }

    #[test]
    fn a_bare_name_reads_and_scores_in_a_lane() {
        let active = ActivePolicy::builtin();
        let (parsed, reading) = reading_of(
            &active,
            "Movie.2004.1080p.BluRay.x265-QxR",
            Some("movies-hd"),
        );
        assert_eq!(parsed.year, Some(2004));
        assert_eq!(reading.quality.as_deref(), Some("Bluray-1080p"));
        assert_eq!(reading.score, Some(100));
        assert_eq!(
            reading_of(&active, "Movie.2004.1080p.BluRay.x265-QxR", None)
                .1
                .score,
            None
        );
    }
}
