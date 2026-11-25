//! The loops this process runs, as jobs: each loop registers itself when it starts, says when
//! it begins and ends a run, and waits through [`Job::wait`], which a request to run it now
//! cuts short. `GET /api/v1/jobs` lists them and `POST /api/v1/jobs/{id}/run` wakes one.
//!
//! The registry is the process's own, like its metrics: one set of loops per process. What it
//! knows (the last run, its result and how long it took) is since the process started; the
//! integrations' lasting state is `integration_health` (one row per pass) and `engine_state`
//! (what the passes found). Each run of an integration's job is also a
//! check of that integration, kept with the tests made from the Settings page
//! ([`Jobs::record_check`]), the last few per integration.

use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Checks kept per integration.
const CHECKS_KEPT: usize = 30;

/// What a loop is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// `radarr:titles`, `qbittorrent:downloads`, `folder:music`.
    pub id: String,
    /// What the loop does: `titles`, `queue`, `history`, `downloads`, `server`, `requests`,
    /// `music`, `home-movies`, `movie-folders`.
    pub pass: String,
    /// The integration it reads, when it reads one.
    pub integration: Option<String>,
    /// How long it waits between runs.
    pub every: Duration,
}

impl JobSpec {
    /// The job of one integration: `radarr:titles`.
    pub fn of(integration: &str, pass: &str, every: Duration) -> JobSpec {
        JobSpec {
            id: format!("{integration}:{pass}"),
            pass: pass.to_string(),
            integration: Some(integration.to_string()),
            every,
        }
    }

    /// A job of Singularr's own, a folder scan: `folder:music`.
    pub fn folder(pass: &str, every: Duration) -> JobSpec {
        JobSpec {
            id: format!("folder:{pass}"),
            pass: pass.to_string(),
            integration: None,
            every,
        }
    }
}

/// How a job is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobState {
    pub last_run: Option<DateTime<Utc>>,
    /// The last run succeeded.
    pub last_ok: Option<bool>,
    /// What the last run did, or why it failed.
    pub last_detail: Option<String>,
    pub took_ms: Option<i64>,
    pub next_run: Option<DateTime<Utc>>,
    pub running: bool,
    pub runs: u64,
}

/// One check of an integration: a run of one of its jobs, or a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub at: DateTime<Utc>,
    pub ok: bool,
    /// What was asked: a job's pass (`titles`), or `test`.
    pub via: String,
    pub ms: i64,
    pub detail: String,
}

pub struct Job {
    pub spec: JobSpec,
    state: Mutex<JobState>,
    wake: Notify,
}

/// Why a job was not started now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunNow {
    Started,
    /// It is running; this run's end is the next result.
    Running,
}

fn millis(d: Duration) -> i64 {
    i64::try_from(d.as_millis()).unwrap_or(i64::MAX)
}

impl Job {
    pub fn state(&self) -> JobState {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// A run starts. Returns when it started, for [`Job::end`].
    pub fn begin(&self) -> Instant {
        let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        s.running = true;
        s.next_run = None;
        Instant::now()
    }

    /// A run ended: what it did (`Ok`) or why it failed (`Err`). A run of an integration's job
    /// is also a check of the integration.
    pub fn end(&self, started: Instant, outcome: Result<String, String>) {
        let took = millis(started.elapsed());
        let now = Utc::now();
        let ok = outcome.is_ok();
        let detail = match outcome {
            Ok(d) | Err(d) => d,
        };
        {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            s.running = false;
            s.last_run = Some(now);
            s.last_ok = Some(ok);
            s.last_detail = Some(detail.clone());
            s.took_ms = Some(took);
            s.runs += 1;
        }
        if let Some(integration) = &self.spec.integration {
            registry().record_check(
                integration,
                Check {
                    at: now,
                    ok,
                    via: self.spec.pass.clone(),
                    ms: took,
                    detail,
                },
            );
        }
    }

    /// Wait `pause`, or less when someone asks for a run now.
    pub async fn wait(&self, pause: Duration) {
        {
            let mut s = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            s.next_run = chrono::Duration::from_std(pause)
                .ok()
                .map(|d| Utc::now() + d);
        }
        tokio::select! {
            _ = tokio::time::sleep(pause) => {}
            _ = self.wake.notified() => {}
        }
    }

    /// Ask for a run now. A job that is running finishes its run first and is not asked again.
    pub fn run_now(&self) -> RunNow {
        if self.state().running {
            return RunNow::Running;
        }
        self.wake.notify_one();
        RunNow::Started
    }
}

/// Every job of this process, and the recent checks of each integration.
#[derive(Default)]
pub struct Jobs {
    jobs: Mutex<BTreeMap<String, Arc<Job>>>,
    checks: Mutex<BTreeMap<String, VecDeque<Check>>>,
}

static REGISTRY: LazyLock<Jobs> = LazyLock::new(Jobs::default);

/// The registry of this process.
pub fn registry() -> &'static Jobs {
    &REGISTRY
}

