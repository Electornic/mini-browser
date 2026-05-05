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
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use rquickjs::{CatchResultExt, CaughtError, Context, Function, Runtime, Value};

use crate::dom::{Document, NodeId};

mod console;
mod util;
mod window;

// Hidden property name used to round-trip a NodeId through any Element
// JsObject — same contract as the boa bridge. 4.8b layers the DOM
// wrapper factory on this; for 4.8a the constant just reserves the name.
pub(crate) const NODE_ID_PROP: &str = "_nodeId";

// 4.8c will replace the unit type with `rquickjs::Persistent<Function>`
// (the rquickjs idiom for keeping a JS callable alive across `with`
// scopes). For 4.8a the field exists so the runtime layout matches the
// boa version even though no listener flows in yet.
pub(crate) type ListenerMap = HashMap<(NodeId, String), Vec<()>>;
pub(crate) type RafQueue = Vec<(u32, ())>;

pub struct JsRuntime {
    // Both handles are refcounted internally; we keep the `Runtime` even
    // though it never gets touched outside `drain_pending_jobs` because
    // dropping it before `Context` would tear the realm out from under us.
    runtime: Runtime,
    context: Context,
    #[allow(dead_code)]
    dom: Rc<RefCell<Document>>,
    #[allow(dead_code)]
    listeners: Rc<RefCell<ListenerMap>>,
    #[allow(dead_code)]
    raf_callbacks: Rc<RefCell<RafQueue>>,
    #[allow(dead_code)]
    cancelled_timers: Rc<RefCell<HashSet<u32>>>,
    #[allow(dead_code)]
    next_timer_id: Rc<Cell<u32>>,
    location_url: Rc<RefCell<String>>,
}

impl JsRuntime {
    pub fn new(dom: Rc<RefCell<Document>>) -> Self {
        let runtime = Runtime::new().expect("rquickjs Runtime should construct");
        let context = Context::full(&runtime).expect("rquickjs Context should construct");
        let listeners: Rc<RefCell<ListenerMap>> = Rc::new(RefCell::new(HashMap::new()));
        let raf_callbacks: Rc<RefCell<RafQueue>> = Rc::new(RefCell::new(Vec::new()));
        let cancelled_timers: Rc<RefCell<HashSet<u32>>> = Rc::new(RefCell::new(HashSet::new()));
        let next_timer_id: Rc<Cell<u32>> = Rc::new(Cell::new(0));
        let location_url: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

        context.with(|ctx| {
            // The display helper turns any JS value into the same shape
            // boa's `value.display()` produces — strings stringified with
            // surrounding quotes (via JSON.stringify), primitives via
            // `String(v)`, objects via JSON.stringify with a String() fallback
            // for circular / unrepresentable shapes. `execute()` calls this
            // on the eval result, so the top-level integration tests that
            // assert against the printed form continue to work.
            ctx.eval::<(), _>(DISPLAY_HELPER)
                .expect("display helper should compile");
            console::register_console(&ctx).expect("console should register");
            window::register_window_aliases(&ctx, location_url.clone())
                .expect("window aliases should register");
        });

        Self {
            runtime,
            context,
            dom,
            listeners,
            raf_callbacks,
            cancelled_timers,
            next_timer_id,
            location_url,
        }
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
        self.execute(source).map_err(|err| {
            if url.is_empty() {
                err
            } else {
                format!("{url}: {err}")
            }
        })
    }

    pub fn drain_pending_jobs(&mut self) {
        // `execute_pending_job` returns Ok(true) when a microtask ran,
        // Ok(false) when the queue is empty, and Err on a runtime-side
        // failure (which we surface but keep draining around — same
        // behaviour boa's `run_jobs` had once we ate the error).
        loop {
            match self.runtime.execute_pending_job() {
                Ok(executed) => {
                    if !executed {
                        break;
                    }
                }
                Err(err) => {
                    eprintln!("[jobs] error draining job queue: {err:?}");
                    break;
                }
            }
        }
    }

    /// 4.8c — fire raf callbacks. Stub for 4.8a.
    pub fn run_animation_frame_callbacks(&mut self) {}

    /// 4.8b/c — DOM event dispatch. Stub for 4.8a.
    pub fn dispatch_event(&mut self, _target: NodeId, _event_type: &str) -> bool {
        false
    }

    pub fn dispatch_event_at(&mut self, _target: NodeId, _event_type: &str) -> bool {
        false
    }

    pub fn dispatch_keyboard_event(
        &mut self,
        _target: NodeId,
        _event_type: &str,
        _key: &str,
    ) -> bool {
        false
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
            if message.is_empty() {
                "exception".to_string()
            } else {
                message
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
    fn history_pushstate_is_silent_noop() {
        let mut rt = fresh();
        // Real router code does this at boot — just must not throw.
        rt.execute("history.pushState({a:1}, '', '/x'); history.replaceState(null, '', '/y')")
            .unwrap();
        assert_eq!(rt.execute("history.length").unwrap(), "1");
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
}
