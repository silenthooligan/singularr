//! SQL over `processing_runs`, and over the processing rules: `processing_rules`, their
//! versions, their log and the place their pass stands in the journal.

use super::model::{
    FlowStat, IMPORTED, MarkRow, Moment, OPEN_STATES, Run, RunFilter, STAGED, StateFilter,
    StoredCounts,
};
use super::rules::{LogRow, NewLog, RuleBody, RuleRow};
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Postgres, QueryBuilder};

/// A run without the job as the service gave it (`raw`), which only one run at a time is read
/// with.
const SELECT: &str = "SELECT r.id, r.integration, r.job_id, r.flow, r.trigger_text, r.state,
        r.created_at, r.updated_at, r.finished_at, r.queue_path, r.source_path, r.source_size,
        r.output_path, r.output_size, r.result_path, r.result_size, r.manager_file_id,
        r.replaced, r.detail, r.events, r.title_id, r.season_number, r.episode_number,
        r.first_seen_at, r.last_seen_at
     FROM processing_runs r";

fn open_states() -> Vec<String> {
    OPEN_STATES.iter().map(|s| s.to_string()).collect()
}

/// What is stored of a run that decides whether a pass writes it again, and whether it closed
/// since.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Known {
    pub id: String,
    pub fingerprint: String,
    pub state: String,
    pub title_id: Option<i64>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub last_seen_at: DateTime<Utc>,
}

/// The stored runs with these ids.
pub async fn known(pool: &PgPool, ids: &[String]) -> Result<Vec<Known>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, Known>(
        "SELECT id, fingerprint, state, title_id, season_number, episode_number, last_seen_at
         FROM processing_runs WHERE id = ANY($1)",
    )
    .bind(ids.to_vec())
    .fetch_all(pool)
    .await
}

/// Insert a run, or write again everything a listing says about it. `first_seen_at` stays.
pub async fn upsert(
    conn: &mut PgConnection,
    run: &Run,
    fingerprint: &str,
    raw: &serde_json::Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO processing_runs (id, integration, job_id, flow, trigger_text, state,
                                      created_at, updated_at, finished_at, queue_path,
                                      source_path, source_size, output_path, output_size,
                                      result_path, result_size, manager_file_id, replaced,
                                      detail, events, raw, title_id, season_number,
                                      episode_number, fingerprint)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18,
                 $19, $20, $21, $22, $23, $24, $25)
         ON CONFLICT (id) DO UPDATE SET
             flow = EXCLUDED.flow, trigger_text = EXCLUDED.trigger_text, state = EXCLUDED.state,
             created_at = EXCLUDED.created_at, updated_at = EXCLUDED.updated_at,
             finished_at = EXCLUDED.finished_at, queue_path = EXCLUDED.queue_path,
             source_path = EXCLUDED.source_path, source_size = EXCLUDED.source_size,
             output_path = EXCLUDED.output_path, output_size = EXCLUDED.output_size,
             result_path = EXCLUDED.result_path, result_size = EXCLUDED.result_size,
             manager_file_id = EXCLUDED.manager_file_id, replaced = EXCLUDED.replaced,
             detail = EXCLUDED.detail, events = EXCLUDED.events, raw = EXCLUDED.raw,
             title_id = EXCLUDED.title_id, season_number = EXCLUDED.season_number,
             episode_number = EXCLUDED.episode_number, fingerprint = EXCLUDED.fingerprint,
             last_seen_at = now()",
    )
    .bind(&run.id)
    .bind(&run.integration)
    .bind(&run.job_id)
    .bind(&run.flow)
    .bind(&run.trigger)
    .bind(&run.state)
    .bind(run.created_at)
    .bind(run.updated_at)
    .bind(run.finished_at)
    .bind(&run.queue_path)
    .bind(&run.source_path)
    .bind(run.source_size)
    .bind(&run.output_path)
    .bind(run.output_size)
    .bind(&run.result_path)
    .bind(run.result_size)
    .bind(run.manager_file_id)
    .bind(Json(&run.replaced))
    .bind(&run.detail)
    .bind(Json(&run.events))
    .bind(raw)
    .bind(run.title_id)
    .bind(run.season)
    .bind(run.episode)
    .bind(fingerprint)
    .execute(conn)
    .await
    .map(|_| ())
}

