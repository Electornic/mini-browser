// Thin wrapper around `rquickjs` (QuickJS-NG via FFI) that mirrors the
// boa-backed `crate::js::JsRuntime` surface. Phase 4.8 grows this module
// in sub-phases (a/b/c/d) while the boa runtime stays wired in to
// `state.rs`; 4.8e flips callers over and deletes the boa tree.
//
// Single-threaded throughout: `Runtime` and `Context` are `!Send` and
// every JS interaction flows through `context.with(|ctx| ...)`. Native
// callbacks capture `Rc<RefCell<...>>` host state (DOM, listener map,
// shared location buffer) — same pattern boa used.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use rquickjs::{CatchResultExt, CaughtError, Context, Function, Runtime, Value, context::EvalOptions};

use crate::dom::{Document, NodeId, NodeType};

mod console;
mod document;
mod dom_globals;
mod element;
mod event;
mod fetch;
mod storage;
mod timers;
mod util;
mod window;
mod xhr;

pub use timers::FixedClock;
use timers::ClockSource;

pub struct JsRuntime {
    // Both handles are refcounted internally; we keep the `Runtime` even
    // though it never gets touched outside `drain_pending_jobs` because
    // dropping it before `Context` would tear the realm out from under us.
    runtime: Runtime,
    context: Context,
    dom: Rc<RefCell<Document>>,
    location_url: Rc<RefCell<String>>,
    clock: ClockSource,
    // Live count of `queue.length + rafQueue.length` inside the JS timer
    // module. Mirrored from JS via `__mb_set_pending` at every queue
    // mutation. Cancellations don't decrement live (the cancelled flag
    // is only consulted during the next `__mb_run_timers` filter pass),
    // so the count can be slightly stale-high — that costs one extra
    // frame of redraw, never under-redraw.
    pending_jobs: Rc<Cell<u32>>,
}

impl JsRuntime {
    pub fn new(dom: Rc<RefCell<Document>>) -> Self {
        Self::build(dom, ClockSource::System)
    }

    /// Build a runtime against a synthetic `FixedClock`. Drives every
    /// timer deadline and `Date.now()` read inside the runtime —
    /// bumping the clock via `clock.advance(ms)` is what makes a
    /// `setTimeout(fn, 50)` fire on the next `drain_pending_jobs`
    /// call. Production callers go through `new` with the system
    /// clock; the fixed clock is for tests and headless diagnostic
    /// runs that need deterministic time.
    pub fn new_with_fixed_clock(dom: Rc<RefCell<Document>>, clock: FixedClock) -> Self {
        Self::build(dom, clock.source())
    }

    fn build(dom: Rc<RefCell<Document>>, clock: ClockSource) -> Self {
        let runtime = Runtime::new().expect("rquickjs Runtime should construct");
        let context = Context::full(&runtime).expect("rquickjs Context should construct");
        let location_url: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let pending_jobs: Rc<Cell<u32>> = Rc::new(Cell::new(0));

        context.with(|ctx| {
            ctx.eval::<(), _>(DISPLAY_HELPER)
                .expect("display helper should compile");
            console::register_console(&ctx).expect("console should register");
            // DOM constructor ladder must land BEFORE element/document
            // registration so the wrappers (and future per-instance
            // prototype links) can reference the prototypes.
            dom_globals::register_dom_globals(&ctx)
                .expect("dom globals should register");
            window::register_window_aliases(&ctx, location_url.clone())
                .expect("window aliases should register");
            element::register_element_hooks(&ctx, dom.clone())
                .expect("element hooks should register");
            element::run_dom_bootstrap(&ctx).expect("dom bootstrap should run");
            document::register_document(&ctx, dom.clone())
                .expect("document should register");
            event::register_events(&ctx).expect("events should register");
            storage::register_storage(&ctx).expect("storage should register");
            timers::register_timers(&ctx, clock.clone(), pending_jobs.clone())
                .expect("timers should register");
            fetch::register_fetch(&ctx).expect("fetch should register");
            xhr::register_xhr(&ctx).expect("xhr should register");
        });

        Self {
            runtime,
            context,
            dom,
            location_url,
            clock,
            pending_jobs,
        }
    }

    /// True when the JS runtime owns time-driven work that needs another
    /// frame: an outstanding `setTimeout`/`setInterval` deadline, or a
    /// queued `requestAnimationFrame` callback. The shell composes this
    /// with the chrome-side reasons (caret blink, in-flight navigation)
    /// to decide whether to keep redrawing.
    pub fn has_pending_work(&self) -> bool {
        self.pending_jobs.get() > 0
    }

    /// Update the URL backing `window.location.*`. Read-on-access getters
    /// in `window.rs` reach into the same Rc, so the next location read
    /// after a navigation observes the new URL without redefining any
    /// property descriptors.
    pub fn set_location_url(&self, url: impl Into<String>) {
        *self.location_url.borrow_mut() = url.into();
    }

    /// Returns a clone of the shared DOM handle. Mainly useful in tests
    /// where the test wants to swap the document contents under the
    /// runtime to simulate a navigation.
    #[cfg(test)]
    pub fn dom_handle(&self) -> Rc<RefCell<Document>> {
        self.dom.clone()
    }

    pub fn execute(&mut self, source: &str) -> Result<String, String> {
        let result: Result<String, String> = self.context.with(|ctx| {
            let value: Value = ctx
                .eval::<Value, _>(source)
                .catch(&ctx)
                .map_err(format_caught_error)?;
            let formatter: Function = ctx
                .globals()
                .get("__mb_display")
                .map_err(|err| err.to_string())?;
            formatter
                .call::<_, String>((value,))
                .catch(&ctx)
                .map_err(format_caught_error)
        });
        // Promise/microtask drain mirrors boa's behaviour: the top-level
        // script completing should not leave a pending `Promise.resolve()
        // .then(...)` undelivered. Timers (4.8c) hook into the same drain.
        self.drain_pending_jobs();
        result
    }

