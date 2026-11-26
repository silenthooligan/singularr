//! The durable job runtime (platform design, section 4). A schedule is a row of `job_schedules`:
//! a key, a handler (`kind`), a queue and an interval. A job is a row of `jobs`: one run.
//!
//! - The tick looks every second and on a wake-up. Each due schedule without an open job gets
//!   one queued job. A unique index holds one queued or running job per schedule: two runs of
//!   one schedule never overlap.
//! - A worker of a queue claims the oldest ready job with a lease (`locked_until`). A heartbeat
//!   beside the run renews the lease every third of it. A run whose lease is lost is dropped.
//!   It commits nothing: its transactions prove the lease ([`Lease::prove`], a fenced
//!   [`Journal`](crate::journal::Journal)) or end the job in their own commit
//!   ([`Lease::finish`], [`JournalTx::finish`](crate::journal::JournalTx::finish)). Every
//!   writer locks a schedule's row before the rows of its jobs.
//! - The job ends `done` or `failed`. Its schedule runs again after its interval. A failed run
//!   backs the schedule off.
//! - A handler that panics fails its job. Its worker goes on with the next job. A run longer
//!   than its kind's limit is dropped and fails.
//! - The reaper gives a job whose lease ran out back to its queue. After its last attempt the
//!   job is `dead`. Once an hour it removes the jobs that ended long ago: a done job after 2
//!   days, a failed or dead job after 14.
//! - A caller may wait on a run it asked for ([`run_and_wait`]): a scan asked for by the API
//!   answers with its run's report.
//! - A wake-up is `NOTIFY singularr_jobs` with a queue's name, sent in the transaction that
//!   queues the job. One listener per process wakes the tick and the workers of that queue. The
//!   workers also look on their own every few seconds.
//! - Each task of the runtime is supervised. A task that ends or panics starts again after a
//!   pause. A stop gives running jobs a grace period. A job still running then is dropped and
//!   goes back to its queue with its attempt returned.
//!
//! The runtime's pool serves its own statements. The listener holds one connection. The tick and
//! the reaper use one each. Each worker uses up to two: its claims and ends, the heartbeat of its
//! run. [`connections`] gives that size. A pool of its own keeps a heartbeat from waiting behind
//! the handlers' queries.

pub mod store;

use crate::journal::Fence;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures::FutureExt;
use serde_json::{Value, json};
use sqlx::postgres::PgListener;
use sqlx::{PgConnection, PgPool};
use std::collections::BTreeMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

/// The channel of the wake-ups. A notification's payload names a queue.
pub const CHANNEL: &str = "singularr_jobs";

/// A done job stays this long. A cancelled one too.
const KEEP_DONE: Duration = Duration::from_secs(2 * 86_400);

/// A failed or dead job stays this long. Dead jobs stay on the Jobs page for 14 days (section
/// 4.4).
const KEEP_FAILED: Duration = Duration::from_secs(14 * 86_400);

/// The reaper removes old jobs this often.
const TIDY: Duration = Duration::from_secs(3600);

/// Where a schedule's interval comes from, by the names the settings give their sources.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Source {
    #[default]
    Default,
    Environment,
    File,
    Saved,
}

impl Source {
    /// The name `job_schedules.source` holds.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::Environment => "environment",
            Source::File => "file",
            Source::Saved => "saved",
        }
    }
}

/// How often a handler runs and on which queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// `folder:music`, `singularr:index`.
    pub key: String,
    /// The handler that runs it: `scan.music`.
    pub kind: String,
    /// The integration it reads, when it reads one.
    pub integration: Option<String>,
    /// `scan`, `index`, `notify`, `sync`, `default`.
    pub queue: String,
    pub every: Duration,
    /// The shortest interval a setting may give it.
    pub floor: Duration,
    /// No run comes sooner than this after a start: each start brings the next run this far
    /// away at least. The first run of a new schedule waits it, then a random part of
    /// `jitter`.
    pub first: Duration,
    /// The random part of the first run's wait: at most this.
    pub jitter: Duration,
    /// The schedule runs after each start too. A start brings its next run to `first` and a
    /// random part of `jitter` away at the latest. Without it a known schedule keeps its next
    /// run across starts.
    pub on_start: bool,
    /// How long one run holds its job between two renewals.
    pub lease: Duration,
    /// Where `every` comes from.
    pub source: Source,
}

/// One job as its handler gets it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub schedule: Option<String>,
    pub integration: Option<String>,
    pub payload: Value,
    /// 1 for the first attempt.
    pub attempt: i32,
}

