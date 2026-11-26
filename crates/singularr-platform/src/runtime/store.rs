//! SQL over `job_schedules` and `jobs`: the schedules of a process, the tick, a claim with its
//! lease, a renewal, the end of a run, the reaper, a run asked for now and the jobs a stop gives
//! back.
//!
//! The row of a schedule is locked before the rows of its jobs: by the tick, by a run asked for
//! now, by the proof of a lease, by the end of a run and by reconcile. Two of them never wait on
//! each other in a circle.

use super::{CHANNEL, Done, Ended, Failed, Job, Lease, Schedule, ScheduleState, backoff};
use crate::outbound::jitter;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgExecutor, PgPool};
use std::time::Duration;

/// Seconds as the tables keep them: at least 1.
fn secs(d: Duration) -> i32 {
    i32::try_from(d.as_secs()).unwrap_or(i32::MAX).max(1)
}

/// Wake the workers of these queues in every process, from the caller's transaction: the
/// notifications go out with its commit.
async fn notify(conn: &mut PgConnection, queues: &[String]) -> Result<(), sqlx::Error> {
    let mut queues: Vec<&str> = queues.iter().map(String::as_str).collect();
    queues.sort_unstable();
    queues.dedup();
    for q in queues {
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(CHANNEL)
            .bind(q)
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Write these schedules and remove the others. A new schedule first runs after its first wait
/// and a random part of its jitter. A known one keeps its next run and whether it is on. Its next
/// run comes closer when its interval shrinks. One that runs after each start has its next run
/// brought to its first wait at the latest. Each start keeps a schedule's next run its first
/// wait away at least. A removed schedule's queued jobs are cancelled. Its finished jobs stay.
/// An empty list changes nothing.
pub async fn reconcile(pool: &PgPool, schedules: &[Schedule]) -> Result<(), sqlx::Error> {
    if schedules.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for s in schedules {
        let first = s.first.as_secs_f64() + s.jitter.as_secs_f64() * jitter();
        sqlx::query(
            "INSERT INTO job_schedules
                 (key, kind, integration, queue, every_secs, floor_secs, jitter_secs, lease_secs,
                  source, next_run_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9,
                     now() + make_interval(secs => $10::float8))
             ON CONFLICT (key) DO UPDATE SET
                 kind = EXCLUDED.kind, integration = EXCLUDED.integration,
                 queue = EXCLUDED.queue, every_secs = EXCLUDED.every_secs,
                 floor_secs = EXCLUDED.floor_secs, jitter_secs = EXCLUDED.jitter_secs,
                 lease_secs = EXCLUDED.lease_secs, source = EXCLUDED.source,
                 next_run_at = GREATEST(
                     CASE
                         WHEN $11 THEN LEAST(job_schedules.next_run_at, EXCLUDED.next_run_at)
                         WHEN EXCLUDED.every_secs < job_schedules.every_secs
                         THEN LEAST(job_schedules.next_run_at,
                                    now() + make_interval(secs => EXCLUDED.every_secs))
                         ELSE job_schedules.next_run_at END,
                     now() + make_interval(secs => $12::float8)),
                 updated_at = now()",
        )
        .bind(&s.key)
        .bind(&s.kind)
        .bind(&s.integration)
        .bind(&s.queue)
        .bind(secs(s.every))
        .bind(secs(s.floor))
        .bind(i32::try_from(s.jitter.as_secs()).unwrap_or(i32::MAX))
        .bind(secs(s.lease))
        .bind(s.source.as_str())
        .bind(first)
        .bind(s.on_start)
        .bind(s.first.as_secs_f64())
        .execute(&mut *tx)
        .await?;
    }
    let keys: Vec<&str> = schedules.iter().map(|s| s.key.as_str()).collect();
    // The rows of the schedules that go first, then the rows of their queued jobs.
    sqlx::query("SELECT key FROM job_schedules WHERE NOT (key = ANY($1)) ORDER BY key FOR UPDATE")
        .bind(&keys)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "UPDATE jobs SET state = 'cancelled', finished_at = now(),
                         last_error = 'its schedule was removed'
         WHERE state = 'queued' AND schedule_key IS NOT NULL AND NOT (schedule_key = ANY($1))",
    )
    .bind(&keys)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM job_schedules WHERE NOT (key = ANY($1))")
        .bind(&keys)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

/// The tick: one queued job for each due schedule without an open job. The schedule's next run
/// moves on by its interval. The queues that got a job are woken in every process. Returns them.
pub async fn queue_due(pool: &PgPool) -> Result<Vec<String>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let queues = sqlx::query_scalar::<_, String>(
        "WITH due AS (
             SELECT s.key FROM job_schedules s
             WHERE s.enabled AND s.next_run_at <= now()
               AND NOT EXISTS (SELECT 1 FROM jobs j
                               WHERE j.schedule_key = s.key AND j.state IN ('queued', 'running'))
             ORDER BY s.next_run_at
             FOR UPDATE SKIP LOCKED
             LIMIT 100
         ), queued AS (
             INSERT INTO jobs (kind, queue, schedule_key, integration, state, run_at)
             SELECT s.kind, s.queue, s.key, s.integration, 'queued', now()
             FROM job_schedules s JOIN due USING (key)
             ON CONFLICT (schedule_key) WHERE state IN ('queued', 'running') DO NOTHING
             RETURNING queue
         ), moved AS (
             UPDATE job_schedules s SET next_run_at = now() + make_interval(secs => s.every_secs)
             FROM due WHERE s.key = due.key
         )
         SELECT DISTINCT queue FROM queued",
    )
    .fetch_all(&mut *tx)
    .await?;
    notify(&mut tx, &queues).await?;
    tx.commit().await?;
    Ok(queues)
}

