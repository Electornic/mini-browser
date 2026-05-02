// Thin wrapper around boa_engine so the rest of the browser does not depend
// on Boa types directly. The runtime owns a single `Context` whose globals
// (var bindings, declared functions) persist across `execute` calls within
// the same document — that lets `<script>` tags later in the page see what
// earlier ones defined, matching real browser semantics.
//
// Boa's `Context` is `!Send`, so JS execution must stay on the main thread.
// Resource fetching uses `thread::scope`; keep `JsRuntime` calls out of those
// scopes.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source,
    context::{
        ContextBuilder,
        time::{Clock, JsInstant, StdClock},
    },
    js_string,
    job::{GenericJob, Job, JobExecutor, PromiseJob, TimeoutJob},
    object::{ObjectInitializer, builtins::JsArray},
    property::Attribute,
};

#[cfg(test)]
use boa_engine::context::time::FixedClock;

use crate::{
    css::{self, Combinator, Selector, SimpleSelector, SimpleSelectorKind},
    dom::{AttrMap, Document, NodeId, NodeType},
};

// Hidden property name used to round-trip a NodeId through any Element
// JsObject — methods like `appendChild(other)` read `other._nodeId` to
// recover the receiver's NodeId without an external wrapper-to-NodeId table.
// JS code shouldn't poke at this; the dynamic mutation methods all
// re-validate the recovered id against the live arena before acting on it.
const NODE_ID_PROP: &str = "_nodeId";

// Per-node event listener registry. Keyed by `(NodeId, event_type_name)`,
// each entry holds the callable JS objects passed to `addEventListener` in
// insertion order. Listeners live on `JsRuntime` rather than on individual
// Element wrappers because multiple wrappers may exist for the same NodeId
// (children getter, repeated `getElementById`, …) and they all need to
// observe the same listener set. We store the original `JsObject` (not a
// converted `JsFunction`) so identity comparisons via `JsObject::equals`
// line up with the wrappers JS code passes back to `removeEventListener`.
type ListenerMap = HashMap<(NodeId, String), Vec<JsObject>>;

// Live registry of requestAnimationFrame callbacks awaiting the next frame.
// Vec rather than HashMap because the toy bridge fires them in registration
// order; cancellation is handled out-of-band via `cancelled_timers`.
type RafQueue = Vec<(u32, JsObject)>;

pub struct JsRuntime {
    context: Context,
    // Shared handle to the parsed Document. The runtime hands clones to
    // every native closure (so each closure observes the live tree without
    // re-reading the field) and reads the field directly when synthesising
    // bubble paths in `dispatch_event`.
    dom: Rc<RefCell<Document>>,
    // Per-node event listener registry. Cloned into every Element wrapper's
    // addEventListener / removeEventListener closure and read directly from
    // `dispatch_event` when invoking handlers.
    listeners: Rc<RefCell<ListenerMap>>,
    // Pending requestAnimationFrame callbacks; drained once per frame by
    // `run_animation_frame_callbacks`. Callbacks scheduled during a drain
    // queue here for the next frame (snapshot-then-fire), matching browser
    // semantics where re-rAF inside an rAF handler runs the next paint.
    raf_callbacks: Rc<RefCell<RafQueue>>,
    // Set of cancelled timer / rAF ids. Both `setTimeout` and
    // `requestAnimationFrame` allocate ids from `next_timer_id`; on
    // `clearTimeout` / `cancelAnimationFrame` the id lands here, and the
    // fire path checks the set before invoking the handler. Recording
    // cancellations out-of-band lets us avoid touching Boa's internal
    // `TimeoutJob::cancelled_flag` (which is `pub(crate)`) and gives the
    // same effect for re-enqueued `setInterval` ticks.
    cancelled_timers: Rc<RefCell<HashSet<u32>>>,
}

impl JsRuntime {
    /// Build a runtime bound to `dom`. The caller keeps a clone of the Rc so
    /// it can read the DOM back (for layout) and mutate it directly (for
    /// page swaps via `*dom.borrow_mut() = …`); JS-side mutations land in the
    /// same arena. Per Step 5.1, ownership is shared at construction time —
    /// there is no `bind_document` afterward because nothing inside the engine
    /// needs to switch Documents mid-life: a navigation rebuilds JsRuntime.
    pub fn new(dom: Rc<RefCell<Document>>) -> Self {
        Self::build(dom, Rc::new(StdClock))
    }

    /// Test-only constructor that wires a `FixedClock` into the engine so
    /// timer tests can advance time deterministically without sleeping the
    /// thread. Production callers always go through `new` with `StdClock`.
    #[cfg(test)]
    pub fn new_with_fixed_clock(dom: Rc<RefCell<Document>>, clock: Rc<FixedClock>) -> Self {
        Self::build(dom, clock)
    }

    fn build<C: Clock + 'static>(dom: Rc<RefCell<Document>>, clock: Rc<C>) -> Self {
        let executor = Rc::new(FrameJobExecutor::new());
        let mut context = ContextBuilder::default()
            .clock(clock)
            .job_executor(executor)
            .build()
            .expect("Boa context should build with default settings");
        let listeners: Rc<RefCell<ListenerMap>> = Rc::new(RefCell::new(HashMap::new()));
        let raf_callbacks: Rc<RefCell<RafQueue>> = Rc::new(RefCell::new(Vec::new()));
        let cancelled_timers: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));
        let next_timer_id: Rc<Cell<u32>> = Rc::new(Cell::new(0));
        register_console(&mut context);
        register_window_aliases(&mut context);
        register_document(&mut context, dom.clone(), listeners.clone());
        register_timers(
            &mut context,
            cancelled_timers.clone(),
            next_timer_id.clone(),
            raf_callbacks.clone(),
        );
        Self {
            context,
            dom,
            listeners,
            raf_callbacks,
            cancelled_timers,
        }
    }

    /// Returns a clone of the shared DOM handle. Mainly useful in tests where
    /// the test wants to swap the document contents under the runtime to
    /// simulate a navigation.
    #[cfg(test)]
    pub fn dom_handle(&self) -> Rc<RefCell<Document>> {
        self.dom.clone()
    }

    // Returns the displayed form of the result on success, or a stringified
    // error on failure. Both branches are surface-level — callers that need
    // structured access to JsValue should reach into `self.context` directly.
    pub fn execute(&mut self, source: &str) -> Result<String, String> {
        let result = self
            .context
            .eval(Source::from_bytes(source))
            .map(|value| value.display().to_string())
            .map_err(|err| err.to_string());
        // After the script body returns we drain any jobs it queued —
        // microtask resolutions from Promises and any setTimeout(0)
        // callbacks scheduled synchronously. That mirrors the HTML
        // event-loop step where microtasks run after every script-or-task;
        // without it `Promise.resolve().then(...)` would never observe the
        // assignment within the same `execute` call.
        self.drain_pending_jobs();
        result
    }

    /// Drain every job currently queued on the runtime's executor: pending
    /// promise/microtask jobs first, then any setTimeout/setInterval handlers
    /// whose deadlines have arrived against the engine clock. The browser
    /// main loop calls this once per frame so timers fire roughly on time
    /// without a separate timer thread; tests call it after advancing the
    /// fixed clock to assert handler-side effects.
    pub fn drain_pending_jobs(&mut self) {
        if let Err(err) = self.context.run_jobs() {
            eprintln!("[jobs] error draining job queue: {err}");
        }
    }

    /// Fire every requestAnimationFrame callback that was registered up to
    /// now, snapshotting the queue first so a handler that re-schedules
    /// itself queues for the *next* frame (browser-spec behaviour). Each
    /// callback receives a single `DOMHighResTimeStamp` argument — the
    /// engine clock's `millis_since_epoch`. After the snapshot drains, any
    /// microtasks the handlers scheduled run too.
    pub fn run_animation_frame_callbacks(&mut self) {
        let snapshot: Vec<(u32, JsObject)> =
            std::mem::take(&mut *self.raf_callbacks.borrow_mut());
        if snapshot.is_empty() {
            return;
        }
        let timestamp = JsValue::from(self.context.clock().now().millis_since_epoch() as f64);
        for (id, callback) in snapshot {
            if self.cancelled_timers.borrow().contains(&id) {
                continue;
            }
            if let Err(err) = callback.call(
                &JsValue::undefined(),
                std::slice::from_ref(&timestamp),
                &mut self.context,
            ) {
                eprintln!("[raf] callback error: {err}");
            }
        }
        self.drain_pending_jobs();
    }

    /// Synthesise a DOM event of the given type at `target` and bubble it
    /// up through the parent chain, invoking every registered listener
    /// along the way. The main loop calls this on left-mouse clicks
    /// (`event_type = "click"`) — Step 6's surface for getting page-level
    /// pointer input back into JS land.
    ///
    /// If `target` lands on a Text node — which is what the hit-tester
    /// returns when the click hits inline text — dispatch retargets to the
    /// nearest Element ancestor. Text wrappers don't expose
    /// `addEventListener`, and almost every author-side click handler
    /// expects `event.target` to be the Element it lives on.
    pub fn dispatch_event(&mut self, target: NodeId, event_type: &str) {
        let event_target = {
            let dom = self.dom.borrow();
            let mut cur = Some(target);
            loop {
                match cur {
                    Some(id) => match dom.get(id) {
                        Some(node) => match &node.node_type {
                            NodeType::Element(_) => break Some(id),
                            NodeType::Text(_) => cur = node.parent,
                        },
                        None => break None,
                    },
                    None => break None,
                }
            }
        };
        let Some(event_target) = event_target else {
            return;
        };
        let chain: Vec<NodeId> = {
            let dom = self.dom.borrow();
            let mut chain = Vec::new();
            let mut cur = Some(event_target);
            while let Some(id) = cur {
                match dom.get(id) {
                    Some(node) => {
                        chain.push(id);
                        cur = node.parent;
                    }
                    None => break,
                }
            }
            chain
        };
        if chain.is_empty() {
            return;
        }
        let event = build_event_object(
            event_type,
            event_target,
            self.dom.clone(),
            self.listeners.clone(),
            &mut self.context,
        );
        let event_value = JsValue::from(event);
        let key_type = event_type.to_string();
        for current_target in chain {
            // Snapshot the listener list so a handler that calls
            // `removeEventListener` on itself mid-iteration doesn't shorten
            // the slice we're walking.
            let snapshot: Vec<JsObject> = self
                .listeners
                .borrow()
                .get(&(current_target, key_type.clone()))
                .cloned()
                .unwrap_or_default();
            if snapshot.is_empty() {
                continue;
            }
            // A previous handler may have removed `current_target` from the
            // tree; skip dispatch on tombstoned ancestors so make_element's
            // "Element NodeId" expect doesn't fire.
            let still_alive = matches!(
                self.dom.borrow().get(current_target).map(|n| &n.node_type),
                Some(NodeType::Element(_))
            );
            if !still_alive {
                continue;
            }
            let this = JsValue::from(make_element(
                current_target,
                self.dom.clone(),
                self.listeners.clone(),
                &mut self.context,
            ));
            for handler in snapshot {
                if let Err(err) =
                    handler.call(&this, std::slice::from_ref(&event_value), &mut self.context)
                {
                    eprintln!("[event] {event_type} handler error: {err}");
                }
            }
        }
        // A handler may have resolved a promise or queued a setTimeout(0);
        // drain those before returning so observers up the call stack see
        // a fully-settled JS state without waiting for the next frame.
        self.drain_pending_jobs();
    }
}

