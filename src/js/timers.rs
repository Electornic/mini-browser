// setTimeout / setInterval / clearTimeout / clearInterval / requestAnimationFrame /
// cancelAnimationFrame, plus the per-frame job executor that drives them.
// Cancellation funnels through a single `HashSet<u32>` shared across all
// timer-shaped APIs so the spec quirk where `clearTimeout(id)` can cancel
// a `setInterval` (and vice versa) falls out for free.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsValue, NativeFunction,
    context::time::JsInstant,
    job::{GenericJob, Job, JobExecutor, PromiseJob, TimeoutJob},
    js_string,
};

use super::RafQueue;

// Custom Boa job executor sized for the toy browser's main loop. Boa's
// stock `SimpleJobExecutor::run_jobs` blocks until every queued timeout has
// fired — fine for a one-shot script, fatal for a 60 fps render loop. This
// executor keeps the same FIFO queues but its `run_jobs` only fires
// timeouts whose deadline has *already arrived*, leaving future ones in the
// queue for a later drain. Promise/microtask jobs always drain to empty.
//
// AsyncJob support is intentionally dropped — we don't expose any host API
// (top-level await, native streams, fetch) that produces them.
pub(super) struct FrameJobExecutor {
    promise_jobs: RefCell<VecDeque<PromiseJob>>,
    generic_jobs: RefCell<VecDeque<GenericJob>>,
    // Multiple timeouts can land on the same JsInstant (millisecond clock
    // resolution + a setTimeout(0) burst from the same script tick), so the
    // value is a Vec rather than a single job. Keys are absolute due times,
    // computed at enqueue from `now + delay`.
    timeout_jobs: RefCell<BTreeMap<JsInstant, Vec<TimeoutJob>>>,
}

impl FrameJobExecutor {
    pub(super) fn new() -> Self {
        Self {
            promise_jobs: RefCell::new(VecDeque::new()),
            generic_jobs: RefCell::new(VecDeque::new()),
            timeout_jobs: RefCell::new(BTreeMap::new()),
        }
    }
}

impl JobExecutor for FrameJobExecutor {
    fn enqueue_job(self: Rc<Self>, job: Job, context: &mut Context) {
        match job {
            Job::PromiseJob(p) => self.promise_jobs.borrow_mut().push_back(p),
            Job::GenericJob(g) => self.generic_jobs.borrow_mut().push_back(g),
            Job::TimeoutJob(t) => {
                let due = context.clock().now() + t.timeout();
                self.timeout_jobs
                    .borrow_mut()
                    .entry(due)
                    .or_default()
                    .push(t);
            }
            Job::AsyncJob(_) => {
                // No host API in the toy bridge produces NativeAsyncJob, but
                // if Boa ever queues one internally we surface the drop
                // rather than silently corrupting the queue.
                eprintln!("[jobs] dropping unsupported AsyncJob");
            }
            // `Job` is `#[non_exhaustive]` upstream; future variants get the
            // same surfaced-drop treatment as AsyncJob until we decide to
            // wire them through.
            _ => eprintln!("[jobs] dropping unrecognised Job variant"),
        }
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        // Loop until a full pass produces no work — handlers can enqueue
        // more microtasks (Promise chains) or arm new timers that may
        // already be due against the now-current clock.
        loop {
            let due_jobs: Vec<TimeoutJob> = {
                let now = context.clock().now();
                let mut map = self.timeout_jobs.borrow_mut();
                let due_keys: Vec<JsInstant> = map.range(..=now).map(|(k, _)| *k).collect();
                let mut out = Vec::new();
                for key in due_keys {
                    if let Some(jobs) = map.remove(&key) {
                        out.extend(jobs);
                    }
                }
                out
            };
            let timeouts_fired = !due_jobs.is_empty();
            for job in due_jobs {
                if job.is_cancelled() {
                    continue;
                }
                if let Err(err) = job.call(context) {
                    eprintln!("[timer] handler error: {err}");
                }
            }

            let promise_drained: VecDeque<PromiseJob> =
                std::mem::take(&mut *self.promise_jobs.borrow_mut());
            let promise_fired = !promise_drained.is_empty();
            for job in promise_drained {
                if let Err(err) = job.call(context) {
                    eprintln!("[promise] job error: {err}");
                }
            }

            let generic_drained: VecDeque<GenericJob> =
                std::mem::take(&mut *self.generic_jobs.borrow_mut());
            let generic_fired = !generic_drained.is_empty();
            for job in generic_drained {
                if let Err(err) = job.call(context) {
                    eprintln!("[generic] job error: {err}");
                }
            }

            if !timeouts_fired && !promise_fired && !generic_fired {
                break;
            }
        }
        Ok(())
    }
}