/// What a handler runs with: its job and the lease it holds the job under.
pub struct Cx {
    pub job: Job,
    pub lease: Lease,
}

/// What a run did. `detail` is its line on the Jobs page. `value` is for whoever waits on the
/// run: a scan's report. `again` sets the schedule's next run in place of its interval.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Done {
    pub detail: String,
    pub value: Option<Value>,
    pub again: Option<Duration>,
}

impl Done {
    /// What `jobs.result` keeps of it.
    fn stored(&self) -> Value {
        match &self.value {
            Some(value) => json!({ "detail": self.detail, "value": value }),
            None => json!({ "detail": self.detail }),
        }
    }
}

impl From<String> for Done {
    fn from(detail: String) -> Done {
        Done {
            detail,
            ..Done::default()
        }
    }
}

impl From<&str> for Done {
    fn from(detail: &str) -> Done {
        Done::from(detail.to_string())
    }
}

/// Why a run failed. `class` sorts the failure for whoever waits on the run: a problem's code,
/// `timeout`. `again` sets the schedule's next run in place of the backoff: a handler that keeps
/// its cadence through failures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Failed {
    pub error: String,
    pub class: Option<String>,
    pub again: Option<Duration>,
}

impl From<String> for Failed {
    fn from(error: String) -> Failed {
        Failed {
            error,
            ..Failed::default()
        }
    }
}

impl From<&str> for Failed {
    fn from(error: &str) -> Failed {
        Failed::from(error.to_string())
    }
}

/// How the one-off jobs of a kind are tried again after a failure. The delay is `base`, doubled
/// with each attempt up to `cap`, with 50 percent jitter. After `attempts` the job is dead. The
/// end of a failed run writes the number into the job's row. The reaper reads it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retry {
    pub base: Duration,
    pub cap: Duration,
    pub attempts: i32,
}

impl Default for Retry {
    fn default() -> Retry {
        Retry {
            base: Duration::from_secs(30),
            cap: Duration::from_secs(1800),
            attempts: 5,
        }
    }
}

/// The work of one kind of job.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    /// Run one job. `Ok` says what the run did, `Err` why it failed. A handler that writes the
    /// pass's data proves its lease in those transactions. It may end the job in the last one
    /// ([`Lease::finish`]). The runtime writes what `run` returns while the job is still open.
    async fn run(&self, cx: Cx) -> Result<Done, Failed>;

    /// The longest one run may take. A run still going then is dropped and its job fails.
    fn limit(&self) -> Duration {
        Duration::from_secs(3600)
    }

    /// How a one-off job of this kind is tried again.
    fn retry(&self) -> Retry {
        Retry::default()
    }
}

/// The hold of one run on its job. A transaction that writes the run's data proves the hold
/// ([`Lease::prove`]) or ends the job ([`Lease::finish`]). Both fail with [`LeaseError::Lost`]
/// once the reaper gave the job back. The caller then rolls back: a run whose lease is lost
/// commits nothing.
#[derive(Debug, Clone)]
pub struct Lease {
    id: i64,
    locker: String,
    length: Duration,
    schedule: Option<String>,
    attempt: i32,
    retry: Retry,
    started: Instant,
    /// Where the lease runs out as last renewed. `None` once it is lost.
    edge: watch::Receiver<Option<Instant>>,
}

/// Why a write under a lease did not go through.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// The job is not this run's any more.
    #[error("the run's lease is lost: it commits nothing more")]
    Lost,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl Lease {
    /// The job's id.
    pub fn job(&self) -> i64 {
        self.id
    }

    /// How long each renewal of the lease lasts.
    pub fn length(&self) -> Duration {
        self.length
    }

    /// When the lease runs out as last renewed. A call should end before it. `None` once the
    /// lease is lost.
    pub fn deadline(&self) -> Option<Instant> {
        *self.edge.borrow()
    }

    /// The lease is lost. The runtime drops the run soon.
    pub fn lost(&self) -> bool {
        self.deadline().is_none()
    }

    /// Prove the lease in the caller's transaction and renew it there. The schedule's row is
    /// locked first, as the end of a run locks it. The job's row stays locked until the commit.
    /// The reaper cannot take the job before it.
    pub async fn prove(&self, conn: &mut PgConnection) -> Result<(), LeaseError> {
        if let Some(key) = &self.schedule {
            store::lock_schedule(&mut *conn, key).await?;
        }
        if store::renew(&mut *conn, self.id, &self.locker, self.length).await? {
            Ok(())
        } else {
            Err(LeaseError::Lost)
        }
    }

    /// End the job in the caller's transaction, as the last write before its commit: the job's
    /// row, then its schedule's next run or the backoff of a failure. The commit makes the run's
    /// data and its end one change. The runtime then writes nothing more for the run. A
    /// transaction of a fenced journal ends a run with [`JournalTx::finish`](crate::journal::JournalTx::finish).
    pub async fn finish(
        &self,
        conn: &mut PgConnection,
        outcome: &Result<Done, Failed>,
    ) -> Result<(), LeaseError> {
        if store::end(conn, self, outcome).await? {
            Ok(())
        } else {
            Err(LeaseError::Lost)
        }
    }
}