// Boa's Context is not Debug; surface a placeholder so containing structs can
// keep deriving Debug for diagnostics.
impl std::fmt::Debug for JsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsRuntime").finish_non_exhaustive()
    }
}

// Wires `console.log/warn/error` to stderr. Boa's default `Context` ships
// without `console`, and adding the optional `boa_runtime` crate would pull
// in extra dependencies just for this — a three-method shim is enough for the
// debug-printf use case scripts actually rely on. Each call coerces every
// argument with the standard JS ToString algorithm so that `console.log("hi")`
// prints `hi`, not `"hi"`.
fn register_console(context: &mut Context) {
    let console = ObjectInitializer::new(context)
        .function(
            NativeFunction::from_fn_ptr(console_log),
            js_string!("log"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(console_warn),
            js_string!("warn"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(console_error),
            js_string!("error"),
            0,
        )
        .build();
    let _ = context.register_global_property(js_string!("console"), console, Attribute::all());
}

fn console_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_console("log", args, context);
    Ok(JsValue::undefined())
}

fn console_warn(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_console("warn", args, context);
    Ok(JsValue::undefined())
}

fn console_error(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_console("error", args, context);
    Ok(JsValue::undefined())
}

// Browsers expose `window` and `self` as aliases of the global object —
// scripts in the wild rely on either name being defined (`window.foo`,
// `self.addEventListener`, `typeof window === 'object'` feature checks).
// Boa already provides `globalThis` per spec; we just bind the two extra
// names to the same object so `window === globalThis === self` and a
// `var x` at top level shows up as `window.x` like every other engine.
fn register_window_aliases(context: &mut Context) {
    let global = context.global_object();
    let _ = context.register_global_property(
        js_string!("window"),
        JsValue::from(global.clone()),
        Attribute::all(),
    );
    let _ = context.register_global_property(
        js_string!("self"),
        JsValue::from(global),
        Attribute::all(),
    );
}

fn write_console(level: &str, args: &[JsValue], context: &mut Context) {
    let parts: Vec<String> = args
        .iter()
        .map(|v| match v.to_string(context) {
            Ok(s) => s.to_std_string_escaped(),
            // ToString failed (rare — Symbol, or a custom toString that threw).
            // Fall back to the debug-style display so something useful still prints.
            Err(_) => v.display().to_string(),
        })
        .collect();
    eprintln!("[console.{level}] {}", parts.join(" "));
}

// Builds the `document` global. Each method captures its own `Rc` clone of
// the shared Document handle so they stay valid after `register_document`
// returns. The closures use `unsafe from_closure` because our captures
// (Rc<RefCell<Document>>) are pure host data — no JS values hide inside, so
// Boa's GC has nothing to trace through them.
fn register_document(
    context: &mut Context,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
) {
    let dom_for_id = dom.clone();
    let listeners_for_id = listeners.clone();
    let get_element_by_id = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let id = first_arg_as_string(args, ctx)?;
            // Borrow scoped to the lookup so make_element below can take its
            // own borrow without the two stepping on each other.
            let node_id = {
                let document = dom_for_id.borrow();
                find_by_id(&document, &id)
            };
            match node_id {
                Some(node_id) => Ok(JsValue::from(make_element(
                    node_id,
                    dom_for_id.clone(),
                    listeners_for_id.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_for_qs = dom.clone();
    let listeners_for_qs = listeners.clone();
    let query_selector = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let selector_text = first_arg_as_string(args, ctx)?;
            let selector = match css::parse_selector(&selector_text) {
                Ok(s) => s,
                Err(err) => {
                    return Err(JsNativeError::syntax()
                        .with_message(format!(
                            "invalid selector `{selector_text}`: {} (at byte {})",
                            err.message, err.position
                        ))
                        .into());
                }
            };
            let node_id = {
                let document = dom_for_qs.borrow();
                find_first_match(&document, &selector)
            };
            match node_id {
                Some(node_id) => Ok(JsValue::from(make_element(
                    node_id,
                    dom_for_qs.clone(),
                    listeners_for_qs.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_for_create = dom.clone();
    let listeners_for_create = listeners.clone();
    let create_element = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let tag = first_arg_as_string(args, ctx)?;
            // Match the parser convention: tag names live lowercase in the
            // arena, regardless of how JS spelled them. The tagName getter
            // surfaces the canonical uppercase form back to JS.
            let tag_lower = tag.to_ascii_lowercase();
            let new_id = dom_for_create
                .borrow_mut()
                .create_element(tag_lower, AttrMap::new());
            Ok(JsValue::from(make_element(
                new_id,
                dom_for_create.clone(),
                listeners_for_create.clone(),
                ctx,
            )))
        })
    };

    let dom_for_text = dom;
    let create_text_node = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let text = first_arg_as_string(args, ctx)?;
            let new_id = dom_for_text.borrow_mut().create_text(text);
            Ok(JsValue::from(make_text(new_id, dom_for_text.clone(), ctx)))
        })
    };

    let document = ObjectInitializer::new(context)
        .function(get_element_by_id, js_string!("getElementById"), 1)
        .function(query_selector, js_string!("querySelector"), 1)
        .function(create_element, js_string!("createElement"), 1)
        .function(create_text_node, js_string!("createTextNode"), 1)
        .build();

    let _ = context.register_global_property(js_string!("document"), document, Attribute::all());
}

// Custom Boa job executor sized for the toy browser's main loop. Boa's
// stock `SimpleJobExecutor::run_jobs` blocks until every queued timeout has
// fired — fine for a one-shot script, fatal for a 60 fps render loop. This
// executor keeps the same FIFO queues but its `run_jobs` only fires
// timeouts whose deadline has *already arrived*, leaving future ones in the
// queue for a later drain. Promise/microtask jobs always drain to empty.
//
// AsyncJob support is intentionally dropped — we don't expose any host API
// (top-level await, native streams, fetch) that produces them.
struct FrameJobExecutor {
    promise_jobs: RefCell<VecDeque<PromiseJob>>,
    generic_jobs: RefCell<VecDeque<GenericJob>>,
    // Multiple timeouts can land on the same JsInstant (millisecond clock
    // resolution + a setTimeout(0) burst from the same script tick), so the
    // value is a Vec rather than a single job. Keys are absolute due times,
    // computed at enqueue from `now + delay`.
    timeout_jobs: RefCell<BTreeMap<JsInstant, Vec<TimeoutJob>>>,
}

impl FrameJobExecutor {
    fn new() -> Self {
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
fn register_timers(
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

fn first_arg_as_string(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    nth_arg_as_string(args, 0, context)
}

fn nth_arg_as_string(args: &[JsValue], n: usize, context: &mut Context) -> JsResult<String> {
    let arg = args.get(n).cloned().unwrap_or_default();
    Ok(arg.to_string(context)?.to_std_string_escaped())
}

// Recovers a NodeId from any Element wrapper by reading the hidden `_nodeId`
// data property the wrapper factory stored. Returns Err for non-Element
// arguments (foreign objects, primitives) — that's the TypeError the DOM
// methods report.
fn read_node_id(arg: &JsValue, context: &mut Context) -> JsResult<NodeId> {
    let object = arg.as_object().ok_or_else(|| {
        JsNativeError::typ().with_message("expected an Element-like argument")
    })?;
    let raw = object
        .get(js_string!(NODE_ID_PROP), context)?
        .to_u32(context)?;
    Ok(NodeId::from_raw(raw))
}

// Builds an Element wrapper that resolves every observable property against
// the shared Document on each access. Multiple wrappers may exist for the
// same NodeId (e.g. one returned from getElementById, another later from
// `.children[0]`) — they're equivalent because all reads/writes funnel
// through the same `Rc<RefCell<Document>>`.
//
// `tagName` is the only static property: the DOM treats it as readonly, so a
// one-time uppercase snapshot is correct and saves a borrow per access.
// Everything else (textContent, children, getAttribute/setAttribute,
// appendChild/removeChild) is dynamic so post-mutation reads observe the
// new tree.
fn make_element(
    node_id: NodeId,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    context: &mut Context,
) -> JsObject {
    let tag = {
        let document = dom.borrow();
        let element = document
            .element_data(node_id)
            .expect("element factory called with non-Element NodeId");
        element.tag_name.to_ascii_uppercase()
    };

    let dom_g = dom.clone();
    let text_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let document = dom_g.borrow();
            // Stale handle (the slot was tombstoned): degrade to null per the
            // Step 5.1.4 silent-degrade policy. Throwing is reserved for a
            // later commit.
            if document.get(node_id).is_none() {
                return Ok(JsValue::null());
            }
            let text = collect_text_content(&document, node_id);
            Ok(JsValue::from(JsString::from(text.as_str())))
        })
    }
    .to_js_function(context.realm());

    let dom_s = dom.clone();
    let text_set = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_text = first_arg_as_string(args, ctx)?;
            // Mutation setters on a stale handle throw — getters keep the
            // older silent-null behaviour because reading a removed node is
            // common (logging, cleanup) and shouldn't blow up scripts, but
            // *writing* through a dead handle is a genuine bug worth
            // surfacing per Step 5.1.5.
            let mut document = dom_s.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            document.replace_with_text(node_id, new_text);
            Ok(JsValue::undefined())
        })
    }
    .to_js_function(context.realm());

    let dom_c = dom.clone();
    let listeners_c = listeners.clone();
    let children_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            // Snapshot the current Element children into a Vec<NodeId> while
            // holding the borrow, then drop it before recursing into
            // make_element — make_element re-borrows to read tag names, so
            // overlapping borrows would panic. Text-only children are
            // filtered out so .children mirrors HTMLCollection (Element
            // kids only) rather than .childNodes.
            let kids: Vec<NodeId> = {
                let document = dom_c.borrow();
                match document.get(node_id) {
                    Some(node) => node
                        .children
                        .iter()
                        .copied()
                        .filter(|cid| {
                            matches!(
                                document.get(*cid).map(|n| &n.node_type),
                                Some(NodeType::Element(_))
                            )
                        })
                        .collect(),
                    None => Vec::new(),
                }
            };
            let array = JsArray::new(ctx);
            for child_id in kids {
                let child_obj = make_element(child_id, dom_c.clone(), listeners_c.clone(), ctx);
                let _ = array.push(JsValue::from(child_obj), ctx);
            }
            Ok(JsValue::from(array))
        })
    }
    .to_js_function(context.realm());

    let dom_ga = dom.clone();
    let get_attribute = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let name = first_arg_as_string(args, ctx)?;
            let document = dom_ga.borrow();
            match document.element_data(node_id) {
                Some(elem) => match elem.attributes.get(&name) {
                    Some(value) => Ok(JsValue::from(JsString::from(value.as_str()))),
                    None => Ok(JsValue::null()),
                },
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_sa = dom.clone();
    let set_attribute = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let name = first_arg_as_string(args, ctx)?;
            let value = nth_arg_as_string(args, 1, ctx)?;
            let mut document = dom_sa.borrow_mut();
            match document.element_data_mut(node_id) {
                Some(elem) => {
                    elem.attributes.insert(name, value);
                    Ok(JsValue::undefined())
                }
                None => Err(stale_node_error()),
            }
        })
    };

    let dom_ac = dom.clone();
    let append_child = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let arg = args.first().cloned().unwrap_or_default();
            let other_id = read_node_id(&arg, ctx)?;
            {
                let mut document = dom_ac.borrow_mut();
                // Both ids must point at live slots; per Step 5.1.5 a stale
                // receiver or argument is a script bug, not a no-op.
                if document.get(node_id).is_none() || document.get(other_id).is_none() {
                    return Err(stale_node_error());
                }
                // A node can only live in one parent at a time — unhook it
                // first so we don't end up with the same NodeId in two
                // children lists.
                document.detach(other_id);
                document.append_child(node_id, other_id);
            }
            Ok(arg)
        })
    };

    let dom_rc = dom.clone();
    let remove_child = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let arg = args.first().cloned().unwrap_or_default();
            let other_id = read_node_id(&arg, ctx)?;
            let mut document = dom_rc.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            // The standard throws NotFoundError when the target isn't a
            // direct child; we surface that as a TypeError so callers see
            // a real exception rather than the previous silent-null result.
            if !document.remove_child(node_id, other_id) {
                return Err(JsNativeError::typ()
                    .with_message("removeChild: target is not a child of this node")
                    .into());
            }
            // Toy bridge convention: tombstone the removed subtree so its
            // wrappers cleanly resolve to None on stale-handle checks.
            // A future commit can park the node in a "free" pool if
            // reattachment turns out to matter.
            document.tombstone_subtree(other_id);
            Ok(arg)
        })
    };

    let dom_ib = dom.clone();
    let insert_before = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_arg = args.first().cloned().unwrap_or_default();
            let ref_arg = args.get(1).cloned().unwrap_or_default();
            let new_id = read_node_id(&new_arg, ctx)?;
            // Spec: when refNode is null, insertBefore degrades to
            // appendChild. Resolving the optional id outside the borrow
            // keeps `read_node_id` (which uses ctx) from racing the
            // dom borrow_mut below.
            let ref_id_opt = if ref_arg.is_null() || ref_arg.is_undefined() {
                None
            } else {
                Some(read_node_id(&ref_arg, ctx)?)
            };
            let mut document = dom_ib.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            match ref_id_opt {
                None => {
                    if document.get(new_id).is_none() {
                        return Err(stale_node_error());
                    }
                    document.detach(new_id);
                    document.append_child(node_id, new_id);
                }
                Some(ref_id) => {
                    if !document.insert_before(node_id, new_id, ref_id) {
                        return Err(JsNativeError::typ()
                            .with_message(
                                "insertBefore: reference node is not a child of this node",
                            )
                            .into());
                    }
                }
            }
            Ok(new_arg)
        })
    };

    let dom_rep = dom.clone();
    let replace_child = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_arg = args.first().cloned().unwrap_or_default();
            let old_arg = args.get(1).cloned().unwrap_or_default();
            let new_id = read_node_id(&new_arg, ctx)?;
            let old_id = read_node_id(&old_arg, ctx)?;
            let mut document = dom_rep.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            if !document.replace_child(node_id, new_id, old_id) {
                return Err(JsNativeError::typ()
                    .with_message("replaceChild: target node is not a child of this node")
                    .into());
            }
            // Standard returns the (now-removed) old node. Our tombstoning
            // means subsequent reads on the returned wrapper observe the
            // usual stale-handle semantics — sufficient for the toy bridge.
            Ok(old_arg)
        })
    };

    let dom_cl = dom.clone();
    let listeners_cl = listeners.clone();
    let clone_node = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let deep = args.first().is_some_and(|v| v.to_boolean());
            let new_id = {
                let mut document = dom_cl.borrow_mut();
                document.clone_node(node_id, deep)
            };
            match new_id {
                Some(id) => Ok(
                    make_node(id, dom_cl.clone(), listeners_cl.clone(), ctx)
                        .map(JsValue::from)
                        .unwrap_or(JsValue::null()),
                ),
                None => Err(stale_node_error()),
            }
        })
    };

    let dom_ael = dom.clone();
    let listeners_ael = listeners.clone();
    let add_event_listener = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let event_type = first_arg_as_string(args, ctx)?;
            let handler_obj = args
                .get(1)
                .and_then(|arg| arg.as_object())
                .filter(|obj| obj.is_callable())
                .ok_or_else(|| {
                    JsNativeError::typ()
                        .with_message("addEventListener: handler must be a function")
                })?;
            // Stale receiver: writing through a removed wrapper is the same
            // bug class as the other mutation entry points, so throw rather
            // than silently pile up listeners on a tombstoned node.
            if dom_ael.borrow().get(node_id).is_none() {
                return Err(stale_node_error());
            }
            let mut map = listeners_ael.borrow_mut();
            let entry = map.entry((node_id, event_type)).or_default();
            // Whatwg dedup: same `(target, type, callback)` tuple registered
            // twice is treated as one listener. Identity-compare the
            // underlying JsObject so two distinct `function () {}` literals
            // (different objects, identical bodies) still count as two.
            if !entry
                .iter()
                .any(|existing| JsObject::equals(existing, &handler_obj))
            {
                entry.push(handler_obj);
            }
            Ok(JsValue::undefined())
        })
    };

    let listeners_rel = listeners.clone();
    let remove_event_listener = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let event_type = first_arg_as_string(args, ctx)?;
            // A non-callable / missing second arg is a no-op per spec —
            // there's nothing to match in the registry.
            let Some(handler_obj) = args.get(1).and_then(|arg| arg.as_object()) else {
                return Ok(JsValue::undefined());
            };
            let mut map = listeners_rel.borrow_mut();
            if let Some(entry) = map.get_mut(&(node_id, event_type)) {
                entry.retain(|existing| !JsObject::equals(existing, &handler_obj));
            }
            Ok(JsValue::undefined())
        })
    };

    ObjectInitializer::new(context)
        .property(
            js_string!(NODE_ID_PROP),
            JsValue::from(node_id.raw()),
            Attribute::all(),
        )
        .property(
            js_string!("nodeType"),
            JsValue::from(1i32),
            Attribute::all(),
        )
        .property(
            js_string!("tagName"),
            JsString::from(tag.as_str()),
            Attribute::all(),
        )
        .accessor(
            js_string!("textContent"),
            Some(text_get),
            Some(text_set),
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("children"),
            Some(children_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .function(get_attribute, js_string!("getAttribute"), 1)
        .function(set_attribute, js_string!("setAttribute"), 2)
        .function(append_child, js_string!("appendChild"), 1)
        .function(remove_child, js_string!("removeChild"), 1)
        .function(insert_before, js_string!("insertBefore"), 2)
        .function(replace_child, js_string!("replaceChild"), 2)
        .function(clone_node, js_string!("cloneNode"), 1)
        .function(add_event_listener, js_string!("addEventListener"), 2)
        .function(remove_event_listener, js_string!("removeEventListener"), 2)
        .build()
}

// Wrapper for a Text node — much thinner than Element since text nodes
// only carry a string. The single accessor (`textContent`) doubles as the
// `data` / `nodeValue` getter and setter; the toy bridge skips those alias
// names rather than cloning the closures three times.
//
// `_nodeId` round-trips just like the Element wrapper so methods like
// `parent.appendChild(textNode)` and `parent.replaceChild(text, oldNode)`
// can recover the NodeId without any extra dispatch.
fn make_text(node_id: NodeId, dom: Rc<RefCell<Document>>, context: &mut Context) -> JsObject {
    let dom_g = dom.clone();
    let text_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let document = dom_g.borrow();
            match document.text(node_id) {
                Some(t) => Ok(JsValue::from(JsString::from(t))),
                None => Ok(JsValue::null()),
            }
        })
    }
    .to_js_function(context.realm());

    let dom_s = dom;
    let text_set = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_text = first_arg_as_string(args, ctx)?;
            // `set_text` returns false when the slot is gone OR when it
            // points at an Element. Both are stale-handle scenarios from
            // the script's perspective: throw rather than silently lose
            // the write.
            if !dom_s.borrow_mut().set_text(node_id, new_text) {
                return Err(stale_node_error());
            }
            Ok(JsValue::undefined())
        })
    }
    .to_js_function(context.realm());

    ObjectInitializer::new(context)
        .property(
            js_string!(NODE_ID_PROP),
            JsValue::from(node_id.raw()),
            Attribute::all(),
        )
        .property(
            js_string!("nodeType"),
            JsValue::from(3i32),
            Attribute::all(),
        )
        .accessor(
            js_string!("textContent"),
            Some(text_get),
            Some(text_set),
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .build()
}

