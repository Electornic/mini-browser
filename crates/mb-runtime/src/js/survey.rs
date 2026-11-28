//! Probe-only counters for real-site surveys (compat round 4 P4).
//!
//! `compat_probe` reads how many classic scripts ran and how many finished
//! without a top-level exception — "how far did the page's JS get". Like
//! `net::survey_stats`, these are process-wide and nothing in the browser
//! reads them: they never reach an observation, an action or a policy
//! decision (determinism rule, AGENTS.md).

use std::sync::atomic::{AtomicUsize, Ordering};

static RUN: AtomicUsize = AtomicUsize::new(0);
static COMPLETED: AtomicUsize = AtomicUsize::new(0);

/// Classic scripts run since the process started, and how many of them
/// finished their top-level run without throwing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScriptSurveyStats {
    pub run: usize,
    pub completed: usize,
}

pub fn script_survey_stats() -> ScriptSurveyStats {
    ScriptSurveyStats {
        run: RUN.load(Ordering::Relaxed),
        completed: COMPLETED.load(Ordering::Relaxed),
    }
}

/// One page script's top-level run ended (`completed` = no exception).
pub(crate) fn note_script_run(completed: bool) {
    RUN.fetch_add(1, Ordering::Relaxed);
    if completed {
        COMPLETED.fetch_add(1, Ordering::Relaxed);
    }
}
