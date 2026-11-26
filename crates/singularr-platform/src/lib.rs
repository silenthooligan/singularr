//! Singularr's platform: the runtime every module of the app stands on.
//!
//! - `db`: the pool and the migration runner. The migrations are the app's: it hands its
//!   migrator over.
//! - `journal` and `store`: the events and the API keys. An event is appended in the
//!   transaction that makes its change. It is published after the commit.
//! - `auth` and `identity`: who is calling (through the edge or with a key) and what they may do.
//! - `error` and `http`: problem documents, request ids, the health routes, the stop signal.
//! - `outbound` sends every HTTP call over the SDK's rules. `smtp` sends every mail.
//! - `jobs`: the loops of this process and their runs. `observe`: the names of the metrics.
//! - `runtime`: the durable jobs. Schedules, a tick, workers with leases and a heartbeat, the
//!   reaper, wake-ups, a supervisor. A run proves its lease where it writes. `markers`: what a
//!   pass knows across starts.
//! - `media`: ffmpeg and ffprobe runs. `model`: pages, scopes, events and keys as the API
//!   answers them.
//! - `settings`: the settings mechanism over the fields the app describes (the sources of a
//!   value, the saved values, the save). `access`: what Singularr may do with each action of
//!   each integration. `integrations`: the integrations' rows (health, state, access).
//! - `config`: the bootstrap, read before anything else. The database login and its pool, the
//!   instance file, the owners and the edge, the time limit of a call and the face.
//!
//! The platform knows no domain module and no connector type. The app re-exports its modules at
//! their old paths: most from its `core`, `settings` from its `settings`, `access` and
//! `integrations` from its `engines`.
#![forbid(unsafe_code)]

pub mod access;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod http;
pub mod identity;
pub mod integrations;
pub mod jobs;
pub mod journal;
pub mod markers;
/// ffmpeg and ffprobe: a run without a shell, capped and timed, and a file's probe.
pub mod media;
pub mod model;
pub mod observe;
pub mod outbound;
pub mod runtime;
pub mod settings;
pub mod smtp;
pub mod store;