// Dispatch helper: hand back the right wrapper for whatever kind of node
// `node_id` happens to be. Used by `cloneNode` (whose result mirrors the
// source's kind) and by anything else that needs to surface a not-yet-typed
// NodeId to JS without the caller pre-computing the variant.
//
// Returns `None` only when the slot is already tombstoned — callers either
// propagate that as a stale-handle throw or fall back to `JsValue::null()`.
fn make_node(
    node_id: NodeId,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    context: &mut Context,
) -> Option<JsObject> {
    let is_element = {
        let document = dom.borrow();
        document
            .get(node_id)
            .map(|n| matches!(n.node_type, NodeType::Element(_)))
    };
    match is_element {
        Some(true) => Some(make_element(node_id, dom, listeners, context)),
        Some(false) => Some(make_text(node_id, dom, context)),
        None => None,
    }
}

// Minimal Event object passed to every dispatched listener. Carries the
// event type string and a wrapper for the original target Element. Future
// commits will round it out with `currentTarget`, `preventDefault`, and
// `stopPropagation` — the toy bridge skips them since clicks always
// bubble fully through and the only side-effect a handler can suppress
// today is link navigation, which Step 6 explicitly leaves running.
fn build_event_object(
    event_type: &str,
    target: NodeId,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    context: &mut Context,
) -> JsObject {
    let target_wrapper = make_element(target, dom, listeners, context);
    ObjectInitializer::new(context)
        .property(
            js_string!("type"),
            JsString::from(event_type),
            Attribute::all(),
        )
        .property(
            js_string!("target"),
            JsValue::from(target_wrapper),
            Attribute::all(),
        )
        .build()
}