/// The commit of a fenced journal failed: the run's lease was lost. Nothing of it was written.
pub fn lost(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::InvalidArgument(words) if *words == LeaseError::Lost.to_string())
}

/// A fenced journal's transactions prove the lease before they commit.
#[async_trait]
impl Fence for Lease {
    async fn check(&self, conn: &mut PgConnection) -> Result<(), sqlx::Error> {
        match self.prove(conn).await {
            Ok(()) => Ok(()),
            Err(LeaseError::Lost) => {
                Err(sqlx::Error::InvalidArgument(LeaseError::Lost.to_string()))
            }
            Err(LeaseError::Db(e)) => Err(e),
        }
    }
}

/// How often the runtime looks. The defaults are the design's. Tests make them shorter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// The tick turns due schedules into jobs.
    pub tick: Duration,
    /// A worker with nothing to do looks again after this.
    pub poll: Duration,
    /// The reaper looks for leases that ran out.
    pub reap: Duration,
    /// The lease of a job without a schedule.
    pub lease: Duration,
    /// At a stop, running jobs have this long to finish.
    pub grace: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing {
            tick: Duration::from_secs(1),
            poll: Duration::from_secs(5),
            reap: Duration::from_secs(10),
            lease: Duration::from_secs(120),
            grace: Duration::from_secs(10),
        }
    }
}

/// What a request to run a schedule now did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunNow {
    /// Its job is queued at once.
    Started,
    /// One of its jobs is queued or running. Nothing more starts.
    Running,
    /// No schedule that is on has this key.
    Unknown,
}

impl From<&store::Asked> for RunNow {
    fn from(asked: &store::Asked) -> RunNow {
        match asked {
            store::Asked::Queued { .. } => RunNow::Started,
            store::Asked::Waiting(_) | store::Asked::Running(_) => RunNow::Running,
            store::Asked::Unknown => RunNow::Unknown,
        }
    }
}

/// Ask a schedule to run now, from any process. Its job is queued at once. Its queue's workers
/// wake through the channel. Nothing starts while one of its jobs is queued or running.
pub async fn run_now(pool: &PgPool, key: &str) -> Result<RunNow, sqlx::Error> {
    Ok(RunNow::from(&store::ask(pool, key, json!({})).await?))
}

/// How a job's run ended, for whoever waited on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    /// `done`, `failed`, `dead` or `cancelled`.
    pub state: String,
    /// The run's line, when it succeeded.
    pub detail: Option<String>,
    /// What the run left for whoever waits on it.
    pub value: Option<Value>,
    pub error: Option<String>,
    /// The failure's class: a problem's code, `timeout`, `lease`.
    pub class: Option<String>,
}

impl Ended {
    /// The run succeeded.
    pub fn ok(&self) -> bool {
        self.state == "done"
    }
}

/// Ask for a run of a schedule that starts after this moment. The run is a new job with
/// `payload`. It is the job queued for the schedule already when there is one. While one of its
/// jobs runs, the ask waits for that run's end. It then asks again. Returns the job. `None`: no
/// schedule that is on has this key.
pub async fn run_after(
    pool: &PgPool,
    key: &str,
    payload: Value,
) -> Result<Option<i64>, sqlx::Error> {
    loop {
        match store::ask(pool, key, payload.clone()).await? {
            store::Asked::Unknown => return Ok(None),
            store::Asked::Queued { id, .. } | store::Asked::Waiting(id) => return Ok(Some(id)),
            store::Asked::Running(id) => {
                ended(pool, id).await?;
            }
        }
    }
}

/// Run a schedule after this moment as [`run_after`] asks. Wait for that run's end. `None`: no
/// schedule that is on has this key. `None` too when the run's job is gone.
pub async fn run_and_wait(
    pool: &PgPool,
    key: &str,
    payload: Value,
) -> Result<Option<Ended>, sqlx::Error> {
    match run_after(pool, key, payload).await? {
        Some(id) => ended(pool, id).await,
        None => Ok(None),
    }
}