/// A job a worker holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Claimed {
    pub job: Job,
    /// How long the lease runs: the schedule's, else the runtime's.
    pub lease: Duration,
    pub max_attempts: i32,
}

type ClaimRow = (
    i64,
    String,
    Option<String>,
    Option<String>,
    Value,
    i32,
    i32,
    f64,
);

/// Claim the oldest ready job of a queue, with a lease of its schedule's length or `lease`.
pub async fn claim(
    pool: &PgPool,
    queue: &str,
    locker: &str,
    lease: Duration,
) -> Result<Option<Claimed>, sqlx::Error> {
    let row = sqlx::query_as::<_, ClaimRow>(
        "UPDATE jobs
         SET state = 'running', locked_by = $2,
             locked_until = now() + make_interval(secs => COALESCE(
                 (SELECT s.lease_secs FROM job_schedules s WHERE s.key = jobs.schedule_key),
                 $3)::float8),
             attempts = attempts + 1, started_at = now()
         WHERE id = (
             SELECT id FROM jobs
             WHERE state = 'queued' AND queue = $1 AND run_at <= now()
             ORDER BY priority DESC, run_at, id
             FOR UPDATE SKIP LOCKED
             LIMIT 1
         )
         RETURNING id, kind, schedule_key, integration, payload, attempts, max_attempts,
                   EXTRACT(EPOCH FROM (locked_until - now()))::float8",
    )
    .bind(queue)
    .bind(locker)
    .bind(secs(lease))
    .fetch_optional(pool)
    .await?;
    Ok(row.map(
        |(id, kind, schedule, integration, payload, attempt, max_attempts, lease)| Claimed {
            job: Job {
                id,
                kind,
                schedule,
                integration,
                payload,
                attempt,
            },
            lease: Duration::from_secs_f64(lease.max(1.0)),
            max_attempts,
        },
    ))
}

/// Renew a lease: on the pool for the heartbeat, in a transaction for a proof. False when the
/// lease is lost: the reaper gave the job back.
pub async fn renew<'c, E: PgExecutor<'c>>(
    conn: E,
    id: i64,
    locker: &str,
    lease: Duration,
) -> Result<bool, sqlx::Error> {
    sqlx::query(
        "UPDATE jobs SET locked_until = now() + make_interval(secs => $3::float8)
         WHERE id = $1 AND locked_by = $2 AND state = 'running'",
    )
    .bind(id)
    .bind(locker)
    .bind(lease.as_secs_f64())
    .execute(conn)
    .await
    .map(|r| r.rows_affected() == 1)
}