/// Say that a pass listed these runs, which it had no reason to write again.
pub async fn touch(conn: &mut PgConnection, ids: &[String]) -> Result<u64, sqlx::Error> {
    if ids.is_empty() {
        return Ok(0);
    }
    sqlx::query("UPDATE processing_runs SET last_seen_at = now() WHERE id = ANY($1)")
        .bind(ids)
        .execute(conn)
        .await
        .map(|r| r.rows_affected())
}

/// How many runs of one integration are stored, and how many of them are open.
pub async fn totals(pool: &PgPool, integration: &str) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT count(*), count(*) FILTER (WHERE state = ANY($2))
         FROM processing_runs WHERE integration = $1",
    )
    .bind(integration)
    .bind(open_states())
    .fetch_one(pool)
    .await
}

/// A word as an `ILIKE` pattern that matches it anywhere.
fn like(word: &str) -> String {
    format!(
        "%{}%",
        word.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    )
}

fn filters(qb: &mut QueryBuilder<Postgres>, f: &RunFilter, moment: &Moment) {
    qb.push(" WHERE true");
    match &f.state {
        None => {}
        Some(StateFilter::Open) => {
            qb.push(" AND r.state = ANY(")
                .push_bind(open_states())
                .push(")");
        }
        Some(StateFilter::Closed) => {
            qb.push(" AND NOT (r.state = ANY(")
                .push_bind(open_states())
                .push("))");
        }
        Some(StateFilter::Working) => {
            qb.push(" AND r.state = ")
                .push_bind(STAGED)
                .push(" AND r.queue_path = ANY(")
                .push_bind(moment.busy_paths.clone())
                .push(")");
        }
        // A run without a queue path has no worker on it: the comparison is then unknown,
        // which counts as no.
        Some(StateFilter::Waiting) => {
            qb.push(" AND r.state = ")
                .push_bind(STAGED)
                .push(" AND NOT COALESCE(r.queue_path = ANY(")
                .push_bind(moment.busy_paths.clone())
                .push("), false)");
        }
        Some(StateFilter::Stuck) => {
            qb.push(" AND r.state = ANY(")
                .push_bind(open_states())
                .push(") AND r.created_at < ")
                .push_bind(moment.stuck_before)
                .push(" AND NOT COALESCE(r.state = ")
                .push_bind(STAGED)
                .push(" AND r.queue_path = ANY(")
                .push_bind(moment.busy_paths.clone())
                .push("), false)");
        }
        Some(StateFilter::Is(state)) => {
            qb.push(" AND r.state = ").push_bind(state.clone());
        }
    }
    if let Some(flows) = &f.flows {
        qb.push(" AND r.flow = ANY(")
            .push_bind(flows.clone())
            .push(")");
    }
    if let Some(integration) = &f.integration {
        qb.push(" AND r.integration = ")
            .push_bind(integration.clone());
    }
    if let Some(titles) = &f.titles {
        qb.push(" AND r.title_id = ANY(")
            .push_bind(titles.clone())
            .push(")");
    }
    // Each word is in the name of the source, of what the worker made, or of the result. The
    // name is the path after its last separator.
    for word in &f.words {
        let pattern = like(word);
        qb.push(" AND (");
        for (i, column) in ["r.source_path", "r.output_path", "r.result_path"]
            .into_iter()
            .enumerate()
        {
            if i > 0 {
                qb.push(" OR ");
            }
            qb.push("regexp_replace(")
                .push(column)
                .push(r", '^.*[/\\]', '') ILIKE ")
                .push_bind(pattern.clone());
        }
        qb.push(")");
    }
}

/// The runs a filter asks for, newest first, with how many there are in all.
pub async fn list(
    pool: &PgPool,
    f: &RunFilter,
    moment: &Moment,
    limit: i64,
    offset: i64,
) -> Result<(Vec<Run>, i64), sqlx::Error> {
    let mut count = QueryBuilder::<Postgres>::new("SELECT count(*) FROM processing_runs r");
    filters(&mut count, f, moment);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut list = QueryBuilder::<Postgres>::new(SELECT);
    filters(&mut list, f, moment);
    list.push(" ORDER BY r.created_at DESC, r.id DESC LIMIT ")
        .push_bind(limit)
        .push(" OFFSET ")
        .push_bind(offset);
    let rows = list.build_query_as::<Run>().fetch_all(pool).await?;
    Ok((rows, total))
}

