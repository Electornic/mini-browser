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
mod timers;
mod util;
mod window;

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
        console::register_console(&mut context);
        window::register_window_aliases(&mut context);
        document::register_document(&mut context, dom.clone(), listeners.clone());
        timers::register_timers(
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
        let event = event::build_event_object(
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
            let this = JsValue::from(element::make_element(
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