/// The end of a run, in the caller's transaction. The schedule's row is locked first. Then the
/// job is `done` or `failed`. Its schedule's next run comes after the interval, the handler's
/// `again` or the backoff of a failure. A one-off job that failed is queued again after its
/// kind's delay. After its kind's last attempt it is `dead`. The job keeps the run's locker:
/// its row says which run ended it ([`ended_by`]). False when the job is not the lease's any
/// more: nothing is written.
pub(crate) async fn end(
    conn: &mut PgConnection,
    lease: &Lease,
    outcome: &Result<Done, Failed>,
) -> Result<bool, sqlx::Error> {
    let ok = outcome.is_ok();
    let (result, detail, error, class, again) = match outcome {
        Ok(done) => (
            Some(done.stored()),
            Some(done.detail.as_str()),
            None,
            None,
            done.again,
        ),
        Err(failed) => (
            None,
            None,
            Some(failed.error.as_str()),
            failed.class.as_deref(),
            failed.again,
        ),
    };
    let took_ms = i32::try_from(lease.started.elapsed().as_millis()).unwrap_or(i32::MAX);
    let schedule = match &lease.schedule {
        Some(key) => {
            sqlx::query_as::<_, (i32, i32)>(
                "SELECT every_secs, failures FROM job_schedules WHERE key = $1 FOR UPDATE",
            )
            .bind(key)
            .fetch_optional(&mut *conn)
            .await?
        }
        None => None,
    };
    let ended = if lease.schedule.is_some() || ok {
        sqlx::query(
            "UPDATE jobs
             SET state = CASE WHEN $3 THEN 'done' ELSE 'failed' END, result = $4,
                 last_error = $5, error_class = $6, finished_at = now(), locked_until = NULL
             WHERE id = $1 AND locked_by = $2 AND state = 'running'",
        )
        .bind(lease.id)
        .bind(&lease.locker)
        .bind(ok)
        .bind(result)
        .bind(error)
        .bind(class)
        .execute(&mut *conn)
        .await?
        .rows_affected()
    } else {
        let retry = lease.retry;
        let delay = retry
            .base
            .saturating_mul(1u32 << (lease.attempt - 1).clamp(0, 20) as u32)
            .min(retry.cap)
            .mul_f64(0.5 + jitter());
        sqlx::query(
            "UPDATE jobs
             SET state = CASE WHEN attempts >= $3 THEN 'dead' ELSE 'queued' END,
                 max_attempts = $3,
                 run_at = now() + make_interval(secs => $4::float8), last_error = $5,
                 error_class = COALESCE($6, 'failed'), locked_until = NULL,
                 finished_at = CASE WHEN attempts >= $3 THEN now() END
             WHERE id = $1 AND locked_by = $2 AND state = 'running'",
        )
        .bind(lease.id)
        .bind(&lease.locker)
        .bind(retry.attempts.max(1))
        .bind(delay.as_secs_f64())
        .bind(error)
        .bind(class)
        .execute(&mut *conn)
        .await?
        .rows_affected()
    };
    if ended == 0 {
        return Ok(false);
    }
    if let (Some(key), Some((every, failures))) = (&lease.schedule, schedule) {
        let failures = if ok { 0 } else { failures.saturating_add(1) };
        let every = Duration::from_secs(u64::try_from(every).unwrap_or(1));
        let next = match again {
            Some(after) => after.max(Duration::from_secs(1)),
            None => backoff(every, failures, jitter()),
        };
        sqlx::query(
            "UPDATE job_schedules
             SET next_run_at = now() + make_interval(secs => $2::float8), failures = $3,
                 last_run_at = now(), last_ok_at = CASE WHEN $4 THEN now() ELSE last_ok_at END,
                 last_error = $5, last_detail = $6, last_took_ms = $7, updated_at = now()
             WHERE key = $1",
        )
        .bind(key)
        .bind(next.as_secs_f64())
        .bind(failures)
        .bind(ok)
        .bind(error)
        .bind(detail)
        .bind(took_ms)
        .execute(&mut *conn)
        .await?;
    }
    Ok(true)
}

