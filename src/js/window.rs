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
//
// Step 18 adds three more globals: `navigator`, `location`, and
// `history`. They're stubs by design — most pages reach for them at
// load time (UA sniffing, reading `location.href` to drive routing,
// checking `history.length` before binding a back button), and a
// missing object would crash those scripts before anything ran. The
// real navigation history lives on `BrowserState`; the JS history stub
// silently accepts pushState/replaceState calls so client-side routers
// don't error out, but it doesn't actually mutate the browser stack.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsError, JsNativeError, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    object::{
        ObjectInitializer,
        builtins::{JsFunction, JsPromise},
    },
    property::Attribute,
};

use crate::net::Url;

pub(super) fn register_window_aliases(
    context: &mut Context,
    location_url: Rc<RefCell<String>>,
) {
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

    register_navigator(context);
    register_location(context, location_url);
    register_history(context);
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

// `navigator` global. Real browsers expose hundreds of properties here;
// we ship the single field UA-sniffing scripts almost always read so
// the cleanly-versioned identifier doesn't surprise pages that branch
// on its presence. Adding more (`platform`, `language`, …) is a one-
// line ObjectInitializer extension when a page actually needs them.
fn register_navigator(context: &mut Context) {
    let navigator = ObjectInitializer::new(context)
        .property(
            js_string!("userAgent"),
            JsString::from("MiniBrowser/0.1"),
            Attribute::all(),
        )
        .build();
    let _ = context.register_global_property(
        js_string!("navigator"),
        JsValue::from(navigator),
        Attribute::all(),
    );
}

// `location` global. Every property is a getter that re-parses the
// shared URL string on each access — that way state.rs can call
// `set_location_url` after a navigation and the next `location.href`
// read inside a still-live Promise observe the new URL without us
// having to touch the property descriptors. Setters are deliberately
// omitted: writing to `window.location.href` is a navigation in real
// browsers, and we don't have a path from the JS bridge back into
// `BrowserState::navigate_to_href` yet. A failed parse (empty buffer
// or unsupported scheme) collapses every accessor to "" — that's what
// scripts running before any URL has been bound observe.
fn register_location(context: &mut Context, location_url: Rc<RefCell<String>>) {
    // Build every accessor closure up-front against the realm — once
    // `ObjectInitializer::new(context)` takes its mutable borrow no
    // further `to_js_function(context.realm())` call can run, so the
    // getters all have to be in hand before the builder starts.
    let href_get = make_string_getter(context, location_url.clone(), |raw| raw.to_string());
    let protocol_get = make_url_getter(context, location_url.clone(), |u| format!("{}:", u.scheme));
    let host_get = make_url_getter(context, location_url.clone(), format_host);
    let hostname_get = make_url_getter(context, location_url.clone(), |u| u.host.clone());
    let pathname_get =
        make_url_getter(context, location_url.clone(), |u| split_path_search_hash(&u.path).0);
    let search_get =
        make_url_getter(context, location_url.clone(), |u| split_path_search_hash(&u.path).1);
    let hash_get =
        make_url_getter(context, location_url.clone(), |u| split_path_search_hash(&u.path).2);
    let origin_get = make_url_getter(context, location_url, |u| {
        format!("{}://{}", u.scheme, format_host(u))
    });

    let location = ObjectInitializer::new(context)
        .accessor(
            js_string!("href"),
            Some(href_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("protocol"),
            Some(protocol_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("host"),
            Some(host_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("hostname"),
            Some(hostname_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("pathname"),
            Some(pathname_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("search"),
            Some(search_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("hash"),
            Some(hash_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("origin"),
            Some(origin_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .build();
    let _ = context.register_global_property(
        js_string!("location"),
        JsValue::from(location),
        Attribute::all(),
    );
}

// `href` is the verbatim buffer — no parsing. Empty buffer returns "".
fn make_string_getter(
    context: &mut Context,
    buf: Rc<RefCell<String>>,
    transform: fn(&str) -> String,
) -> boa_engine::object::builtins::JsFunction {
    unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let raw = buf.borrow().clone();
            Ok(JsValue::from(JsString::from(transform(&raw).as_str())))
        })
    }
    .to_js_function(context.realm())
}

// All other location accessors re-parse the buffer per read so that
// `set_location_url` updates flow through immediately. A parse failure
// (empty buffer, unsupported scheme) collapses to "" — same shape JS
// observes before any URL has been bound.
fn make_url_getter(
    context: &mut Context,
    buf: Rc<RefCell<String>>,
    compute: fn(&Url) -> String,
) -> boa_engine::object::builtins::JsFunction {
    unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let raw = buf.borrow().clone();
            let value = match Url::parse(&raw) {
                Ok(url) => compute(&url),
                Err(_) => String::new(),
            };
            Ok(JsValue::from(JsString::from(value.as_str())))
        })
    }
    .to_js_function(context.realm())
}

// `history` global. The toy already keeps a back/forward stack on
// `BrowserState`, but routing those into JS is a bigger plumbing
// exercise than the rest of the stub work — for now `length` is fixed
// at 1 and the mutators silently accept their arguments. Plenty of
// client-side routers (React Router, Vue Router) call pushState during
// initialisation; without these stubs they'd throw "history.pushState
// is not a function" before the page rendered.
fn register_history(context: &mut Context) {
    let history = ObjectInitializer::new(context)
        .property(js_string!("length"), JsValue::from(1i32), Attribute::all())
        .property(js_string!("state"), JsValue::null(), Attribute::all())
        .function(
            NativeFunction::from_fn_ptr(history_state_noop),
            js_string!("pushState"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(history_state_noop),
            js_string!("replaceState"),
            3,
        )
        .function(
            NativeFunction::from_fn_ptr(history_nav_noop),
            js_string!("back"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(history_nav_noop),
            js_string!("forward"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(history_nav_noop),
            js_string!("go"),
            1,
        )
        .build();
    let _ = context.register_global_property(
        js_string!("history"),
        JsValue::from(history),
        Attribute::all(),
    );
}

fn history_state_noop(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn history_nav_noop(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

// Default-port-aware host serialisation: an http:80 / https:443 URL
// drops the port, anything else keeps it. Matches the WHATWG URL
// spec's `host` property and what real browsers expose on
// `location.host`.
fn format_host(url: &Url) -> String {
    let default_port = match url.scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => 0,
    };
    if url.port == default_port {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    }
}

// Split the path component into (pathname, search, hash). `Url::parse`
// stores everything after the authority verbatim in `path`, including
// the query string and fragment, so we slice them apart at the first
// `?` and `#`. The query starts with `?`, the fragment with `#`, the
// pathname is the leftover prefix — all matching what the WHATWG URL
// `pathname` / `search` / `hash` accessors return.
fn split_path_search_hash(raw_path: &str) -> (String, String, String) {
    let (head, hash) = match raw_path.split_once('#') {
        Some((head, frag)) => (head.to_string(), format!("#{frag}")),
        None => (raw_path.to_string(), String::new()),
    };
    let (pathname, search) = match head.split_once('?') {
        Some((path, query)) => (path.to_string(), format!("?{query}")),
        None => (head, String::new()),
    };
    (pathname, search, hash)
}
