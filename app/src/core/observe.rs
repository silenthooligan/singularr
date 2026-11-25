//! Names of the metrics Singularr publishes at /metrics, and the helpers that set them.
//! HTTP request counts and durations by route and status come from the axum-prometheus layer.

use std::time::Duration;

pub const SYNC_DURATION: &str = "singularr_sync_duration_seconds";
pub const SYNC_RUNS: &str = "singularr_sync_runs_total";
pub const ENGINE_UP: &str = "singularr_engine_up";
pub const TITLES: &str = "singularr_titles";
pub const COPIES: &str = "singularr_copies";
pub const DOWNLOADS: &str = "singularr_downloads";
pub const DOWNLOAD_ORPHANS: &str = "singularr_download_orphans";
pub const DOWNLOADS_STUCK: &str = "singularr_downloads_stuck";
pub const SHADOW_DISAGREEMENTS: &str = "singularr_shadow_disagreements";

/// One pass of one engine: how long it took and how it ended.
pub fn sync_pass(engine: &str, pass: &'static str, took: Duration, ok: bool) {
    metrics::histogram!(SYNC_DURATION, "engine" => engine.to_string(), "pass" => pass)
        .record(took.as_secs_f64());
    metrics::counter!(
        SYNC_RUNS,
        "engine" => engine.to_string(),
        "pass" => pass,
        "result" => if ok { "ok" } else { "error" }
    )
    .increment(1);
}

pub fn engine_up(engine: &str, up: bool) {
    metrics::gauge!(ENGINE_UP, "engine" => engine.to_string()).set(if up { 1.0 } else { 0.0 });
}

pub fn gauge(name: &'static str, value: i64) {
    metrics::gauge!(name).set(value as f64);
}

pub fn gauge_labeled(name: &'static str, label: &'static str, value_of: &str, value: i64) {
    metrics::gauge!(name, label => value_of.to_string()).set(value as f64);
}
