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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use boa_engine::{
    Context, JsObject, JsValue, Source,
    context::{
        ContextBuilder,
        time::{Clock, StdClock},
    },
};

#[cfg(test)]
use boa_engine::context::time::FixedClock;

use crate::dom::{Document, NodeId, NodeType};

mod console;
mod document;
mod element;
mod event;
mod fetch;
mod storage;
mod timers;
mod util;
mod window;
mod xhr;

// Hidden property name used to round-trip a NodeId through any Element
// JsObject — methods like `appendChild(other)` read `other._nodeId` to
// recover the receiver's NodeId from any wrapper without a parallel handle
// table. JS code shouldn't poke at this; the dynamic mutation methods all
// re-validate the recovered id against the live arena before acting on it.
pub(crate) const NODE_ID_PROP: &str = "_nodeId";

// Per-node event listener registry. Keyed by `(NodeId, event_type_name)`,
// each entry holds the callable JS objects passed to `addEventListener` in
// insertion order. Listeners live on `JsRuntime` rather than on individual
// Element wrappers because multiple wrappers may exist for the same NodeId
// (children getter, repeated `getElementById`, …) and they all need to
// observe the same listener set. We store the original `JsObject` (not a
// converted `JsFunction`) so identity comparisons via `JsObject::equals`
// line up with the wrappers JS code passes back to `removeEventListener`.
pub(crate) type ListenerMap = HashMap<(NodeId, String), Vec<JsObject>>;

// Live registry of requestAnimationFrame callbacks awaiting the next frame.
// Vec rather than HashMap because the toy bridge fires them in registration
// order; cancellation is handled out-of-band via `cancelled_timers`.
pub(crate) type RafQueue = Vec<(u32, JsObject)>;

pub struct JsRuntime {
    context: Context,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    raf_callbacks: Rc<RefCell<RafQueue>>,
    cancelled_timers: Rc<RefCell<HashSet<u32>>>,
    // Backing buffer for `window.location.*`. The accessors registered
    // on the `location` global capture a clone of this Rc and re-parse
    // the buffer on every read, so `set_location_url` is enough to
    // change what JS observes after a navigation — no property-redefine
    // dance required.
    location_url: Rc<RefCell<String>>,
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
        let executor = Rc::new(timers::FrameJobExecutor::new());
        let mut context = ContextBuilder::default()
            .clock(clock)
            .job_executor(executor)
            .build()
            .expect("Boa context should build with default settings");
        let listeners: Rc<RefCell<ListenerMap>> = Rc::new(RefCell::new(HashMap::new()));
        let raf_callbacks: Rc<RefCell<RafQueue>> = Rc::new(RefCell::new(Vec::new()));
        let cancelled_timers: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));
        let next_timer_id: Rc<Cell<u32>> = Rc::new(Cell::new(0));
        let location_url: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        console::register_console(&mut context);
        window::register_window_aliases(&mut context, location_url.clone());
        document::register_document(&mut context, dom.clone(), listeners.clone());
        timers::register_timers(
            &mut context,
            cancelled_timers.clone(),
            next_timer_id.clone(),
            raf_callbacks.clone(),
        );
        fetch::register_fetch(&mut context);
        storage::register_storage(&mut context);
        xhr::register_xmlhttprequest(&mut context);
        Self {
            context,
            dom,
            listeners,
            raf_callbacks,
            cancelled_timers,
            location_url,
        }
    }

    /// Update the URL backing `window.location.*`. Production callers go
    /// through this once at runtime construction (after the page loader
    /// resolves the address) and again on every navigation, so the
    /// accessors registered against the `location` global observe the
    /// new URL on the next read. An empty string represents "no URL
    /// bound yet" — every accessor collapses to the empty string until
    /// a real value lands here.
    pub fn set_location_url(&self, url: impl Into<String>) {
        *self.location_url.borrow_mut() = url.into();
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

    // Same as `execute`, but tags any error with the source URL so that
    // browser-side logging can point at the offending script. Inline
    // scripts get a synthetic `{page_url}#inline-script-N` URL from the
    // collector; external scripts get their `src` value verbatim. An empty
    // `url` falls back to the bare error string — useful for one-off
    // evaluations where there is no document context.
    pub fn execute_with_url(&mut self, source: &str, url: &str) -> Result<String, String> {
        self.execute(source).map_err(|err| {
            if url.is_empty() {
                err
            } else {
                format!("{url}: {err}")
            }
        })
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
    ///
    /// Returns `true` when any handler called `event.preventDefault()`.
    /// Callers (BrowserState's click path) use this to decide whether to
    /// run the default action — e.g. skipping link navigation when JS
    /// handled the click itself.
    pub fn dispatch_event(&mut self, target: NodeId, event_type: &str) -> bool {
        self.dispatch_event_inner(target, event_type, None, true)
    }

    /// Direct dispatch — fires every listener registered on `target` for
    /// `event_type`, but does not walk up the parent chain afterwards.
    /// Used for events that don't bubble per spec, primarily `focus` /
    /// `blur` (the bubbling forms are `focusin` / `focusout`, which the
    /// toy doesn't ship yet). Real-world handlers register on the
    /// focused element directly, so a bubble would erroneously fire
    /// ancestor listeners that expected only their own focus state.
    pub fn dispatch_event_at(&mut self, target: NodeId, event_type: &str) -> bool {
        self.dispatch_event_inner(target, event_type, None, false)
    }

    /// Bubbling dispatch with a `key` payload exposed on the Event object.
    /// Used by BrowserState for `keydown`/`keyup` — handlers commonly read
    /// `event.key` (e.g. `if (event.key === "Enter")`), and `preventDefault`
    /// on `keydown` suppresses the default text-insertion / backspace
    /// action the BrowserState typing path would otherwise apply.
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
        let chain: Vec<NodeId> = if bubbles {
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
        } else {
            // Non-bubbling: only the target's own listeners run.
            vec![event_target]
        };
        if chain.is_empty() {
            return false;
        }
        let (event, event_state) = event::build_event_object(
            event_type,
            event_target,
            key,
            self.dom.clone(),
            self.listeners.clone(),
            &mut self.context,
        );
        let event_value = JsValue::from(event);
        let key_type = event_type.to_string();
        for current_target in chain {
            // `stopPropagation` (set inside a handler one ancestor below)
            // breaks the bubble before this ancestor's handlers run. The
            // ancestor that called stopPropagation still finished its own
            // handler list — only further bubbling is suppressed.
            if event_state.borrow().propagation_stopped {
                break;
            }
            // Tell the Event object which ancestor's handlers are about to
            // run; the `currentTarget` accessor reads this on each access.
            event_state.borrow_mut().current_target = Some(current_target);
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
            let this = JsValue::from(element::make_element(
                current_target,
                self.dom.clone(),
                self.listeners.clone(),
                &mut self.context,
            ));
            for handler in snapshot {
                // `stopImmediatePropagation` skips the rest of THIS
                // ancestor's listeners; the propagation flag it also sets
                // then breaks the outer loop on the next iteration.
                if event_state.borrow().immediate_propagation_stopped {
                    break;
                }
                if let Err(err) =
                    handler.call(&this, std::slice::from_ref(&event_value), &mut self.context)
                {
                    eprintln!("[event] {event_type} handler error: {err}");
                }
            }
        }
        // Clear `currentTarget` once the bubble has fully unwound so a
        // post-dispatch read (rare, but real handlers can stash the event
        // on a global) sees null instead of the last ancestor we visited.
        event_state.borrow_mut().current_target = None;
        // A handler may have resolved a promise or queued a setTimeout(0);
        // drain those before returning so observers up the call stack see
        // a fully-settled JS state without waiting for the next frame.
        self.drain_pending_jobs();
        event_state.borrow().default_prevented
    }
}