/// Every open run, oldest first.
pub async fn open(pool: &PgPool) -> Result<Vec<Run>, sqlx::Error> {
    sqlx::query_as::<_, Run>(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE r.state = ANY($1) ORDER BY r.created_at, r.id"
    )))
    .bind(open_states())
    .fetch_all(pool)
    .await
}

/// The closed runs that closed last.
pub async fn recent_closed(pool: &PgPool, limit: i64) -> Result<Vec<Run>, sqlx::Error> {
    sqlx::query_as::<_, Run>(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE NOT (r.state = ANY($1))
         ORDER BY COALESCE(r.finished_at, r.updated_at, r.created_at) DESC, r.id DESC LIMIT $2"
    )))
    .bind(open_states())
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The runs in one of these states that closed at or after `since`, the last to close first.
pub async fn closed_since(
    pool: &PgPool,
    states: &[&str],
    since: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Run>, sqlx::Error> {
    let states: Vec<String> = states.iter().map(|s| s.to_string()).collect();
    sqlx::query_as::<_, Run>(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE r.state = ANY($1) AND r.finished_at >= $2
         ORDER BY r.finished_at DESC, r.id DESC LIMIT $3"
    )))
    .bind(states)
    .bind(since)
    .bind(limit)
    .fetch_all(pool)
    .await
}

pub async fn one(pool: &PgPool, id: &str) -> Result<Option<Run>, sqlx::Error> {
    sqlx::query_as::<_, Run>(sqlx::AssertSqlSafe(format!("{SELECT} WHERE r.id = $1")))
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// The service's own ids of the open runs of one integration, the oldest first, at most
/// `limit`: the jobs a pass asks its service about by id when a listing did not carry them.
pub async fn open_job_ids(
    pool: &PgPool,
    integration: &str,
    limit: i64,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT job_id FROM processing_runs
         WHERE integration = $1 AND state = ANY($2)
         ORDER BY created_at, id LIMIT $3",
    )
    .bind(integration)
    .bind(open_states())
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// The newest open run of one integration that was staged from this file, by its id.
pub async fn open_of_source(
    pool: &PgPool,
    integration: &str,
    source_path: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT id FROM processing_runs
         WHERE integration = $1 AND source_path = $2 AND state = ANY($3)
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(integration)
    .bind(source_path)
    .bind(open_states())
    .fetch_optional(pool)
    .await
}

/// The open runs of one integration that started before `before` and were never seen stuck,
/// the oldest first: the runs that may have gone stuck since they were last looked at.
pub async fn stuck_candidates(
    pool: &PgPool,
    integration: &str,
    before: DateTime<Utc>,
    limit: i64,
) -> Result<Vec<Run>, sqlx::Error> {
    sqlx::query_as::<_, Run>(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE r.integration = $1 AND r.state = ANY($2) AND r.created_at < $3
           AND r.stuck_noted_at IS NULL
         ORDER BY r.created_at, r.id LIMIT $4"
    )))
    .bind(integration)
    .bind(open_states())
    .bind(before)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Mark runs as seen stuck. Returns the ids this marked: a run is marked once.
pub async fn note_stuck(
    conn: &mut PgConnection,
    ids: &[String],
    at: DateTime<Utc>,
) -> Result<Vec<String>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar::<_, String>(
        "UPDATE processing_runs SET stuck_noted_at = $2
         WHERE id = ANY($1) AND stuck_noted_at IS NULL
         RETURNING id",
    )
    .bind(ids)
    .bind(at)
    .fetch_all(conn)
    .await
}

/// A run's job exactly as its service gave it.
pub async fn raw(pool: &PgPool, id: &str) -> Result<Option<serde_json::Value>, sqlx::Error> {
    sqlx::query_scalar::<_, serde_json::Value>("SELECT raw FROM processing_runs WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// The newest runs linked to a title.
pub async fn of_title(pool: &PgPool, title_id: i64, limit: i64) -> Result<Vec<Run>, sqlx::Error> {
    sqlx::query_as::<_, Run>(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE r.title_id = $1 ORDER BY r.created_at DESC, r.id DESC LIMIT $2"
    )))
    .bind(title_id)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// How many runs are linked to a title, and how many of them are open.
pub async fn title_counts(pool: &PgPool, title_id: i64) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as::<_, (i64, i64)>(
        "SELECT count(*), count(*) FILTER (WHERE state = ANY($2))
         FROM processing_runs WHERE title_id = $1",
    )
    .bind(title_id)
    .bind(open_states())
    .fetch_one(pool)
    .await
}