/// Lock a schedule's row in the caller's transaction, before the rows of its jobs.
pub(crate) async fn lock_schedule(conn: &mut PgConnection, key: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1 FROM job_schedules WHERE key = $1 FOR UPDATE")
        .bind(key)
        .execute(conn)
        .await
        .map(|_| ())
}

/// Whether the run of `locker` ended this job in a commit of its own: the job is no longer
/// running and still names that run. The reaper and a stop clear the locker when they give a job
/// back.
pub async fn ended_by<'c, E: PgExecutor<'c>>(
    conn: E,
    id: i64,
    locker: &str,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT state <> 'running' AND locked_by IS NOT DISTINCT FROM $2 FROM jobs WHERE id = $1",
    )
    .bind(id)
    .bind(locker)
    .fetch_optional(conn)
    .await
    .map(|ended| ended.unwrap_or(false))
}

/// The reaper: a running job whose lease ran out goes back to its queue at once. After its last
/// attempt it is `dead`. Returns the queue of each job and whether it is dead. The queues of the
/// jobs given back are woken in every process.
pub async fn reap(pool: &PgPool) -> Result<Vec<(String, bool)>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let reaped = sqlx::query_as::<_, (String, bool)>(
        "UPDATE jobs
         SET state = CASE WHEN attempts >= max_attempts THEN 'dead' ELSE 'queued' END,
             run_at = now(), last_error = 'lease expired', error_class = 'lease',
             locked_by = NULL, locked_until = NULL,
             finished_at = CASE WHEN attempts >= max_attempts THEN now() END
         WHERE state = 'running' AND locked_until < now()
         RETURNING queue, state = 'dead'",
    )
    .fetch_all(&mut *tx)
    .await?;
    let queued: Vec<String> = reaped
        .iter()
        .filter(|(_, dead)| !dead)
        .map(|(q, _)| q.clone())
        .collect();
    notify(&mut tx, &queued).await?;
    tx.commit().await?;
    Ok(reaped)
}

/// A job's state and what its run left. `None` when there is no such job.
pub async fn job(pool: &PgPool, id: i64) -> Result<Option<Ended>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, Option<Value>, Option<String>, Option<String>)>(
        "SELECT state, result, last_error, error_class FROM jobs WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(state, result, error, class)| {
        let detail = result
            .as_ref()
            .and_then(|r| r.get("detail"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let value = result.and_then(|mut r| r.get_mut("value").map(Value::take));
        Ended {
            state,
            detail,
            value,
            error,
            class,
        }
    }))
}

type StateRow = (
    String,
    String,
    Option<String>,
    i32,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
    Option<String>,
    Option<String>,
    Option<i32>,
    Option<String>,
    i64,
);

/// The schedules of these keys, by key, with the state of the open job of each and its runs
/// that ended since `since`.
pub async fn states(
    pool: &PgPool,
    since: DateTime<Utc>,
    keys: &[&str],
) -> Result<Vec<ScheduleState>, sqlx::Error> {
    let rows = sqlx::query_as::<_, StateRow>(
        "SELECT s.key, s.kind, s.integration, s.every_secs, s.next_run_at, s.last_run_at,
                s.last_error, s.last_detail, s.last_took_ms,
                (SELECT j.state FROM jobs j
                 WHERE j.schedule_key = s.key AND j.state IN ('queued', 'running') LIMIT 1),
                (SELECT count(*) FROM jobs j
                 WHERE j.schedule_key = s.key AND j.state IN ('done', 'failed')
                   AND j.finished_at >= $1)
         FROM job_schedules s WHERE s.key = ANY($2) ORDER BY s.key",
    )
    .bind(since)
    .bind(keys)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(
                key,
                kind,
                integration,
                every,
                next_run_at,
                last_run_at,
                last_error,
                last_detail,
                last_took_ms,
                open,
                runs,
            )| {
                ScheduleState {
                    key,
                    kind,
                    integration,
                    every: Duration::from_secs(u64::try_from(every).unwrap_or(1)),
                    next_run_at,
                    last_run_at,
                    last_error,
                    last_detail,
                    last_took_ms,
                    open,
                    runs,
                }
            },
        )
        .collect())
}

