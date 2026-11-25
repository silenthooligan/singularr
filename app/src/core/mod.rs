//! Plumbing: configuration, the database pool, the journal, the one way out, callers and errors.

pub mod model;

#[cfg(feature = "ssr")]
pub mod auth;
#[cfg(feature = "ssr")]
pub mod config;
#[cfg(feature = "ssr")]
pub mod db;
#[cfg(feature = "ssr")]
pub mod error;
#[cfg(feature = "ssr")]
pub mod geoip;
#[cfg(feature = "ssr")]
pub mod http;
#[cfg(feature = "ssr")]
pub mod identity;
#[cfg(feature = "ssr")]
pub mod jobs;
#[cfg(feature = "ssr")]
pub mod journal;
/// ffmpeg and ffprobe: a run without a shell, capped and timed, and a file's probe.
#[cfg(feature = "ssr")]
pub mod media;
#[cfg(feature = "ssr")]
pub mod observe;
#[cfg(feature = "ssr")]
pub mod outbound;
#[cfg(feature = "ssr")]
pub mod schema;
#[cfg(feature = "ssr")]
pub mod smtp;
#[cfg(feature = "ssr")]
pub mod store;
#[cfg(feature = "ssr")]
pub mod units;
