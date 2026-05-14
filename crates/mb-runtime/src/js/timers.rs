// setTimeout / setInterval / clearTimeout / clearInterval /
// requestAnimationFrame / cancelAnimationFrame, plus the synthetic clock
// that backs `Date.now()` and every timer deadline.
//
// Design (Phase 4.8c): the entire timer queue lives JS-side inside a
// closure-scoped IIFE. Rust owns only the clock and a tiny set of hooks
// (`__mb_clock_now`, `__mb_run_timers`, `__mb_run_raf`) that the
// scheduler calls from the main loop. Tests use `FixedClock` to drive
// time deterministically — same role boa's `boa_engine::context::time::
// FixedClock` played in the previous bridge.

use std::cell::Cell;
use std::rc::Rc;

use rquickjs::{Ctx, Result, prelude::Func};

#[derive(Clone)]
pub(super) enum ClockSource {
    System,
    Fixed(Rc<Cell<u64>>),
}

impl ClockSource {
    pub(super) fn now_ms(&self) -> u64 {
        match self {
            Self::System => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            Self::Fixed(cell) => cell.get(),
        }
    }
}

/// Test-only synthetic clock. Replaces boa's `FixedClock` for the
/// rquickjs bridge — same `from_millis` / `advance` surface, so the
/// integration tests in 4.8e can drop in without behaviour drift.
/// `JsRuntime::new_with_fixed_clock(dom, clock.clone())` wires the
/// shared `Cell` into the engine; bumping the cell mutates what every
/// `Date.now()` and every pending timer's `deadline <= now` check
/// observes on the next drain.
#[derive(Clone)]
pub struct FixedClock {
    inner: Rc<Cell<u64>>,
}

impl FixedClock {
    pub fn from_millis(ms: u64) -> Self {
        Self {
            inner: Rc::new(Cell::new(ms)),
        }
    }

    pub fn advance(&self, ms: u64) {
        self.inner.set(self.inner.get() + ms);
    }

    pub fn millis(&self) -> u64 {
        self.inner.get()
    }

    pub(super) fn source(&self) -> ClockSource {
        ClockSource::Fixed(self.inner.clone())
    }
}

pub(super) fn register_timers(
    ctx: &Ctx<'_>,
    clock: ClockSource,
    pending: Rc<Cell<u32>>,
) -> Result<()> {
    let clock_now = clock.clone();
    ctx.globals().set(
        "__mb_clock_now",
        Func::from(move || -> f64 { clock_now.now_ms() as f64 }),
    )?;
    // JS-side queue mutations call `__mb_set_pending(queue.length +
    // rafQueue.length)` so the Rust side knows whether the runtime
    // still has time-driven callbacks to fire. `JsRuntime::
    // has_pending_work()` reads this; `wants_continuous_redraw`
    // uses it to keep scheduling frames while timers/rAFs are live.
    let pending_setter = pending.clone();
    ctx.globals().set(
        "__mb_set_pending",
        Func::from(move |count: u32| {
            pending_setter.set(count);
        }),
    )?;
    ctx.eval::<(), _>(TIMERS_BOOT)
}

const TIMERS_BOOT: &str = r#"
(function () {
    // Pending timer queue + raf queue + cancellation set.
    // Plain JS arrays / object — every operation runs synchronously inside
    // a single Rust drain, so there's no concurrency to worry about.
    var queue = [];
    var rafQueue = [];
    var nextId = 0;
    var nextRafId = 0;
    var cancelled = Object.create(null);

    // Push the current pending count out to Rust so `has_pending_work()`
    // (and through it `wants_continuous_redraw`) can decide whether the
    // shell needs to keep redrawing. Called at every mutation point that
    // changes `queue.length + rafQueue.length`. Cancellations don't
    // change the live count — the entry sits in the queue until the
    // next `__mb_run_timers` filter pass — so they piggyback on that
    // drain's sync instead of syncing here.
    function syncPending() {
        globalThis.__mb_set_pending(queue.length + rafQueue.length);
    }

    globalThis.setTimeout = function (cb, ms) {
        var id = ++nextId;
        if (typeof cb !== 'function') { cancelled[id] = true; return id; }
        var delay = Math.max(0, +ms || 0);
        queue.push({
            id: id, cb: cb, delay: delay, repeat: false,
            deadline: globalThis.__mb_clock_now() + delay,
        });
        syncPending();
        return id;
    };

    globalThis.setInterval = function (cb, ms) {
        var id = ++nextId;
        if (typeof cb !== 'function') { cancelled[id] = true; return id; }
        var delay = Math.max(0, +ms || 0);
        queue.push({
            id: id, cb: cb, delay: delay, repeat: true,
            deadline: globalThis.__mb_clock_now() + delay,
        });
        syncPending();
        return id;
    };

    globalThis.clearTimeout = function (id) { cancelled[id] = true; };
    globalThis.clearInterval = function (id) { cancelled[id] = true; };

    globalThis.requestAnimationFrame = function (cb) {
        var id = ++nextRafId;
        if (typeof cb !== 'function') { cancelled['raf:' + id] = true; return id; }
        rafQueue.push({ id: id, cb: cb });
        syncPending();
        return id;
    };
    globalThis.cancelAnimationFrame = function (id) { cancelled['raf:' + id] = true; };

    // Fire every timer whose deadline has passed against the engine
    // clock, then loop so handlers that re-arm via setTimeout(0) still
    // get their slot in the same drain. Returns total fired so the Rust
    // drain knows whether to keep alternating with microtask drains.
    globalThis.__mb_run_timers = function () {
        var fired = 0;
        var iter = 0;
        while (iter < 1024) {
            iter++;
            var now = globalThis.__mb_clock_now();
            var due = [];
            var still = [];
            for (var i = 0; i < queue.length; i++) {
                var e = queue[i];
                if (cancelled[e.id]) continue;
                if (e.deadline <= now) due.push(e); else still.push(e);
            }
            queue = still;
            if (due.length === 0) break;
            for (var j = 0; j < due.length; j++) {
                var entry = due[j];
                if (cancelled[entry.id]) continue;
                fired++;
                try { entry.cb(); } catch (err) { /* swallow handler errors */ }
                if (entry.repeat && !cancelled[entry.id]) {
                    queue.push({
                        id: entry.id, cb: entry.cb, delay: entry.delay, repeat: true,
                        deadline: globalThis.__mb_clock_now() + entry.delay,
                    });
                }
            }
        }
        syncPending();
        return fired;
    };

    globalThis.__mb_run_raf = function (timestamp) {
        // Snapshot so a handler that re-schedules itself queues for the
        // *next* frame (browser-spec behaviour).
        var snapshot = rafQueue;
        rafQueue = [];
        syncPending();
        for (var i = 0; i < snapshot.length; i++) {
            var entry = snapshot[i];
            if (cancelled['raf:' + entry.id]) continue;
            try { entry.cb(timestamp); } catch (err) { /* swallow */ }
        }
        // A handler may have queued a new rAF or timer; resync after.
        syncPending();
    };

    // Patch `Date.now()` so it tracks the engine clock — tests using
    // FixedClock get deterministic `Date.now` reads, production runs see
    // wall-clock time. Other Date methods continue to use the host
    // implementation; only `now()` is engine-driven.
    Date.now = function () { return globalThis.__mb_clock_now(); };
})();
"#;
