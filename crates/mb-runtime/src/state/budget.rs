// `[mb-budget]` watchdog (AGENTS.md "Robustness Rules" 3, docs/compat-round3.md
// 0.1): style+layout must finish within a time budget; when it doesn't we say
// so on stderr instead of hanging silently (round 2's 2^depth inline measure
// took minutes on Wikipedia with no trace).
//
// Determinism exception: this is the one place the browser reads the wall
// clock for its own behaviour, and the reading goes to stderr only — never
// into observations, actions, policy output or session logs. Nothing is
// aborted; a half-laid-out page would confuse more than a slow one.

use std::time::Duration;

/// Budget for one style+layout pass, release build. Debug builds are ~10×
/// slower (GitHub took 21 s in round 2), so they get ten times the budget.
const RELEASE_BUDGET: Duration = Duration::from_secs(2);

pub(super) fn budget() -> Duration {
    if cfg!(debug_assertions) {
        RELEASE_BUDGET * 10
    } else {
        RELEASE_BUDGET
    }
}

/// The stderr line for a pass that took `elapsed`, or `None` within budget.
/// `nodes` is only counted when the line is printed (it walks the arena).
/// Per-node microseconds separate a slow machine from a big page.
pub(super) fn report(
    elapsed: Duration,
    budget: Duration,
    url: Option<&str>,
    nodes: impl FnOnce() -> usize,
) -> Option<String> {
    if elapsed <= budget {
        return None;
    }
    let nodes = nodes();
    let ms = elapsed.as_millis();
    let us_per_node = elapsed.as_micros() / nodes.max(1) as u128;
    let url = url.map_or_else(
        || "about:blank".to_string(),
        crate::js::error_location::display_url,
    );
    Some(format!(
        "[mb-budget] style+layout took {ms}ms on {url} ({nodes} nodes, {us_per_node}µs/node)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_budget_says_nothing() {
        let line = report(
            Duration::from_millis(5),
            Duration::from_secs(2),
            Some("https://a.example/"),
            || unreachable!("nodes are only counted for a report"),
        );
        assert_eq!(line, None);
    }

    #[test]
    fn over_budget_reports_time_size_and_url_without_query() {
        let line = report(
            Duration::from_millis(2500),
            Duration::from_secs(2),
            Some("https://a.example/wiki/Page?token=secret#top"),
            || 1000,
        )
        .expect("over budget");
        assert_eq!(
            line,
            "[mb-budget] style+layout took 2500ms on https://a.example/wiki/Page (1000 nodes, 2500µs/node)"
        );
    }

    #[test]
    fn debug_builds_get_ten_times_the_budget() {
        let expected = if cfg!(debug_assertions) {
            Duration::from_secs(20)
        } else {
            Duration::from_secs(2)
        };
        assert_eq!(budget(), expected);
    }
}