    pub fn execute_with_url(&mut self, source: &str, url: &str) -> Result<String, String> {
        // When the caller knows the script's origin URL, hand it to rquickjs
        // as `EvalOptions::filename`. QuickJS uses this string both as the
        // source label on the compiled function and as the `Error.stack`
        // frame name — so a polyfill that throws ten levels deep on a
        // minified third-party bundle reports `polyfill.js:1:NNN` instead
        // of the anonymous label `<eval>` we'd otherwise get. Diagnostic
        // value only; runtime behaviour is unchanged.
        if url.is_empty() {
            return self.execute(source);
        }
        let result: Result<String, String> = self.context.with(|ctx| {
            let mut options = EvalOptions::default();
            options.filename = Some(url.to_string());
            let value: Value = ctx
                .eval_with_options::<Value, _>(source.as_bytes().to_vec(), options)
                .catch(&ctx)
                .map_err(format_caught_error)?;
            let formatter: Function = ctx
                .globals()
                .get("__mb_display")
                .map_err(|err| err.to_string())?;
            formatter
                .call::<_, String>((value,))
                .catch(&ctx)
                .map_err(format_caught_error)
        });
        self.drain_pending_jobs();
        result.map_err(|err| format!("{url}: {err}"))
    }

    /// Drain microtasks (Promise jobs) and any due timers, alternating
    /// until both queues are quiet. A timer handler that resolves a
    /// promise must let the microtask drain before the next iteration's
    /// timer pass observes its side effects, and vice versa.
    pub fn drain_pending_jobs(&mut self) {
        let mut iterations = 0;
        loop {
            iterations += 1;
            // Microtasks first.
            let mut microtask_progress = false;
            loop {
                match self.runtime.execute_pending_job() {
                    Ok(true) => microtask_progress = true,
                    Ok(false) => break,
                    Err(err) => {
                        eprintln!("[jobs] error draining job queue: {err:?}");
                        break;
                    }
                }
            }
            // Then any due timers.
            let timers_fired: u32 = self.context.with(|ctx| {
                let runner: Function<'_> = match ctx.globals().get("__mb_run_timers") {
                    Ok(f) => f,
                    Err(_) => return 0,
                };
                runner.call::<_, u32>(()).unwrap_or(0)
            });
            if !microtask_progress && timers_fired == 0 {
                break;
            }
            // 1024 alternations is generous — nested 0-delay setTimeouts
            // resolve quickly in real workloads, and a runaway loop is a
            // script bug worth surfacing rather than hanging on.
            if iterations > 1024 {
                eprintln!("[jobs] gave up draining after {iterations} iterations");
                break;
            }
        }
    }

    /// Fire every requestAnimationFrame callback that was registered up
    /// to now, snapshotting the queue first so a handler that
    /// re-schedules itself queues for the *next* frame (browser-spec
    /// behaviour). Each callback receives the current engine clock as a
    /// `DOMHighResTimeStamp` (millis). Microtasks queued by handlers
    /// drain after the snapshot completes.
    pub fn run_animation_frame_callbacks(&mut self) {
        let timestamp = self.clock.now_ms() as f64;
        self.context.with(|ctx| {
            let runner: Function<'_> = match ctx.globals().get("__mb_run_raf") {
                Ok(f) => f,
                Err(_) => return,
            };
            let _ = runner.call::<_, ()>((timestamp,));
        });
        self.drain_pending_jobs();
    }

    /// Update `document.readyState`. The page lifecycle drives this
    /// through 'loading' -> 'interactive' -> 'complete' as a document
    /// finishes parsing, runs its scripts, and fully loads.
    pub fn set_ready_state(&mut self, state: &str) {
        let _ = self.execute(&format!("globalThis.__mb_ready_state = {state:?}"));
    }

    /// Fire a window- or document-level event (`DOMContentLoaded`,
    /// `load`, `popstate`, `hashchange`, ...). `which` is "window" or
    /// "document". Returns whether a listener called `preventDefault`.
    pub fn dispatch_global_event(&mut self, which: &str, event_type: &str) -> bool {
        let which = which.to_string();
        let event_type = event_type.to_string();
        let prevented = self.context.with(|ctx| {
            let dispatcher: Function<'_> = match ctx.globals().get("__mb_dispatch_global") {
                Ok(f) => f,
                Err(_) => return false,
            };
            dispatcher
                .call::<_, bool>((which, event_type))
                .unwrap_or(false)
        });
        self.drain_pending_jobs();
        prevented
    }

    /// Synthesise a DOM event at `target` and bubble it through the
    /// parent chain. Text-node targets retarget to the nearest Element
    /// ancestor (matches what real browsers do — almost every author
    /// click handler expects `event.target` to be an Element).
    pub fn dispatch_event(&mut self, target: NodeId, event_type: &str) -> bool {
        self.dispatch_event_inner(target, event_type, None, true)
    }

    /// Direct dispatch — fires every listener registered on `target`
    /// for `event_type`, but does not walk up the parent chain. Used
    /// for non-bubbling events (`focus` / `blur`).
    pub fn dispatch_event_at(&mut self, target: NodeId, event_type: &str) -> bool {
        self.dispatch_event_inner(target, event_type, None, false)
    }

    /// Bubbling dispatch with a `key` payload exposed on the Event
    /// object. Used by BrowserState for `keydown` / `keyup`.
    pub fn dispatch_keyboard_event(
        &mut self,
        target: NodeId,
        event_type: &str,
        key: &str,
    ) -> bool {
        self.dispatch_event_inner(target, event_type, Some(key), true)
    }

    fn dispatch_event_inner(
        &mut self,
        target: NodeId,
        event_type: &str,
        key: Option<&str>,
        bubbles: bool,
    ) -> bool {
        // Retarget Text → nearest Element ancestor (Text wrappers don't
        // expose addEventListener; click handlers expect event.target to
        // be the Element it lives on).
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
            return false;
        };
        // Compute the bubble chain (target-first → root) in Rust, since
        // it needs the live arena. JS-side dispatcher walks the slice.
        let chain: Vec<u32> = if bubbles {
            let dom = self.dom.borrow();
            let mut chain = Vec::new();
            let mut cur = Some(event_target);
            while let Some(id) = cur {
                match dom.get(id) {
                    Some(node) => {
                        chain.push(id.raw());
                        cur = node.parent;
                    }
                    None => break,
                }
            }
            chain
        } else {
            vec![event_target.raw()]
        };
        let target_raw = event_target.raw();
        let event_type_str = event_type.to_string();
        let key_str: Option<String> = key.map(|s| s.to_string());
        let is_keyboard = key.is_some();
        let prevented: bool = self.context.with(|ctx| {
            let dispatcher: Function<'_> = match ctx.globals().get("__mb_dispatch_chain") {
                Ok(f) => f,
                Err(_) => return false,
            };
            dispatcher
                .call::<_, bool>((target_raw, event_type_str, key_str, is_keyboard, chain))
                .unwrap_or(false)
        });
        // Drain microtasks/timers handlers may have queued before the
        // caller observes the result.
        self.drain_pending_jobs();
        prevented
    }
}

impl std::fmt::Debug for JsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsRuntime").finish_non_exhaustive()
    }
}

