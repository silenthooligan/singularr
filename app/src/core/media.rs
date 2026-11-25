//! The media tools: ffmpeg and ffprobe at /usr/local/bin. The image carries both (the Dockerfile's
//! `FFMPEG` stages). A run passes an argument list to the program. No shell is involved. Its
//! stdin is closed and its environment is empty. Its stdout and stderr are capped. A time limit
//! ends it with a kill.
//! Scratch files go to the data folder (`scratch_dir`): `/tmp` is a small tmpfs in the compose
//! files. Nothing calls the tools yet except the doctor and the tests (platform design, step A2).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

/// Where the image puts ffmpeg.
pub const FFMPEG: &str = "/usr/local/bin/ffmpeg";
/// Where the image puts ffprobe.
pub const FFPROBE: &str = "/usr/local/bin/ffprobe";

/// What one run may take: its time, and how much of its stdout and its stderr is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub time: Duration,
    pub stdout: usize,
    pub stderr: usize,
}

impl Default for Limits {
    /// A minute, 8 MiB of stdout (ffprobe's JSON of a file with many streams is far less), 64 KiB
    /// of stderr.
    fn default() -> Limits {
        Limits {
            time: Duration::from_secs(60),
            stdout: 8 << 20,
            stderr: 64 << 10,
        }
    }
}

/// A run that ended by itself: its exit status and what it wrote, each output cut at its limit.
#[derive(Debug, Clone)]
pub struct Output {
    /// `None`: a signal ended it.
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// An output was longer than its limit. The rest was read and dropped.
    pub cut: bool,
}

impl Output {
    /// The last line of stderr: where ffmpeg and ffprobe say what went wrong.
    pub fn last_error_line(&self) -> String {
        String::from_utf8_lossy(&self.stderr)
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("{0} does not exist")]
    Missing(String),
    #[error("{program} could not start: {error}")]
    Start {
        program: String,
        error: std::io::Error,
    },
    #[error("{program} reached its limit of {secs} s and was stopped")]
    TimedOut {
        program: String,
        secs: u64,
        /// The stopped process.
        pid: Option<u32>,
    },
    #[error("{program} ended with {status}: {message}")]
    Failed {
        program: String,
        status: String,
        message: String,
    },
    #[error("ffprobe's answer is unreadable: {0}")]
    Unreadable(String),
    #[error("{0} is not an absolute path")]
    Relative(String),
    #[error("{0} climbs out of its folder (..)")]
    Upward(String),
}

/// Runs `program` with `args`: no shell, stdin closed, an empty environment, `dir` as the
/// working directory when one is given. Stdout and stderr are read as they come, each kept up to
/// its limit. When the time limit passes, the process is killed.
pub async fn run(
    program: &Path,
    args: &[OsString],
    dir: Option<&Path>,
    limits: Limits,
) -> Result<Output, MediaError> {
    let name = program.display().to_string();
    if !program.is_file() {
        return Err(MediaError::Missing(name));
    }
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let mut child = command.spawn().map_err(|error| MediaError::Start {
        program: name.clone(),
        error,
    })?;
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let finished = tokio::time::timeout(limits.time, async {
        tokio::join!(
            capped(stdout, limits.stdout),
            capped(stderr, limits.stderr),
            child.wait()
        )
    })
    .await;
    let Ok(((stdout, cut_out), (stderr, cut_err), status)) = finished else {
        // The kill is sent. The wait for it is bounded: a process stuck in the kernel (on a hung
        // network mount, say) may not end at once. kill_on_drop sends it again when `child` goes.
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        return Err(MediaError::TimedOut {
            program: name,
            secs: limits.time.as_secs(),
            pid,
        });
    };
    let status = status.map_err(|error| MediaError::Start {
        program: name,
        error,
    })?;
    Ok(Output {
        code: status.code(),
        stdout,
        stderr,
        cut: cut_out || cut_err,
    })
}

/// Reads a stream to its end, keeping at most `limit` bytes.
async fn capped<R: AsyncRead + Unpin>(stream: Option<R>, limit: usize) -> (Vec<u8>, bool) {
    let Some(mut stream) = stream else {
        return (Vec::new(), false);
    };
    let mut kept = Vec::new();
    let mut cut = false;
    let mut chunk = [0u8; 8192];
    while let Ok(n) = stream.read(&mut chunk).await {
        if n == 0 {
            break;
        }
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..n.min(room)]);
        cut |= n > room;
    }
    (kept, cut)
}

