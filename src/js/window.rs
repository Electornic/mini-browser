// Browsers expose `window` and `self` as aliases of the global object —
// scripts in the wild rely on either name being defined (`window.foo`,
// `self.addEventListener`, `typeof window === 'object'` feature checks).
// Boa already provides `globalThis` per spec; we just bind the two extra
// names to the same object so `window === globalThis === self` and a
// `var x` at top level shows up as `window.x` like every other engine.
//
// On top of the aliases this module installs no-op `addEventListener` /
// `removeEventListener` on the global object. Real browsers dispatch
// `load`, `DOMContentLoaded`, scroll, resize, … to window-level
// listeners; the toy can't yet, but countless author scripts call
// `window.addEventListener('load', …)` at module top level and would
// otherwise crash with "addEventListener is not a function" before any
// page logic runs. The stub silently accepts the registration so the
// rest of the script keeps executing.
//
// We also expose `queueMicrotask` here — a tiny shim that delegates to
// the Promise job queue Boa already runs. Once `fetch` lands the
// pattern `fetch(...).then(...)` already does the heavy lifting; this
// global is for the leftover handful of libraries that prefer the
// explicit microtask scheduling primitive.

use boa_engine::{
    Context, JsError, JsNativeError, JsResult, JsValue, NativeFunction,
    js_string,
    object::builtins::{JsFunction, JsPromise},
    property::Attribute,
};

pub(super) fn register_window_aliases(context: &mut Context) {
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

    // Both methods sit on the global object, which means
    // `window.addEventListener('load', fn)`, `self.addEventListener(…)`,
    // and bare `addEventListener(…)` all reach the same stub.
    let _ = context.register_global_builtin_callable(
        js_string!("addEventListener"),
        2,
        NativeFunction::from_fn_ptr(noop_event_listener),
    );
    let _ = context.register_global_builtin_callable(
        js_string!("removeEventListener"),
        2,
        NativeFunction::from_fn_ptr(noop_event_listener),
    );

    let _ = context.register_global_builtin_callable(
        js_string!("queueMicrotask"),
        1,
        NativeFunction::from_fn_ptr(queue_microtask),
    );
}

// Silent no-op shared between add/removeEventListener at the window
// level. Returns undefined regardless of argument shape — same shape an
// uninstalled listener would produce, so scripts that only register
// (without expecting a side effect) keep running.
fn noop_event_listener(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

// `queueMicrotask(fn)` schedules `fn` as a microtask — equivalent to
// `Promise.resolve().then(fn)` per the WHATWG HTML spec but without the
// allocation of a thenable result. We piggy-back on Boa's promise job
// queue: an already-resolved Promise's `then` enqueues a `PromiseJob`
// that the runtime drains at the same point as any other microtask.
// Throwing or non-callable arguments raise TypeError, matching real
// browsers (and avoiding a silent drop that hides bugs).
fn queue_microtask(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let callback_value = args.first().cloned().unwrap_or_default();
    let Some(callback_obj) = callback_value.as_callable() else {
        return Err(JsError::from_native(
            JsNativeError::typ()
                .with_message("queueMicrotask: argument must be a function"),
        ));
    };
    let callback_fn = JsFunction::from_object(callback_obj)
        .expect("as_callable returned a non-callable JsObject");
    // `JsPromise::resolve(undefined).then(callback, _, ctx)` is the
    // textbook polyfill for queueMicrotask, and Boa's job executor
    // picks the resulting PromiseJob up on the next drain.
    let promise = JsPromise::resolve(JsValue::undefined(), context);
    promise.then(Some(callback_fn), None, context);
    Ok(JsValue::undefined())
}