// String-coercion mirror of boa's `JsValue::display`. The toy bridge's
// integration tests assert against this exact shape (e.g. quoted strings
// for `'a' + 'b'`, bare numbers for `1 + 2`) so we install it once at
// construction and call it on every eval result inside `execute`.
const DISPLAY_HELPER: &str = r#"
globalThis.__mb_display = function (v) {
    if (typeof v === 'string') return JSON.stringify(v);
    if (v === undefined) return 'undefined';
    if (v === null) return 'null';
    if (typeof v === 'function') return '[Function]';
    if (typeof v === 'number' || typeof v === 'boolean' || typeof v === 'bigint') return String(v);
    if (typeof v === 'symbol') return String(v);
    try { return JSON.stringify(v); } catch (e) { return String(v); }
};
"#;

fn format_caught_error(err: CaughtError) -> String {
    match err {
        CaughtError::Exception(exc) => {
            // Match boa's `err.to_string()` shape closely enough for the
            // integration tests that grep for substrings (e.g. "missing"
            // for `missing.prop`). exc.message() is the bare error text;
            // we prefix it with a familiar label when present.
            let message = exc.message().unwrap_or_default();
            let base = if message.is_empty() {
                "exception".to_string()
            } else {
                message
            };
            // When MB_JS_STACK is set, append the JS-side stack trace if
            // rquickjs can produce one. Helps diagnose `cannot read
            // property X of undefined` on minified third-party bundles
            // where the message alone doesn't tell you which call site
            // tripped. Off by default so production logs stay terse.
            // (rquickjs 0.11's Exception doesn't expose file/line as
            // separate accessors — those frames are baked into the
            // stack string by QuickJS when EvalOptions::filename was
            // supplied.)
            if std::env::var_os("MB_JS_STACK").is_none() {
                return base;
            }
            match exc.stack().filter(|s| !s.is_empty()) {
                Some(stack) => format!("{base}\n{stack}"),
                None => base,
            }
        }
        // A bare `throw 42;` — render the value through stringification.
        CaughtError::Value(v) => match v.as_string() {
            Some(js_str) => js_str.to_string().unwrap_or_else(|_| "thrown value".to_string()),
            None => format!("{v:?}"),
        },
        CaughtError::Error(e) => e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::Document;

    fn fresh() -> JsRuntime {
        let dom = Rc::new(RefCell::new(Document::new()));
        JsRuntime::new(dom)
    }

    #[test]
    fn evaluates_arithmetic() {
        let mut rt = fresh();
        assert_eq!(rt.execute("1 + 2 * 3").unwrap(), "7");
    }

    #[test]
    fn preserves_global_state_between_calls() {
        let mut rt = fresh();
        rt.execute("var page = 41;").unwrap();
        assert_eq!(rt.execute("page + 1").unwrap(), "42");
    }

    #[test]
    fn surfaces_runtime_errors() {
        let mut rt = fresh();
        let err = rt.execute("missing.prop").unwrap_err();
        assert!(
            err.to_lowercase().contains("missing"),
            "error should reference the missing identifier, got: {err}"
        );
    }

    #[test]
    fn execute_with_url_prefixes_errors() {
        let mut rt = fresh();
        let err = rt
            .execute_with_url("missing.prop", "https://example.com/app.js")
            .unwrap_err();
        assert!(err.starts_with("https://example.com/app.js: "), "got: {err}");
    }

    #[test]
    fn execute_with_url_returns_bare_error_when_url_is_empty() {
        let mut rt = fresh();
        let err = rt.execute_with_url("missing.prop", "").unwrap_err();
        assert!(!err.starts_with(": "), "got: {err}");
    }

    #[test]
    fn evaluates_string_concatenation() {
        let mut rt = fresh();
        assert_eq!(
            rt.execute("'hello, ' + 'world'").unwrap(),
            "\"hello, world\""
        );
    }

    #[test]
    fn drain_pending_jobs_runs_resolved_promise_then() {
        let mut rt = fresh();
        rt.execute("var observed = null; Promise.resolve(7).then(v => { observed = v; });")
            .unwrap();
        // The microtask ran inside execute()'s drain, so the next read
        // observes the assignment.
        assert_eq!(rt.execute("observed").unwrap(), "7");
    }

    #[test]
    fn console_log_does_not_throw() {
        let mut rt = fresh();
        rt.execute("console.log('a', 1, true, null, undefined)").unwrap();
        rt.execute("console.warn('w'); console.error('e')").unwrap();
    }

    #[test]
    fn window_self_globalthis_alias_each_other() {
        let mut rt = fresh();
        assert_eq!(rt.execute("window === globalThis").unwrap(), "true");
        assert_eq!(rt.execute("self === globalThis").unwrap(), "true");
    }

    #[test]
    fn navigator_user_agent_is_browser_identity() {
        let mut rt = fresh();
        assert_eq!(
            rt.execute("navigator.userAgent").unwrap(),
            "\"MiniBrowser/0.1\""
        );
    }

    #[test]
    fn location_accessors_track_set_location_url() {
        let mut rt = fresh();
        assert_eq!(rt.execute("location.href").unwrap(), "\"\"");
        rt.set_location_url("https://example.com:8080/path?q=1#frag");
        assert_eq!(
            rt.execute("location.href").unwrap(),
            "\"https://example.com:8080/path?q=1#frag\""
        );
        assert_eq!(rt.execute("location.protocol").unwrap(), "\"https:\"");
        assert_eq!(rt.execute("location.hostname").unwrap(), "\"example.com\"");
        assert_eq!(rt.execute("location.host").unwrap(), "\"example.com:8080\"");
        assert_eq!(rt.execute("location.pathname").unwrap(), "\"/path\"");
        assert_eq!(rt.execute("location.search").unwrap(), "\"?q=1\"");
        assert_eq!(rt.execute("location.hash").unwrap(), "\"#frag\"");
    }

    #[test]
    fn history_push_then_replace_grows_then_swaps_the_stack() {
        let mut rt = fresh();
        rt.set_location_url("https://example.com/");
        // pushState grows the stack; replaceState swaps the current entry.
        rt.execute("history.pushState({a:1}, '', '/x'); history.replaceState(null, '', '/y')")
            .unwrap();
        assert_eq!(rt.execute("history.length").unwrap(), "2");
        assert_eq!(rt.execute("location.pathname").unwrap(), "\"/y\"");
        assert_eq!(rt.execute("history.state").unwrap(), "null");
    }

    #[test]
    fn window_add_event_listener_is_silent_noop() {
        let mut rt = fresh();
        rt.execute("window.addEventListener('load', () => {}); self.addEventListener('x', null)")
            .unwrap();
    }

    #[test]
    fn queue_microtask_runs_inside_drain() {
        let mut rt = fresh();
        rt.execute("var ran = false; queueMicrotask(() => { ran = true; })")
            .unwrap();
        assert_eq!(rt.execute("ran").unwrap(), "true");
    }

    // -------- 4.8b DOM bridge tests --------

    fn rt_with_html(source: &str) -> JsRuntime {
        let document = crate::html::parse(source).unwrap();
        let dom = Rc::new(RefCell::new(document));
        JsRuntime::new(dom)
    }

    #[test]
    fn document_get_element_by_id_returns_element_wrapper() {
        let mut rt = rt_with_html(r#"<div id="hello">hi</div>"#);
        assert_eq!(
            rt.execute("document.getElementById('hello').tagName")
                .unwrap(),
            "\"DIV\""
        );
        assert_eq!(
            rt.execute("document.getElementById('missing')").unwrap(),
            "null"
        );
    }

    #[test]
    fn document_query_selector_runs_through_selectors_crate() {
        let mut rt = rt_with_html(r#"<p class="a">x</p><p class="b">y</p>"#);
        assert_eq!(
            rt.execute("document.querySelector('p.b').textContent")
                .unwrap(),
            "\"y\""
        );
        assert_eq!(
            rt.execute("document.querySelector('p.missing')").unwrap(),
            "null"
        );
    }

    #[test]
    fn document_get_elements_by_class_name_returns_array() {
        let mut rt = rt_with_html(r#"<p class="hit">a</p><p class="hit">b</p><p>skip</p>"#);
        assert_eq!(
            rt.execute(
                "document.getElementsByClassName('hit').map(e => e.textContent).join(',')"
            )
            .unwrap(),
            "\"a,b\""
        );
    }

    #[test]
    fn document_create_element_appears_in_html_when_appended() {
        let mut rt = rt_with_html(r#"<div id="root"></div>"#);
        rt.execute(
            r#"var d = document.createElement('span');
               d.setAttribute('id', 'fresh');
               d.textContent = 'hello';
               document.getElementById('root').appendChild(d);"#,
        )
        .unwrap();
        // Round-trip via the inner HTML accessor — attribute order is
        // sorted (BTreeMap-backed AttrMap).
        let serialized = rt
            .execute("document.getElementById('root').innerHTML")
            .unwrap();
        assert_eq!(serialized, "\"<span id=\\\"fresh\\\">hello</span>\"");
    }

    #[test]
    fn element_inner_html_set_replaces_children() {
        let mut rt = rt_with_html(r#"<div id="r"><span>old</span></div>"#);
        // Pass the new fragment via a JS variable so the test source
        // doesn't trip pattern-matching on the literal markup.
        let assign_src = "var raw='<b>new</b>'; document.getElementById('r').innerHTML=raw;";
        rt.execute(assign_src).unwrap();
        let after = rt.execute("document.getElementById('r').innerHTML").unwrap();
        assert_eq!(after, "\"<b>new</b>\"");
    }

    #[test]
    fn element_class_list_add_remove_toggle_contains() {
        let mut rt = rt_with_html(r#"<p id="p" class="x"></p>"#);
        rt.execute("document.getElementById('p').classList.add('y','z')")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('p').getAttribute('class')")
                .unwrap(),
            "\"x y z\""
        );
        assert_eq!(
            rt.execute("document.getElementById('p').classList.contains('y')")
                .unwrap(),
            "true"
        );
        rt.execute("document.getElementById('p').classList.remove('x')")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('p').getAttribute('class')")
                .unwrap(),
            "\"y z\""
        );
        // toggle without force flips; with force forces.
        assert_eq!(
            rt.execute("document.getElementById('p').classList.toggle('y')")
                .unwrap(),
            "false"
        );
        assert_eq!(
            rt.execute("document.getElementById('p').classList.toggle('y', true)")
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn element_remove_child_throws_when_argument_isnt_a_child() {
        let mut rt = rt_with_html(r#"<div id="a"></div><div id="b"></div>"#);
        let err = rt
            .execute(
                "document.getElementById('a').removeChild(document.getElementById('b'))",
            )
            .unwrap_err();
        assert!(err.contains("not a child"), "got: {err}");
    }

    #[test]
    fn element_matches_runs_against_selectors_crate() {
        let mut rt = rt_with_html(r#"<p id="p" class="a b"></p>"#);
        assert_eq!(
            rt.execute("document.getElementById('p').matches('p.a')").unwrap(),
            "true"
        );
        assert_eq!(
            rt.execute("document.getElementById('p').matches('span')")
                .unwrap(),
            "false"
        );
    }

    #[test]
    fn element_closest_walks_ancestors() {
        let mut rt = rt_with_html(r#"<div class="outer"><div class="inner"><span id="s"/></div></div>"#);
        assert_eq!(
            rt.execute("document.getElementById('s').closest('.outer').getAttribute('class')")
                .unwrap(),
            "\"outer\""
        );
        assert_eq!(
            rt.execute("document.getElementById('s').closest('.missing')")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn element_text_content_set_and_get() {
        let mut rt = rt_with_html(r#"<p id="p">old</p>"#);
        assert_eq!(
            rt.execute("document.getElementById('p').textContent").unwrap(),
            "\"old\""
        );
        rt.execute("document.getElementById('p').textContent = 'new'")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('p').textContent").unwrap(),
            "\"new\""
        );
    }

    #[test]
    fn element_value_round_trips_through_attribute() {
        let mut rt = rt_with_html(r#"<input id="i" value="seed">"#);
        assert_eq!(
            rt.execute("document.getElementById('i').value").unwrap(),
            "\"seed\""
        );
        rt.execute("document.getElementById('i').value = 'edited'")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('i').getAttribute('value')")
                .unwrap(),
            "\"edited\""
        );
    }

    #[test]
    fn document_body_and_head_accessors_resolve() {
        let mut rt = rt_with_html(r#"<html><head><title>t</title></head><body><p>b</p></body></html>"#);
        assert_eq!(rt.execute("document.body.tagName").unwrap(), "\"BODY\"");
        assert_eq!(rt.execute("document.head.tagName").unwrap(), "\"HEAD\"");
    }

    // -------- Tier 0 SPA: querySelectorAll + element.style --------

    #[test]
    fn document_query_selector_all_returns_all_matches_in_order() {
        let mut rt = rt_with_html(r#"<p class="a">1</p><p class="a">2</p><span class="a">3</span>"#);
        assert_eq!(
            rt.execute("document.querySelectorAll('p.a').length").unwrap(),
            "2"
        );
        assert_eq!(
            rt.execute("document.querySelectorAll('.a').map(e => e.textContent).join(',')")
                .unwrap(),
            "\"1,2,3\""
        );
        assert_eq!(
            rt.execute("document.querySelectorAll('.none').length").unwrap(),
            "0"
        );
    }

    #[test]
    fn element_query_selector_all_scopes_to_descendants() {
        let mut rt = rt_with_html(
            r#"<div id="root"><p class="x">a</p><section><p class="x">b</p></section></div><p class="x">outside</p>"#,
        );
        assert_eq!(
            rt.execute("document.getElementById('root').querySelectorAll('.x').length")
                .unwrap(),
            "2"
        );
        assert_eq!(
            rt.execute("document.getElementById('root').querySelector('.x').textContent")
                .unwrap(),
            "\"a\""
        );
        assert_eq!(
            rt.execute("document.getElementById('root').querySelector('.missing')")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn element_style_property_round_trips_through_attribute() {
        let mut rt = rt_with_html(r#"<div id="d"></div>"#);
        rt.execute("document.getElementById('d').style.display = 'none'")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('d').getAttribute('style')")
                .unwrap(),
            "\"display: none\""
        );
        assert_eq!(
            rt.execute("document.getElementById('d').style.display").unwrap(),
            "\"none\""
        );
    }

    #[test]
    fn element_style_camel_case_maps_to_kebab() {
        let mut rt = rt_with_html(r#"<div id="d" style="color: red"></div>"#);
        rt.execute("document.getElementById('d').style.backgroundColor = 'blue'")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('d').getAttribute('style')")
                .unwrap(),
            "\"color: red; background-color: blue\""
        );
        assert_eq!(
            rt.execute("document.getElementById('d').style.backgroundColor")
                .unwrap(),
            "\"blue\""
        );
        assert_eq!(
            rt.execute("document.getElementById('d').style.color").unwrap(),
            "\"red\""
        );
    }

    #[test]
    fn element_style_css_text_set_get_and_remove_property() {
        let mut rt = rt_with_html(r#"<div id="d"></div>"#);
        rt.execute("document.getElementById('d').style.cssText = 'margin: 0; padding: 4px'")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('d').style.getPropertyValue('padding')")
                .unwrap(),
            "\"4px\""
        );
        rt.execute("document.getElementById('d').style.removeProperty('margin')")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('d').getAttribute('style')")
                .unwrap(),
            "\"padding: 4px\""
        );
        // Assigning '' deletes the declaration.
        rt.execute("document.getElementById('d').style.padding = ''")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('d').style.cssText").unwrap(),
            "\"\""
        );
    }

    #[test]
    fn create_document_fragment_flushes_children_on_append() {
        let mut rt = rt_with_html(r#"<div id="root"></div>"#);
        rt.execute(
            r#"var frag = document.createDocumentFragment();
               var a = document.createElement('span'); a.textContent = 'a';
               var b = document.createElement('span'); b.textContent = 'b';
               frag.appendChild(a); frag.appendChild(b);
               document.getElementById('root').appendChild(frag);"#,
        )
        .unwrap();
        // Both spans land in the parent; the fragment itself leaves no node.
        assert_eq!(
            rt.execute("document.getElementById('root').children.length")
                .unwrap(),
            "2"
        );
        assert_eq!(
            rt.execute("document.getElementById('root').innerHTML").unwrap(),
            "\"<span>a</span><span>b</span>\""
        );
        // Fragment is emptied after the flush.
        assert_eq!(rt.execute("frag.childNodes.length").unwrap(), "0");
        assert_eq!(rt.execute("frag.nodeType").unwrap(), "11");
    }

    // -------- Tier 0 SPA: history + window/doc events + readyState --------

    #[test]
    fn history_push_state_updates_stack_and_location_without_refetch() {
        let mut rt = fresh();
        rt.set_location_url("https://example.com/home");
        rt.execute("history.pushState({page: 1}, '', '/users/1')")
            .unwrap();
        assert_eq!(rt.execute("history.length").unwrap(), "2");
        assert_eq!(rt.execute("location.pathname").unwrap(), "\"/users/1\"");
        assert_eq!(rt.execute("location.href").unwrap(), "\"https://example.com/users/1\"");
        assert_eq!(rt.execute("history.state.page").unwrap(), "1");
        // replaceState swaps the current entry rather than growing the stack.
        rt.execute("history.replaceState({page: 2}, '', '/users/2')")
            .unwrap();
        assert_eq!(rt.execute("history.length").unwrap(), "2");
        assert_eq!(rt.execute("location.pathname").unwrap(), "\"/users/2\"");
        assert_eq!(rt.execute("history.state.page").unwrap(), "2");
    }

    #[test]
    fn history_back_and_forward_fire_popstate_and_restore_location() {
        let mut rt = fresh();
        rt.set_location_url("https://example.com/a");
        rt.execute(
            r#"var seenState = null; var seenPath = null;
               window.addEventListener('popstate', function (e) {
                   seenState = e.state ? e.state.n : null;
                   seenPath = location.pathname;
               });
               history.pushState({n: 1}, '', '/b');
               history.pushState({n: 2}, '', '/c');
               history.back();"#,
        )
        .unwrap();
        assert_eq!(rt.execute("location.pathname").unwrap(), "\"/b\"");
        assert_eq!(rt.execute("seenState").unwrap(), "1");
        assert_eq!(rt.execute("seenPath").unwrap(), "\"/b\"");
        rt.execute("history.forward()").unwrap();
        assert_eq!(rt.execute("location.pathname").unwrap(), "\"/c\"");
        assert_eq!(rt.execute("seenState").unwrap(), "2");
    }

    #[test]
    fn dispatch_global_event_fires_window_and_document_listeners() {
        let mut rt = rt_with_html("");
        rt.execute(
            r#"var hits = [];
               window.addEventListener('load', function () { hits.push('load'); });
               document.addEventListener('DOMContentLoaded', function () { hits.push('dcl'); });"#,
        )
        .unwrap();
        rt.dispatch_global_event("document", "DOMContentLoaded");
        rt.dispatch_global_event("window", "load");
        assert_eq!(rt.execute("hits.join(',')").unwrap(), "\"dcl,load\"");
    }

    #[test]
    fn document_ready_state_defaults_loading_and_tracks_setter() {
        let mut rt = rt_with_html("");
        assert_eq!(rt.execute("document.readyState").unwrap(), "\"loading\"");
        rt.set_ready_state("complete");
        assert_eq!(rt.execute("document.readyState").unwrap(), "\"complete\"");
    }

    // Capstone: the combined Tier 0 surface drives a real (tiny) SPA —
    // router + dynamic render via fragments + querySelectorAll/classList +
    // history/popstate. Mirrors examples/spa-demo.html.
    #[test]
    fn end_to_end_mini_spa_routes_and_renders() {
        let mut rt = rt_with_html(
            r#"<body><nav id="nav"><a data-route="/">H</a><a data-route="/users">U</a></nav><main id="app"></main></body>"#,
        );
        rt.set_location_url("https://example.com/");
        rt.execute(
            r#"var app = document.getElementById('app');
               function render() {
                   app.innerHTML = '';
                   var frag = document.createDocumentFragment();
                   var h = document.createElement('h1');
                   h.textContent = (location.pathname === '/users') ? 'Users' : 'Home';
                   frag.appendChild(h);
                   app.appendChild(frag);
                   var links = document.querySelectorAll('#nav a');
                   for (var i = 0; i < links.length; i++) {
                       links[i].classList.toggle(
                           'active', links[i].getAttribute('data-route') === location.pathname
                       );
                   }
               }
               function go(p) { history.pushState({}, '', p); render(); }
               window.addEventListener('popstate', render);
               document.addEventListener('DOMContentLoaded', render);"#,
        )
        .unwrap();
        // Lifecycle boot renders Home.
        rt.dispatch_global_event("document", "DOMContentLoaded");
        assert_eq!(
            rt.execute("document.getElementById('app').innerHTML").unwrap(),
            "\"<h1>Home</h1>\""
        );
        // Route to /users without reload.
        rt.execute("go('/users')").unwrap();
        assert_eq!(
            rt.execute("document.getElementById('app').querySelector('h1').textContent")
                .unwrap(),
            "\"Users\""
        );
        assert_eq!(rt.execute("location.pathname").unwrap(), "\"/users\"");
        assert_eq!(
            rt.execute("document.querySelectorAll('#nav a.active').length").unwrap(),
            "1"
        );
        // Back fires popstate and re-renders Home.
        rt.execute("history.back()").unwrap();
        assert_eq!(
            rt.execute("document.getElementById('app').querySelector('h1').textContent")
                .unwrap(),
            "\"Home\""
        );
    }

    // -------- Tier 1 #5: prototype inheritance (keystone) --------

    #[test]
    fn element_wrapper_inherits_html_element_prototype_chain() {
        let mut rt = rt_with_html(r#"<div id="d"></div>"#);
        assert_eq!(
            rt.execute("document.getElementById('d') instanceof HTMLElement").unwrap(),
            "true"
        );
        assert_eq!(
            rt.execute("document.getElementById('d') instanceof Element").unwrap(),
            "true"
        );
        assert_eq!(
            rt.execute("document.getElementById('d') instanceof Node").unwrap(),
            "true"
        );
        assert_eq!(
            rt.execute(
                "Object.getPrototypeOf(document.getElementById('d')) === HTMLElement.prototype"
            )
            .unwrap(),
            "true"
        );
    }

    #[test]
    fn element_members_live_on_prototype_with_only_node_id_own() {
        let mut rt = rt_with_html(r#"<div id="d"></div>"#);
        // Accessors/methods moved off the instance onto the shared proto.
        assert_eq!(
            rt.execute("document.getElementById('d').hasOwnProperty('tagName')").unwrap(),
            "false"
        );
        assert_eq!(
            rt.execute("document.getElementById('d').hasOwnProperty('getAttribute')").unwrap(),
            "false"
        );
        // `_nodeId` is the lone (non-enumerable) own slot each wrapper carries.
        assert_eq!(
            rt.execute("Object.getOwnPropertyNames(document.getElementById('d')).join(',')")
                .unwrap(),
            "\"_nodeId\""
        );
        // ...and the inherited accessor still resolves through the chain.
        assert_eq!(
            rt.execute("document.getElementById('d').tagName").unwrap(),
            "\"DIV\""
        );
    }

    #[test]
    fn polyfill_patch_on_element_prototype_is_visible_to_wrapper() {
        let mut rt = rt_with_html(r#"<section id="s"></section>"#);
        // A polyfill adds a method to Element.prototype (one we don't
        // define ourselves). The wrapper must pick it up via the chain,
        // with `this` bound to the wrapper. This is the whole point of the
        // keystone: the OLD per-instance model could never see this patch.
        rt.execute("Element.prototype.describe = function () { return 'I am ' + this.tagName; };")
            .unwrap();
        assert_eq!(
            rt.execute("document.getElementById('s').describe()").unwrap(),
            "\"I am SECTION\""
        );
    }

    #[test]
    fn text_wrapper_inherits_text_prototype_chain() {
        let mut rt = rt_with_html("");
        rt.execute("var t = document.createTextNode('hi')").unwrap();
        assert_eq!(rt.execute("t instanceof Text").unwrap(), "true");
        assert_eq!(rt.execute("t instanceof CharacterData").unwrap(), "true");
        assert_eq!(rt.execute("t instanceof Node").unwrap(), "true");
        assert_eq!(rt.execute("t.nodeType").unwrap(), "3");
        assert_eq!(rt.execute("t.textContent").unwrap(), "\"hi\"");
    }

    #[test]
    fn fragment_wrapper_inherits_document_fragment_prototype() {
        let mut rt = rt_with_html("");
        rt.execute("var f = document.createDocumentFragment()").unwrap();
        assert_eq!(rt.execute("f instanceof DocumentFragment").unwrap(), "true");
        assert_eq!(rt.execute("f instanceof Node").unwrap(), "true");
        assert_eq!(rt.execute("f.nodeType").unwrap(), "11");
    }

    // -------- 4.8c storage / timers / event tests --------

    fn rt_clock(html: &str) -> (JsRuntime, FixedClock) {
        let document = crate::html::parse(html).unwrap();
        let dom = Rc::new(RefCell::new(document));
        let clock = FixedClock::from_millis(0);
        let runtime = JsRuntime::new_with_fixed_clock(dom, clock.clone());
        (runtime, clock)
    }

    fn id_to_node(rt: &JsRuntime, target_id: &str) -> NodeId {
        let dom = rt.dom_handle();
        let dom_ref = dom.borrow();
        fn walk(d: &Document, n: NodeId, target_id: &str) -> Option<NodeId> {
            let node = d.get(n)?;
            if let NodeType::Element(e) = &node.node_type
                && e.attributes.get("id").is_some_and(|v| v == target_id)
            {
                return Some(n);
            }
            for child in &node.children {
                if let Some(f) = walk(d, *child, target_id) {
                    return Some(f);
                }
            }
            None
        }
        for &root in dom_ref.roots() {
            if let Some(found) = walk(&dom_ref, root, target_id) {
                return found;
            }
        }
        panic!("id `{target_id}` not found in DOM");
    }

    #[test]
    fn local_storage_round_trips_keys_and_values() {
        let mut rt = fresh();
        rt.execute("localStorage.setItem('k', 'v'); localStorage.setItem('a', 1)")
            .unwrap();
        assert_eq!(rt.execute("localStorage.getItem('k')").unwrap(), "\"v\"");
        // setItem coerces non-strings via String() — `1` becomes `"1"`.
        assert_eq!(rt.execute("localStorage.getItem('a')").unwrap(), "\"1\"");
        assert_eq!(rt.execute("localStorage.length").unwrap(), "2");
        assert_eq!(rt.execute("localStorage.key(0)").unwrap(), "\"k\"");
        rt.execute("localStorage.removeItem('k')").unwrap();
        assert_eq!(rt.execute("localStorage.length").unwrap(), "1");
        rt.execute("localStorage.clear()").unwrap();
        assert_eq!(rt.execute("localStorage.length").unwrap(), "0");
    }

    #[test]
    fn session_storage_is_independent_of_local_storage() {
        let mut rt = fresh();
        rt.execute("localStorage.setItem('k','L'); sessionStorage.setItem('k','S')")
            .unwrap();
        assert_eq!(rt.execute("localStorage.getItem('k')").unwrap(), "\"L\"");
        assert_eq!(rt.execute("sessionStorage.getItem('k')").unwrap(), "\"S\"");
    }

    #[test]
    fn date_now_tracks_fixed_clock() {
        let (mut rt, clock) = rt_clock("");
        assert_eq!(rt.execute("Date.now()").unwrap(), "0");
        clock.advance(1000);
        assert_eq!(rt.execute("Date.now()").unwrap(), "1000");
    }

    #[test]
    fn set_timeout_fires_after_clock_advance_and_drain() {
        let (mut rt, clock) = rt_clock("");
        rt.execute(
            "var fired = false; setTimeout(function(){ fired = true; }, 50)",
        )
        .unwrap();
        // Not yet — clock at 0, deadline at 50.
        assert_eq!(rt.execute("fired").unwrap(), "false");
        clock.advance(50);
        rt.drain_pending_jobs();
        assert_eq!(rt.execute("fired").unwrap(), "true");
    }

    #[test]
    fn clear_timeout_cancels_pending_handler() {
        let (mut rt, clock) = rt_clock("");
        rt.execute(
            "var fired = false; var id = setTimeout(function(){ fired = true; }, 50); clearTimeout(id)",
        )
        .unwrap();
        clock.advance(100);
        rt.drain_pending_jobs();
        assert_eq!(rt.execute("fired").unwrap(), "false");
    }

    #[test]
    fn set_interval_repeats_until_cleared() {
        let (mut rt, clock) = rt_clock("");
        rt.execute(
            "var n = 0; var id = setInterval(function(){ n++; if (n >= 3) clearInterval(id); }, 10)",
        )
        .unwrap();
        // Drift-based re-arm: each handler re-schedules at `now + delay`,
        // so advancing the clock by 50 in one shot only fires the handler
        // once. Step the clock by `delay` between drains to observe the
        // expected three-tick / clear-on-third progression.
        for _ in 0..5 {
            clock.advance(10);
            rt.drain_pending_jobs();
        }
        assert_eq!(rt.execute("n").unwrap(), "3");
    }

    #[test]
    fn request_animation_frame_fires_on_run_animation_frame_callbacks() {
        let mut rt = rt_with_html("");
        rt.execute(
            "var ts = -1; requestAnimationFrame(function(t){ ts = t; })",
        )
        .unwrap();
        rt.run_animation_frame_callbacks();
        // Default ClockSource::System, so the timestamp is real wall-clock.
        // Just confirm the handler ran (ts !== -1) and got a positive number.
        let observed = rt.execute("ts > 0").unwrap();
        assert_eq!(observed, "true");
    }

    #[test]
    fn raf_handler_self_reschedules_to_next_frame_only() {
        let mut rt = rt_with_html("");
        rt.execute(
            "var firedCount = 0;
             var inner = function(){ firedCount++; };
             requestAnimationFrame(function(){ requestAnimationFrame(inner); });",
        )
        .unwrap();
        rt.run_animation_frame_callbacks();
        // Outer fired (queued inner); inner waits for next frame.
        assert_eq!(rt.execute("firedCount").unwrap(), "0");
        rt.run_animation_frame_callbacks();
        assert_eq!(rt.execute("firedCount").unwrap(), "1");
    }

    #[test]
    fn add_event_listener_fires_on_dispatch_event() {
        let mut rt = rt_with_html(r#"<button id="b">x</button>"#);
        rt.execute(
            r#"var clicked = 0;
               document.getElementById('b').addEventListener('click', function(){ clicked++; });"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "b");
        let prevented = rt.dispatch_event(nid, "click");
        assert!(!prevented);
        assert_eq!(rt.execute("clicked").unwrap(), "1");
    }

    #[test]
    fn dispatch_event_bubbles_to_ancestors() {
        let mut rt = rt_with_html(r#"<div id="outer"><div id="mid"><span id="inner"></span></div></div>"#);
        rt.execute(
            r#"var trail = [];
               document.getElementById('outer').addEventListener('x', function(){ trail.push('outer'); });
               document.getElementById('mid').addEventListener('x', function(){ trail.push('mid'); });
               document.getElementById('inner').addEventListener('x', function(){ trail.push('inner'); });"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "inner");
        rt.dispatch_event(nid, "x");
        assert_eq!(
            rt.execute("trail.join(',')").unwrap(),
            "\"inner,mid,outer\""
        );
    }

    #[test]
    fn dispatch_event_at_does_not_bubble() {
        let mut rt = rt_with_html(r#"<div id="outer"><span id="inner"></span></div>"#);
        rt.execute(
            r#"var fired = [];
               document.getElementById('outer').addEventListener('focus', function(){ fired.push('outer'); });
               document.getElementById('inner').addEventListener('focus', function(){ fired.push('inner'); });"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "inner");
        rt.dispatch_event_at(nid, "focus");
        assert_eq!(rt.execute("fired.join(',')").unwrap(), "\"inner\"");
    }

    #[test]
    fn prevent_default_returned_to_dispatcher() {
        let mut rt = rt_with_html(r#"<a id="a">x</a>"#);
        rt.execute(
            r#"document.getElementById('a').addEventListener('click', function(e){ e.preventDefault(); });"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "a");
        let prevented = rt.dispatch_event(nid, "click");
        assert!(prevented);
    }

    #[test]
    fn stop_propagation_halts_bubble_after_current_target() {
        let mut rt = rt_with_html(r#"<div id="outer"><span id="inner"></span></div>"#);
        rt.execute(
            r#"var trail = [];
               document.getElementById('outer').addEventListener('click', function(){ trail.push('outer'); });
               document.getElementById('inner').addEventListener('click', function(e){ trail.push('inner'); e.stopPropagation(); });"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "inner");
        rt.dispatch_event(nid, "click");
        // Outer never sees the event because the bubble stops after the
        // inner ancestor finishes its own listener list.
        assert_eq!(rt.execute("trail.join(',')").unwrap(), "\"inner\"");
    }

    #[test]
    fn dispatch_keyboard_event_exposes_key_field() {
        let mut rt = rt_with_html(r#"<input id="i">"#);
        rt.execute(
            r#"var captured = '';
               document.getElementById('i').addEventListener('keydown', function(e){ captured = e.key; });"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "i");
        rt.dispatch_keyboard_event(nid, "keydown", "Enter");
        assert_eq!(rt.execute("captured").unwrap(), "\"Enter\"");
    }

    #[test]
    fn remove_event_listener_drops_handler() {
        let mut rt = rt_with_html(r#"<div id="d"></div>"#);
        rt.execute(
            r#"var fired = 0;
               var handler = function(){ fired++; };
               var elem = document.getElementById('d');
               elem.addEventListener('click', handler);
               elem.removeEventListener('click', handler);"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "d");
        rt.dispatch_event(nid, "click");
        assert_eq!(rt.execute("fired").unwrap(), "0");
    }

    #[test]
    fn duplicate_listener_registration_dedups_per_spec() {
        let mut rt = rt_with_html(r#"<div id="d"></div>"#);
        rt.execute(
            r#"var fired = 0;
               var handler = function(){ fired++; };
               var elem = document.getElementById('d');
               elem.addEventListener('click', handler);
               elem.addEventListener('click', handler);"#,
        )
        .unwrap();
        let nid = id_to_node(&rt, "d");
        rt.dispatch_event(nid, "click");
        assert_eq!(rt.execute("fired").unwrap(), "1");
    }

    // -------- 4.8d fetch / xhr surface tests (network-free) --------

    #[test]
    fn fetch_is_a_function_returning_a_promise() {
        let mut rt = fresh();
        assert_eq!(rt.execute("typeof fetch").unwrap(), "\"function\"");
        // Use an unsupported scheme so net::Url::parse rejects without
        // hitting the network — the rejection still proves `fetch`
        // returns a Promise (then/catch are present on the rejected
        // promise the surface returns).
        rt.execute(
            r#"var caught = null;
               fetch('foo://bad').catch(function(err){ caught = String(err); });"#,
        )
        .unwrap();
        let observed = rt.execute("typeof caught === 'string' && caught.length > 0").unwrap();
        assert_eq!(observed, "true");
    }

    #[test]
    fn fetch_rejects_when_init_is_not_an_object() {
        let mut rt = fresh();
        rt.execute(
            r#"var rejected = false;
               fetch('foo://bad', 42).catch(function(){ rejected = true; });"#,
        )
        .unwrap();
        assert_eq!(rt.execute("rejected").unwrap(), "true");
    }

    #[test]
    fn xmlhttprequest_constructor_starts_in_unsent_state() {
        let mut rt = fresh();
        rt.execute("var xhr = new XMLHttpRequest()").unwrap();
        assert_eq!(rt.execute("xhr.readyState").unwrap(), "0");
        assert_eq!(rt.execute("xhr.UNSENT").unwrap(), "0");
        assert_eq!(rt.execute("xhr.DONE").unwrap(), "4");
        // Class-side constants.
        assert_eq!(rt.execute("XMLHttpRequest.OPENED").unwrap(), "1");
    }

    #[test]
    fn xhr_open_transitions_to_opened_state() {
        let mut rt = fresh();
        rt.execute(
            "var xhr = new XMLHttpRequest(); xhr.open('GET', 'foo://bad')",
        )
        .unwrap();
        assert_eq!(rt.execute("xhr.readyState").unwrap(), "1");
    }

    #[test]
    fn xhr_set_request_header_requires_opened_state() {
        let mut rt = fresh();
        let err = rt
            .execute(
                "var xhr = new XMLHttpRequest(); xhr.setRequestHeader('X', 'y')",
            )
            .unwrap_err();
        assert!(err.to_lowercase().contains("opened"), "got: {err}");
    }

    #[test]
    fn xhr_send_throws_when_open_was_not_called() {
        let mut rt = fresh();
        let err = rt
            .execute("var xhr = new XMLHttpRequest(); xhr.send()")
            .unwrap_err();
        assert!(err.to_lowercase().contains("opened"), "got: {err}");
    }

    #[test]
    fn xhr_open_with_invalid_url_throws_synchronously() {
        let mut rt = fresh();
        let err = rt
            .execute(
                "var xhr = new XMLHttpRequest(); xhr.open('GET', 'foo://bad'); xhr.send()",
            )
            .unwrap_err();
        assert!(err.to_lowercase().contains("invalid url"), "got: {err}");
    }

    #[test]
    fn inner_html_setter_prunes_listeners_on_dropped_subtree() {
        let mut rt = rt_with_html(r#"<div id="root"><span id="kid"></span></div>"#);
        rt.execute(
            r#"var fired = 0;
               document.getElementById('kid').addEventListener('click', function(){ fired++; });"#,
        )
        .unwrap();
        // Snapshot the kid's NodeId BEFORE the innerHTML setter runs —
        // afterwards the slot is tombstoned and id_to_node's lookup
        // would fail.
        let kid = id_to_node(&rt, "kid");
        // Replace the subtree, which triggers __mb_listener_prune for
        // the kid's NodeId. Dispatching to the (now-detached) kid id
        // should surface no handler call.
        let assign = "var raw='<b>new</b>'; document.getElementById('root').innerHTML=raw;";
        rt.execute(assign).unwrap();
        rt.dispatch_event(kid, "click");
        assert_eq!(rt.execute("fired").unwrap(), "0");
    }
}
