//! Runs as stored, and what is made of them without asking anything: which are open, which a
//! worker is on, which are stuck, what a run saved and how long it took.

use crate::engines::{
    FlowNeeds, ProcessorEvent, ProcessorReplaced, ProcessorRun, ProcessorWorker, WorkerFile,
};
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};

/// The setting that says after how many seconds an open run counts as stuck, and its default
/// (six hours).
pub const STUCK_AFTER: &str = "jobs.processing.stuckAfter";
pub const STUCK_AFTER_DEFAULT: u64 = 21_600;

/// A run waits in a queue folder for a worker.
pub const STAGED: &str = "staged";
/// What the worker made is being checked before it is imported.
pub const VERIFYING: &str = "verifying";
/// The result is in the library.
pub const IMPORTED: &str = "imported";
/// A staged run a worker is on is shown as this. It is never stored.
pub const WORKING: &str = "working";
/// The states a run is open in. Every other state is closed.
pub const OPEN_STATES: [&str; 2] = [STAGED, VERIFYING];
/// The states a closed run can be in.
pub const CLOSED_STATES: [&str; 5] = [IMPORTED, "failed", "refused", "pruned", "superseded"];

pub fn is_open(state: &str) -> bool {
    OPEN_STATES.contains(&state)
}

/// Where a run belongs in the catalog: the title whose folder its files lie in, and the
/// episode its file name reads as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Link {
    pub title_id: Option<i64>,
    pub season: Option<i32>,
    pub episode: Option<i32>,
}

/// One job of a processing service, as stored. The job as the service gave it (`raw`) is kept
/// beside these columns and read only for one run.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Run {
    /// `<integration id>:<the service's job id>`.
    pub id: String,
    pub integration: String,
    pub job_id: String,
    /// The flow, by the service's id of it.
    pub flow: String,
    /// What started it, in the service's words.
    #[sqlx(rename = "trigger_text")]
    pub trigger: String,
    /// `staged`, `verifying`, `imported`, `failed`, `refused`, `pruned` or `superseded`.
    pub state: String,
    pub created_at: DateTime<Utc>,
    /// When the service last changed the job.
    pub updated_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
    pub queue_path: Option<String>,
    pub source_path: Option<String>,
    pub source_size: Option<i64>,
    pub output_path: Option<String>,
    pub output_size: Option<i64>,
    pub result_path: Option<String>,
    pub result_size: Option<i64>,
    pub manager_file_id: Option<i64>,
    #[sqlx(json)]
    pub replaced: Vec<ProcessorReplaced>,
    pub detail: Option<String>,
    #[sqlx(json)]
    pub events: Vec<ProcessorEvent>,
    pub title_id: Option<i64>,
    #[sqlx(rename = "season_number")]
    pub season: Option<i32>,
    #[sqlx(rename = "episode_number")]
    pub episode: Option<i32>,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

impl Run {
    /// A job a service lists, as the row it becomes.
    pub fn listed(integration: &str, run: &ProcessorRun, link: Link, seen: DateTime<Utc>) -> Run {
        Run {
            id: run_id(integration, &run.job_id),
            integration: integration.to_string(),
            job_id: run.job_id.clone(),
            flow: run.flow.clone(),
            trigger: run.trigger.clone(),
            state: run.state.clone(),
            created_at: run.created,
            updated_at: run.updated,
            finished_at: run.finished,
            queue_path: run.queue_path.clone(),
            source_path: run.source.as_ref().map(|f| f.path.clone()),
            source_size: run.source.as_ref().and_then(|f| f.size),
            output_path: run.output.as_ref().map(|f| f.path.clone()),
            output_size: run.output.as_ref().and_then(|f| f.size),
            result_path: run.result.as_ref().map(|f| f.path.clone()),
            result_size: run.result.as_ref().and_then(|f| f.size),
            manager_file_id: run.manager_file_id,
            replaced: run.replaced.clone(),
            detail: run.detail.clone(),
            events: run.events.clone(),
            title_id: link.title_id,
            season: link.season,
            episode: link.episode,
            first_seen_at: seen,
            last_seen_at: seen,
        }
    }

    pub fn is_open(&self) -> bool {
        is_open(&self.state)
    }
}