// Wires the four timer-shaped globals onto the runtime. setTimeout and
// setInterval enqueue a `Job::TimeoutJob` against the executor; clearTimeout
// / clearInterval / cancelAnimationFrame all funnel into the same cancelled
// id set, which the fire path checks before calling the handler. Sharing
// one set across timers and rAF means the spec quirk that `clearTimeout(id)`
// can cancel a `setInterval` (and vice versa) falls out for free, matching
// every browser implementation.
pub(super) fn register_timers(
    context: &mut Context,
    cancelled: Rc<RefCell<HashSet<u32>>>,
    next_id: Rc<Cell<u32>>,
    raf: Rc<RefCell<RafQueue>>,
) {
    let cancelled_st = cancelled.clone();
    let next_id_st = next_id.clone();
    let set_timeout = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let id = enqueue_timer(args, ctx, &cancelled_st, &next_id_st, false);
            Ok(JsValue::from(id))
        })
    };
    let _ = context.register_global_builtin_callable(js_string!("setTimeout"), 2, set_timeout);

    let cancelled_si = cancelled.clone();
    let next_id_si = next_id.clone();
    let set_interval = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let id = enqueue_timer(args, ctx, &cancelled_si, &next_id_si, true);
            Ok(JsValue::from(id))
        })
    };
    let _ = context.register_global_builtin_callable(js_string!("setInterval"), 2, set_interval);

    let cancelled_ct = cancelled.clone();
    let clear_timeout = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            if let Some(id) = args.first().and_then(|v| v.to_u32(ctx).ok()) {
                cancelled_ct.borrow_mut().insert(id);
            }
            Ok(JsValue::undefined())
        })
    };
    let _ = context.register_global_builtin_callable(js_string!("clearTimeout"), 1, clear_timeout);

    let cancelled_ci = cancelled.clone();
    let clear_interval = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            if let Some(id) = args.first().and_then(|v| v.to_u32(ctx).ok()) {
                cancelled_ci.borrow_mut().insert(id);
            }
            Ok(JsValue::undefined())
        })
    };
    let _ =
        context.register_global_builtin_callable(js_string!("clearInterval"), 1, clear_interval);

    let raf_req = raf.clone();
    let next_id_raf = next_id.clone();
    let request_animation_frame = unsafe {
        NativeFunction::from_closure(move |_this, args, _ctx| {
            let callback = args
                .first()
                .and_then(|a| a.as_object())
                .filter(|o| o.is_callable())
                .ok_or_else(|| {
                    JsNativeError::typ()
                        .with_message("requestAnimationFrame: callback must be a function")
                })?;
            let id = next_id_raf.get().wrapping_add(1);
            next_id_raf.set(id);
            raf_req.borrow_mut().push((id, callback));
            Ok(JsValue::from(id))
        })
    };
    let _ = context.register_global_builtin_callable(
        js_string!("requestAnimationFrame"),
        1,
        request_animation_frame,
    );

    let cancelled_caf = cancelled;
    let cancel_animation_frame = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            if let Some(id) = args.first().and_then(|v| v.to_u32(ctx).ok()) {
                cancelled_caf.borrow_mut().insert(id);
            }
            Ok(JsValue::undefined())
        })
    };
    let _ = context.register_global_builtin_callable(
        js_string!("cancelAnimationFrame"),
        1,
        cancel_animation_frame,
    );
}

// Allocates the next id, validates the callback, and enqueues the timeout
// job. Shared between setTimeout and setInterval — the only difference is
// whether the closure re-enqueues itself after firing.
//
// A non-callable first arg gets a pre-cancelled id back, matching the spec
// observation that `setTimeout("not a fn")` in browsers returns a real id
// even though nothing fires; the toy bridge skips the string-as-code path
// entirely (we have no eval-by-string) and just inserts the id into the
// cancelled set so callers don't crash.
fn enqueue_timer(
    args: &[JsValue],
    context: &mut Context,
    cancelled: &Rc<RefCell<HashSet<u32>>>,
    next_id: &Rc<Cell<u32>>,
    interval: bool,
) -> u32 {
    let id = next_id.get().wrapping_add(1);
    next_id.set(id);
    let Some(handler) = args
        .first()
        .and_then(|a| a.as_object())
        .filter(|o| o.is_callable())
    else {
        cancelled.borrow_mut().insert(id);
        return id;
    };
    // Coerce the delay through ToNumber so `setTimeout(fn, "10")` and
    // `setTimeout(fn)` (delay omitted, treated as 0) both behave like
    // browsers. Negative / NaN values clamp to 0.
    let delay_ms = args
        .get(1)
        .map(|v| v.to_number(context).unwrap_or(0.0))
        .unwrap_or(0.0)
        .max(0.0) as u64;
    schedule_timer(context, handler, delay_ms, id, cancelled.clone(), interval);
    id
}

fn schedule_timer(
    context: &mut Context,
    handler: JsObject,
    delay_ms: u64,
    id: u32,
    cancelled: Rc<RefCell<HashSet<u32>>>,
    interval: bool,
) {
    let job = TimeoutJob::from_duration(
        move |ctx| {
            // Cancellation can land between enqueue and fire — check first
            // so a `clearTimeout(id)` issued during the delay window still
            // wins even though the job is already in the queue.
            if cancelled.borrow().contains(&id) {
                return Ok(JsValue::undefined());
            }
            if let Err(err) = handler.call(&JsValue::undefined(), &[], ctx) {
                eprintln!("[timer] handler error: {err}");
            }
            // Re-arm intervals from inside the handler so each tick gets a
            // fresh due time relative to *its own* completion. A handler
            // that calls `clearInterval(id)` on itself shows up in the set
            // by the time we re-check, so the interval cleanly stops.
            if interval && !cancelled.borrow().contains(&id) {
                schedule_timer(ctx, handler.clone(), delay_ms, id, cancelled.clone(), true);
            }
            Ok(JsValue::undefined())
        },
        Duration::from_millis(delay_ms),
    );
    context.enqueue_job(Job::TimeoutJob(job));
}