impl Jobs {
    /// A loop starts: its job. A job registered again under the same id (a loop started anew)
    /// replaces the old one.
    pub fn register(&self, spec: JobSpec) -> Arc<Job> {
        let job = Arc::new(Job {
            spec,
            state: Mutex::new(JobState::default()),
            wake: Notify::new(),
        });
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(job.spec.id.clone(), job.clone());
        job
    }

    /// Every job, by id.
    pub fn list(&self) -> Vec<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(id)
            .cloned()
    }

    /// The jobs that read one integration.
    pub fn of_integration(&self, integration: &str) -> Vec<Arc<Job>> {
        self.list()
            .into_iter()
            .filter(|j| j.spec.integration.as_deref() == Some(integration))
            .collect()
    }

    pub fn record_check(&self, integration: &str, check: Check) {
        let mut all = self.checks.lock().unwrap_or_else(PoisonError::into_inner);
        let list = all.entry(integration.to_string()).or_default();
        list.push_front(check);
        list.truncate(CHECKS_KEPT);
    }

    /// The recent checks of an integration, newest first.
    pub fn checks(&self, integration: &str) -> Vec<Check> {
        self.checks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(integration)
            .map(|l| l.iter().cloned().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_job_records_its_runs_and_wakes_when_asked() {
        let jobs = Jobs::default();
        let job = jobs.register(JobSpec::of("test-arr", "titles", Duration::from_secs(3600)));
        assert_eq!(job.spec.id, "test-arr:titles");
        assert_eq!(jobs.of_integration("test-arr").len(), 1);
        let started = job.begin();
        assert!(job.state().running);
        assert_eq!(
            job.run_now(),
            RunNow::Running,
            "a running job is not asked again"
        );
        job.end(started, Ok("3 titles".into()));
        let s = job.state();
        assert_eq!(
            (s.running, s.last_ok, s.last_detail.as_deref(), s.runs),
            (false, Some(true), Some("3 titles"), 1)
        );
        // The run is a check of its integration, in the process's registry.
        let check = registry().checks("test-arr").into_iter().next().unwrap();
        assert_eq!((check.ok, check.via.as_str()), (true, "titles"));

        // A wait of an hour ends when someone asks for a run now.
        let waiting = {
            let job = job.clone();
            tokio::spawn(async move { job.wait(Duration::from_secs(3600)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(job.state().next_run.is_some());
        assert_eq!(job.run_now(), RunNow::Started);
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the wait ended")
            .unwrap();
        job.end(job.begin(), Err("connection refused".into()));
        assert_eq!(job.state().last_ok, Some(false));
    }

    #[test]
    fn checks_keep_the_newest_few() {
        let jobs = Jobs::default();
        for n in 0..(CHECKS_KEPT + 5) {
            jobs.record_check(
                "x",
                Check {
                    at: Utc::now(),
                    ok: true,
                    via: "test".into(),
                    ms: n as i64,
                    detail: String::new(),
                },
            );
        }
        let kept = jobs.checks("x");
        assert_eq!(kept.len(), CHECKS_KEPT);
        assert_eq!(kept[0].ms, (CHECKS_KEPT + 4) as i64, "newest first");
    }
}
