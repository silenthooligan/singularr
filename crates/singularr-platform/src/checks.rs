//! The recent checks of each integration, this process's own like its metrics: each run of one
//! of its jobs ([`crate::runtime`]) and each test made from the Settings page, the last few per
//! integration. What they hold is since the process started. The integrations' lasting state is
//! `integration_health` (one row per pass) and `engine_state` (what the passes found).

use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{LazyLock, Mutex, PoisonError};

/// Checks kept per integration.
const CHECKS_KEPT: usize = 30;

/// One check of an integration: a run of one of its jobs or a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub at: DateTime<Utc>,
    pub ok: bool,
    /// What was asked: a job's pass (`titles`) or `test`.
    pub via: String,
    pub ms: i64,
    pub detail: String,
}

/// The recent checks of every integration.
#[derive(Default)]
pub struct Checks {
    log: Mutex<BTreeMap<String, VecDeque<Check>>>,
}

static RECENT: LazyLock<Checks> = LazyLock::new(Checks::default);

/// The checks of this process.
pub fn recent() -> &'static Checks {
    &RECENT
}

impl Checks {
    /// Keep a check of an integration, the newest first. The oldest beyond the last few go.
    pub fn record(&self, integration: &str, check: Check) {
        let mut all = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        let list = all.entry(integration.to_string()).or_default();
        list.push_front(check);
        list.truncate(CHECKS_KEPT);
    }

    /// The recent checks of an integration, newest first.
    pub fn of(&self, integration: &str) -> Vec<Check> {
        self.log
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

    #[test]
    fn checks_keep_the_newest_few() {
        let checks = Checks::default();
        for n in 0..(CHECKS_KEPT + 5) {
            checks.record(
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
        let kept = checks.of("x");
        assert_eq!(kept.len(), CHECKS_KEPT);
        assert_eq!(kept[0].ms, (CHECKS_KEPT + 4) as i64, "newest first");
        assert!(checks.of("y").is_empty());
    }
}