/// The id of a run: its integration and the service's own id of the job.
pub fn run_id(integration: &str, job_id: &str) -> String {
    format!("{integration}:{job_id}")
}

/// A digest of everything a listing says about a job. A pass rewrites a stored run only when
/// this or its link changed.
pub fn fingerprint(run: &ProcessorRun) -> String {
    let bytes = serde_json::to_vec(run).unwrap_or_else(|_| format!("{run:?}").into_bytes());
    let digest = Sha256::digest(&bytes);
    hex::encode(&digest[..16])
}

/// A worker of a processing service, as last read.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveWorker {
    /// The integration id of its service.
    pub service: String,
    pub worker: ProcessorWorker,
}

/// What a worker service's own records say of the file of an open run, as last asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveFile {
    /// The integration id of the worker service that was asked.
    pub service: String,
    /// The path asked for: where the run's file waits.
    pub path: String,
    /// `None`: the worker has no record of the file.
    pub file: Option<WorkerFile>,
}

/// Why a flow does not fit a file, from what the flow needs and what the file is: its lines
/// (1080, 2160) and its video codec in the words files are shown with. `None` when it fits,
/// and when nothing is known that says otherwise.
pub fn unfit(needs: Option<&FlowNeeds>, lines: Option<i32>, codec: Option<&str>) -> Option<String> {
    let needs = needs?;
    if let (Some(least), Some(lines)) = (needs.min_resolution, lines)
        && lines < least
    {
        let wanted = if least >= 2160 {
            "4K".to_string()
        } else {
            format!("{least}p")
        };
        return Some(format!("Needs a {wanted} file"));
    }
    let codec = codec?;
    needs
        .not_codec
        .iter()
        .any(|c| c == codec)
        .then(|| format!("Already {codec}"))
}

/// Where a run's file waits for a worker: its queue path, when it has one.
pub fn queue_of(run: &Run) -> Option<&str> {
    run.queue_path
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
}

/// Why a stuck run is stuck, in a few words, from where its file stands in the worker's own
/// records. `None` when the records say nothing that explains it.
pub fn stuck_why(
    state: Option<&str>,
    words: Option<&str>,
    held_until: Option<DateTime<Utc>>,
) -> Option<String> {
    Some(match state? {
        "absent" => "Not in worker queue".to_string(),
        "queued" => "In worker queue".to_string(),
        "held" => match held_until {
            Some(until) => format!("Held until {}", until.format("%Y-%m-%d %H:%M UTC")),
            None => "Held by worker".to_string(),
        },
        "failed" => match words.map(str::trim).filter(|w| !w.is_empty()) {
            Some(words) => format!("Worker failed: {words}"),
            None => "Worker failed".to_string(),
        },
        _ => return None,
    })
}

/// The last folder or file name of a path.
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The worker that names the footprint a file's record carries: a worker of the service whose
/// record it is. `None` for a file the worker has no record of, and for a record without a
/// footprint.
pub fn worker_by_footprint<'w>(
    file: &LiveFile,
    workers: &'w [LiveWorker],
) -> Option<&'w LiveWorker> {
    let mark = file.file.as_ref()?.footprint.as_deref()?;
    workers
        .iter()
        .find(|w| w.service == file.service && w.worker.footprint.as_deref() == Some(mark))
}

/// The worker that is on a run. A worker is on it when its file is the run's queue path, or
/// when it names the footprint that the worker's own record of the run's queue file carries
/// (`files`: what the workers' records said of the files of open runs). A worker that runs a
/// flow works on a copy of the file in its own cache folder, under the file's own name: its
/// file is then not where the run waits, and a name alone joins nothing. Only a staged run
/// waits in a queue, so only a staged run has one.
pub fn worker_of<'w>(
    run: &Run,
    workers: &'w [LiveWorker],
    files: &[LiveFile],
) -> Option<&'w LiveWorker> {
    if run.state != STAGED {
        return None;
    }
    let queue = run.queue_path.as_deref().map(str::trim)?;
    workers
        .iter()
        .find(|w| w.worker.file.as_deref().map(str::trim) == Some(queue))
        .or_else(|| {
            files
                .iter()
                .filter(|f| f.path == queue)
                .find_map(|f| worker_by_footprint(f, workers))
        })
}