/// Arguments from words.
pub fn words(list: &[&str]) -> Vec<OsString> {
    list.iter().map(|word| OsString::from(*word)).collect()
}

/// The folder for scratch files: `scratch` in the data folder (`SINGULARR_DATA_DIR`), made when
/// it is missing.
pub fn scratch_dir(data_dir: &Path) -> std::io::Result<PathBuf> {
    let dir = data_dir.join("scratch");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// A tool's version: the third word of `<tool> -version`, such as `n9.0.2-39-g7adfbe40dc-20261031`.
pub async fn version(program: &Path) -> Result<String, MediaError> {
    let limits = Limits {
        time: Duration::from_secs(10),
        stdout: 64 << 10,
        stderr: 16 << 10,
    };
    let out = run(program, &words(&["-hide_banner", "-version"]), None, limits).await?;
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next().unwrap_or("");
    match (out.code, first.split_whitespace().nth(2)) {
        (Some(0), Some(version)) if first.contains(" version ") => Ok(version.to_string()),
        _ => Err(MediaError::Failed {
            program: program.display().to_string(),
            status: status_text(out.code),
            message: first_or(&out, first),
        }),
    }
}

fn status_text(code: Option<i32>) -> String {
    match code {
        Some(code) => format!("status {code}"),
        None => "a signal".to_string(),
    }
}

fn first_or(out: &Output, first: &str) -> String {
    let error = out.last_error_line();
    if error.is_empty() {
        first.to_string()
    } else {
        error
    }
}

/// What ffprobe reads of a file: its container and every stream
/// (`ffprobe -print_format json -show_format -show_streams`).
#[derive(Debug, Clone, Deserialize)]
pub struct Probe {
    pub format: Format,
    #[serde(default)]
    pub streams: Vec<Stream>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Format {
    /// Comma-separated short names: `matroska,webm`, `mov,mp4,m4a,3gp,3g2,mj2`.
    pub format_name: String,
    #[serde(default)]
    pub format_long_name: Option<String>,
    /// Seconds, as ffprobe writes them: `1.000000`.
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub size: Option<String>,
    #[serde(default)]
    pub bit_rate: Option<String>,
    #[serde(default)]
    pub nb_streams: u32,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

impl Format {
    pub fn seconds(&self) -> Option<f64> {
        self.duration.as_deref()?.parse().ok()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Stream {
    pub index: u32,
    /// `video`, `audio`, `subtitle`, `data` or `attachment`.
    #[serde(default)]
    pub codec_type: Option<String>,
    #[serde(default)]
    pub codec_name: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub pix_fmt: Option<String>,
    #[serde(default)]
    pub sample_rate: Option<String>,
    #[serde(default)]
    pub channels: Option<u32>,
    #[serde(default)]
    pub channel_layout: Option<String>,
    #[serde(default)]
    pub duration: Option<String>,
    #[serde(default)]
    pub bit_rate: Option<String>,
    /// Flags such as `default` and `forced`, 0 or 1 each.
    #[serde(default)]
    pub disposition: BTreeMap<String, i64>,
    /// Such as `language` and `title`.
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

/// Probes a file with ffprobe at [`FFPROBE`].
pub async fn probe(path: &Path) -> Result<Probe, MediaError> {
    probe_with(Path::new(FFPROBE), path, Limits::default()).await
}

/// Probes a file, an absolute path without `..` ([`input_args`]).
pub async fn probe_with(ffprobe: &Path, path: &Path, limits: Limits) -> Result<Probe, MediaError> {
    let mut args = words(&[
        "-hide_banner",
        "-v",
        "error",
        "-print_format",
        "json",
        "-show_format",
        "-show_streams",
    ]);
    args.extend(input_args(path)?);
    let out = run(ffprobe, &args, None, limits).await?;
    if out.code != Some(0) {
        return Err(MediaError::Failed {
            program: ffprobe.display().to_string(),
            status: status_text(out.code),
            message: out.last_error_line(),
        });
    }
    if out.cut {
        return Err(MediaError::Unreadable(format!(
            "longer than {} bytes",
            limits.stdout
        )));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| MediaError::Unreadable(e.to_string()))
}

/// A file as an argument: `file:` and its absolute path. The `file:` protocol keeps a name such
/// as `http:...` or `concat:...` from being read as an address. A path that is relative or holds
/// `..` is refused.
pub fn file_arg(path: &Path) -> Result<OsString, MediaError> {
    if !path.is_absolute() {
        return Err(MediaError::Relative(path.display().to_string()));
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(MediaError::Upward(path.display().to_string()));
    }
    let mut arg = OsString::from("file:");
    arg.push(path.as_os_str());
    Ok(arg)
}

/// An input file: `-protocol_whitelist file -i file:<path>`. The file, and any file it names (a
/// playlist, say), is read through the `file:` protocol alone. No address is opened.
pub fn input_args(path: &Path) -> Result<Vec<OsString>, MediaError> {
    let mut args = words(&["-protocol_whitelist", "file", "-i"]);
    args.push(file_arg(path)?);
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tools are at /usr/local/bin in the image and in the builder and check stages. Those
    /// stages set SINGULARR_TEST_MEDIA=required: a test without the tools fails there. Elsewhere it
    /// says so and passes.
    fn tools(test: &str) -> bool {
        let present = Path::new(FFMPEG).is_file() && Path::new(FFPROBE).is_file();
        if !present {
            assert!(
                std::env::var("SINGULARR_TEST_MEDIA").as_deref() != Ok("required"),
                "{test}: {FFMPEG} and {FFPROBE} must be there (SINGULARR_TEST_MEDIA=required)"
            );
            eprintln!("{test}: skipped, no {FFMPEG} or {FFPROBE}");
        }
        present
    }

    /// A data folder of the test's own.
    fn data_dir(test: &str) -> PathBuf {
        std::env::temp_dir().join(format!("singularr-{test}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn a_second_of_test_sources_probes_as_it_was_made() {
        if !tools("media_probe") {
            return;
        }
        let data = data_dir("media-probe");
        let scratch = scratch_dir(&data).unwrap();
        assert_eq!(scratch, data.join("scratch"));
        let cue = scratch.join("cue.srt");
        std::fs::write(&cue, "1\n00:00:00,000 --> 00:00:00,900\nA test cue\n").unwrap();
        let file = scratch.join("second.mkv");
        // ffmpeg's test sources make a picture with a clock and a 1 kHz tone, one second of each.
        // The cue is the subtitle stream. The codecs are lossless ones every build has.
        let mut args = words(&[
            "-hide_banner",
            "-v",
            "error",
            "-nostdin",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=320x240:rate=25:duration=1",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=1000:sample_rate=48000:duration=1",
        ]);
        args.extend(input_args(&cue).unwrap());
        args.extend(words(&[
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:s",
            "-c:v",
            "ffv1",
            "-c:a",
            "flac",
            "-c:s",
            "copy",
            "-metadata:s:s:0",
            "language=eng",
            "-t",
            "1",
        ]));
        args.push(file_arg(&file).unwrap());
        let made = run(Path::new(FFMPEG), &args, Some(&scratch), Limits::default())
            .await
            .unwrap();
        assert_eq!(made.code, Some(0), "{}", made.last_error_line());

        let probe = probe(&file).await.unwrap();
        assert!(
            probe.format.format_name.split(',').any(|n| n == "matroska"),
            "{}",
            probe.format.format_name
        );
        assert_eq!(probe.format.nb_streams, 3);
        let seconds = probe.format.seconds().unwrap();
        assert!((0.9..=1.2).contains(&seconds), "{seconds} s");
        let kinds: Vec<(&str, &str)> = probe
            .streams
            .iter()
            .map(|s| {
                (
                    s.codec_type.as_deref().unwrap_or(""),
                    s.codec_name.as_deref().unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            kinds,
            vec![("video", "ffv1"), ("audio", "flac"), ("subtitle", "subrip")]
        );
        assert_eq!(
            (probe.streams[0].width, probe.streams[0].height),
            (Some(320), Some(240))
        );
        assert_eq!(
            (
                probe.streams[1].sample_rate.as_deref(),
                probe.streams[1].channels
            ),
            (Some("48000"), Some(1))
        );
        assert_eq!(
            probe.streams[2].tags.get("language").map(String::as_str),
            Some("eng")
        );
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[tokio::test]
    async fn a_run_past_its_time_is_killed_and_outputs_are_capped() {
        if !tools("media_limits") {
            return;
        }
        // Ten minutes of test picture at its own pace, sent nowhere: stopped after half a second.
        let started = std::time::Instant::now();
        let endless = words(&[
            "-hide_banner",
            "-nostdin",
            "-re",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=duration=600",
            "-f",
            "null",
            "-",
        ]);
        let limits = Limits {
            time: Duration::from_millis(500),
            ..Limits::default()
        };
        let e = run(Path::new(FFMPEG), &endless, None, limits)
            .await
            .unwrap_err();
        let MediaError::TimedOut { pid, .. } = e else {
            panic!("{e}");
        };
        assert!(started.elapsed() < Duration::from_secs(10));
        // The process is gone: no process holds its id any more.
        let pid = pid.expect("the process id");
        if Path::new("/proc/self").exists() {
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "process {pid} still runs"
            );
        }
        // The version text, cut at ten bytes.
        let limits = Limits {
            stdout: 10,
            ..Limits::default()
        };
        let out = run(Path::new(FFPROBE), &words(&["-version"]), None, limits)
            .await
            .unwrap();
        assert_eq!((out.code, out.stdout.len(), out.cut), (Some(0), 10, true));
        assert!(version(Path::new(FFMPEG)).await.unwrap().len() > 1);
        assert!(version(Path::new(FFPROBE)).await.unwrap().len() > 1);
        // A file ffprobe cannot read is its error, in its words.
        let data = data_dir("media-limits");
        let scratch = scratch_dir(&data).unwrap();
        let junk = scratch.join("junk.mkv");
        std::fs::write(&junk, b"not a video").unwrap();
        let e = probe(&junk).await.unwrap_err();
        assert!(matches!(e, MediaError::Failed { .. }), "{e}");
        std::fs::remove_dir_all(&data).unwrap();
    }

    #[tokio::test]
    async fn no_tool_runs_for_a_relative_path_or_a_missing_program() {
        let e = probe(Path::new("movie.mkv")).await.unwrap_err();
        assert!(matches!(e, MediaError::Relative(_)), "{e}");
        let e = probe(Path::new("/media/movies/../../etc/passwd"))
            .await
            .unwrap_err();
        assert!(matches!(e, MediaError::Upward(_)), "{e}");
        assert_eq!(
            file_arg(Path::new("/media/http:x/a.mkv")).unwrap(),
            OsString::from("file:/media/http:x/a.mkv")
        );
        assert!(file_arg(Path::new("a.mkv")).is_err());
        assert_eq!(
            input_args(Path::new("/media/a.mkv")).unwrap(),
            words(&["-protocol_whitelist", "file", "-i", "file:/media/a.mkv"])
        );
        let e = run(
            Path::new("/no/such/ffmpeg"),
            &words(&["-version"]),
            None,
            Limits::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, MediaError::Missing(_)), "{e}");
    }

    #[test]
    fn ffprobe_s_answer_reads_with_its_numbers_as_text() {
        let probe: Probe = serde_json::from_str(
            r#"{"streams": [{"index": 0, "codec_name": "hevc", "codec_type": "video", "width": 3840,
                 "height": 2160, "disposition": {"default": 1, "forced": 0}},
                {"index": 1, "codec_name": "eac3", "codec_type": "audio", "sample_rate": "48000",
                 "channels": 6, "channel_layout": "5.1(side)", "tags": {"language": "eng"}}],
                "format": {"filename": "/media/movie.mkv", "nb_streams": 2, "format_name": "matroska,webm",
                 "duration": "7342.123000", "size": "41234567890", "bit_rate": "44930000"}}"#,
        )
        .unwrap();
        assert_eq!(probe.format.seconds(), Some(7342.123));
        assert_eq!(probe.streams[0].disposition.get("default"), Some(&1));
        assert_eq!(probe.streams[1].channels, Some(6));
        assert_eq!(
            probe.streams[1].tags.get("language").map(String::as_str),
            Some("eng")
        );
    }
}