/// Wait for a job's end: its row is read again until the job is no longer queued or running.
/// `None` when its row is gone.
pub async fn ended(pool: &PgPool, id: i64) -> Result<Option<Ended>, sqlx::Error> {
    let mut delay = Duration::from_millis(50);
    loop {
        match store::job(pool, id).await? {
            Some(now) if matches!(now.state.as_str(), "queued" | "running") => {}
            other => return Ok(other),
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(1));
    }
}

/// A schedule as the Jobs page reads it: its row, the state of its open job, its runs since a
/// moment.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleState {
    pub key: String,
    pub kind: String,
    pub integration: Option<String>,
    pub every: Duration,
    pub next_run_at: DateTime<Utc>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub last_detail: Option<String>,
    pub last_took_ms: Option<i32>,
    /// `queued` or `running` while one of its jobs is open.
    pub open: Option<String>,
    /// Its runs that ended since the moment asked about.
    pub runs: i64,
}

/// The schedules of these keys, by key, with how their runs went. `runs` counts the runs that
/// ended since `since`.
pub async fn states(
    pool: &PgPool,
    since: DateTime<Utc>,
    keys: &[&str],
) -> Result<Vec<ScheduleState>, sqlx::Error> {
    store::states(pool, since, keys).await
}

/// The size of a pool of the runtime's own for `workers` workers in all: the listener, the tick,
/// the reaper and two for each worker.
pub fn connections(workers: usize) -> u32 {
    u32::try_from(3 + 2 * workers).unwrap_or(u32::MAX)
}

/// The runtime of one process, before it starts: its handlers and its workers per queue.
pub struct Runtime {
    pool: PgPool,
    me: String,
    handlers: BTreeMap<String, Arc<dyn Handler>>,
    queues: BTreeMap<String, usize>,
    timing: Timing,
}

/// The state the tasks of a started runtime share.
struct Shared {
    pool: PgPool,
    me: String,
    handlers: BTreeMap<String, Arc<dyn Handler>>,
    timing: Timing,
    tick: Notify,
    queues: BTreeMap<String, Notify>,
    /// Tasks started again after they ended or panicked.
    restarts: AtomicUsize,
}

impl Shared {
    /// Wake the workers of a queue: those that wait now and the next one to wait.
    fn wake(&self, queue: &str) {
        if let Some(n) = self.queues.get(queue) {
            n.notify_waiters();
            n.notify_one();
        }
    }
}

/// The runtime stops when it is asked to. It stops too when its handle is gone.
fn stopped(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow() || stop.has_changed().is_err()
}

/// A started runtime: its tasks and the way to stop them.
pub struct Running {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    workers: Vec<JoinHandle<()>>,
    others: Vec<JoinHandle<()>>,
}

impl Runtime {
    /// A runtime over this pool, its process named by a new id. The pool serves the runtime's
    /// own statements alone ([`connections`]).
    pub fn new(pool: PgPool, timing: Timing) -> Runtime {
        Runtime {
            pool,
            me: uuid::Uuid::new_v4().to_string(),
            handlers: BTreeMap::new(),
            queues: BTreeMap::new(),
            timing,
        }
    }

    /// The handler of one kind of job.
    pub fn handle(mut self, kind: &str, handler: Arc<dyn Handler>) -> Runtime {
        self.handlers.insert(kind.to_string(), handler);
        self
    }

    /// The runtime has a handler for jobs of this kind.
    pub fn handles(&self, kind: &str) -> bool {
        self.handlers.contains_key(kind)
    }

    /// How many workers a queue runs. The app gives the sync queue one per schedule and the scan
    /// queue one per folder scan. The design's defaults for the others are index 1, notify 2,
    /// default 2.
    pub fn workers(mut self, queue: &str, n: usize) -> Runtime {
        self.queues.insert(queue.to_string(), n.max(1));
        self
    }

    /// Write the schedules of this process ([`store::reconcile`]).
    pub async fn reconcile(pool: &PgPool, schedules: &[Schedule]) -> Result<(), sqlx::Error> {
        store::reconcile(pool, schedules).await
    }

    /// Give a schedule another interval while the process runs ([`store::set_every`]).
    pub async fn set_every(
        pool: &PgPool,
        key: &str,
        every: Duration,
        source: Source,
    ) -> Result<bool, sqlx::Error> {
        store::set_every(pool, key, every, source).await
    }