/// The state a run is shown in: `working` for a staged run a worker is on, else its own.
pub fn shown_state(run: &Run, worked_on: bool) -> &str {
    if worked_on && run.state == STAGED {
        WORKING
    } else {
        &run.state
    }
}

/// Stuck: open, started longer ago than `after`, and no worker on it.
pub fn is_stuck(run: &Run, worked_on: bool, now: DateTime<Utc>, after: Duration) -> bool {
    run.is_open() && !worked_on && now - run.created_at > after
}

/// The bytes an imported run saved: its source's size less its result's. `None` unless it was
/// imported and both sizes are known. Negative when the result is the larger file.
pub fn saved(run: &Run) -> Option<i64> {
    if run.state != IMPORTED {
        return None;
    }
    Some(run.source_size? - run.result_size?)
}

/// The seconds from a run's start to its end.
pub fn took(run: &Run) -> Option<i64> {
    run.finished_at
        .map(|end| (end - run.created_at).num_seconds().max(0))
}

/// What the table holds, by state.
#[derive(Debug, Clone, Default, PartialEq, sqlx::FromRow)]
pub struct StoredCounts {
    pub total: i64,
    pub staged: i64,
    pub verifying: i64,
    pub imported: i64,
    pub failed: i64,
    pub refused: i64,
    pub pruned: i64,
    pub superseded: i64,
    /// Source size less result size, over the imported runs.
    pub saved_bytes: i64,
    /// The start of the oldest run.
    pub since: Option<DateTime<Utc>>,
}

/// The runs of one flow of one service, in numbers.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct FlowStat {
    pub integration: String,
    /// The flow's id: the first of its modes, or the run's own flow when no flow holds it.
    pub flow: String,
    pub runs: i64,
    pub imported: i64,
    pub last_run: Option<DateTime<Utc>>,
    /// The sizes of the sources and of the results of its imported runs.
    pub source_bytes: Option<i64>,
    pub result_bytes: Option<i64>,
    /// The median of the seconds its imported runs took.
    pub median_seconds: Option<f64>,
}

impl FlowStat {
    /// The share of its sources' size its imported runs saved, in percent to one decimal.
    pub fn saved_percent(&self) -> Option<f64> {
        let (source, result) = (self.source_bytes?, self.result_bytes?);
        if source <= 0 {
            return None;
        }
        let percent = (source - result) as f64 * 100.0 / source as f64;
        Some((percent * 10.0).round() / 10.0)
    }
}

/// A run of a title that marks one of its files: an imported run (by its result) or an open
/// one (by its source).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct MarkRow {
    pub id: String,
    pub state: String,
    pub source_path: Option<String>,
    pub result_path: Option<String>,
}

/// Which runs `GET /api/v1/processing/runs` asks for, by state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateFilter {
    /// Staged (worked on or not) and verifying.
    Open,
    Closed,
    /// Staged, with a worker on it.
    Working,
    /// Staged, with no worker on it.
    Waiting,
    /// Open, past the stuck threshold, with no worker on it.
    Stuck,
    /// One stored state: `verifying`, `imported`, `failed`, ...
    Is(String),
}

impl StateFilter {
    /// The filter a state word asks for. `None` for a word that is no state.
    pub fn parse(word: &str) -> Option<StateFilter> {
        let w = word.trim().to_ascii_lowercase();
        Some(match w.as_str() {
            "open" => StateFilter::Open,
            "closed" => StateFilter::Closed,
            "working" => StateFilter::Working,
            "staged" => StateFilter::Waiting,
            "stuck" => StateFilter::Stuck,
            s if s == VERIFYING || CLOSED_STATES.contains(&s) => StateFilter::Is(s.to_string()),
            _ => return None,
        })
    }
}

/// What `GET /api/v1/processing/runs` filters by, resolved to what the table knows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFilter {
    pub state: Option<StateFilter>,
    /// The run's own flow is one of these.
    pub flows: Option<Vec<String>>,
    /// The integration id.
    pub integration: Option<String>,
    /// The run is linked to one of these catalog titles. Empty: no run matches.
    pub titles: Option<Vec<i64>>,
    /// Words that each must be in one of the run's file names.
    pub words: Vec<String>,
}

