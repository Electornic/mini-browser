// Browser-shaped globals: `window` / `self` aliases, `navigator`,
// `location`, `history`, plus the `addEventListener` / `queueMicrotask`
// shims author scripts call at module top level. Same contract as the
// boa version — every property author code reaches for at boot exists
// and either works or no-ops without throwing.
//
// `location.*` accessors live behind `Object.defineProperty` getters
// that re-read the shared `Rc<RefCell<String>>` on each access. That
// way `JsRuntime::set_location_url` flows through to the next JS read
// without redefining any descriptors.

use std::cell::RefCell;
use std::rc::Rc;

use rquickjs::{Ctx, Object, Result, Value, prelude::Func, prelude::Rest};

use crate::net::Url;

pub(super) fn register_window_aliases(
    ctx: &Ctx<'_>,
    location_url: Rc<RefCell<String>>,
) -> Result<()> {
    // `window === self === globalThis`. rquickjs has no direct "set the
    // global object as a property" helper — easier to do it from JS.
    let alias_src = r#"globalThis.window = globalThis;
                       globalThis.self = globalThis;"#;
    ctx.eval::<(), _>(alias_src)?;

    let globals = ctx.globals();

    // Window-level addEventListener / removeEventListener: silent stubs
    // until 4.8c. Author scripts (React / jQuery boot) call these at
    // module top level — without the shim every page would crash before
    // any handler ran. `Rest<Value>` swallows whatever shape the caller
    // hands in.
    globals.set(
        "addEventListener",
        Func::from(|_args: Rest<Value<'_>>| {}),
    )?;
    globals.set(
        "removeEventListener",
        Func::from(|_args: Rest<Value<'_>>| {}),
    )?;

    // queueMicrotask(fn): equivalent to `Promise.resolve().then(fn)` per
    // the WHATWG HTML spec. We define the shim entirely in JS — it
    // piggy-backs on rquickjs' microtask queue (drained from
    // `JsRuntime::drain_pending_jobs`) and throws TypeError for
    // non-callable arguments to match real browsers.
    let qmt_src = r#"
        globalThis.queueMicrotask = function (cb) {
            if (typeof cb !== 'function') {
                throw new TypeError('queueMicrotask: argument must be a function');
            }
            Promise.resolve().then(cb);
        };
    "#;
    ctx.eval::<(), _>(qmt_src)?;

    register_navigator(ctx)?;
    register_location(ctx, location_url)?;
    register_history(ctx)?;

    Ok(())
}

// `navigator` global. We ship the single field UA-sniffing scripts
// almost always read; `platform`/`language` etc. fall in when actually
// needed.
fn register_navigator(ctx: &Ctx<'_>) -> Result<()> {
    let nav = Object::new(ctx.clone())?;
    nav.set("userAgent", "MiniBrowser/0.1")?;
    ctx.globals().set("navigator", nav)?;
    Ok(())
}

// `location` global. Each property is a getter that re-parses the
// shared URL string on each access — that way state.rs can call
// `set_location_url` after a navigation and the next `location.href`
// read inside a still-live Promise observes the new URL without us
// touching the descriptors. Setters are deliberately omitted: writing
// to `window.location.href` is a navigation in real browsers, and the
// JS bridge has no path back into `BrowserState::navigate_to_href`
// yet. A failed parse (empty buffer or unsupported scheme) collapses
// every accessor to "" — same shape JS observes before any URL has
// been bound.
//
// Implementation trick: register the seven Rust-backed getters under
// `__mb_loc_*` names on the global, then run a tiny JS bootstrap that
// calls `Object.defineProperty` for each one and deletes the temporary
// globals. Cheaper than reaching for rquickjs' lower-level Atom /
// PropertyDescriptor APIs and keeps the JS side reading naturally.
fn register_location(ctx: &Ctx<'_>, buf: Rc<RefCell<String>>) -> Result<()> {
    let globals = ctx.globals();

    install_string_getter(&globals, "__mb_loc_href", buf.clone(), |raw| {
        raw.to_string()
    })?;
    install_url_getter(&globals, "__mb_loc_protocol", buf.clone(), |u| {
        format!("{}:", u.scheme)
    })?;
    install_url_getter(&globals, "__mb_loc_host", buf.clone(), format_host)?;
    install_url_getter(&globals, "__mb_loc_hostname", buf.clone(), |u| {
        u.host.clone()
    })?;
    install_url_getter(&globals, "__mb_loc_pathname", buf.clone(), |u| {
        split_path_search_hash(&u.path).0
    })?;
    install_url_getter(&globals, "__mb_loc_search", buf.clone(), |u| {
        split_path_search_hash(&u.path).1
    })?;
    install_url_getter(&globals, "__mb_loc_hash", buf.clone(), |u| {
        split_path_search_hash(&u.path).2
    })?;
    install_url_getter(&globals, "__mb_loc_origin", buf, |u| {
        format!("{}://{}", u.scheme, format_host(u))
    })?;

    ctx.eval::<(), _>(LOCATION_BOOT)?;

    Ok(())
}

const LOCATION_BOOT: &str = r#"
(function () {
    var src = {
        href:     globalThis.__mb_loc_href,
        protocol: globalThis.__mb_loc_protocol,
        host:     globalThis.__mb_loc_host,
        hostname: globalThis.__mb_loc_hostname,
        pathname: globalThis.__mb_loc_pathname,
        search:   globalThis.__mb_loc_search,
        hash:     globalThis.__mb_loc_hash,
        origin:   globalThis.__mb_loc_origin,
    };
    var loc = {};
    Object.keys(src).forEach(function (name) {
        Object.defineProperty(loc, name, {
            get: src[name],
            configurable: true,
            enumerable: true,
        });
    });
    loc.toString = function () { return this.href; };
    globalThis.location = loc;
    Object.keys(src).forEach(function (name) {
        delete globalThis['__mb_loc_' + name];
    });
})();
"#;

fn install_string_getter(
    globals: &Object<'_>,
    name: &str,
    buf: Rc<RefCell<String>>,
    transform: fn(&str) -> String,
) -> Result<()> {
    globals.set(
        name,
        Func::from(move || -> String {
            let raw = buf.borrow().clone();
            transform(&raw)
        }),
    )
}

fn install_url_getter(
    globals: &Object<'_>,
    name: &str,
    buf: Rc<RefCell<String>>,
    compute: fn(&Url) -> String,
) -> Result<()> {
    globals.set(
        name,
        Func::from(move || -> String {
            let raw = buf.borrow().clone();
            match Url::parse(&raw) {
                Ok(url) => compute(&url),
                Err(_) => String::new(),
            }
        }),
    )
}

// `history` global. The toy already keeps a back/forward stack on
// `BrowserState`; routing it into JS land is bigger than the rest of
// the stub work, so for now `length` is fixed at 1, `state` is null,
// and the mutators silently accept their args. Plenty of client-side
// routers call `pushState` during initialisation; without the shim
// they'd throw "history.pushState is not a function".
fn register_history(ctx: &Ctx<'_>) -> Result<()> {
    let history = Object::new(ctx.clone())?;
    history.set("length", 1)?;
    history.set("state", Value::new_null(ctx.clone()))?;
    history.set("pushState", Func::from(|_args: Rest<Value<'_>>| {}))?;
    history.set("replaceState", Func::from(|_args: Rest<Value<'_>>| {}))?;
    history.set("back", Func::from(|_args: Rest<Value<'_>>| {}))?;
    history.set("forward", Func::from(|_args: Rest<Value<'_>>| {}))?;
    history.set("go", Func::from(|_args: Rest<Value<'_>>| {}))?;
    ctx.globals().set("history", history)?;
    Ok(())
}

// Default-port-aware host serialisation: an http:80 / https:443 URL
// drops the port, anything else keeps it. Matches the WHATWG URL spec's
// `host` property.
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
// query string and fragment, so we slice them apart at the first `?`
// and `#` matching what the WHATWG URL accessors return.
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