    /// Start the tick, the reaper, the listener and the workers, each supervised.
    pub fn start(self) -> Running {
        let (stop, stopped) = watch::channel(false);
        let shared = Arc::new(Shared {
            pool: self.pool,
            me: self.me,
            handlers: self.handlers,
            timing: self.timing,
            tick: Notify::new(),
            queues: self
                .queues
                .keys()
                .map(|q| (q.clone(), Notify::new()))
                .collect(),
            restarts: AtomicUsize::new(0),
        });
        let mut others = Vec::new();
        let s = shared.clone();
        others.push(supervise(
            "tick",
            shared.clone(),
            stopped.clone(),
            move |stop| tick(s.clone(), stop),
        ));
        let s = shared.clone();
        others.push(supervise(
            "reaper",
            shared.clone(),
            stopped.clone(),
            move |stop| reap(s.clone(), stop),
        ));
        let s = shared.clone();
        others.push(supervise(
            "listener",
            shared.clone(),
            stopped.clone(),
            move |stop| listen(s.clone(), stop),
        ));
        let mut workers = Vec::new();
        for (queue, n) in &self.queues {
            for i in 0..*n {
                let s = shared.clone();
                let queue = queue.clone();
                workers.push(supervise(
                    "worker",
                    shared.clone(),
                    stopped.clone(),
                    move |stop| work(s.clone(), queue.clone(), i, stop),
                ));
            }
        }
        Running {
            shared,
            stop,
            workers,
            others,
        }
    }
}

impl Running {
    /// Ask a schedule to run now. Its job is queued at once. This process's workers of its queue
    /// wake. Nothing starts while one of its jobs is queued or running.
    pub async fn run_now(&self, key: &str) -> Result<RunNow, sqlx::Error> {
        let asked = store::ask(&self.shared.pool, key, json!({})).await?;
        if let store::Asked::Queued { queue, .. } = &asked {
            self.shared.wake(queue);
        }
        Ok(RunNow::from(&asked))
    }

    /// The tasks started again since the start: each ended or panicked. A handler's panic is not
    /// one of them. Its worker goes on.
    pub fn restarts(&self) -> usize {
        self.shared.restarts.load(Ordering::Relaxed)
    }

    /// Stop. The workers claim nothing more. Running jobs have the grace period to finish. A job
    /// still running then is dropped. It goes back to its queue with its attempt returned: 2 s
    /// at most for that. Then every task ends. A stop takes the grace period and 3 s at most.
    pub async fn stop(self) {
        let Running {
            shared,
            stop,
            mut workers,
            others,
        } = self;
        let _ = stop.send(true);
        shared.tick.notify_waiters();
        for n in shared.queues.values() {
            n.notify_waiters();
        }
        // The workers that end within the grace period, one after the other.
        let mut ended = 0;
        let in_time = tokio::time::timeout(shared.timing.grace, async {
            for w in workers.iter_mut() {
                let _ = w.await;
                ended += 1;
            }
        })
        .await;
        if in_time.is_err() {
            let late = &mut workers[ended..];
            for w in late.iter() {
                w.abort();
            }
            let _ = tokio::time::timeout(Duration::from_secs(1), async {
                for w in late.iter_mut() {
                    let _ = w.await;
                }
            })
            .await;
            let given = tokio::time::timeout(
                Duration::from_secs(2),
                store::give_back(&shared.pool, &shared.me),
            )
            .await;
            match given {
                Ok(Ok(n)) => {
                    tracing::warn!(target: "singularr::runtime", jobs = n, "jobs still running after the grace period went back to their queue")
                }
                Ok(Err(e)) => {
                    tracing::warn!(target: "singularr::runtime", error = %e, "jobs still running after the grace period could not be given back: the reaper gives them back after their lease")
                }
                Err(_) => {
                    tracing::warn!(target: "singularr::runtime", "jobs still running after the grace period were not given back within 2 s: the reaper gives them back after their lease")
                }
            }
        }
        for t in others {
            t.abort();
        }
    }

    /// End every task at once, as a process that is killed ends. Running jobs keep their rows
    /// until the reaper gives them back.
    #[cfg(feature = "testkit")]
    pub fn abort(self) {
        for t in self.workers.into_iter().chain(self.others) {
            t.abort();
        }
    }
}