// Boa's Context is not Debug; surface a placeholder so containing structs can
// keep deriving Debug for diagnostics.
impl std::fmt::Debug for JsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsRuntime").finish_non_exhaustive()
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
    fn execute_with_url_prefixes_errors_with_source_label() {
        // The URL is prepended to the error string so the script-error log
        // can name the offending file. The bare Boa error follows the
        // colon — both the label and the original message stay visible.
        let mut runtime = runtime_with("");
        let err = runtime
            .execute_with_url("missing.prop", "https://example.com/app.js")
            .unwrap_err();
        assert!(
            err.starts_with("https://example.com/app.js: "),
            "error should be prefixed with the source URL, got: {err}"
        );
        assert!(
            err.to_lowercase().contains("missing"),
            "error should still reference the missing identifier, got: {err}"
        );
    }

    #[test]
    fn execute_with_url_returns_bare_error_when_url_is_empty() {
        // An empty URL string falls back to the unprefixed error so callers
        // that don't have a source URL (e.g. the REPL or one-off evals) get
        // exactly the same output as `execute`.
        let mut runtime = runtime_with("");
        let err = runtime.execute_with_url("missing.prop", "").unwrap_err();
        assert!(
            !err.starts_with(": "),
            "empty URL must not produce a leading separator, got: {err}"
        );
        assert!(
            err.to_lowercase().contains("missing"),
            "error should reference the missing identifier, got: {err}"
        );
    }

    #[test]
    fn execute_with_url_passes_through_success_values() {
        // Successful evaluations do not get the URL prefix — the label is
        // only attached to errors. This keeps the success path identical
        // to `execute` for callers that only care about the value.
        let mut runtime = runtime_with("");
        assert_eq!(
            runtime
                .execute_with_url("1 + 2", "https://example.com/app.js")
                .unwrap(),
            "3"
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
    fn window_and_document_add_event_listener_stubs_silently_accept_registrations() {
        // Author scripts routinely register `load` / `DOMContentLoaded`
        // listeners at module top level; without a window/document
        // addEventListener method the call throws and the rest of the
        // script never runs. The stub returns undefined and drops the
        // registration so subsequent code keeps executing.
        let mut runtime = runtime_with("");
        assert_eq!(
            runtime.execute("typeof window.addEventListener").unwrap(),
            "\"function\""
        );
        assert_eq!(
            runtime.execute("typeof window.removeEventListener").unwrap(),
            "\"function\""
        );
        assert_eq!(
            runtime
                .execute("typeof document.addEventListener")
                .unwrap(),
            "\"function\""
        );
        assert_eq!(
            runtime
                .execute("typeof document.removeEventListener")
                .unwrap(),
            "\"function\""
        );
        // No-op contract: real call shapes return undefined and must not
        // throw, even with arbitrary handler shapes (which the spec'd
        // method would normally validate).
        assert_eq!(
            runtime
                .execute("window.addEventListener('load', function () {})")
                .unwrap(),
            "undefined"
        );
        assert_eq!(
            runtime
                .execute(
                    "document.addEventListener('DOMContentLoaded', function () {}); 'after'"
                )
                .unwrap(),
            "\"after\""
        );
        // `self`/bare also resolve to the same global stub (window === self === globalThis).
        assert_eq!(
            runtime
                .execute("self.addEventListener('load', function () {})")
                .unwrap(),
            "undefined"
        );
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

    // ---- navigator / location / history (Step 18 stubs) ----
    //
    // These globals exist mostly so author scripts that read them at
    // module top-level (UA branches, client-side routers binding to
    // pushState, code that mirrors `location.href` into in-app state)
    // don't crash before the page renders. Coverage therefore focuses
    // on shape rather than behaviour: the right properties exist, the
    // `location` accessors decompose the URL the way the WHATWG URL
    // spec says they should, and `history` mutators silently accept.

    #[test]
    fn navigator_user_agent_is_present_and_versioned() {
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("typeof navigator").unwrap(), "\"object\"");
        let ua = runtime.execute("navigator.userAgent").unwrap();
        assert!(
            ua.contains("MiniBrowser"),
            "navigator.userAgent should advertise MiniBrowser, got: {ua}"
        );
    }

    #[test]
    fn location_accessors_collapse_to_empty_when_no_url_bound() {
        // Default JsRuntime starts with an empty URL buffer (the
        // bootstrap before BrowserState pushes a real URL through).
        // Every accessor must surface "" rather than throwing on the
        // failed Url::parse — scripts at module top often read these
        // before the loader has resolved anything.
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("location.href").unwrap(), "\"\"");
        assert_eq!(runtime.execute("location.protocol").unwrap(), "\"\"");
        assert_eq!(runtime.execute("location.host").unwrap(), "\"\"");
        assert_eq!(runtime.execute("location.hostname").unwrap(), "\"\"");
        assert_eq!(runtime.execute("location.pathname").unwrap(), "\"\"");
        assert_eq!(runtime.execute("location.search").unwrap(), "\"\"");
        assert_eq!(runtime.execute("location.hash").unwrap(), "\"\"");
        assert_eq!(runtime.execute("location.origin").unwrap(), "\"\"");
    }

    #[test]
    fn location_decomposes_url_into_whatwg_components() {
        let mut runtime = runtime_with("");
        runtime.set_location_url("https://example.com:8443/foo/bar?q=1&n=2#frag");
        assert_eq!(
            runtime.execute("location.href").unwrap(),
            "\"https://example.com:8443/foo/bar?q=1&n=2#frag\""
        );
        assert_eq!(runtime.execute("location.protocol").unwrap(), "\"https:\"");
        assert_eq!(
            runtime.execute("location.hostname").unwrap(),
            "\"example.com\""
        );
        assert_eq!(
            runtime.execute("location.host").unwrap(),
            "\"example.com:8443\""
        );
        assert_eq!(
            runtime.execute("location.pathname").unwrap(),
            "\"/foo/bar\""
        );
        assert_eq!(runtime.execute("location.search").unwrap(), "\"?q=1&n=2\"");
        assert_eq!(runtime.execute("location.hash").unwrap(), "\"#frag\"");
        assert_eq!(
            runtime.execute("location.origin").unwrap(),
            "\"https://example.com:8443\""
        );
    }

    #[test]
    fn location_host_drops_default_port_for_http_and_https() {
        // Default-port-aware host serialisation matches the WHATWG URL
        // spec: an http URL on 80 and an https URL on 443 both expose
        // `host` without the port (`hostname` is the same either way).
        let mut runtime = runtime_with("");
        runtime.set_location_url("https://example.com/page");
        assert_eq!(
            runtime.execute("location.host").unwrap(),
            "\"example.com\""
        );
        assert_eq!(
            runtime.execute("location.origin").unwrap(),
            "\"https://example.com\""
        );
        runtime.set_location_url("http://example.com/page");
        assert_eq!(
            runtime.execute("location.host").unwrap(),
            "\"example.com\""
        );
    }

    #[test]
    fn location_observes_subsequent_set_location_url_updates() {
        // The accessors capture an Rc<RefCell<String>> and re-parse on
        // every read, so back-to-back `set_location_url` calls flow
        // through to JS without re-defining any property.
        let mut runtime = runtime_with("");
        runtime.set_location_url("https://a.example/x");
        assert_eq!(
            runtime.execute("location.hostname").unwrap(),
            "\"a.example\""
        );
        runtime.set_location_url("https://b.example/y");
        assert_eq!(
            runtime.execute("location.hostname").unwrap(),
            "\"b.example\""
        );
    }

    #[test]
    fn history_stub_exposes_length_state_and_silent_mutators() {
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("typeof history").unwrap(), "\"object\"");
        // Stub: a single entry, null state. Real values come once
        // the JS bridge plumbs the BrowserState back/forward stack.
        assert_eq!(runtime.execute("history.length").unwrap(), "1");
        assert_eq!(runtime.execute("history.state").unwrap(), "null");
        // Mutators accept their canonical arg shapes without throwing.
        // Client-side routers call pushState during init; an exception
        // here would break the page before it could render.
        assert_eq!(
            runtime
                .execute("history.pushState({a:1}, '', '/x')")
                .unwrap(),
            "undefined"
        );
        assert_eq!(
            runtime
                .execute("history.replaceState(null, 't', '/y')")
                .unwrap(),
            "undefined"
        );
        assert_eq!(runtime.execute("history.back()").unwrap(), "undefined");
        assert_eq!(runtime.execute("history.forward()").unwrap(), "undefined");
        assert_eq!(runtime.execute("history.go(-1)").unwrap(), "undefined");
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
            runtime.execute("typeof document.getElementsByClassName").unwrap(),
            "\"function\""
        );
        assert_eq!(
            runtime.execute("typeof document.createElement").unwrap(),
            "\"function\""
        );
    }

    #[test]
    fn get_elements_by_class_name_collects_every_matching_element() {
        // HN-style helpers (`byClass('athing')`) iterate the result, so the
        // surface needs `.length` and indexed access. The match is
        // whitespace-tokenized: an element with `class="row hot"` is hit by
        // either `'row'` or `'hot'`. Document order is preserved.
        let mut runtime = runtime_with(
            r#"<div class="row hot"><span class="row">a</span><span class="row hot">b</span></div>"#,
        );
        assert_eq!(
            runtime
                .execute("document.getElementsByClassName('row').length")
                .unwrap(),
            "3"
        );
        assert_eq!(
            runtime
                .execute("document.getElementsByClassName('row')[0].tagName")
                .unwrap(),
            "\"DIV\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementsByClassName('row')[1].tagName")
                .unwrap(),
            "\"SPAN\""
        );
    }

    #[test]
    fn get_elements_by_class_name_requires_all_tokens_to_match() {
        // Multi-token argument: every whitespace-separated token must appear
        // in the element's class list. `'row hot'` matches an element classed
        // `"row hot"` but not one classed only `"row"`.
        let mut runtime = runtime_with(
            r#"<div class="row hot"><span class="row">a</span><span class="row hot">b</span></div>"#,
        );
        assert_eq!(
            runtime
                .execute("document.getElementsByClassName('row hot').length")
                .unwrap(),
            "2"
        );
    }

    #[test]
    fn local_and_session_storage_are_globals_with_storage_interface() {
        // Both globals must exist and expose the same minimal Storage
        // surface; quite a few client libraries probe `typeof
        // localStorage !== 'undefined' && typeof
        // localStorage.getItem === 'function'` before using it.
        let mut runtime = runtime_with("");
        for global in ["localStorage", "sessionStorage"] {
            assert_eq!(
                runtime.execute(&format!("typeof {global}")).unwrap(),
                "\"object\"",
            );
            for method in ["getItem", "setItem", "removeItem", "clear", "key"] {
                assert_eq!(
                    runtime
                        .execute(&format!("typeof {global}.{method}"))
                        .unwrap(),
                    "\"function\"",
                );
            }
            assert_eq!(
                runtime.execute(&format!("{global}.length")).unwrap(),
                "0"
            );
        }
    }

    #[test]
    fn local_storage_round_trips_string_values_and_coerces_non_strings() {
        // setItem stores; getItem returns the stored string; missing keys
        // produce null. Non-string values are ToString-coerced (the spec
        // requirement that turns numbers and booleans into their string
        // forms) — many sites rely on this for `setItem('count', 1)`.
        let mut runtime = runtime_with("");
        runtime
            .execute("localStorage.setItem('a', 'hello'); localStorage.setItem('b', 42);")
            .unwrap();
        assert_eq!(
            runtime.execute("localStorage.getItem('a')").unwrap(),
            "\"hello\""
        );
        assert_eq!(
            runtime.execute("localStorage.getItem('b')").unwrap(),
            "\"42\""
        );
        assert_eq!(
            runtime.execute("localStorage.getItem('absent')").unwrap(),
            "null"
        );
        assert_eq!(runtime.execute("localStorage.length").unwrap(), "2");
    }

    #[test]
    fn local_storage_remove_and_clear_drop_entries() {
        // removeItem deletes a single key; clear empties the entire store.
        // length tracks both. Removing a missing key is a no-op (per spec).
        let mut runtime = runtime_with("");
        runtime
            .execute("localStorage.setItem('a','1'); localStorage.setItem('b','2');")
            .unwrap();
        runtime.execute("localStorage.removeItem('a')").unwrap();
        assert_eq!(
            runtime.execute("localStorage.getItem('a')").unwrap(),
            "null"
        );
        assert_eq!(runtime.execute("localStorage.length").unwrap(), "1");
        runtime.execute("localStorage.removeItem('absent')").unwrap();
        assert_eq!(runtime.execute("localStorage.length").unwrap(), "1");
        runtime.execute("localStorage.clear()").unwrap();
        assert_eq!(runtime.execute("localStorage.length").unwrap(), "0");
    }

    #[test]
    fn local_storage_key_returns_inserted_position_or_null() {
        // key(n) reflects insertion order; out-of-range indices return null.
        // Re-setting an existing key keeps its position so the index of
        // earlier keys doesn't shift under the script's feet.
        let mut runtime = runtime_with("");
        runtime
            .execute("localStorage.setItem('a','1'); localStorage.setItem('b','2');")
            .unwrap();
        assert_eq!(runtime.execute("localStorage.key(0)").unwrap(), "\"a\"");
        assert_eq!(runtime.execute("localStorage.key(1)").unwrap(), "\"b\"");
        assert_eq!(runtime.execute("localStorage.key(2)").unwrap(), "null");
        runtime.execute("localStorage.setItem('a','99')").unwrap();
        assert_eq!(runtime.execute("localStorage.key(0)").unwrap(), "\"a\"");
        assert_eq!(
            runtime.execute("localStorage.getItem('a')").unwrap(),
            "\"99\""
        );
    }

    #[test]
    fn local_storage_and_session_storage_are_independent_buckets() {
        // Writes to one must not leak to the other — every real script
        // uses sessionStorage for "this tab only" data and localStorage
        // for "across visits", and conflating them would corrupt both.
        let mut runtime = runtime_with("");
        runtime
            .execute("localStorage.setItem('shared','local');")
            .unwrap();
        runtime
            .execute("sessionStorage.setItem('shared','session');")
            .unwrap();
        assert_eq!(
            runtime.execute("localStorage.getItem('shared')").unwrap(),
            "\"local\""
        );
        assert_eq!(
            runtime.execute("sessionStorage.getItem('shared')").unwrap(),
            "\"session\""
        );
    }

    #[test]
    fn document_body_and_head_return_first_matching_element_or_null() {
        // `document.body.appendChild(...)` is one of the most common boot
        // patterns; the accessor must resolve to the live <body> element so
        // mutations land in the right place. Same for <head> when scripts
        // inject <script>/<link>/<style> tags into the document head.
        let mut runtime = runtime_with(
            r#"<html><head><meta charset="utf-8"/></head><body><p>hi</p></body></html>"#,
        );
        assert_eq!(
            runtime.execute("document.body.tagName").unwrap(),
            "\"BODY\""
        );
        assert_eq!(
            runtime.execute("document.body.children[0].tagName").unwrap(),
            "\"P\""
        );
        assert_eq!(
            runtime.execute("document.head.tagName").unwrap(),
            "\"HEAD\""
        );
    }

    #[test]
    fn document_body_and_head_return_null_when_missing() {
        // A fragment without an explicit <body>/<head> wrapper — the
        // accessors must surface null rather than throw, so script defenses
        // like `document.body && document.body.classList.add(...)` work.
        let mut runtime = runtime_with(r#"<div>only this</div>"#);
        assert_eq!(runtime.execute("document.body").unwrap(), "null");
        assert_eq!(runtime.execute("document.head").unwrap(), "null");
    }

    #[test]
    fn document_body_reflects_text_content_writes_through_the_wrapper() {
        // Confirm the accessor returns a live wrapper, not a one-shot
        // snapshot — writing through `document.body.textContent` and
        // reading it back must round-trip via the same Document handle.
        let mut runtime = runtime_with(r#"<body><p>old</p></body>"#);
        runtime
            .execute("document.body.textContent = 'new copy';")
            .unwrap();
        assert_eq!(
            runtime.execute("document.body.textContent").unwrap(),
            "\"new copy\""
        );
    }

    #[test]
    fn get_elements_by_class_name_returns_empty_array_for_no_match() {
        // A miss must still return an array (not null) so `.length` and
        // for-loops on the call site stay safe. Same goes for an empty input.
        let mut runtime = runtime_with(r#"<div class="row">x</div>"#);
        assert_eq!(
            runtime
                .execute("document.getElementsByClassName('absent').length")
                .unwrap(),
            "0"
        );
        assert_eq!(
            runtime
                .execute("document.getElementsByClassName('   ').length")
                .unwrap(),
            "0"
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

    // ---- Element.matches / Element.closest ----
    //
    // Both share the same parsed-selector path that querySelector uses, so
    // the matcher coverage above already exercises the engine itself. The
    // tests below pin down what's specific to the new entry points: matches
    // is *node-local* (just the receiver) while closest walks self-then-
    // parents and stops at the first hit.

    #[test]
    fn matches_reports_whether_self_satisfies_selector() {
        let mut runtime = runtime_with(
            r#"<section><article class="card"><p id="target" class="hit">hi</p></article></section>"#,
        );
        let target = "document.getElementById('target')";
        // True positives: tag, class, descendant combinator, child combinator.
        assert_eq!(
            runtime.execute(&format!("{target}.matches('p')")).unwrap(),
            "true"
        );
        assert_eq!(
            runtime
                .execute(&format!("{target}.matches('.hit')"))
                .unwrap(),
            "true"
        );
        assert_eq!(
            runtime
                .execute(&format!("{target}.matches('section .hit')"))
                .unwrap(),
            "true"
        );
        assert_eq!(
            runtime
                .execute(&format!("{target}.matches('article > p')"))
                .unwrap(),
            "true"
        );
        // True negatives: wrong tag, wrong class, child combinator that
        // skips an intermediate ancestor (`section > .hit` requires .hit
        // to be a direct child of section, but article is in between).
        assert_eq!(
            runtime.execute(&format!("{target}.matches('div')")).unwrap(),
            "false"
        );
        assert_eq!(
            runtime
                .execute(&format!("{target}.matches('.miss')"))
                .unwrap(),
            "false"
        );
        assert_eq!(
            runtime
                .execute(&format!("{target}.matches('section > .hit')"))
                .unwrap(),
            "false"
        );
    }

    #[test]
    fn matches_throws_on_invalid_selector() {
        let mut runtime = runtime_with(r#"<p id="t">hi</p>"#);
        let err = runtime
            .execute("document.getElementById('t').matches('!!')")
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("selector"),
            "error should mention the bad selector, got: {err}"
        );
    }

    #[test]
    fn closest_returns_self_when_selector_already_matches_receiver() {
        let mut runtime = runtime_with(
            r#"<article class="card"><p id="target" class="hit">hi</p></article>"#,
        );
        // Self-hit must be returned even though there is also an ancestor
        // that matches a *different* selector — closest is a self-first walk.
        assert_eq!(
            runtime
                .execute("document.getElementById('target').closest('.hit').tagName")
                .unwrap(),
            "\"P\""
        );
    }

    #[test]
    fn closest_walks_parent_chain_until_match() {
        let mut runtime = runtime_with(
            r#"<section id="root"><article class="card"><p id="target">hi</p></article></section>"#,
        );
        let target = "document.getElementById('target')";
        // Skips `<p>` (no match), hits `<article class="card">`.
        assert_eq!(
            runtime
                .execute(&format!("{target}.closest('.card').tagName"))
                .unwrap(),
            "\"ARTICLE\""
        );
        // Walks two hops up: past article, lands on section.
        assert_eq!(
            runtime
                .execute(&format!("{target}.closest('#root').tagName"))
                .unwrap(),
            "\"SECTION\""
        );
    }

    #[test]
    fn closest_returns_null_when_no_ancestor_matches() {
        let mut runtime = runtime_with(r#"<article><p id="target">hi</p></article>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('target').closest('.absent')")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn closest_evaluates_combinators_against_each_candidates_own_ancestors() {
        // `section > article` must fail when checked against `<p>` (its
        // ancestors are section→article, but the candidate must itself be
        // an article that is the direct child of a section). Once the walk
        // reaches `<article>`, the same selector now matches because the
        // candidate's own parent is the section.
        let mut runtime = runtime_with(
            r#"<section><article class="card"><p id="target">hi</p></article></section>"#,
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('target').closest('section > article').tagName")
                .unwrap(),
            "\"ARTICLE\""
        );
    }

    #[test]
    fn closest_throws_on_invalid_selector() {
        let mut runtime = runtime_with(r#"<p id="t">hi</p>"#);
        let err = runtime
            .execute("document.getElementById('t').closest('!!')")
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("selector"),
            "error should mention the bad selector, got: {err}"
        );
    }

    // ---- parentElement / previousElementSibling / nextElementSibling ----
    //
    // Element-only sibling traversal: text nodes are intentionally skipped
    // so authors who whitespace-format their HTML don't have to filter them
    // out client-side. `parentElement` collapses to null when the parent is
    // the implicit document root (matches the parentElement vs parentNode
    // distinction in the standard).

    #[test]
    fn parent_element_returns_immediate_element_parent() {
        let mut runtime = runtime_with(
            r#"<section><article><p id="target">hi</p></article></section>"#,
        );
        let target = "document.getElementById('target')";
        assert_eq!(
            runtime
                .execute(&format!("{target}.parentElement.tagName"))
                .unwrap(),
            "\"ARTICLE\""
        );
        assert_eq!(
            runtime
                .execute(&format!("{target}.parentElement.parentElement.tagName"))
                .unwrap(),
            "\"SECTION\""
        );
    }

    #[test]
    fn parent_element_is_null_at_root() {
        // Walking off the top of the document yields null — the section's
        // parent in our arena is "no node", not a Document wrapper.
        let mut runtime = runtime_with(r#"<section id="root"><p>hi</p></section>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('root').parentElement")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn previous_and_next_element_sibling_skip_text_nodes() {
        // The whitespace between tags in source HTML becomes Text nodes in
        // the arena; the *Element* sibling getters must hop over them and
        // hand back the next real element on either side.
        let mut runtime = runtime_with(
            r#"<section>
                 <p class="a">first</p>
                 <p id="target" class="b">middle</p>
                 <p class="c">last</p>
               </section>"#,
        );
        let target = "document.getElementById('target')";
        assert_eq!(
            runtime
                .execute(&format!(
                    "{target}.previousElementSibling.getAttribute('class')"
                ))
                .unwrap(),
            "\"a\""
        );
        assert_eq!(
            runtime
                .execute(&format!(
                    "{target}.nextElementSibling.getAttribute('class')"
                ))
                .unwrap(),
            "\"c\""
        );
    }

    #[test]
    fn sibling_accessors_return_null_at_edges() {
        let mut runtime = runtime_with(
            r#"<section><p id="first">a</p><p id="last">b</p></section>"#,
        );
        // The first child has no preceding element; the last has no
        // following element — both must surface as JS null rather than
        // wrapping around or returning the parent.
        assert_eq!(
            runtime
                .execute("document.getElementById('first').previousElementSibling")
                .unwrap(),
            "null"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('last').nextElementSibling")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn sibling_accessors_chain_across_multiple_hops() {
        // Chained traversal in both directions — the accessors return real
        // wrappers (not lazy proxies) so each hop re-reads the underlying
        // arena with no caching surprises.
        let mut runtime = runtime_with(
            r#"<section><p id="a">a</p><p id="b">b</p><p id="c">c</p></section>"#,
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('a').nextElementSibling.nextElementSibling.getAttribute('id')")
                .unwrap(),
            "\"c\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('c').previousElementSibling.previousElementSibling.getAttribute('id')")
                .unwrap(),
            "\"a\""
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

    // ---- Element.value accessor (#6 in Notion: <input type=text>) ----

    #[test]
    fn value_accessor_reads_initial_value_attribute() {
        // The getter pulls straight from the `value` attribute the parser
        // recorded — same data path the layout/render pipeline already
        // uses to draw the field text.
        let mut runtime = runtime_with(r#"<input id="q" value="hi"/>"#);
        assert_eq!(
            runtime.execute("document.getElementById('q').value").unwrap(),
            "\"hi\""
        );
    }

    #[test]
    fn value_accessor_returns_empty_string_when_attribute_missing() {
        // Real <input>.value is the empty string when the attribute is
        // absent (NOT undefined / null). Diverging from that would surprise
        // any script that does `if (input.value === '')` or string-concat.
        let mut runtime = runtime_with(r#"<input id="q"/>"#);
        assert_eq!(
            runtime.execute("document.getElementById('q').value").unwrap(),
            "\"\""
        );
    }

    #[test]
    fn value_setter_writes_value_attribute_observable_via_get_attribute() {
        // Writing `.value = …` lands in the SAME attribute slot the toy's
        // typing pipeline writes to. That's what makes JS-driven default
        // values and live read/write parity work uniformly with keyboard
        // input.
        let mut runtime = runtime_with(r#"<input id="q" value="old"/>"#);
        runtime
            .execute("document.getElementById('q').value = 'fresh';")
            .unwrap();
        assert_eq!(
            runtime.execute("document.getElementById('q').value").unwrap(),
            "\"fresh\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('q').getAttribute('value')")
                .unwrap(),
            "\"fresh\""
        );
    }

    #[test]
    fn value_setter_coerces_non_string_arguments() {
        // ToString-coercion mirrors how setAttribute handles non-string
        // arguments — JS scripts often assign numbers (`.value = 42`)
        // and expect the field to display "42".
        let mut runtime = runtime_with(r#"<input id="q"/>"#);
        runtime
            .execute("document.getElementById('q').value = 42;")
            .unwrap();
        assert_eq!(
            runtime.execute("document.getElementById('q').value").unwrap(),
            "\"42\""
        );
    }

    // ---- classList: add / remove / toggle / contains ----

    #[test]
    fn class_list_add_appends_unique_tokens_to_class_attribute() {
        // Add dedupes against existing tokens (the second `active` is a no-op)
        // and accepts variadic args. Read-back via getAttribute confirms the
        // mutation lands in the same Document arena BrowserState reads.
        let mut runtime = runtime_with(r#"<div id="x" class="card"></div>"#);
        runtime
            .execute(
                "var el = document.getElementById('x');\
                 el.classList.add('active', 'theme-dark');\
                 el.classList.add('active');",
            )
            .unwrap();
        assert_eq!(
            runtime.execute("el.getAttribute('class')").unwrap(),
            "\"card active theme-dark\""
        );
    }

    #[test]
    fn class_list_remove_drops_tokens_and_keeps_others() {
        let mut runtime = runtime_with(r#"<div id="x" class="a b c"></div>"#);
        runtime
            .execute(
                "var el = document.getElementById('x');\
                 el.classList.remove('b');\
                 el.classList.remove('missing');",
            )
            .unwrap();
        assert_eq!(
            runtime.execute("el.getAttribute('class')").unwrap(),
            "\"a c\""
        );
    }

    #[test]
    fn class_list_toggle_flips_membership_and_returns_new_state() {
        let mut runtime = runtime_with(r#"<div id="x" class="a"></div>"#);
        // Without `force`: present → removed (returns false), absent → added (returns true).
        assert_eq!(
            runtime
                .execute("document.getElementById('x').classList.toggle('a')")
                .unwrap(),
            "false"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('x').classList.toggle('b')")
                .unwrap(),
            "true"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('x').getAttribute('class')")
                .unwrap(),
            "\"b\""
        );
        // With `force`: idempotent add / idempotent remove.
        assert_eq!(
            runtime
                .execute("document.getElementById('x').classList.toggle('b', true)")
                .unwrap(),
            "true"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('x').classList.toggle('b', false)")
                .unwrap(),
            "false"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('x').getAttribute('class')")
                .unwrap(),
            "\"\""
        );
    }

    #[test]
    fn class_list_contains_reports_token_membership() {
        let mut runtime = runtime_with(r#"<div id="x" class="a b"></div>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('x').classList.contains('a')")
                .unwrap(),
            "true"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('x').classList.contains('missing')")
                .unwrap(),
            "false"
        );
        // Element with no class attribute reads as zero-token list.
        let mut runtime = runtime_with(r#"<div id="y"></div>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('y').classList.contains('foo')")
                .unwrap(),
            "false"
        );
    }

    #[test]
    fn class_list_rejects_empty_or_whitespace_tokens() {
        // DOMTokenList raises SyntaxError on these per spec; the toy mirrors
        // the throw so author code that catches it (rare but real) sees the
        // expected shape.
        let mut runtime = runtime_with(r#"<div id="x"></div>"#);
        assert!(
            runtime
                .execute("document.getElementById('x').classList.add('')")
                .is_err()
        );
        assert!(
            runtime
                .execute("document.getElementById('x').classList.add('a b')")
                .is_err()
        );
    }

    #[test]
    fn class_list_mutations_are_visible_to_style_pass_via_class_attribute() {
        // Smoke check that classList writes flow through to the same DOM arena
        // BrowserState reads for layout — same shared-arena contract Step 5.x
        // pinned for textContent / appendChild.
        let mut runtime = runtime_with(r#"<div id="x" class="card"></div>"#);
        runtime
            .execute(
                "document.getElementById('x').classList.add('active');\
                 document.getElementById('x').classList.toggle('card');",
            )
            .unwrap();
        let dom = runtime.dom_handle();
        let document = dom.borrow();
        let target = document.roots()[0];
        let class_attr = document
            .element_data(target)
            .and_then(|e| e.attributes.get("class"))
            .map(|s| s.as_str());
        assert_eq!(class_attr, Some("active"));
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

    // ---- innerHTML (Step 13) ----

    #[test]
    fn inner_html_get_serializes_children_to_html_string() {
        let mut runtime =
            runtime_with(r#"<div id="host"><p class="lead">hi</p><span>!</span></div>"#);
        // Children only — the host element itself is NOT in the output.
        // Attribute order is alphabetical (BTreeMap), so `class="lead"`
        // comes out as written here.
        assert_eq!(
            runtime
                .execute("document.getElementById('host').innerHTML")
                .unwrap(),
            "\"<p class=\\\"lead\\\">hi</p><span>!</span>\""
        );
    }

    #[test]
    fn inner_html_get_returns_empty_string_for_a_leaf_element() {
        let mut runtime = runtime_with(r#"<div id="host"></div>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('host').innerHTML")
                .unwrap(),
            "\"\""
        );
    }

    #[test]
    fn inner_html_set_replaces_children_with_parsed_fragment() {
        let mut runtime = runtime_with(r#"<div id="host"><p>old</p></div>"#);
        runtime
            .execute(
                "var fresh = '<span>fresh</span>';\
                 document.getElementById('host').innerHTML = fresh;",
            )
            .unwrap();
        assert_eq!(
            runtime
                .execute("document.getElementById('host').children.length")
                .unwrap(),
            "1"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('host').children[0].tagName")
                .unwrap(),
            "\"SPAN\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('host').children[0].textContent")
                .unwrap(),
            "\"fresh\""
        );
    }

    #[test]
    fn inner_html_set_supports_multiple_top_level_siblings() {
        // The fragment parser is not document-level: zero, one, or many
        // top-level siblings are all valid input. `host` should end up
        // with three Element children.
        let mut runtime = runtime_with(r#"<div id="host"></div>"#);
        runtime
            .execute(
                "var sibs = '<span>a</span><em>b</em><b>c</b>';\
                 document.getElementById('host').innerHTML = sibs;",
            )
            .unwrap();
        assert_eq!(
            runtime
                .execute("document.getElementById('host').children.length")
                .unwrap(),
            "3"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"abc\""
        );
    }

    #[test]
    fn inner_html_set_with_empty_string_clears_children() {
        let mut runtime = runtime_with(r#"<div id="host"><p>old</p><span>x</span></div>"#);
        runtime
            .execute(
                "var blank = '';\
                 document.getElementById('host').innerHTML = blank;",
            )
            .unwrap();
        assert_eq!(
            runtime
                .execute("document.getElementById('host').children.length")
                .unwrap(),
            "0"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"\""
        );
    }

    #[test]
    fn inner_html_set_invalidates_handles_to_replaced_children() {
        // The standard says the old subtree is detached. Our toy goes a
        // step further and tombstones the slots so outstanding wrappers
        // resolve to None — same convention `removeChild` follows. A read
        // through the dead handle should degrade to null per the lenient
        // getter policy.
        let mut runtime = runtime_with(r#"<div id="host"><p id="old">old</p></div>"#);
        runtime
            .execute(
                "var oldKid = document.getElementById('old');\
                 var fresh = '<p>new</p>';\
                 document.getElementById('host').innerHTML = fresh;",
            )
            .unwrap();
        assert_eq!(runtime.execute("oldKid.textContent").unwrap(), "null");
        assert_eq!(
            runtime.execute("document.getElementById('old')").unwrap(),
            "null"
        );
    }

    #[test]
    fn inner_html_set_throws_syntax_error_on_malformed_fragment() {
        // Mismatched closing tag is exactly the kind of input scripts
        // shouldn't be feeding innerHTML; surface it as SyntaxError so
        // `try { …innerHTML = unsafeString }` works.
        let mut runtime = runtime_with(r#"<div id="host"></div>"#);
        let err = runtime
            .execute(
                "var bad = '<div><p></div>';\
                 document.getElementById('host').innerHTML = bad;",
            )
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("innerhtml"),
            "expected innerHTML SyntaxError, got: {err}"
        );
    }

    #[test]
    fn inner_html_set_throws_on_a_stale_receiver() {
        let mut runtime = runtime_with(
            r#"<div id="parent"><div id="host"><p>old</p></div></div>"#,
        );
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 document.getElementById('parent').removeChild(host);",
            )
            .unwrap();
        let err = runtime
            .execute(
                "var fresh = '<span>fresh</span>';\
                 host.innerHTML = fresh;",
            )
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("detached") || err.to_lowercase().contains("removed"),
            "expected stale-handle TypeError, got: {err}"
        );
    }

    #[test]
    fn inner_html_set_drops_listeners_registered_on_replaced_subtree() {
        // A listener on the soon-to-be-replaced kid must not fire after
        // the swap. The map is also pruned so it can't grow without
        // bound on innerHTML-heavy pages.
        let mut runtime = runtime_with(r#"<div id="host"><p id="kid">old</p></div>"#);
        runtime
            .execute(
                "var hits = 0;\
                 document.getElementById('kid').addEventListener('click', function () { hits = hits + 1; });",
            )
            .unwrap();
        // Sanity: the listener fires before the swap.
        let kid_id_before = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let host = dom.roots()[0];
            dom.get(host).unwrap().children[0]
        };
        runtime.dispatch_event(kid_id_before, "click");
        assert_eq!(runtime.execute("hits").unwrap(), "1");

        // Swap. The old <p> is tombstoned, the listener entry pruned.
        runtime
            .execute(
                "var fresh = '<p>new</p>';\
                 document.getElementById('host').innerHTML = fresh;",
            )
            .unwrap();
        // Dispatching against the (now stale) old NodeId should be a
        // no-op — the dispatcher skips tombstoned targets.
        runtime.dispatch_event(kid_id_before, "click");
        assert_eq!(runtime.execute("hits").unwrap(), "1");
    }

    #[test]
    fn inner_html_get_escapes_special_characters_and_emits_void_open_tag() {
        // <br> serializes as `<br>` (no `</br>`), and a text node containing
        // `<` / `&` / `>` is escaped per the HTML serialization spec so the
        // output stays well-formed if fed back through the parser later.
        let mut runtime = runtime_with(r#"<div id="host"></div>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 host.appendChild(document.createElement('br'));\
                 var t = document.createElement('p');\
                 t.textContent = 'a<b>&c';\
                 host.appendChild(t);",
            )
            .unwrap();
        assert_eq!(
            runtime.execute("host.innerHTML").unwrap(),
            "\"<br><p>a&lt;b&gt;&amp;c</p>\""
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
    fn event_current_target_updates_per_ancestor_during_bubble() {
        // `currentTarget` reads the ancestor whose listener is *currently*
        // running, not the original target. Each ancestor's handler should
        // see its own element via `e.currentTarget`, while `e.target`
        // stays pinned to the deepest hit (here, the inner div).
        let mut runtime = runtime_with(
            r#"<div id="outer"><div id="inner">x</div></div>"#,
        );
        runtime
            .execute(
                "var trace = '';\
                 document.getElementById('outer').addEventListener('click', function(e) {\
                     trace += e.currentTarget.getAttribute('id') + '/' + e.target.getAttribute('id') + ';';\
                 });\
                 document.getElementById('inner').addEventListener('click', function(e) {\
                     trace += e.currentTarget.getAttribute('id') + '/' + e.target.getAttribute('id') + ';';\
                 });",
            )
            .unwrap();
        let inner_id = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let outer = dom.roots()[0];
            dom.get(outer).unwrap().children[0]
        };
        runtime.dispatch_event(inner_id, "click");
        // Inner handler runs first (bubble order), currentTarget=inner.
        // Outer handler runs second, currentTarget=outer. target stays inner.
        assert_eq!(
            runtime.execute("trace").unwrap(),
            "\"inner/inner;outer/inner;\""
        );
    }

    #[test]
    fn event_current_target_reads_null_after_dispatch_returns() {
        // Real handlers occasionally stash the event object on a global
        // and inspect it later (analytics flush, retry-on-error). Once
        // the bubble has unwound `currentTarget` should read null —
        // matches what every browser exposes after dispatch.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var stash = null;\
                 document.getElementById('x').addEventListener('click', function(e) { stash = e; });",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        runtime.dispatch_event(id, "click");
        assert_eq!(runtime.execute("stash.currentTarget").unwrap(), "null");
        // The original target is preserved (event.target is set once at
        // dispatch start and never moves).
        assert_eq!(
            runtime.execute("stash.target.getAttribute('id')").unwrap(),
            "\"x\""
        );
    }

    #[test]
    fn event_stop_propagation_skips_remaining_ancestors_but_finishes_current() {
        // stopPropagation set on the inner handler must NOT skip the
        // second listener registered on the same ancestor — only further
        // bubbling. The outer ancestor sees nothing.
        let mut runtime = runtime_with(
            r#"<div id="outer"><div id="inner">x</div></div>"#,
        );
        runtime
            .execute(
                "var trace = '';\
                 document.getElementById('outer').addEventListener('click', function() { trace += 'outer;'; });\
                 var inner = document.getElementById('inner');\
                 inner.addEventListener('click', function(e) { trace += 'inner1;'; e.stopPropagation(); });\
                 inner.addEventListener('click', function() { trace += 'inner2;'; });",
            )
            .unwrap();
        let inner_id = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let outer = dom.roots()[0];
            dom.get(outer).unwrap().children[0]
        };
        runtime.dispatch_event(inner_id, "click");
        assert_eq!(runtime.execute("trace").unwrap(), "\"inner1;inner2;\"");
    }

    #[test]
    fn event_stop_immediate_propagation_also_skips_remaining_listeners_on_target() {
        // stopImmediatePropagation goes one step further: the same
        // ancestor's later handlers are skipped too, then the bubble
        // breaks just like stopPropagation.
        let mut runtime = runtime_with(
            r#"<div id="outer"><div id="inner">x</div></div>"#,
        );
        runtime
            .execute(
                "var trace = '';\
                 document.getElementById('outer').addEventListener('click', function() { trace += 'outer;'; });\
                 var inner = document.getElementById('inner');\
                 inner.addEventListener('click', function(e) { trace += 'inner1;'; e.stopImmediatePropagation(); });\
                 inner.addEventListener('click', function() { trace += 'inner2;'; });",
            )
            .unwrap();
        let inner_id = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let outer = dom.roots()[0];
            dom.get(outer).unwrap().children[0]
        };
        runtime.dispatch_event(inner_id, "click");
        assert_eq!(runtime.execute("trace").unwrap(), "\"inner1;\"");
    }

    #[test]
    fn event_prevent_default_flips_default_prevented_and_dispatch_returns_true() {
        // dispatch_event returns whether any handler called
        // preventDefault(). Read-back via `defaultPrevented` confirms the
        // flag is observable from JS too.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var prevented_in_handler = null;\
                 document.getElementById('x').addEventListener('click', function(e) {\
                     e.preventDefault();\
                     prevented_in_handler = e.defaultPrevented;\
                 });",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        let returned_prevented = runtime.dispatch_event(id, "click");
        assert!(returned_prevented);
        assert_eq!(
            runtime.execute("prevented_in_handler").unwrap(),
            "true"
        );
    }

    #[test]
    fn dispatch_event_at_fires_only_on_target_and_skips_ancestors() {
        // `dispatch_event_at` is the no-bubble path used for focus/blur.
        // The inner div's listener fires; the outer div's identical
        // listener never sees the event because the chain is just
        // [target], not [target, ...ancestors].
        let mut runtime =
            runtime_with(r#"<div id="outer"><div id="inner">x</div></div>"#);
        runtime
            .execute(
                "var trace = '';\
                 document.getElementById('outer').addEventListener('focus', function() { trace += 'outer;'; });\
                 document.getElementById('inner').addEventListener('focus', function() { trace += 'inner;'; });",
            )
            .unwrap();
        let inner_id = {
            let dom = runtime.dom_handle();
            let dom = dom.borrow();
            let outer = dom.roots()[0];
            dom.get(outer).unwrap().children[0]
        };
        runtime.dispatch_event_at(inner_id, "focus");
        assert_eq!(runtime.execute("trace").unwrap(), "\"inner;\"");
    }

    #[test]
    fn event_default_prevented_starts_false_when_no_handler_calls_prevent_default() {
        // Symmetric check: a handler that just inspects the event must
        // see defaultPrevented=false, and dispatch returns false too.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        runtime
            .execute(
                "var seen = null;\
                 document.getElementById('x').addEventListener('click', function(e) { seen = e.defaultPrevented; });",
            )
            .unwrap();
        let id = runtime.dom_handle().borrow().roots()[0];
        let returned = runtime.dispatch_event(id, "click");
        assert!(!returned);
        assert_eq!(runtime.execute("seen").unwrap(), "false");
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

    // ---- Step 8 (#14 in Notion): async / await + queueMicrotask ----

    #[test]
    fn async_function_returns_a_promise_that_resolves_to_its_return_value() {
        // The bare `async function` syntax compiles into a Promise
        // wrapper at parse time; awaiting nothing returns a resolved
        // Promise carrying the function's return value. Confirms the
        // engine surface (already in Boa) reaches scripts unchanged.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var resolved;\
                 async function answer() { return 42; }\
                 answer().then(function (v) { resolved = v; });",
            )
            .unwrap();
        // execute() drains microtasks, so the .then callback already ran.
        assert_eq!(runtime.execute("resolved").unwrap(), "42");
    }

    #[test]
    fn await_inside_async_function_resumes_with_resolved_value() {
        // The `await` keyword pauses the async function until the
        // awaited Promise settles, then resumes with the resolved
        // value. Without microtask drain at end of execute() the
        // continuation wouldn't run before our assertion.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var seen;\
                 async function chain() {\
                   var first = await Promise.resolve(7);\
                   var second = await Promise.resolve(first + 1);\
                   seen = second;\
                 }\
                 chain();",
            )
            .unwrap();
        assert_eq!(runtime.execute("seen").unwrap(), "8");
    }

    #[test]
    fn await_propagates_rejection_into_catch() {
        // The "throw across an await boundary" path: a rejected
        // Promise inside `await` raises in the async function,
        // matching real browsers' control-flow semantics.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var caught = '';\
                 async function blow() { await Promise.reject('boom'); }\
                 blow().catch(function (e) { caught = e; });",
            )
            .unwrap();
        assert_eq!(runtime.execute("caught").unwrap(), "\"boom\"");
    }

    #[test]
    fn queue_microtask_runs_callback_during_drain() {
        // The polyfill-equivalent path: `queueMicrotask(fn)` schedules
        // `fn` on the same microtask queue that Promise jobs use, and
        // execute()'s end-of-run drain flushes it before returning.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var hits = 0;\
                 queueMicrotask(function () { hits = hits + 1; });",
            )
            .unwrap();
        assert_eq!(runtime.execute("hits").unwrap(), "1");
    }

    #[test]
    fn queue_microtask_with_non_callable_first_arg_throws_type_error() {
        // Spec contract: a non-function argument is a TypeError. Real
        // engines surface this so authors don't silently lose work
        // when they pass `undefined` from a misconfigured config.
        let mut runtime = runtime_with("");
        let err = runtime.execute("queueMicrotask(123);").unwrap_err();
        assert!(
            err.to_lowercase().contains("function"),
            "TypeError should mention `function`, got: {err}"
        );
    }

    #[test]
    fn queue_microtask_runs_after_synchronous_code() {
        // The microtask runs *after* the rest of the script body
        // returns, never re-entering it. The trace pin captures the
        // ordering: sync work first, then the queued microtask.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var trace = '';\
                 queueMicrotask(function () { trace += 'micro;'; });\
                 trace += 'sync;';",
            )
            .unwrap();
        assert_eq!(runtime.execute("trace").unwrap(), "\"sync;micro;\"");
    }

    // ---- Step 8b (#16/#17 in Notion): fetch + Response ----
    //
    // The HTTP exchange uses `crate::net::fetch`, which is sync, so a
    // single `execute()` plus its end-of-run microtask drain is enough
    // to land the resolved Promise's `.then` callbacks. Each test
    // spawns a tiny local TCP responder and tears it down via a
    // `JoinHandle`, mirroring the pattern in `net.rs` integration tests.

    #[test]
    fn fetch_resolves_with_response_carrying_status_and_url() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var status; var ok; var url;\
             fetch('http://127.0.0.1:{port}/').then(function (r) {{\
                 status = r.status; ok = r.ok; url = r.url;\
             }});",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("status").unwrap(), "200");
        assert_eq!(runtime.execute("ok").unwrap(), "true");
        assert_eq!(
            runtime.execute("url").unwrap(),
            format!("\"http://127.0.0.1:{port}/\"")
        );
    }

    #[test]
    fn fetch_response_text_returns_body_as_resolved_promise() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = "hello fetch";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var body;\
             fetch('http://127.0.0.1:{port}/')\
                 .then(function (r) {{ return r.text(); }})\
                 .then(function (t) {{ body = t; }});",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("body").unwrap(), "\"hello fetch\"");
    }

    #[test]
    fn fetch_response_json_parses_body_via_json_parse() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = r#"{"name":"alice","score":42}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var name; var score;\
             fetch('http://127.0.0.1:{port}/')\
                 .then(function (r) {{ return r.json(); }})\
                 .then(function (j) {{ name = j.name; score = j.score; }});",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("name").unwrap(), "\"alice\"");
        assert_eq!(runtime.execute("score").unwrap(), "42");
    }

    #[test]
    fn fetch_non_2xx_status_resolves_with_ok_false() {
        // 404 is a successful HTTP exchange — fetch resolves, the
        // caller inspects `ok`/`status` to decide the outcome. This
        // is the standard contract real backends rely on for
        // optimistic fetches that handle "missing" inline.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var ok; var status;\
             fetch('http://127.0.0.1:{port}/missing')\
                 .then(function (r) {{ ok = r.ok; status = r.status; }});",
        );
        runtime.execute(&script).unwrap();
        // Allow the server to finish writing/closing before joining.
        let _ = server.join();

        assert_eq!(runtime.execute("status").unwrap(), "404");
        assert_eq!(runtime.execute("ok").unwrap(), "false");
    }

    #[test]
    fn fetch_with_invalid_url_rejects_promise() {
        // No scheme separator → URL parse fails before we touch the
        // network. The Promise rejects with a TypeError; the script's
        // `.catch` handler captures it.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var caught = '';\
                 fetch('not a url').catch(function (e) { caught = String(e); });",
            )
            .unwrap();
        let caught = runtime.execute("caught").unwrap();
        assert!(
            caught.contains("invalid URL") || caught.to_lowercase().contains("typeerror"),
            "expected invalid-URL TypeError, got: {caught}"
        );
    }

    #[test]
    fn await_fetch_in_async_function_resumes_with_response() {
        // Closes the loop with the async/await tests above: the same
        // microtask drain that lets `await Promise.resolve(...)` resume
        // also lets `await fetch(...)` resume with a Response object,
        // since `fetch` returns a Promise like any other.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = "awaited";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var got;\
             (async function () {{\
                 var r = await fetch('http://127.0.0.1:{port}/');\
                 got = await r.text();\
             }})();",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("got").unwrap(), "\"awaited\"");
    }

    #[test]
    fn fetch_response_json_with_invalid_body_rejects() {
        // Malformed JSON: text() would still succeed, but json()
        // surfaces the underlying `JSON.parse` SyntaxError as a
        // rejected Promise so callers can `.catch` it.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = "not-json";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var caught = '';\
             fetch('http://127.0.0.1:{port}/')\
                 .then(function (r) {{ return r.json(); }})\
                 .catch(function (e) {{ caught = String(e); }});",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        let caught = runtime.execute("caught").unwrap();
        assert!(
            caught.to_lowercase().contains("syntax")
                || caught.to_lowercase().contains("json"),
            "expected SyntaxError from JSON.parse, got: {caught}"
        );
    }

    // ---- Step 8c (#17 leftover in Notion): fetch POST + headers ----
    //
    // The `init` second arg routes the request through
    // net::fetch_with_request, so coverage focuses on the wire-level
    // shape: a POST stays a POST on the request line, the body is
    // forwarded verbatim, and author headers ride alongside the
    // toy's defaults. The test server captures what it received and
    // hands it back over JoinHandle so the assertion can read it.

    /// Read a complete HTTP request off `stream` (header block plus
    /// `Content-Length`-bounded body). Returns the entire raw bytes
    /// the client sent so the test can assert against the request
    /// line, individual headers, and the body in one shot. Reading
    /// in two passes (headers first, then exactly N body bytes) is
    /// required because the toy uses keep-alive — a naive single
    /// `read` may stop after the headers and miss the body that's
    /// already in the kernel buffer.
    fn read_full_request(stream: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        // Read until we see the end-of-headers marker.
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap();
            if n == 0 {
                break buf.len();
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(idx) = find_subsequence(&buf, b"\r\n\r\n") {
                break idx + 4;
            }
        };
        // If a Content-Length header announced more body bytes than
        // we already buffered, drain those too. Real servers parse
        // the headers properly; the test only needs Content-Length
        // because all the test scripts send fixed-size string bodies.
        let so_far = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let body_len = so_far
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
            .and_then(|line| line.split_once(':'))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let already_buffered = buf.len() - header_end;
        if body_len > already_buffered {
            let need = body_len - already_buffered;
            let mut tail = vec![0u8; need];
            stream.read_exact(&mut tail).unwrap();
            buf.extend_from_slice(&tail);
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    #[test]
    fn fetch_with_post_method_sends_post_on_the_request_line() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let received = read_full_request(&mut stream);
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            received
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var ok;\
             fetch('http://127.0.0.1:{port}/api', {{ method: 'POST' }})\
                 .then(function (r) {{ ok = r.ok; }});"
        );
        runtime.execute(&script).unwrap();
        let received = server.join().unwrap();

        assert_eq!(runtime.execute("ok").unwrap(), "true");
        assert!(
            received.starts_with("POST /api HTTP/1.1"),
            "expected POST request line, got: {received:?}"
        );
    }

    #[test]
    fn fetch_lowercase_method_is_normalised_to_uppercase_on_the_wire() {
        // The HTTP spec is case-sensitive on the request line. JS
        // authors routinely write `method: 'post'`, so we upper-case
        // before sending — the same normalisation real browsers do.
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let received = read_full_request(&mut stream);
            let response = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            received
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "fetch('http://127.0.0.1:{port}/x', {{ method: 'put' }}).then(function () {{}});"
        );
        runtime.execute(&script).unwrap();
        let received = server.join().unwrap();

        assert!(
            received.starts_with("PUT /x HTTP/1.1"),
            "method should be upper-cased on the wire, got: {received:?}"
        );
    }

    #[test]
    fn fetch_post_body_is_forwarded_with_content_length() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let received = read_full_request(&mut stream);
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            received
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "fetch('http://127.0.0.1:{port}/post', {{ method: 'POST', body: 'hello world' }})\
                 .then(function () {{}});",
        );
        runtime.execute(&script).unwrap();
        let received = server.join().unwrap();

        // Content-Length tracks the body, and the body sits at the
        // very end after the blank line that terminates the header
        // block. Both invariants matter: a server that only reads
        // Content-Length bytes after CRLFCRLF needs both to line up.
        assert!(
            received.contains("Content-Length: 11"),
            "expected Content-Length: 11 (length of 'hello world'), got: {received:?}"
        );
        assert!(
            received.ends_with("hello world"),
            "body must be appended after the headers, got: {received:?}"
        );
    }

    #[test]
    fn fetch_init_headers_are_appended_to_the_default_headers() {
        // Author headers ride after the toy's User-Agent / Accept /
        // Accept-Encoding defaults. The order is fixed so a server
        // looking for X-Auth doesn't have to scan past anything
        // unexpected.
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let received = read_full_request(&mut stream);
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            received
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "fetch('http://127.0.0.1:{port}/', {{ headers: {{ 'X-Auth': 'token123', 'X-Trace': 'abc' }} }})\
                 .then(function () {{}});",
        );
        runtime.execute(&script).unwrap();
        let received = server.join().unwrap();

        assert!(
            received.contains("X-Auth: token123"),
            "expected X-Auth header in request, got: {received:?}"
        );
        assert!(
            received.contains("X-Trace: abc"),
            "expected X-Trace header in request, got: {received:?}"
        );
    }

    #[test]
    fn fetch_init_must_be_an_object_or_omitted() {
        // Passing a primitive (number, boolean, …) for init is a
        // synchronous TypeError — same shape real browsers raise. The
        // toy rejects the returned Promise rather than throwing
        // synchronously so existing `fetch(...).catch` patterns still
        // work, but the message identifies the offender.
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var caught = '';\
                 fetch('http://127.0.0.1:1/', 42)\
                     .catch(function (e) { caught = String(e); });",
            )
            .unwrap();
        let caught = runtime.execute("caught").unwrap();
        assert!(
            caught.to_lowercase().contains("init")
                || caught.to_lowercase().contains("object")
                || caught.to_lowercase().contains("typeerror"),
            "expected init-must-be-object error, got: {caught}"
        );
    }

    // ---- XMLHttpRequest (Step 15) ----

    #[test]
    fn xhr_constructor_yields_instance_in_unsent_state() {
        // A fresh instance starts in UNSENT (readyState=0) with no
        // status / response data. The numeric constants are exposed on
        // the instance so callers can compare against `xhr.DONE`
        // instead of the literal `4`.
        let mut runtime = runtime_with("");
        runtime
            .execute("var xhr = new XMLHttpRequest();")
            .unwrap();
        assert_eq!(runtime.execute("xhr.readyState").unwrap(), "0");
        assert_eq!(runtime.execute("xhr.status").unwrap(), "0");
        assert_eq!(runtime.execute("xhr.responseText").unwrap(), "\"\"");
        assert_eq!(runtime.execute("xhr.UNSENT").unwrap(), "0");
        assert_eq!(runtime.execute("xhr.OPENED").unwrap(), "1");
        assert_eq!(runtime.execute("xhr.HEADERS_RECEIVED").unwrap(), "2");
        assert_eq!(runtime.execute("xhr.LOADING").unwrap(), "3");
        assert_eq!(runtime.execute("xhr.DONE").unwrap(), "4");
    }

    #[test]
    fn xhr_open_transitions_state_to_opened() {
        let mut runtime = runtime_with("");
        runtime
            .execute(
                "var xhr = new XMLHttpRequest();\
                 xhr.open('GET', 'http://example.test/');",
            )
            .unwrap();
        assert_eq!(runtime.execute("xhr.readyState").unwrap(), "1");
    }

    #[test]
    fn xhr_send_populates_response_fields_after_done() {
        // Happy path: a successful GET pushes status, statusText, and
        // responseText through to the JS side; readyState lands on
        // DONE (4) and responseURL reflects the requested URL.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = "ok";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var xhr = new XMLHttpRequest();\
             xhr.open('GET', 'http://127.0.0.1:{port}/');\
             xhr.send();",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("xhr.readyState").unwrap(), "4");
        assert_eq!(runtime.execute("xhr.status").unwrap(), "200");
        assert_eq!(runtime.execute("xhr.statusText").unwrap(), "\"OK\"");
        assert_eq!(runtime.execute("xhr.responseText").unwrap(), "\"ok\"");
        // `response` mirrors `responseText` for the default
        // (empty-string) responseType the toy uses.
        assert_eq!(runtime.execute("xhr.response").unwrap(), "\"ok\"");
        assert_eq!(
            runtime.execute("xhr.responseURL").unwrap(),
            format!("\"http://127.0.0.1:{port}/\"")
        );
    }

    #[test]
    fn xhr_setrequestheader_carries_to_outgoing_request() {
        // The header registered via setRequestHeader must land in the
        // wire request — otherwise auth / API-key flows break. We
        // capture the full request bytes via the existing
        // `read_full_request` helper so the assertion isn't racy
        // against the kernel's TCP framing.
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let received = read_full_request(&mut stream);
            let response = "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            received
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var xhr = new XMLHttpRequest();\
             xhr.open('GET', 'http://127.0.0.1:{port}/');\
             xhr.setRequestHeader('X-Trace', 'abc');\
             xhr.send();",
        );
        runtime.execute(&script).unwrap();
        let received = server.join().unwrap();

        assert!(
            received.contains("X-Trace: abc"),
            "expected X-Trace header in request, got: {received:?}"
        );
    }

    #[test]
    fn xhr_send_writes_post_body_to_server() {
        use std::io::Write;
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let received = read_full_request(&mut stream);
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
            received
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var xhr = new XMLHttpRequest();\
             xhr.open('POST', 'http://127.0.0.1:{port}/submit');\
             xhr.send('payload=42');",
        );
        runtime.execute(&script).unwrap();
        let received = server.join().unwrap();

        assert!(
            received.starts_with("POST /submit"),
            "expected POST request line, got: {received:?}"
        );
        assert!(
            received.ends_with("payload=42"),
            "body must be appended after the headers, got: {received:?}"
        );
    }

    #[test]
    fn xhr_onreadystatechange_fires_until_done() {
        // The toy collapses HEADERS_RECEIVED → LOADING → DONE inside
        // send(); each transition fires `readystatechange`, so a
        // handler that records states sees `2,3,4`. jQuery's $.ajax
        // listens here (or on `onload`) for completion.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var trace = '';\
             var xhr = new XMLHttpRequest();\
             xhr.onreadystatechange = function () {{ trace += String(xhr.readyState) + ','; }};\
             xhr.open('GET', 'http://127.0.0.1:{port}/');\
             xhr.send();",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("trace").unwrap(), "\"2,3,4,\"");
    }

    #[test]
    fn xhr_onload_fires_after_done() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let body = "loaded";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var seen = null;\
             var xhr = new XMLHttpRequest();\
             xhr.onload = function () {{ seen = xhr.responseText; }};\
             xhr.open('GET', 'http://127.0.0.1:{port}/');\
             xhr.send();",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("seen").unwrap(), "\"loaded\"");
    }

    #[test]
    fn xhr_addeventlistener_load_fires_alongside_onload() {
        // Both delivery channels should fire — property-style first,
        // then addEventListener-registered handlers in registration
        // order. Two distinct entries in `trace` confirms the
        // listener registry isn't shadowed by the property handler.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var trace = '';\
             var xhr = new XMLHttpRequest();\
             xhr.onload = function () {{ trace += 'prop,'; }};\
             xhr.addEventListener('load', function () {{ trace += 'listener,'; }});\
             xhr.open('GET', 'http://127.0.0.1:{port}/');\
             xhr.send();",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        assert_eq!(runtime.execute("trace").unwrap(), "\"prop,listener,\"");
    }

    #[test]
    fn xhr_get_response_header_lookup_is_case_insensitive() {
        // Header lookup is case-insensitive per spec — `XHR.getResponseHeader`
        // is the standard way to read a Content-Type back, and authors
        // routinely spell it `content-type`.
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Custom: yes\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var xhr = new XMLHttpRequest();\
             xhr.open('GET', 'http://127.0.0.1:{port}/');\
             xhr.send();",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        // Mixed-case input still finds the header value.
        assert_eq!(
            runtime
                .execute("xhr.getResponseHeader('content-type')")
                .unwrap(),
            "\"application/json\""
        );
        assert_eq!(
            runtime
                .execute("xhr.getResponseHeader('X-CUSTOM')")
                .unwrap(),
            "\"yes\""
        );
        // Missing header returns null.
        assert_eq!(
            runtime
                .execute("xhr.getResponseHeader('not-present')")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn xhr_get_all_response_headers_joins_headers_with_crlf() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request).unwrap();
            let response = "HTTP/1.1 200 OK\r\nA: 1\r\nB: 2\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
            stream.write_all(response.as_bytes()).unwrap();
        });

        let mut runtime = runtime_with("");
        let script = format!(
            "var xhr = new XMLHttpRequest();\
             xhr.open('GET', 'http://127.0.0.1:{port}/');\
             xhr.send();\
             var s = xhr.getAllResponseHeaders();",
        );
        runtime.execute(&script).unwrap();
        server.join().unwrap();

        // Probing through JS sidesteps the JsValue::display escaping
        // dance — `\r\n` in a real string round-trips through indexOf
        // regardless of how the harness prints it. The toy network
        // layer lowercases response header names while parsing, so
        // `A: 1` arrives as `a: 1`; we match the post-parse form.
        // Other headers (content-length, connection) may be present
        // alongside the two we explicitly wrote.
        assert_eq!(
            runtime
                .execute("s.indexOf('a: 1\\r\\n') >= 0")
                .unwrap(),
            "true"
        );
        assert_eq!(
            runtime
                .execute("s.indexOf('b: 2\\r\\n') >= 0")
                .unwrap(),
            "true"
        );
    }

    #[test]
    fn xhr_send_before_open_throws() {
        let mut runtime = runtime_with("");
        let err = runtime
            .execute(
                "var xhr = new XMLHttpRequest();\
                 xhr.send();",
            )
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("opened"),
            "expected state-must-be-OPENED error, got: {err}"
        );
    }

    #[test]
    fn xhr_set_request_header_before_open_throws() {
        let mut runtime = runtime_with("");
        let err = runtime
            .execute(
                "var xhr = new XMLHttpRequest();\
                 xhr.setRequestHeader('X-Foo', 'bar');",
            )
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("opened"),
            "expected state-must-be-OPENED error, got: {err}"
        );
    }

    #[test]
    fn xhr_open_with_invalid_url_throws() {
        let mut runtime = runtime_with("");
        let err = runtime
            .execute(
                "var xhr = new XMLHttpRequest();\
                 xhr.open('GET', 'not a url');",
            )
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("invalid url"),
            "expected invalid-URL TypeError, got: {err}"
        );
    }
}