/// The runs of a title that mark one of its files, newest first: the imported ones (by their
/// result) and the open ones (by their source).
pub async fn marks(pool: &PgPool, title_id: i64) -> Result<Vec<MarkRow>, sqlx::Error> {
    sqlx::query_as::<_, MarkRow>(
        "SELECT id, state, source_path, result_path FROM processing_runs
         WHERE title_id = $1 AND (state = $2 OR state = ANY($3))
         ORDER BY created_at DESC, id DESC",
    )
    .bind(title_id)
    .bind(IMPORTED)
    .bind(open_states())
    .fetch_all(pool)
    .await
}

pub async fn counts(pool: &PgPool) -> Result<StoredCounts, sqlx::Error> {
    sqlx::query_as::<_, StoredCounts>(
        "SELECT count(*) AS total,
                count(*) FILTER (WHERE state = 'staged') AS staged,
                count(*) FILTER (WHERE state = 'verifying') AS verifying,
                count(*) FILTER (WHERE state = 'imported') AS imported,
                count(*) FILTER (WHERE state = 'failed') AS failed,
                count(*) FILTER (WHERE state = 'refused') AS refused,
                count(*) FILTER (WHERE state = 'pruned') AS pruned,
                count(*) FILTER (WHERE state = 'superseded') AS superseded,
                COALESCE(sum(source_size - result_size) FILTER (WHERE state = 'imported'),
                         0)::bigint AS saved_bytes,
                min(created_at) AS since
         FROM processing_runs",
    )
    .fetch_one(pool)
    .await
}

/// The runs in numbers, by service and flow. A flow a service runs under several ids of its own
/// is one flow: `groups` gives, in three lists of one length, the integration, each of its
/// modes, and the flow that mode belongs to. A run whose flow no group names counts under its
/// own.
pub async fn flow_stats(
    pool: &PgPool,
    integrations: &[String],
    modes: &[String],
    flows: &[String],
) -> Result<Vec<FlowStat>, sqlx::Error> {
    sqlx::query_as::<_, FlowStat>(
        "SELECT r.integration, COALESCE(g.flow, r.flow) AS flow,
                count(*) AS runs,
                count(*) FILTER (WHERE r.state = 'imported') AS imported,
                max(r.created_at) AS last_run,
                (sum(r.source_size) FILTER (WHERE r.state = 'imported'
                     AND r.source_size IS NOT NULL AND r.result_size IS NOT NULL))::bigint
                    AS source_bytes,
                (sum(r.result_size) FILTER (WHERE r.state = 'imported'
                     AND r.source_size IS NOT NULL AND r.result_size IS NOT NULL))::bigint
                    AS result_bytes,
                percentile_cont(0.5) WITHIN GROUP (
                    ORDER BY EXTRACT(EPOCH FROM (r.finished_at - r.created_at))::double precision
                ) FILTER (WHERE r.state = 'imported' AND r.finished_at IS NOT NULL)
                    AS median_seconds
         FROM processing_runs r
         LEFT JOIN unnest($1::text[], $2::text[], $3::text[]) AS g(integration, mode, flow)
                ON g.integration = r.integration AND g.mode = r.flow
         GROUP BY r.integration, COALESCE(g.flow, r.flow)
         ORDER BY r.integration, COALESCE(g.flow, r.flow)",
    )
    .bind(integrations.to_vec())
    .bind(modes.to_vec())
    .bind(flows.to_vec())
    .fetch_all(pool)
    .await
}

// Rules --------------------------------------------------------------------------------------

const RULE: &str = "SELECT id, name, enabled, run_on, conditions, service, flow, version,
        created_at, created_by, updated_at, updated_by
     FROM processing_rules";

/// The rules that are not removed, oldest first.
pub async fn rules(pool: &PgPool) -> Result<Vec<RuleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuleRow>(sqlx::AssertSqlSafe(format!(
        "{RULE} WHERE removed_at IS NULL ORDER BY id"
    )))
    .fetch_all(pool)
    .await
}