/// Remove the done and cancelled jobs that ended more than `done` ago, the failed and dead ones
/// more than `failed` ago. Returns how many went.
pub async fn tidy(pool: &PgPool, done: Duration, failed: Duration) -> Result<u64, sqlx::Error> {
    sqlx::query(
        "DELETE FROM jobs
         WHERE (state IN ('done', 'cancelled')
                AND finished_at < now() - make_interval(secs => $1::float8))
            OR (state IN ('failed', 'dead')
                AND finished_at < now() - make_interval(secs => $2::float8))",
    )
    .bind(done.as_secs_f64())
    .bind(failed.as_secs_f64())
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
}

/// What an ask for a run now found or did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    /// No schedule that is on has this key.
    Unknown,
    /// A new job, queued: its id and its queue.
    Queued { id: i64, queue: String },
    /// A job of the schedule was queued already. Its run starts after the ask.
    Waiting(i64),
    /// A job of the schedule is running. Its run started before the ask.
    Running(i64),
}

/// Ask a schedule for a run now: one job queued at once with `payload`, unless one of its jobs is
/// queued or running. A new job wakes its queue in every process.
pub async fn ask(pool: &PgPool, key: &str, payload: Value) -> Result<Asked, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let schedule = sqlx::query_as::<_, (String, String, Option<String>)>(
        "SELECT kind, queue, integration FROM job_schedules WHERE key = $1 AND enabled FOR UPDATE",
    )
    .bind(key)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((kind, queue, integration)) = schedule else {
        return Ok(Asked::Unknown);
    };
    // While the schedule's row is held, only the reaper can end its open job. The next turn of
    // the loop then queues one.
    loop {
        let queued = sqlx::query_scalar::<_, i64>(
            "INSERT INTO jobs (kind, queue, schedule_key, integration, payload, state, run_at)
             VALUES ($1, $2, $3, $4, $5, 'queued', now())
             ON CONFLICT (schedule_key) WHERE state IN ('queued', 'running') DO NOTHING
             RETURNING id",
        )
        .bind(&kind)
        .bind(&queue)
        .bind(key)
        .bind(&integration)
        .bind(&payload)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(id) = queued {
            notify(&mut tx, std::slice::from_ref(&queue)).await?;
            tx.commit().await?;
            return Ok(Asked::Queued { id, queue });
        }
        let open = sqlx::query_as::<_, (i64, String)>(
            "SELECT id, state FROM jobs
             WHERE schedule_key = $1 AND state IN ('queued', 'running')",
        )
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((id, state)) = open {
            tx.commit().await?;
            return Ok(if state == "queued" {
                Asked::Waiting(id)
            } else {
                Asked::Running(id)
            });
        }
    }
}

/// Give the jobs this process still runs back to their queues at once, their attempt returned: a
/// stop during a run uses no attempt. Returns how many went back.
pub async fn give_back(pool: &PgPool, me: &str) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let queues = sqlx::query_scalar::<_, String>(
        "UPDATE jobs
         SET state = 'queued', run_at = now(), attempts = GREATEST(attempts - 1, 0),
             locked_by = NULL, locked_until = NULL,
             last_error = 'stopped before its end', error_class = 'stopped'
         WHERE state = 'running' AND starts_with(locked_by, $1)
         RETURNING queue",
    )
    .bind(format!("{me}:"))
    .fetch_all(&mut *tx)
    .await?;
    notify(&mut tx, &queues).await?;
    tx.commit().await?;
    Ok(u64::try_from(queues.len()).unwrap_or(u64::MAX))
}