/// Run a task and start it again when it ends or panics, after a pause from 1 s to 60 s, until
/// the runtime stops.
fn supervise<F, Fut>(
    name: &'static str,
    shared: Arc<Shared>,
    mut stop: watch::Receiver<bool>,
    task: F,
) -> JoinHandle<()>
where
    F: Fn(watch::Receiver<bool>) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        let mut delay = Duration::from_secs(1);
        loop {
            let began = Instant::now();
            let ended = AssertUnwindSafe(task(stop.clone())).catch_unwind().await;
            if stopped(&stop) {
                return;
            }
            match ended {
                Ok(()) => {
                    tracing::warn!(target: "singularr::runtime", task = name, "ended: it starts again after a pause")
                }
                Err(_) => {
                    tracing::error!(target: "singularr::runtime", task = name, "panicked: it starts again after a pause")
                }
            }
            shared.restarts.fetch_add(1, Ordering::Relaxed);
            // A task that ran a while starts the pauses over.
            if began.elapsed() >= Duration::from_secs(60) {
                delay = Duration::from_secs(1);
            }
            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = stop.changed() => {}
            }
            if stopped(&stop) {
                return;
            }
            delay = (delay * 2).min(Duration::from_secs(60));
        }
    })
}

/// Wait for a wake-up, the timeout or the stop.
async fn pause(wake: &Notify, timeout: Duration, stop: &mut watch::Receiver<bool>) {
    tokio::select! {
        _ = wake.notified() => {}
        _ = tokio::time::sleep(timeout) => {}
        _ = stop.changed() => {}
    }
}

/// The tick: due schedules become queued jobs. Their queues' workers wake.
async fn tick(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    while !stopped(&stop) {
        match store::queue_due(&shared.pool).await {
            Ok(queues) => {
                for q in queues {
                    shared.wake(&q);
                }
            }
            Err(e) => {
                tracing::warn!(target: "singularr::runtime", error = %e, "the tick could not read the schedules")
            }
        }
        pause(&shared.tick, shared.timing.tick, &mut stop).await;
    }
}

/// The reaper: a job whose lease ran out goes back to its queue. After its last attempt it is
/// dead. Once an hour the reaper removes the jobs that ended long ago ([`KEEP_DONE`],
/// [`KEEP_FAILED`]).
async fn reap(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let never = Notify::new();
    let mut tidied: Option<Instant> = None;
    while !stopped(&stop) {
        if tidied.is_none_or(|at| at.elapsed() >= TIDY) {
            match store::tidy(&shared.pool, KEEP_DONE, KEEP_FAILED).await {
                Ok(0) => {}
                Ok(n) => {
                    tracing::info!(target: "singularr::runtime", jobs = n, "old jobs were removed: a done job after 2 days, a failed or dead one after 14")
                }
                Err(e) => {
                    tracing::warn!(target: "singularr::runtime", error = %e, "old jobs could not be removed")
                }
            }
            tidied = Some(Instant::now());
        }
        match store::reap(&shared.pool).await {
            Ok(reaped) => {
                for (queue, dead) in reaped {
                    if dead {
                        tracing::warn!(target: "singularr::runtime", queue = %queue, "a lease ran out at the job's last attempt: the job is dead");
                    } else {
                        tracing::warn!(target: "singularr::runtime", queue = %queue, "a lease ran out: its job is queued again");
                        shared.wake(&queue);
                    }
                }
            }
            Err(e) => {
                tracing::warn!(target: "singularr::runtime", error = %e, "the reaper could not read the jobs")
            }
        }
        pause(&never, shared.timing.reap, &mut stop).await;
    }
}

/// The listener: a notification on the channel wakes the tick and the queue it names. A lost
/// connection ends the task. The supervisor connects again after its pause.
async fn listen(shared: Arc<Shared>, mut stop: watch::Receiver<bool>) {
    let mut listener = match PgListener::connect_with(&shared.pool).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(target: "singularr::runtime", error = %e, "the listener could not connect");
            return;
        }
    };
    listener.eager_reconnect(false);
    if let Err(e) = listener.listen(CHANNEL).await {
        tracing::warn!(target: "singularr::runtime", error = %e, "the listener could not listen");
        return;
    }
    // What came while no listener was connected: one look by the tick and by every queue.
    shared.tick.notify_one();
    for queue in shared.queues.keys() {
        shared.wake(queue);
    }
    while !stopped(&stop) {
        tokio::select! {
            got = listener.try_recv() => match got {
                Ok(Some(n)) => {
                    shared.tick.notify_one();
                    shared.wake(n.payload());
                }
                Ok(None) => {
                    tracing::warn!(target: "singularr::runtime", "the listener lost its connection");
                    return;
                }
                Err(e) => {
                    tracing::warn!(target: "singularr::runtime", error = %e, "the listener failed");
                    return;
                }
            },
            _ = stop.changed() => {}
        }
    }
}