// Standard error returned by every mutation entry point when the receiver
// or argument refers to a slot that's been tombstoned. Step 5.1.5 promoted
// this from the previous silent-null degrade because writing through a
// dead handle is a script bug worth surfacing — getters keep the lenient
// behaviour since reading after removal is a common cleanup pattern.
fn stale_node_error() -> boa_engine::JsError {
    JsNativeError::typ()
        .with_message("operation on detached or removed node")
        .into()
}

fn collect_text_content(document: &Document, node_id: NodeId) -> String {
    let mut buf = String::new();
    walk_text(document, node_id, &mut buf);
    buf
}

fn walk_text(document: &Document, node_id: NodeId, buf: &mut String) {
    let Some(node) = document.get(node_id) else {
        return;
    };
    match &node.node_type {
        NodeType::Text(text) => buf.push_str(text),
        NodeType::Element(_) => {
            for child in &node.children {
                walk_text(document, *child, buf);
            }
        }
    }
}

fn find_by_id(document: &Document, id: &str) -> Option<NodeId> {
    for &root in document.roots() {
        if let Some(found) = walk_for_id(document, root, id) {
            return Some(found);
        }
    }
    None
}

fn walk_for_id(document: &Document, node_id: NodeId, id: &str) -> Option<NodeId> {
    let node = document.get(node_id)?;
    if let NodeType::Element(elem) = &node.node_type
        && elem.attributes.get("id").is_some_and(|v| v == id)
    {
        return Some(node_id);
    }
    for child in &node.children {
        if let Some(found) = walk_for_id(document, *child, id) {
            return Some(found);
        }
    }
    None
}

fn find_first_match(document: &Document, selector: &Selector) -> Option<NodeId> {
    let mut ancestors: Vec<NodeId> = Vec::new();
    for &root in document.roots() {
        if let Some(found) = walk_for_match(document, root, selector, &mut ancestors) {
            return Some(found);
        }
    }
    None
}

fn walk_for_match(
    document: &Document,
    node_id: NodeId,
    selector: &Selector,
    ancestors: &mut Vec<NodeId>,
) -> Option<NodeId> {
    if matches_static_selector(document, node_id, ancestors, selector) {
        return Some(node_id);
    }
    // Snapshot children before recursing so a lookup against the
    // arena doesn't conflict with the recursive borrows.
    let children: Vec<NodeId> = match document.get(node_id) {
        Some(node) => node.children.clone(),
        None => return None,
    };
    ancestors.push(node_id);
    for child in &children {
        if let Some(found) = walk_for_match(document, *child, selector, ancestors) {
            ancestors.pop();
            return Some(found);
        }
    }
    ancestors.pop();
    None
}

