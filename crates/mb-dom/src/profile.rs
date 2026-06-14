// Lightweight stderr profiler gated by the `MB_PROFILE` env var so production
// runs and the test suite stay quiet. Set `MB_PROFILE=1` (any non-empty,
// non-"0" value) to enable per-span lines like `[mb-profile] navigation::fetch: 234.12ms`.
//
// Designed for one-shot exploratory measurement, not a long-lived tracing layer
// — no aggregation, no spans nested by context, just "wrap a block, log its
// elapsed time when enabled".

use std::sync::OnceLock;
use std::time::Instant;

static ENABLED: OnceLock<bool> = OnceLock::new();

pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("MB_PROFILE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

/// Run `f` and, when profiling is enabled, print a `[mb-profile] <label>: X.XXms`
/// line to stderr afterward. The label is `'static` so it cost-free when
/// profiling is off — the closure runs through the same path either way.
pub fn time_block<R>(label: &'static str, f: impl FnOnce() -> R) -> R {
    if !enabled() {
        return f();
    }
    let start = Instant::now();
    let result = f();
    let elapsed = start.elapsed();
    eprintln!(
        "[mb-profile] {}: {:.2}ms",
        label,
        elapsed.as_secs_f64() * 1000.0
    );
    result
}

/// Manual span for cases where `time_block` doesn't fit (e.g. the timed work
/// spans multiple borrows and rearranging it under a closure would be
/// awkward). Caller is responsible for calling `.end()`; dropping without
/// `.end()` is a silent no-op so early returns don't log misleading totals.
pub struct Span {
    label: &'static str,
    start: Instant,
}

impl Span {
    pub fn start(label: &'static str) -> Self {
        Self {
            label,
            start: Instant::now(),
        }
    }

    pub fn end(self) {
        if enabled() {
            eprintln!(
                "[mb-profile] {}: {:.2}ms",
                self.label,
                self.start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }
}