/// One worker of a queue. It claims a job, runs it under its lease and ends it.
async fn work(shared: Arc<Shared>, queue: String, i: usize, mut stop: watch::Receiver<bool>) {
    let locker = format!("{}:{queue}:{i}", shared.me);
    let Some(wake) = shared.queues.get(&queue) else {
        return;
    };
    while !stopped(&stop) {
        let asked = Instant::now();
        match store::claim(&shared.pool, &queue, &locker, shared.timing.lease).await {
            Ok(Some(claimed)) => run(&shared, &locker, claimed, asked).await,
            Ok(None) => pause(wake, shared.timing.poll, &mut stop).await,
            Err(e) => {
                tracing::warn!(target: "singularr::runtime", queue = %queue, error = %e, "a worker could not claim a job");
                pause(wake, shared.timing.poll, &mut stop).await;
            }
        }
    }
}

/// A task beside a run that ends with it: the run's heartbeat.
struct Beside(JoinHandle<Beat>);

impl Drop for Beside {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// How a heartbeat ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Beat {
    /// The lease is lost.
    Lost,
    /// The run ended its job in its own transaction.
    Ended,
}

/// Resolves when the heartbeat says the lease is lost. A heartbeat that panicked counts as lost.
async fn gone(beat: &mut Beside) {
    if matches!((&mut beat.0).await, Ok(Beat::Ended)) {
        std::future::pending::<()>().await;
    }
}

/// Run one claimed job: its handler with a heartbeat beside it, then its end. A panic of the
/// handler fails the job. A run whose lease is lost is dropped and writes nothing. A run over its
/// kind's limit is dropped and fails.
async fn run(shared: &Shared, locker: &str, claimed: store::Claimed, asked: Instant) {
    let job = claimed.job;
    let handler = shared.handlers.get(&job.kind).cloned();
    let (edge, watched) = watch::channel(Some(asked + claimed.lease));
    let lease = Lease {
        id: job.id,
        locker: locker.to_string(),
        length: claimed.lease,
        schedule: job.schedule.clone(),
        attempt: job.attempt,
        retry: handler.as_ref().map(|h| h.retry()).unwrap_or_default(),
        started: Instant::now(),
        edge: watched,
    };
    let (id, kind) = (job.id, job.kind.clone());
    let outcome = match handler {
        None => Some(Err(Failed::from(format!(
            "no handler for jobs of the kind `{kind}`"
        )))),
        Some(handler) => {
            let mut beat = Beside(tokio::spawn(heartbeat(
                shared.pool.clone(),
                lease.clone(),
                edge,
            )));
            let limit = handler.limit();
            let cx = Cx {
                job,
                lease: lease.clone(),
            };
            let mut running = Box::pin(AssertUnwindSafe(handler.run(cx)).catch_unwind());
            let outcome = tokio::select! {
                ended = &mut running => {
                    Some(ended.unwrap_or_else(|_| Err(Failed::from("the handler panicked"))))
                }
                () = gone(&mut beat) => None,
                () = tokio::time::sleep(limit) => Some(Err(Failed {
                    error: format!("the run passed its limit of {} s", limit.as_secs()),
                    class: Some("timeout".to_string()),
                    again: None,
                })),
            };
            // The run goes before its end is written: a dropped run writes nothing more.
            drop(running);
            drop(beat);
            outcome
        }
    };
    let Some(outcome) = outcome else {
        tracing::warn!(target: "singularr::runtime", job = id, kind = %kind, "the lease is lost: the run is dropped and writes nothing");
        return;
    };
    if let Err(why) = &outcome {
        tracing::warn!(target: "singularr::runtime", job = id, kind = %kind, error = %why.error, "a job failed");
    }
    match end(&shared.pool, &lease, &outcome).await {
        Ok(true) => {}
        // Nothing to write: the run ended its job in a commit of its own. Otherwise the lease is
        // lost.
        Ok(false) => {
            if !store::ended_by(&shared.pool, id, locker)
                .await
                .unwrap_or(false)
            {
                tracing::warn!(target: "singularr::runtime", job = id, "the lease is lost: the end of the run is not written")
            }
        }
        Err(e) => {
            tracing::warn!(target: "singularr::runtime", job = id, error = %e, "the end of a job could not be written")
        }
    }
}

/// Write the end of a run in a transaction of the runtime's own. False when the job is not the
/// run's any more.
async fn end(
    pool: &PgPool,
    lease: &Lease,
    outcome: &Result<Done, Failed>,
) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let ended = store::end(&mut tx, lease, outcome).await?;
    if ended {
        tx.commit().await?;
    }
    Ok(ended)
}