// Mirrors style::matches_selector but skips pseudo-class state — querySelector
// is a static lookup against the parsed Document, no hover/focus context to
// thread through. Pseudo-classes parse-but-ignore here: `.btn:hover` matches
// the same set as `.btn`.
fn matches_static_selector(
    document: &Document,
    node_id: NodeId,
    ancestors: &[NodeId],
    selector: &Selector,
) -> bool {
    let Some((target, leading)) = selector.parts.split_last() else {
        return false;
    };
    if !matches_simple_static(document, node_id, target) {
        return false;
    }
    let mut iter = ancestors.iter().rev();
    for (j, part) in leading.iter().enumerate().rev() {
        let combinator = selector.combinators[j];
        match combinator {
            Combinator::Descendant => loop {
                match iter.next() {
                    Some(ancestor) if matches_simple_static(document, *ancestor, part) => break,
                    Some(_) => continue,
                    None => return false,
                }
            },
            Combinator::Child => match iter.next() {
                Some(ancestor) if matches_simple_static(document, *ancestor, part) => {}
                _ => return false,
            },
        }
    }
    true
}

fn matches_simple_static(document: &Document, node_id: NodeId, simple: &SimpleSelector) -> bool {
    let element = match document.get(node_id).map(|n| &n.node_type) {
        Some(NodeType::Element(e)) => e,
        _ => return false,
    };
    match &simple.kind {
        SimpleSelectorKind::Tag(tag) => element.tag_name == *tag,
        SimpleSelectorKind::Class(class) => element
            .attributes
            .get("class")
            .is_some_and(|v| v.split_whitespace().any(|c| c == class)),
        SimpleSelectorKind::Id(id) => element.attributes.get("id").is_some_and(|v| v == id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    fn runtime_with(html: &str) -> JsRuntime {
        let document = html::parse(html).unwrap();
        let dom = Rc::new(RefCell::new(document));
        JsRuntime::new(dom)
    }

    // Step 7 helper — pairs a `JsRuntime` with the `FixedClock` it sees,
    // so timer tests can advance simulated time without touching the wall
    // clock. The Rc clone is what gives the test its handle on the same
    // clock the engine uses internally.
    fn runtime_with_fixed_clock(html: &str) -> (JsRuntime, Rc<FixedClock>) {
        let document = html::parse(html).unwrap();
        let dom = Rc::new(RefCell::new(document));
        let clock = Rc::new(FixedClock::from_millis(0));
        let runtime = JsRuntime::new_with_fixed_clock(dom, clock.clone());
        (runtime, clock)
    }

    #[test]
    fn evaluates_arithmetic() {
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("1 + 2 * 3").unwrap(), "7");
    }

    #[test]
    fn preserves_global_state_between_calls() {
        let mut runtime = runtime_with("");
        runtime.execute("var page = 41;").unwrap();
        assert_eq!(runtime.execute("page + 1").unwrap(), "42");
    }

    #[test]
    fn surfaces_runtime_errors() {
        let mut runtime = runtime_with("");
        let err = runtime.execute("missing.prop").unwrap_err();
        assert!(
            err.to_lowercase().contains("missing"),
            "error should reference the missing identifier, got: {err}"
        );
    }

    #[test]
    fn evaluates_string_concatenation() {
        let mut runtime = runtime_with("");
        assert_eq!(
            runtime.execute("'hello, ' + 'world'").unwrap(),
            "\"hello, world\""
        );
    }

    #[test]
    fn window_and_self_alias_the_global_object() {
        // Real-world scripts (analytics, polyfills) feature-detect via
        // `window`/`self` and crash with ReferenceError if either is
        // missing. We bind both to the global object so `window === self`,
        // top-level `var foo` shows up as `window.foo`, and `typeof window`
        // reports `"object"` like every browser.
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("typeof window").unwrap(), "\"object\"");
        assert_eq!(runtime.execute("typeof self").unwrap(), "\"object\"");
        assert_eq!(runtime.execute("window === globalThis").unwrap(), "true");
        assert_eq!(runtime.execute("self === globalThis").unwrap(), "true");
        runtime.execute("var page = 7;").unwrap();
        // var bindings at the top level are reflected as own properties of
        // the global object; the alias should make them visible via window.
        assert_eq!(runtime.execute("window.page").unwrap(), "7");
    }

    #[test]
    fn console_object_is_registered_with_log_warn_error() {
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("typeof console").unwrap(), "\"object\"");
        assert_eq!(runtime.execute("typeof console.log").unwrap(), "\"function\"");
        assert_eq!(runtime.execute("typeof console.warn").unwrap(), "\"function\"");
        assert_eq!(runtime.execute("typeof console.error").unwrap(), "\"function\"");
    }

    #[test]
    fn console_log_returns_undefined_and_does_not_throw() {
        let mut runtime = runtime_with("");
        // Multiple args + mixed types — exercises the ToString coercion path
        // and confirms the binding accepts variadic invocation.
        assert_eq!(
            runtime.execute("console.log('hi', 42, true)").unwrap(),
            "undefined"
        );
    }

    #[test]
    fn document_global_exposes_get_element_by_id_and_query_selector() {
        let mut runtime = runtime_with("<p>hi</p>");
        assert_eq!(runtime.execute("typeof document").unwrap(), "\"object\"");
        assert_eq!(
            runtime.execute("typeof document.getElementById").unwrap(),
            "\"function\""
        );
        assert_eq!(
            runtime.execute("typeof document.querySelector").unwrap(),
            "\"function\""
        );
        assert_eq!(
            runtime.execute("typeof document.createElement").unwrap(),
            "\"function\""
        );
    }

    #[test]
    fn get_element_by_id_returns_null_for_missing() {
        let mut runtime = runtime_with(r#"<div id="x"></div>"#);
        assert_eq!(
            runtime.execute("document.getElementById('absent')").unwrap(),
            "null"
        );
    }

    #[test]
    fn get_element_by_id_returns_uppercase_tag_name() {
        // Parser stores `div` lowercase; tagName must surface uppercase to
        // match how every real browser exposes the attribute.
        let mut runtime = runtime_with(r#"<div id="x">hi</div>"#);
        assert_eq!(
            runtime.execute("document.getElementById('x').tagName").unwrap(),
            "\"DIV\""
        );
    }

    #[test]
    fn text_content_concatenates_descendant_text() {
        // Each text node is wrapped in its own element so the inter-element
        // whitespace stripping the parser performs (consumed before each tag)
        // doesn't change what survives — the test stays focused on the JS
        // bridge's tree-walk concatenation behavior.
        let mut runtime = runtime_with(
            r#"<section id="s"><p>hello </p><span>and <b>world</b></span></section>"#,
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('s').textContent")
                .unwrap(),
            "\"hello and world\""
        );
    }

    #[test]
    fn get_attribute_reads_raw_value_or_returns_null() {
        let mut runtime = runtime_with(r#"<a id="link" href="/about" data-x="42">about</a>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('href')")
                .unwrap(),
            "\"/about\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('data-x')")
                .unwrap(),
            "\"42\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('missing')")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn children_lists_only_element_kids_in_document_order() {
        let mut runtime = runtime_with(
            r#"<ul id="list">leading <li>a</li>between<li>b</li> trailing<li>c</li></ul>"#,
        );
        // Text siblings filtered out so .children mirrors HTMLCollection
        // semantics rather than .childNodes.
        assert_eq!(
            runtime
                .execute("document.getElementById('list').children.length")
                .unwrap(),
            "3"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('list').children[0].tagName")
                .unwrap(),
            "\"LI\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('list').children[2].textContent")
                .unwrap(),
            "\"c\""
        );
    }

    #[test]
    fn query_selector_matches_tag_class_and_id() {
        let mut runtime = runtime_with(
            r#"<div><p class="hit" id="target">hi</p><p class="miss">x</p></div>"#,
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('p').textContent")
                .unwrap(),
            "\"hi\""
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('.hit').getAttribute('id')")
                .unwrap(),
            "\"target\""
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('#target').tagName")
                .unwrap(),
            "\"P\""
        );
    }

    #[test]
    fn query_selector_supports_descendant_and_child_combinators() {
        let mut runtime = runtime_with(
            r#"<section><div><span class="t">deep</span></div><span class="t">shallow</span></section>"#,
        );
        // Descendant must reach through `<div>`; child combinator must skip it.
        assert_eq!(
            runtime
                .execute("document.querySelector('section .t').textContent")
                .unwrap(),
            "\"deep\""
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('section > .t').textContent")
                .unwrap(),
            "\"shallow\""
        );
    }

    #[test]
    fn query_selector_returns_null_for_no_match() {
        let mut runtime = runtime_with("<p>hi</p>");
        assert_eq!(
            runtime.execute("document.querySelector('.absent')").unwrap(),
            "null"
        );
    }

    #[test]
    fn query_selector_throws_on_invalid_selector() {
        let mut runtime = runtime_with("<p>hi</p>");
        let err = runtime.execute("document.querySelector('!!')").unwrap_err();
        assert!(
            err.to_lowercase().contains("selector"),
            "error should mention the bad selector, got: {err}"
        );
    }

    #[test]
    fn swapping_dom_under_runtime_redirects_subsequent_lookups() {
        // The closures capture an Rc<RefCell<…>>, not a Document snapshot —
        // so replacing the inner Document under the runtime must redirect
        // the next getElementById call. This is the contract BrowserState
        // relies on for navigation: a fresh JsRuntime is built per page,
        // but the test exercises the in-place swap path that the production
        // arena now also uses for read-only DOM updates.
        let mut runtime = runtime_with(r#"<p id="a">first</p>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('a').textContent")
                .unwrap(),
            "\"first\""
        );
        let next = html::parse(r#"<p id="b">second</p>"#).unwrap();
        *runtime.dom_handle().borrow_mut() = next;
        assert_eq!(
            runtime.execute("document.getElementById('a')").unwrap(),
            "null"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('b').textContent")
                .unwrap(),
            "\"second\""
        );
    }

    // ---- Step 5.1 mutation API ----

    #[test]
    fn text_content_setter_replaces_descendants_with_text() {
        let mut runtime = runtime_with(r#"<div id="host"><span>old</span><b>more</b></div>"#);
        runtime
            .execute("document.getElementById('host').textContent = 'fresh';")
            .unwrap();
        // Subsequent reads observe the new text; the old element children
        // are gone (children list now empty since the only child is text).
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"fresh\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('host').children.length")
                .unwrap(),
            "0"
        );
    }

    #[test]
    fn set_attribute_round_trips_through_get_attribute() {
        let mut runtime = runtime_with(r#"<a id="link">x</a>"#);
        runtime
            .execute("document.getElementById('link').setAttribute('href', '/about');")
            .unwrap();
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('href')")
                .unwrap(),
            "\"/about\""
        );
    }

    #[test]
    fn append_child_attaches_freshly_created_element_into_parent() {
        let mut runtime = runtime_with(r#"<div id="host"></div>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var p = document.createElement('p');\
                 p.textContent = 'inserted';\
                 host.appendChild(p);",
            )
            .unwrap();
        assert_eq!(runtime.execute("host.children.length").unwrap(), "1");
        assert_eq!(
            runtime.execute("host.children[0].tagName").unwrap(),
            "\"P\""
        );
        assert_eq!(
            runtime.execute("host.children[0].textContent").unwrap(),
            "\"inserted\""
        );
    }

    #[test]
    fn append_child_reparents_existing_node_rather_than_duplicating() {
        let mut runtime = runtime_with(
            r#"<div id="src"><span id="movable">m</span></div><div id="dst"></div>"#,
        );
        runtime
            .execute(
                "var src = document.getElementById('src');\
                 var dst = document.getElementById('dst');\
                 var m = document.getElementById('movable');\
                 dst.appendChild(m);",
            )
            .unwrap();
        // The node moved — src is empty and dst owns it now.
        assert_eq!(runtime.execute("src.children.length").unwrap(), "0");
        assert_eq!(runtime.execute("dst.children.length").unwrap(), "1");
        assert_eq!(
            runtime
                .execute("dst.children[0].getAttribute('id')")
                .unwrap(),
            "\"movable\""
        );
    }

    #[test]
    fn element_and_text_wrappers_expose_node_type() {
        // 1 = ELEMENT_NODE, 3 = TEXT_NODE per the standard. The toy bridge
        // exposes just these two — the rest (comment, document, etc.) aren't
        // produced by the parser or by any createX() factory.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        assert_eq!(
            runtime.execute("document.getElementById('x').nodeType").unwrap(),
            "1"
        );
        assert_eq!(
            runtime.execute("document.createTextNode('hi').nodeType").unwrap(),
            "3"
        );
    }

    #[test]
    fn create_text_node_returns_text_wrapper_appendable_into_a_parent() {
        let mut runtime = runtime_with(r#"<p id="host"></p>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var t = document.createTextNode('first ');\
                 host.appendChild(t);\
                 host.appendChild(document.createTextNode('second'));",
            )
            .unwrap();
        // textContent walks all descendants so two adjacent text nodes
        // surface as the concatenated string.
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"first second\""
        );
    }

    #[test]
    fn text_node_text_content_is_writable() {
        // The Text wrapper's textContent doubles as `data`/`nodeValue` —
        // setting it edits the text in place rather than replacing the node.
        let mut runtime = runtime_with(r#"<p id="host"></p>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var t = document.createTextNode('initial');\
                 host.appendChild(t);\
                 t.textContent = 'updated';",
            )
            .unwrap();
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"updated\""
        );
    }

    #[test]
    fn insert_before_places_node_at_ref_child_position() {
        let mut runtime = runtime_with(
            r#"<ul id="list"><li id="a">a</li><li id="c">c</li></ul>"#,
        );
        runtime
            .execute(
                "var list = document.getElementById('list');\
                 var c = document.getElementById('c');\
                 var b = document.createElement('li');\
                 b.textContent = 'b';\
                 list.insertBefore(b, c);",
            )
            .unwrap();
        // Final order is a, b, c.
        assert_eq!(runtime.execute("list.children.length").unwrap(), "3");
        assert_eq!(
            runtime.execute("list.children[0].textContent").unwrap(),
            "\"a\""
        );
        assert_eq!(
            runtime.execute("list.children[1].textContent").unwrap(),
            "\"b\""
        );
        assert_eq!(
            runtime.execute("list.children[2].textContent").unwrap(),
            "\"c\""
        );
    }

    #[test]
    fn insert_before_with_null_ref_appends_to_end() {
        // Spec: insertBefore(node, null) === appendChild(node). Useful for
        // generic insertion code that doesn't special-case the empty list.
        let mut runtime = runtime_with(r#"<ul id="list"><li>first</li></ul>"#);
        runtime
            .execute(
                "var list = document.getElementById('list');\
                 var x = document.createElement('li');\
                 x.textContent = 'tail';\
                 list.insertBefore(x, null);",
            )
            .unwrap();
        assert_eq!(runtime.execute("list.children.length").unwrap(), "2");
        assert_eq!(
            runtime.execute("list.children[1].textContent").unwrap(),
            "\"tail\""
        );
    }

    #[test]
    fn insert_before_throws_when_ref_is_not_a_child() {
        let mut runtime = runtime_with(
            r#"<div id="a"><p id="kid">k</p></div><div id="b"></div>"#,
        );
        let err = runtime
            .execute(
                "var a = document.getElementById('a');\
                 var b = document.getElementById('b');\
                 var kid = document.getElementById('kid');\
                 var x = document.createElement('span');\
                 b.insertBefore(x, kid);",
            )
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("insertbefore"),
            "expected insertBefore TypeError, got: {err}"
        );
    }

    #[test]
    fn replace_child_swaps_node_and_tombstones_the_old_subtree() {
        let mut runtime = runtime_with(
            r#"<section id="host"><p id="old">old</p></section>"#,
        );
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var oldNode = document.getElementById('old');\
                 var fresh = document.createElement('p');\
                 fresh.textContent = 'fresh';\
                 host.replaceChild(fresh, oldNode);",
            )
            .unwrap();
        // The replacement is in place …
        assert_eq!(runtime.execute("host.children.length").unwrap(), "1");
        assert_eq!(
            runtime.execute("host.children[0].textContent").unwrap(),
            "\"fresh\""
        );
        // … and the old node is gone from the document entirely.
        assert_eq!(
            runtime.execute("document.getElementById('old')").unwrap(),
            "null"
        );
    }

    #[test]
    fn clone_node_shallow_drops_descendants_and_does_not_attach() {
        let mut runtime = runtime_with(
            r#"<div id="src"><span>kid</span></div>"#,
        );
        runtime
            .execute(
                "var src = document.getElementById('src');\
                 var dup = src.cloneNode(false);",
            )
            .unwrap();
        // Same tag, no children, not yet in the document.
        assert_eq!(runtime.execute("dup.tagName").unwrap(), "\"DIV\"");
        assert_eq!(runtime.execute("dup.children.length").unwrap(), "0");
        // Original is untouched.
        assert_eq!(runtime.execute("src.children.length").unwrap(), "1");
    }

    #[test]
    fn clone_node_deep_duplicates_subtree_into_fresh_handles() {
        let mut runtime = runtime_with(
            r#"<ul id="src"><li>one</li><li>two</li></ul>"#,
        );
        // Mutating the original after the clone confirms independence —
        // a shared subtree would let the textContent= rewrite collapse the
        // clone too, surfacing as a length=0 in the next assertion.
        runtime
            .execute(
                "var src = document.getElementById('src');\
                 var dup = src.cloneNode(true);\
                 src.textContent = 'wiped';",
            )
            .unwrap();
        // The clone keeps both <li> children with their text.
        assert_eq!(runtime.execute("dup.children.length").unwrap(), "2");
        assert_eq!(
            runtime.execute("dup.children[0].textContent").unwrap(),
            "\"one\""
        );
        assert_eq!(
            runtime.execute("dup.children[1].textContent").unwrap(),
            "\"two\""
        );
    }

    #[test]
    fn mutation_setter_throws_on_a_stale_handle() {
        // Step 5.1.5: writing through a removed wrapper raises rather than
        // silently dropping the write. Reading (textContent get on a stale
        // handle) keeps the previous null-degrade behaviour — that path is
        // exercised by `remove_child_unhooks_node_and_invalidates_its_handle`.
        let mut runtime = runtime_with(r#"<ul id="list"><li id="kid">a</li></ul>"#);
        runtime
            .execute("var kid = document.getElementById('kid'); document.getElementById('list').removeChild(kid);")
            .unwrap();
        let err = runtime.execute("kid.textContent = 'x';").unwrap_err();
        assert!(
            err.to_lowercase().contains("detached") || err.to_lowercase().contains("removed"),
            "expected stale-handle TypeError, got: {err}"
        );
    }

    #[test]
    fn append_child_throws_when_receiver_is_stale() {
        let mut runtime = runtime_with(
            r#"<div id="parent"><div id="host"><p>old</p></div></div>"#,
        );
        runtime
            .execute(
                "var parent = document.getElementById('parent');\
                 var host = document.getElementById('host');\
                 parent.removeChild(host);",
            )
            .unwrap();
        let err = runtime
            .execute("host.appendChild(document.createElement('span'));")
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("detached") || err.to_lowercase().contains("removed"),
            "expected stale-handle TypeError, got: {err}"
        );
    }

    #[test]
    fn remove_child_unhooks_node_and_invalidates_its_handle() {
        let mut runtime = runtime_with(r#"<ul id="list"><li id="kid">a</li></ul>"#);
        runtime
            .execute(
                "var list = document.getElementById('list');\
                 var kid = document.getElementById('kid');\
                 list.removeChild(kid);",
            )
            .unwrap();
        // Parent observes the removal.
        assert_eq!(runtime.execute("list.children.length").unwrap(), "0");
        // Stale handle: textContent on the removed wrapper degrades to null
        // per the Step 5.1.4 silent-degrade policy.
        assert_eq!(runtime.execute("kid.textContent").unwrap(), "null");
        // Re-querying the document confirms the node is gone, not just
        // unhooked from `list`.
        assert_eq!(
            runtime.execute("document.getElementById('kid')").unwrap(),
            "null"
        );
    }

    // ---- Step 6 events ----

    #[test]
    fn dispatch_event_invokes_registered_listener() {
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var hits = 0;\
                 document.getElementById('x').addEventListener('click', function() { hits = hits + 1; });",
            )
            .unwrap();
        let target = runtime.dom_handle().borrow().roots()[0];
        runtime.dispatch_event(target, "click");
        assert_eq!(runtime.execute("hits").unwrap(), "1");
    }

    #[test]
    fn dispatch_event_bubbles_through_ancestor_listeners_in_target_first_order() {
        let mut runtime =
            runtime_with(r#"<div id="outer"><div id="inner">x</div></div>"#);
        runtime
            .execute(
                "var trace = '';\
                 document.getElementById('outer').addEventListener('click', function() { trace += 'outer:'; });\
                 document.getElementById('inner').addEventListener('click', function() { trace += 'inner:'; });",
            )
            .unwrap();
        let inner_id = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let outer = dom.roots()[0];
            dom.get(outer).unwrap().children[0]
        };
        runtime.dispatch_event(inner_id, "click");
        // Standard bubble order: target first, then each ancestor.
        assert_eq!(runtime.execute("trace").unwrap(), "\"inner:outer:\"");
    }

    #[test]
    fn dispatch_event_retargets_text_node_clicks_to_parent_element() {
        // The hit-tester returns the deepest layout box, which for inline
        // text is the text node itself. Text wrappers don't expose
        // addEventListener, so dispatch promotes the target to the nearest
        // Element ancestor before walking the chain.
        let mut runtime = runtime_with(r#"<p id="host">hello</p>"#);
        runtime
            .execute(
                "var ttype = ''; var ttag = '';\
                 document.getElementById('host').addEventListener('click', function(e) {\
                     ttype = e.type; ttag = e.target.tagName;\
                 });",
            )
            .unwrap();
        let text_id = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let host = dom.roots()[0];
            dom.get(host).unwrap().children[0]
        };
        runtime.dispatch_event(text_id, "click");
        assert_eq!(runtime.execute("ttype").unwrap(), "\"click\"");
        assert_eq!(runtime.execute("ttag").unwrap(), "\"P\"");
    }

    #[test]
    fn remove_event_listener_unsubscribes_the_callback() {
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var hits = 0;\
                 var fn = function() { hits = hits + 1; };\
                 var x = document.getElementById('x');\
                 x.addEventListener('click', fn);\
                 x.removeEventListener('click', fn);",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        runtime.dispatch_event(id, "click");
        assert_eq!(runtime.execute("hits").unwrap(), "0");
    }

    #[test]
    fn add_event_listener_dedupes_identical_callable() {
        // WHATWG: registering the same `(target, type, callback)` tuple
        // twice equals one listener. Two distinct function objects with
        // identical bodies still count as two — the dedup key is identity.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var hits = 0;\
                 var fn = function() { hits = hits + 1; };\
                 var x = document.getElementById('x');\
                 x.addEventListener('click', fn);\
                 x.addEventListener('click', fn);",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        runtime.dispatch_event(id, "click");
        assert_eq!(runtime.execute("hits").unwrap(), "1");
    }

    #[test]
    fn dispatch_event_continues_when_an_earlier_handler_throws() {
        // A buggy first handler shouldn't suppress later ones registered
        // on the same node — toy semantics surface the error to stderr but
        // keep delivering the event.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var second = 0;\
                 var x = document.getElementById('x');\
                 x.addEventListener('click', function() { throw new Error('boom'); });\
                 x.addEventListener('click', function() { second = second + 1; });",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        runtime.dispatch_event(id, "click");
        assert_eq!(runtime.execute("second").unwrap(), "1");
    }

    #[test]
    fn add_event_listener_throws_for_non_callable_handler() {
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        let err = runtime
            .execute("document.getElementById('x').addEventListener('click', 42);")
            .unwrap_err();
        assert!(err.to_lowercase().contains("function"), "got: {err}");
    }

    #[test]
    fn click_handler_can_mutate_the_dom_during_dispatch() {
        let mut runtime = runtime_with(r#"<div id="host"></div>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 host.addEventListener('click', function() {\
                     var p = document.createElement('p');\
                     p.textContent = 'clicked';\
                     host.appendChild(p);\
                 });",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        runtime.dispatch_event(id, "click");
        assert_eq!(runtime.execute("host.children.length").unwrap(), "1");
        assert_eq!(
            runtime.execute("host.children[0].textContent").unwrap(),
            "\"clicked\""
        );
    }

    #[test]
    fn dispatch_event_skips_listeners_on_ancestors_removed_mid_bubble() {
        // A handler can remove its own ancestor from the tree; the bubble
        // loop must not panic on the now-tombstoned NodeId. Listener
        // registration on `outer` here matters: dispatch only attempts to
        // re-wrap `current_target` if there's at least one listener to
        // call, which is exactly the case that exercised the panic before
        // the live-element guard was added.
        let mut runtime = runtime_with(
            r#"<div id="grand"><div id="outer"><div id="inner">x</div></div></div>"#,
        );
        runtime
            .execute(
                "var trace = '';\
                 document.getElementById('inner').addEventListener('click', function() {\
                     trace += 'inner:';\
                     document.getElementById('grand').removeChild(document.getElementById('outer'));\
                 });\
                 document.getElementById('outer').addEventListener('click', function() {\
                     trace += 'outer:';\
                 });\
                 document.getElementById('grand').addEventListener('click', function() {\
                     trace += 'grand:';\
                 });",
            )
            .unwrap();
        let inner_id = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let grand = dom.roots()[0];
            let outer = dom.get(grand).unwrap().children[0];
            dom.get(outer).unwrap().children[0]
        };
        runtime.dispatch_event(inner_id, "click");
        // inner runs first; it removes the outer subtree (tombstoning
        // outer + inner). The bubble walk reaches outer next but skips it
        // (now stale). It still finds grand alive and runs its listener.
        assert_eq!(runtime.execute("trace").unwrap(), "\"inner:grand:\"");
    }

    // ---- Step 7 async (microtasks + setTimeout/setInterval/rAF) ----

    #[test]
    fn execute_drains_microtasks_so_promise_callbacks_observe_synchronously() {
        // After the script body returns, `execute` runs the job queue.
        // A `Promise.resolve().then(...)` chain therefore lands its
        // assignment before the call returns — same observable behaviour
        // as `<script>` in a real browser, where microtasks flush before
        // the next task starts.
        let mut runtime = runtime_with("");
        runtime
            .execute("var x = 0; Promise.resolve(42).then(function (v) { x = v; });")
            .unwrap();
        assert_eq!(runtime.execute("x").unwrap(), "42");
    }

    #[test]
    fn set_timeout_zero_fires_on_drain() {
        // setTimeout(fn, 0) lands in the timer queue with a now-aligned
        // due time, so the very next `drain_pending_jobs` fires it. The
        // microtask drain already happens at the end of `execute`, so we
        // schedule and check in two `execute` calls.
        let mut runtime = runtime_with("");
        runtime
            .execute("var hits = 0; setTimeout(function () { hits = hits + 1; }, 0);")
            .unwrap();
        // The first `execute` already drained jobs after the script ran.
        assert_eq!(runtime.execute("hits").unwrap(), "1");
    }

    #[test]
    fn set_timeout_with_delay_does_not_fire_before_clock_advances() {
        let (mut runtime, clock) = runtime_with_fixed_clock("");
        runtime
            .execute("var hits = 0; setTimeout(function () { hits = hits + 1; }, 50);")
            .unwrap();
        // Without advancing the clock the deadline hasn't arrived; a
        // drain must leave the job in place.
        runtime.drain_pending_jobs();
        assert_eq!(runtime.execute("hits").unwrap(), "0");
        clock.forward(49);
        runtime.drain_pending_jobs();
        assert_eq!(runtime.execute("hits").unwrap(), "0");
        // Stepping past the deadline fires the handler exactly once.
        clock.forward(1);
        runtime.drain_pending_jobs();
        assert_eq!(runtime.execute("hits").unwrap(), "1");
    }

    #[test]
    fn clear_timeout_before_fire_suppresses_handler() {
        let (mut runtime, clock) = runtime_with_fixed_clock("");
        runtime
            .execute(
                "var hits = 0;\
                 var id = setTimeout(function () { hits = hits + 1; }, 100);\
                 clearTimeout(id);",
            )
            .unwrap();
        clock.forward(200);
        runtime.drain_pending_jobs();
        assert_eq!(runtime.execute("hits").unwrap(), "0");
    }

    #[test]
    fn set_interval_re_arms_until_cleared() {
        let (mut runtime, clock) = runtime_with_fixed_clock("");
        runtime
            .execute(
                "var hits = 0;\
                 var id = setInterval(function () { hits = hits + 1; }, 10);\
                 globalThis.intervalId = id;",
            )
            .unwrap();
        // Each tick re-arms the next deadline as `now + delay` (HTML
        // setInterval spec, not absolute cadence) — so with the clock
        // frozen, one drain fires at most one tick. Step the clock per
        // tick to count three fires deterministically.
        for _ in 0..3 {
            clock.forward(10);
            runtime.drain_pending_jobs();
        }
        assert_eq!(runtime.execute("hits").unwrap(), "3");

        runtime.execute("clearInterval(intervalId);").unwrap();
        for _ in 0..3 {
            clock.forward(10);
            runtime.drain_pending_jobs();
        }
        // No further ticks after clearInterval — the re-arm path checks
        // the cancelled set before scheduling the next deadline.
        assert_eq!(runtime.execute("hits").unwrap(), "3");
    }

    #[test]
    fn clear_interval_from_inside_handler_stops_subsequent_ticks() {
        let (mut runtime, clock) = runtime_with_fixed_clock("");
        runtime
            .execute(
                "var hits = 0;\
                 var id = setInterval(function () {\
                     hits = hits + 1;\
                     if (hits === 2) clearInterval(id);\
                 }, 5);",
            )
            .unwrap();
        // Fire enough ticks that the handler's self-clear must take
        // effect — without the in-handler `clearInterval` we'd see
        // four hits across four periods.
        for _ in 0..4 {
            clock.forward(5);
            runtime.drain_pending_jobs();
        }
        assert_eq!(runtime.execute("hits").unwrap(), "2");
    }

    #[test]
    fn timer_handler_error_does_not_block_later_timers() {
        // A buggy first timer mustn't take down later ones — surface the
        // error to stderr (run_jobs catches it) and keep draining.
        let (mut runtime, clock) = runtime_with_fixed_clock("");
        runtime
            .execute(
                "var ok = 0;\
                 setTimeout(function () { throw new Error('boom'); }, 5);\
                 setTimeout(function () { ok = ok + 1; }, 10);",
            )
            .unwrap();
        clock.forward(20);
        runtime.drain_pending_jobs();
        assert_eq!(runtime.execute("ok").unwrap(), "1");
    }

    #[test]
    fn request_animation_frame_runs_callback_with_high_res_timestamp() {
        let (mut runtime, clock) = runtime_with_fixed_clock("");
        // The clock starts at 0 ms; advance it before scheduling so the
        // callback receives a non-trivial timestamp argument and we can
        // assert the exact value.
        clock.forward(1234);
        runtime
            .execute("var stamp = -1; requestAnimationFrame(function (t) { stamp = t; });")
            .unwrap();
        runtime.run_animation_frame_callbacks();
        assert_eq!(runtime.execute("stamp").unwrap(), "1234");
    }

    #[test]
    fn request_animation_frame_re_request_runs_on_next_drain() {
        // A handler that calls `requestAnimationFrame` again queues for
        // the *next* call to `run_animation_frame_callbacks`, not the
        // current one — snapshot-then-fire, mirroring browser behaviour.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var hits = 0;\
                 function tick() { hits = hits + 1; requestAnimationFrame(tick); }\
                 requestAnimationFrame(tick);",
            )
            .unwrap();
        runtime.run_animation_frame_callbacks();
        assert_eq!(runtime.execute("hits").unwrap(), "1");
        runtime.run_animation_frame_callbacks();
        assert_eq!(runtime.execute("hits").unwrap(), "2");
    }

    #[test]
    fn cancel_animation_frame_skips_pending_callback() {
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var hits = 0;\
                 var id = requestAnimationFrame(function () { hits = hits + 1; });\
                 cancelAnimationFrame(id);",
            )
            .unwrap();
        runtime.run_animation_frame_callbacks();
        assert_eq!(runtime.execute("hits").unwrap(), "0");
    }

    #[test]
    fn dispatch_event_drains_microtasks_scheduled_in_handler() {
        // A click handler that schedules `Promise.then` should observe
        // the resolution by the time `dispatch_event` returns; the bubble
        // loop ends with a microtask drain.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var stage = '';\
                 document.getElementById('x').addEventListener('click', function () {\
                     stage = 'sync';\
                     Promise.resolve().then(function () { stage = 'micro'; });\
                 });",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        runtime.dispatch_event(id, "click");
        assert_eq!(runtime.execute("stage").unwrap(), "\"micro\"");
    }

    #[test]
    fn set_interval_re_arms_even_after_handler_throws() {
        // A throwing tick should still trigger the next-period re-arm —
        // otherwise a single transient error would silently kill the
        // interval. Run a handful of ticks; the counter advances at every
        // odd tick and the throws never fire because the test injects
        // them at every other tick via a flag, so the assertion confirms
        // the interval kept ticking past the error.
        let (mut runtime, clock) = runtime_with_fixed_clock("");
        runtime
            .execute(
                "var hits = 0;\
                 setInterval(function () {\
                     hits = hits + 1;\
                     if (hits === 1) throw new Error('first tick boom');\
                 }, 5);",
            )
            .unwrap();
        for _ in 0..3 {
            clock.forward(5);
            runtime.drain_pending_jobs();
        }
        // First tick threw, but ticks 2 and 3 still ran — we see 3.
        assert_eq!(runtime.execute("hits").unwrap(), "3");
    }

    #[test]
    fn set_timeout_with_non_callable_first_arg_returns_pre_cancelled_id() {
        // The toy bridge has no eval-by-string path, so `setTimeout("…")`
        // is a no-op. The id still comes back so callers can store it
        // without TypeError, but the registry pre-marks it cancelled.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var id = setTimeout('not a function', 0);\
                 globalThis.captured = typeof id === 'number';",
            )
            .unwrap();
        assert_eq!(runtime.execute("captured").unwrap(), "true");
    }
}