/// One rule that is not removed.
pub async fn rule(pool: &PgPool, id: i64) -> Result<Option<RuleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuleRow>(sqlx::AssertSqlSafe(format!(
        "{RULE} WHERE id = $1 AND removed_at IS NULL"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await
}

async fn save_version(
    conn: &mut PgConnection,
    rule: &RuleRow,
    actor: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO processing_rule_versions (rule_id, version, body, saved_by)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(rule.id)
    .bind(rule.version)
    .bind(Json(rule.body()))
    .bind(actor)
    .execute(conn)
    .await?;
    Ok(())
}

/// The columns of a rule, read from a statement's `RETURNING` rows named `name`.
fn rule_from(name: &str) -> String {
    RULE.replace("FROM processing_rules", &format!("FROM {name}"))
}

/// A new rule, at version 1, with that version kept.
pub async fn insert_rule(
    conn: &mut PgConnection,
    body: &RuleBody,
    actor: &str,
) -> Result<RuleRow, sqlx::Error> {
    let rule = sqlx::query_as::<_, RuleRow>(sqlx::AssertSqlSafe(format!(
        "WITH new AS (
             INSERT INTO processing_rules
                 (name, enabled, run_on, conditions, service, flow, created_by, updated_by)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $7)
             RETURNING *
         ) {}",
        rule_from("new")
    )))
    .bind(&body.name)
    .bind(body.enabled)
    .bind(&body.when)
    .bind(Json(&body.conditions))
    .bind(&body.service)
    .bind(&body.flow)
    .bind(actor)
    .fetch_one(&mut *conn)
    .await?;
    save_version(conn, &rule, actor).await?;
    Ok(rule)
}

/// A rule saved again: its next version, kept beside the ones before. `None` for a rule that
/// is not there or is removed.
pub async fn update_rule(
    conn: &mut PgConnection,
    id: i64,
    body: &RuleBody,
    actor: &str,
) -> Result<Option<RuleRow>, sqlx::Error> {
    let rule = sqlx::query_as::<_, RuleRow>(sqlx::AssertSqlSafe(format!(
        "WITH changed AS (
             UPDATE processing_rules
             SET name = $2, enabled = $3, run_on = $4, conditions = $5, service = $6,
                 flow = $7, version = version + 1, updated_at = now(), updated_by = $8
             WHERE id = $1 AND removed_at IS NULL
             RETURNING *
         ) {}",
        rule_from("changed")
    )))
    .bind(id)
    .bind(&body.name)
    .bind(body.enabled)
    .bind(&body.when)
    .bind(Json(&body.conditions))
    .bind(&body.service)
    .bind(&body.flow)
    .bind(actor)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(rule) = &rule {
        save_version(conn, rule, actor).await?;
    }
    Ok(rule)
}

/// Remove a rule: it is never evaluated again, and its log and versions stay. `None` for a
/// rule that is not there or is removed already.
pub async fn remove_rule(
    conn: &mut PgConnection,
    id: i64,
    actor: &str,
) -> Result<Option<RuleRow>, sqlx::Error> {
    sqlx::query_as::<_, RuleRow>(sqlx::AssertSqlSafe(format!(
        "WITH gone AS (
             UPDATE processing_rules
             SET removed_at = now(), updated_at = now(), updated_by = $2
             WHERE id = $1 AND removed_at IS NULL
             RETURNING *
         ) {}",
        rule_from("gone")
    )))
    .bind(id)
    .bind(actor)
    .fetch_optional(conn)
    .await
}

/// One version of a rule as it was saved.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct RuleVersion {
    pub version: i32,
    pub body: serde_json::Value,
    pub saved_at: DateTime<Utc>,
    pub saved_by: String,
}

/// Every version of a rule, the newest first.
pub async fn rule_versions(pool: &PgPool, id: i64) -> Result<Vec<RuleVersion>, sqlx::Error> {
    sqlx::query_as::<_, RuleVersion>(
        "SELECT version, body, saved_at, saved_by FROM processing_rule_versions
         WHERE rule_id = $1 ORDER BY version DESC",
    )
    .bind(id)
    .fetch_all(pool)
    .await
}