/// What the filters by state need to know of the moment: the paths workers are on, and the
/// start before which an open run is stuck.
#[derive(Debug, Clone, PartialEq)]
pub struct Moment {
    pub busy_paths: Vec<String>,
    pub stuck_before: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::ProcessorFile;

    fn at(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    fn listed(state: &str, queue: Option<&str>) -> Run {
        let run = ProcessorRun {
            job_id: "j1".to_string(),
            flow: "convert".to_string(),
            trigger: "by hand".to_string(),
            state: state.to_string(),
            created: at("2026-11-23T10:00:00Z"),
            updated: None,
            finished: None,
            queue_path: queue.map(str::to_string),
            source: Some(ProcessorFile {
                path: "/library/Show/Season 01/Show - S01E02.mkv".to_string(),
                size: Some(1000),
            }),
            output: None,
            result: None,
            manager_file_id: None,
            replaced: Vec::new(),
            detail: None,
            events: Vec::new(),
            raw: serde_json::json!({"id": "j1"}),
        };
        Run::listed("bridge", &run, Link::default(), at("2026-11-23T10:00:05Z"))
    }

    fn worker(file: Option<&str>) -> LiveWorker {
        LiveWorker {
            service: "worker".to_string(),
            worker: ProcessorWorker {
                id: None,
                node_id: None,
                node: Some("node-1".to_string()),
                file: file.map(str::to_string),
                percent: Some(40.0),
                fps: None,
                eta: None,
                status: None,
                kind: None,
                started_at: None,
                footprint: None,
                raw: serde_json::Value::Null,
            },
        }
    }

    #[test]
    fn a_staged_run_whose_queue_path_a_worker_is_on_is_working() {
        let workers = [worker(None), worker(Some("/queue/a.mkv "))];
        let staged = listed(STAGED, Some("/queue/a.mkv"));
        assert_eq!(staged.id, "bridge:j1");
        let on_it = worker_of(&staged, &workers, &[]);
        assert_eq!(on_it, Some(&workers[1]));
        assert_eq!(shown_state(&staged, on_it.is_some()), WORKING);
        // Another file, no queue path, or no worker: it waits.
        let other = listed(STAGED, Some("/queue/b.mkv"));
        assert_eq!(worker_of(&other, &workers, &[]), None);
        assert_eq!(shown_state(&other, false), STAGED);
        assert_eq!(worker_of(&listed(STAGED, None), &workers, &[]), None);
        assert_eq!(worker_of(&staged, &[], &[]), None);
        // Only a staged run waits in a queue: a run in another state keeps its own word, whatever
        // a worker is on.
        for state in [VERIFYING, IMPORTED, "failed"] {
            let run = listed(state, Some("/queue/a.mkv"));
            assert_eq!(worker_of(&run, &workers, &[]), None, "{state}");
            assert_eq!(shown_state(&run, true), state);
        }
    }

    #[test]
    fn an_open_run_past_the_threshold_with_no_worker_on_it_is_stuck() {
        let six_hours = Duration::seconds(STUCK_AFTER_DEFAULT as i64);
        let staged = listed(STAGED, Some("/queue/a.mkv"));
        let soon = at("2026-11-23T15:59:59Z");
        let late = at("2026-11-23T16:00:01Z");
        assert!(!is_stuck(&staged, false, soon, six_hours));
        assert!(is_stuck(&staged, false, late, six_hours));
        assert!(
            !is_stuck(&staged, true, late, six_hours),
            "a worker is on it"
        );
        assert!(is_stuck(&listed(VERIFYING, None), false, late, six_hours));
        for state in CLOSED_STATES {
            assert!(!is_open(state));
            assert!(!is_stuck(&listed(state, None), false, late, six_hours));
        }
        assert!(is_open(STAGED) && is_open(VERIFYING) && !is_open(WORKING));
    }

    #[test]
    fn saved_and_took_come_from_sizes_and_times() {
        let mut run = listed(IMPORTED, None);
        assert_eq!((saved(&run), took(&run)), (None, None), "no result yet");
        run.result_size = Some(600);
        run.finished_at = Some(at("2026-11-23T10:11:15Z"));
        assert_eq!((saved(&run), took(&run)), (Some(400), Some(675)));
        run.result_size = Some(1200);
        assert_eq!(saved(&run), Some(-200), "the result is the larger file");
        // Only an import saves anything; a run that ended before it started took no time.
        let mut failed = listed("failed", None);
        failed.result_size = Some(600);
        failed.finished_at = Some(at("2026-11-23T09:00:00Z"));
        assert_eq!((saved(&failed), took(&failed)), (None, Some(0)));
        let stat = FlowStat {
            integration: "bridge".to_string(),
            flow: "convert".to_string(),
            runs: 3,
            imported: 2,
            last_run: None,
            source_bytes: Some(3000),
            result_bytes: Some(2358),
            median_seconds: Some(690.0),
        };
        assert_eq!(stat.saved_percent(), Some(21.4));
        let none = FlowStat {
            source_bytes: None,
            result_bytes: None,
            ..stat.clone()
        };
        assert_eq!(none.saved_percent(), None);
        let empty = FlowStat {
            source_bytes: Some(0),
            result_bytes: Some(0),
            ..stat
        };
        assert_eq!(empty.saved_percent(), None);
    }

    #[test]
    fn a_fingerprint_follows_what_the_service_says() {
        let job = |state: &str, note: &str| ProcessorRun {
            job_id: "j1".to_string(),
            flow: "convert".to_string(),
            trigger: "by hand".to_string(),
            state: state.to_string(),
            created: at("2026-11-23T10:00:00Z"),
            updated: None,
            finished: None,
            queue_path: None,
            source: None,
            output: None,
            result: None,
            manager_file_id: None,
            replaced: Vec::new(),
            detail: None,
            events: Vec::new(),
            raw: serde_json::json!({"id": "j1", "note": note}),
        };
        let a = fingerprint(&job(STAGED, "x"));
        assert_eq!(a.len(), 32);
        assert_eq!(a, fingerprint(&job(STAGED, "x")));
        assert_ne!(a, fingerprint(&job(VERIFYING, "x")));
        assert_ne!(a, fingerprint(&job(STAGED, "y")));
    }

    #[test]
    fn a_stuck_run_says_why_from_the_worker_s_records() {
        let until = Some(at("2026-11-24T03:10:00Z"));
        assert_eq!(
            stuck_why(Some("absent"), None, None).as_deref(),
            Some("Not in worker queue")
        );
        assert_eq!(
            stuck_why(Some("queued"), Some("Queued"), None).as_deref(),
            Some("In worker queue")
        );
        assert_eq!(
            stuck_why(Some("held"), Some("Queued"), until).as_deref(),
            Some("Held until 2026-11-24 03:10 UTC")
        );
        assert_eq!(
            stuck_why(Some("held"), None, None).as_deref(),
            Some("Held by worker")
        );
        assert_eq!(
            stuck_why(Some("failed"), Some("Transcode error"), None).as_deref(),
            Some("Worker failed: Transcode error")
        );
        assert_eq!(
            stuck_why(Some("failed"), Some(" "), None).as_deref(),
            Some("Worker failed")
        );
        // What explains nothing is no sentence: the worker is done, on it, or was not asked.
        for state in [Some("done"), Some("notRequired"), Some("working"), None] {
            assert_eq!(stuck_why(state, Some("words"), until), None, "{state:?}");
        }
        let mut waiting = listed(STAGED, Some(" /queue/a.mkv "));
        assert_eq!(queue_of(&waiting), Some("/queue/a.mkv"));
        waiting.queue_path = Some("  ".to_string());
        assert_eq!(queue_of(&waiting), None);
        assert_eq!(queue_of(&listed(STAGED, None)), None);
    }

    #[test]
    fn a_run_is_its_worker_s_by_the_footprint_of_its_file() {
        // A worker that runs a flow is on a copy of the file in its own folder, and names
        // the footprint of the file.
        let mut busy = worker(Some("/cache/work-1/a.mkv"));
        busy.worker.footprint = Some("F1".to_string());
        let workers = [worker(None), busy];
        let staged = listed(STAGED, Some("/queue/a.mkv"));
        let record = |service: &str, path: &str, footprint: Option<&str>| LiveFile {
            service: service.to_string(),
            path: path.to_string(),
            file: Some(WorkerFile {
                library: None,
                state: Some("queued"),
                words: None,
                health: None,
                old_size: None,
                new_size: None,
                transcoded_at: None,
                hold_until: None,
                footprint: footprint.map(str::to_string),
            }),
        };
        // The worker's record of the file that waits carries the footprint the worker names.
        let files = [record("worker", "/queue/a.mkv", Some("F1"))];
        assert_eq!(worker_of(&staged, &workers, &files), Some(&workers[1]));
        assert_eq!(worker_by_footprint(&files[0], &workers), Some(&workers[1]));
        // Not without the record. Not with a record of another footprint, of none, of another
        // service, or of a file of the same name that waits elsewhere. Not when the worker
        // has no record of the file.
        assert_eq!(worker_of(&staged, &workers, &[]), None);
        for other in [
            record("worker", "/queue/a.mkv", Some("F2")),
            record("worker", "/queue/a.mkv", None),
            record("another", "/queue/a.mkv", Some("F1")),
            record("worker", "/other-queue/a.mkv", Some("F1")),
            LiveFile {
                service: "worker".to_string(),
                path: "/queue/a.mkv".to_string(),
                file: None,
            },
        ] {
            assert_eq!(
                worker_of(&staged, &workers, std::slice::from_ref(&other)),
                None,
                "{other:?}"
            );
        }
        // A worker that names no footprint is joined by its path alone, never by the name of
        // its file.
        let unmarked = [worker(Some("/cache/work-1/a.mkv"))];
        assert_eq!(worker_of(&staged, &unmarked, &files), None);
        // Only a staged run has a worker.
        let verifying = listed(VERIFYING, Some("/queue/a.mkv"));
        assert_eq!(worker_of(&verifying, &workers, &files), None);
    }

    #[test]
    fn a_flow_fits_a_file_or_says_why_not() {
        let downscale = FlowNeeds {
            min_resolution: Some(2160),
            not_codec: Vec::new(),
        };
        let to_hevc = FlowNeeds {
            min_resolution: None,
            not_codec: vec!["HEVC".to_string(), "AV1".to_string()],
        };
        assert_eq!(
            unfit(Some(&downscale), Some(1080), Some("HEVC")).as_deref(),
            Some("Needs a 4K file")
        );
        assert_eq!(unfit(Some(&downscale), Some(2160), Some("HEVC")), None);
        assert_eq!(
            unfit(Some(&to_hevc), Some(1080), Some("HEVC")).as_deref(),
            Some("Already HEVC")
        );
        assert_eq!(
            unfit(Some(&to_hevc), Some(2160), Some("AV1")).as_deref(),
            Some("Already AV1")
        );
        assert_eq!(unfit(Some(&to_hevc), Some(1080), Some("H.264")), None);
        // A smaller target says its own lines; what is not known does not rule a flow out; a
        // flow that needs nothing fits everything.
        let to_hd = FlowNeeds {
            min_resolution: Some(1080),
            not_codec: vec!["HEVC".to_string()],
        };
        assert_eq!(
            unfit(Some(&to_hd), Some(720), Some("HEVC")).as_deref(),
            Some("Needs a 1080p file")
        );
        assert_eq!(unfit(Some(&downscale), None, None), None);
        assert_eq!(unfit(Some(&to_hevc), Some(1080), None), None);
        assert_eq!(unfit(None, Some(480), Some("HEVC")), None);
    }

    #[test]
    fn a_state_word_is_a_filter() {
        assert_eq!(StateFilter::parse(" Open "), Some(StateFilter::Open));
        assert_eq!(StateFilter::parse("closed"), Some(StateFilter::Closed));
        assert_eq!(StateFilter::parse("working"), Some(StateFilter::Working));
        assert_eq!(StateFilter::parse("staged"), Some(StateFilter::Waiting));
        assert_eq!(StateFilter::parse("stuck"), Some(StateFilter::Stuck));
        for state in [
            "verifying",
            "imported",
            "failed",
            "refused",
            "pruned",
            "superseded",
        ] {
            assert_eq!(
                StateFilter::parse(state),
                Some(StateFilter::Is(state.to_string()))
            );
        }
        assert_eq!(StateFilter::parse("promoted"), None);
        assert_eq!(StateFilter::parse(""), None);
        assert_eq!(file_name("/a/b/c.mkv"), "c.mkv");
        assert_eq!(file_name("C:\\a\\c.mkv"), "c.mkv");
        assert_eq!(file_name("c.mkv"), "c.mkv");
    }
}