/// The heartbeat of a run: it renews the lease every third of it until the lease is lost. A
/// renewal that fails is tried again at the next beat. The lease counts as lost at its edge: the
/// last good renewal plus its length. `edge` tells the run where the edge is.
async fn heartbeat(pool: PgPool, lease: Lease, edge: watch::Sender<Option<Instant>>) -> Beat {
    let beat = (lease.length / 3).max(Duration::from_millis(100));
    let mut until = (*edge.borrow()).unwrap_or_else(Instant::now);
    loop {
        tokio::time::sleep(beat.min(until.saturating_duration_since(Instant::now()))).await;
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            tracing::warn!(target: "singularr::runtime", job = lease.id, "the lease ran out before a renewal went through");
            edge.send_replace(None);
            return Beat::Lost;
        }
        let asked = Instant::now();
        let renewed = tokio::time::timeout(
            left,
            store::renew(&pool, lease.id, &lease.locker, lease.length),
        )
        .await;
        match renewed {
            Ok(Ok(true)) => {
                until = asked + lease.length;
                edge.send_replace(Some(until));
            }
            // Nothing to renew: the job's row says whether this run ended it in a commit of its
            // own. A run whose end rolled back still holds its row and renews it.
            Ok(Ok(false)) => match store::ended_by(&pool, lease.id, &lease.locker).await {
                Ok(true) => return Beat::Ended,
                Ok(false) => {
                    tracing::warn!(target: "singularr::runtime", job = lease.id, "the lease is lost: the reaper gave the job back");
                    edge.send_replace(None);
                    return Beat::Lost;
                }
                Err(e) => {
                    tracing::warn!(target: "singularr::runtime", job = lease.id, error = %e, "the job's row could not be read: the next beat tries again")
                }
            },
            Ok(Err(e)) => {
                tracing::warn!(target: "singularr::runtime", job = lease.id, error = %e, "the lease could not be renewed: the next beat tries again")
            }
            Err(_) => {
                tracing::warn!(target: "singularr::runtime", job = lease.id, "the lease ran out while its renewal waited");
                edge.send_replace(None);
                return Beat::Lost;
            }
        }
    }
}

/// The delay before a periodic schedule runs again after `failures` failed runs in a row:
/// `min(cap, every x 2^(failures - 1))`. The cap is `max(every, min(30 min, 5 x every))`: a 60 s
/// pass waits 300 s at most, a 900 s pass 1,800 s, a 3,600 s pass its interval. A jitter of 10
/// percent moves it. Its interval while none failed.
pub fn backoff(every: Duration, failures: i32, jitter: f64) -> Duration {
    if failures <= 0 {
        return every;
    }
    let cap = every.max(Duration::from_secs(1800).min(every.saturating_mul(5)));
    let grown = every.saturating_mul(1u32 << (failures - 1).clamp(0, 20) as u32);
    let base = grown.min(cap);
    base.mul_f64(0.9 + 0.2 * jitter.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_schedule_backs_off_to_its_cap() {
        let at = |every: u64, failures: i32| {
            backoff(Duration::from_secs(every), failures, 0.5)
                .as_secs_f64()
                .round() as u64
        };
        assert_eq!(at(30, 0), 30);
        // Five intervals at most below 30 min: a 30 s pass stops at 150 s, a 60 s pass at 300 s.
        assert_eq!(
            (1..=6).map(|n| at(30, n)).collect::<Vec<_>>(),
            vec![30, 60, 120, 150, 150, 150]
        );
        assert_eq!(
            (1..=6).map(|n| at(60, n)).collect::<Vec<_>>(),
            vec![60, 120, 240, 300, 300, 300]
        );
        assert_eq!((at(900, 1), at(900, 2), at(900, 3)), (900, 1800, 1800));
        // Never below the interval: a 3,600 s pass keeps its interval.
        assert_eq!((at(3600, 1), at(3600, 4)), (3600, 3600));
        assert_eq!((at(21_600, 1), at(21_600, 3)), (21_600, 21_600));
        // The jitter moves it 10 percent each way.
        let secs = |j: f64| {
            backoff(Duration::from_secs(100), 1, j)
                .as_secs_f64()
                .round() as u64
        };
        assert_eq!((secs(0.0), secs(1.0)), (90, 110));
    }

    #[test]
    fn a_run_keeps_its_detail_and_its_value() {
        assert_eq!(
            Done::from("3 albums").stored(),
            json!({ "detail": "3 albums" })
        );
        let done = Done {
            detail: "3 albums".into(),
            value: Some(json!({ "found": 3 })),
            again: None,
        };
        assert_eq!(
            done.stored(),
            json!({ "detail": "3 albums", "value": { "found": 3 } })
        );
        assert_eq!(connections(4), 11);
    }
}