/// Write entries of the rules' log. A second `sent` entry for one file is not written: a file
/// is sent once.
pub async fn insert_log(conn: &mut PgConnection, entries: &[NewLog]) -> Result<u64, sqlx::Error> {
    let mut written = 0;
    for e in entries {
        written += sqlx::query(
            "INSERT INTO processing_rule_log
                 (rule_id, rule_version, event_id, title_id, copy_id, path, service, flow,
                  outcome, detail, evidence, run_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
             ON CONFLICT DO NOTHING",
        )
        .bind(e.rule_id)
        .bind(e.rule_version)
        .bind(e.event_id)
        .bind(e.title_id)
        .bind(e.copy_id)
        .bind(&e.path)
        .bind(&e.service)
        .bind(&e.flow)
        .bind(e.outcome)
        .bind(&e.detail)
        .bind(&e.evidence)
        .bind(&e.run_id)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    }
    Ok(written)
}

/// A rule's log, the newest first, at most `limit` entries.
pub async fn rule_log(pool: &PgPool, rule: i64, limit: i64) -> Result<Vec<LogRow>, sqlx::Error> {
    sqlx::query_as::<_, LogRow>(
        "SELECT id, rule_id, rule_version, at, event_id, title_id, copy_id, path, service, flow,
                outcome, detail, evidence, run_id
         FROM processing_rule_log WHERE rule_id = $1 ORDER BY at DESC, id DESC LIMIT $2",
    )
    .bind(rule)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// How many entries each rule's log holds of each outcome, and when it last matched.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct LogCount {
    pub rule_id: i64,
    pub outcome: String,
    pub entries: i64,
    pub last: Option<DateTime<Utc>>,
}

pub async fn log_counts(pool: &PgPool) -> Result<Vec<LogCount>, sqlx::Error> {
    sqlx::query_as::<_, LogCount>(
        "SELECT rule_id, outcome, count(*)::bigint AS entries, max(at) AS last
         FROM processing_rule_log GROUP BY rule_id, outcome ORDER BY rule_id, outcome",
    )
    .fetch_all(pool)
    .await
}

/// Of these paths, the ones a rule sent.
pub async fn sent_paths(pool: &PgPool, paths: &[String]) -> Result<Vec<String>, sqlx::Error> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_scalar::<_, String>(
        "SELECT path FROM processing_rule_log WHERE outcome = 'sent' AND path = ANY($1)",
    )
    .bind(paths.to_vec())
    .fetch_all(pool)
    .await
}

/// Where the rules pass stands in the journal. `None` before its first pass.
pub async fn rules_cursor(pool: &PgPool) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT event_id FROM processing_rules_cursor WHERE id = 1")
        .fetch_optional(pool)
        .await
}

/// Mark where the rules pass stands.
pub async fn set_rules_cursor(pool: &PgPool, event: i64) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    move_rules_cursor(&mut conn, event).await
}

/// Move where the rules pass stands, never back.
pub async fn move_rules_cursor(conn: &mut PgConnection, event: i64) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO processing_rules_cursor (id, event_id) VALUES (1, $1)
         ON CONFLICT (id) DO UPDATE
         SET event_id = GREATEST(processing_rules_cursor.event_id, EXCLUDED.event_id)",
    )
    .bind(event)
    .execute(conn)
    .await?;
    Ok(())
}

/// The result paths of the runs in `state` of these titles, by title.
pub async fn results_of_titles(
    pool: &PgPool,
    titles: &[i64],
    state: &str,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    if titles.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, (i64, String)>(
        "SELECT title_id, result_path FROM processing_runs
         WHERE title_id = ANY($1) AND state = $2 AND result_path IS NOT NULL",
    )
    .bind(titles.to_vec())
    .bind(state)
    .fetch_all(pool)
    .await
}

/// The runs of these titles, the newest first, at most `limit`.
pub async fn of_titles(pool: &PgPool, titles: &[i64], limit: i64) -> Result<Vec<Run>, sqlx::Error> {
    if titles.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, Run>(sqlx::AssertSqlSafe(format!(
        "{SELECT} WHERE r.title_id = ANY($1) ORDER BY r.created_at DESC, r.id DESC LIMIT $2"
    )))
    .bind(titles.to_vec())
    .bind(limit)
    .fetch_all(pool)
    .await
}
