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

use rquickjs::{Ctx, Object, Result, prelude::Func};

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

    // Window-level addEventListener / removeEventListener register real
    // handlers under the 'window' key in the shared event registry
    // (event.rs). The registry functions are resolved at call time, so it
    // doesn't matter that event.rs registers after this module. `load`,
    // `popstate`, and `hashchange` are dispatched here via
    // `__mb_dispatch_global`.
    ctx.eval::<(), _>(
        r#"
        globalThis.addEventListener = function (type, fn) {
            if (typeof fn === 'function') {
                globalThis.__mb_listener_add('window', String(type), fn);
            }
        };
        globalThis.removeEventListener = function (type, fn) {
            if (typeof fn === 'function') {
                globalThis.__mb_listener_remove('window', String(type), fn);
            }
        };
        "#,
    )?;

    // queueMicrotask(fn): equivalent to `Promise.resolve().then(fn)` per
    // the WHATWG HTML spec. We define the shim entirely in JS — it
    // piggy-backs on rquickjs' microtask queue (drained from
    // `JsRuntime::drain_pending_jobs`) and throws TypeError for
    // non-callable arguments to match real browsers.
    //
    // The engine's own `Promise` is bound here, once: pages replace
    // `globalThis.Promise` (core-js does when it distrusts the native one)
    // and then schedule that polyfill's reactions *through*
    // `queueMicrotask` — looking `Promise` up at call time made the two
    // feed each other until the stack ran out (seen inside core-js on
    // hani, bbc and daum in the compat round 3 corpus).
    let qmt_src = r#"
        (function () {
            var resolved = Promise.resolve();
            var then = Promise.prototype.then;
            globalThis.queueMicrotask = function (cb) {
                if (typeof cb !== 'function') {
                    throw new TypeError('queueMicrotask: argument must be a function');
                }
                then.call(resolved, cb);
            };
        })();
    "#;
    ctx.eval::<(), _>(qmt_src)?;

    register_navigator(ctx)?;
    register_location(ctx, location_url.clone())?;
    register_history(ctx, location_url)?;

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

// `history` global — a real same-document history stack living JS-side.
// `pushState` / `replaceState` update `location.*` *without* re-fetching
// (they only rewrite the shared URL buffer via `__mb_loc_set`), push/
// replace the entry stack, and `back` / `forward` / `go` move the index
// and fire a `popstate` event on `window`. This is the same-document
// half of routing; cross-document navigation (a real load) is still the
// chrome's job via `BrowserState`.
//
// `__mb_loc_set(url)` resolves `url` against the current buffer, writes
// it back, and returns the resolved absolute href so the stack can store
// it (and replay it on back/forward).
fn register_history(ctx: &Ctx<'_>, location_url: Rc<RefCell<String>>) -> Result<()> {
    let buf = location_url;
    ctx.globals().set(
        "__mb_loc_set",
        Func::from(move |next: String| -> String {
            let current = buf.borrow().clone();
            let resolved = resolve_location(&current, &next);
            // Same-document history entries can't change the origin (the
            // spec throws SecurityError). Beyond correctness this guards
            // the agent: the browser mirrors this URL into the trusted
            // `url:` line, so a page must not be able to claim another
            // site's address. "" tells the JS caller to throw.
            if !current.is_empty() && !same_origin(&current, &resolved) {
                return String::new();
            }
            *buf.borrow_mut() = resolved.clone();
            resolved
        }),
    )?;
    ctx.eval::<(), _>(HISTORY_BOOT)?;
    Ok(())
}

const HISTORY_BOOT: &str = r#"
(function () {
    // entries[index] is the current entry. Each: { state, url }.
    var entries = [{ state: null, url: null }];
    var index = 0;

    var history = {};
    Object.defineProperty(history, 'length', {
        get: function () { return entries.length; },
        configurable: true, enumerable: true,
    });
    Object.defineProperty(history, 'state', {
        get: function () { return entries[index].state; },
        configurable: true, enumerable: true,
    });
    Object.defineProperty(history, 'scrollRestoration', {
        value: 'auto', writable: true, configurable: true, enumerable: true,
    });

    function normState(s) { return s === undefined ? null : s; }
    function hashOf(href) { var i = href.indexOf('#'); return i < 0 ? '' : href.slice(i); }
    function withoutHash(href) { var i = href.indexOf('#'); return i < 0 ? href : href.slice(0, i); }

    // Resolve + write the location buffer, or throw like browsers do when
    // a same-document entry would change the origin.
    function moveTo(url, method) {
        var resolved = globalThis.__mb_loc_set(String(url));
        if (resolved === '') {
            var err = new Error("Failed to execute '" + method + "' on 'History': " +
                "a history state object with URL '" + url + "' cannot be created " +
                "in a document with a different origin.");
            err.name = 'SecurityError';
            throw err;
        }
        return resolved;
    }

    // The first entry's URL is bound after this module boots (the host
    // calls set_location_url post-construction), so capture the live href
    // onto the current entry before leaving it — otherwise back() to
    // entry 0 would have no URL to restore.
    function bindCurrentUrl() {
        if (entries[index].url == null) entries[index].url = globalThis.location.href;
    }

    function push(state, resolved) {
        // Drop any forward entries, then append.
        entries.length = index + 1;
        entries.push({ state: normState(state), url: resolved });
        index = entries.length - 1;
    }

    history.pushState = function (state, title, url) {
        bindCurrentUrl();
        var resolved = (url == null) ? globalThis.location.href : moveTo(url, 'pushState');
        push(state, resolved);
    };

    history.replaceState = function (state, title, url) {
        var resolved = (url == null) ? globalThis.location.href : moveTo(url, 'replaceState');
        entries[index] = { state: normState(state), url: resolved };
    };

    // Returns whether it moved. Traversing between entries that differ
    // only in the fragment also fires hashchange (after popstate, as
    // browsers do).
    function go(delta) {
        delta = delta | 0;
        if (delta === 0) return false;
        var target = index + delta;
        if (target < 0 || target >= entries.length) return false;
        bindCurrentUrl();
        var from = globalThis.location.href;
        index = target;
        var entry = entries[index];
        if (entry.url != null) globalThis.__mb_loc_set(entry.url);
        var to = globalThis.location.href;
        globalThis.__mb_dispatch_global('window', 'popstate', { state: entry.state });
        if (withoutHash(from) === withoutHash(to) && hashOf(from) !== hashOf(to)) {
            globalThis.__mb_dispatch_global('window', 'hashchange', { oldURL: from, newURL: to });
        }
        return true;
    }
    history.go = function (delta) { go(delta); };
    history.back = function () { go(-1); };
    history.forward = function () { go(1); };

    globalThis.history = history;

    // Host hooks (BrowserState): the chrome's back/forward walk these
    // same-document entries before leaving the document, and a link to
    // `#fragment` of the current document becomes a new entry here
    // instead of a reload.
    function hostHook(name, fn) {
        Object.defineProperty(globalThis, name, { value: fn, enumerable: false });
    }
    hostHook('__mb_history_traverse', go);
    hostHook('__mb_history_can_traverse', function (delta) {
        var target = index + (delta | 0);
        return delta !== 0 && target >= 0 && target < entries.length;
    });
    hostHook('__mb_fragment_navigate', function (href) {
        bindCurrentUrl();
        var from = globalThis.location.href;
        var to = globalThis.__mb_loc_set(String(href));
        if (to === '') return;
        push(null, to);
        globalThis.__mb_dispatch_global('window', 'popstate', { state: null });
        globalThis.__mb_dispatch_global('window', 'hashchange', { oldURL: from, newURL: to });
    });
})();
"#;

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

// Resolve a `pushState`/`replaceState` URL argument against the current
// location href, returning a new absolute href. Pure string surgery (no
// port-default guessing) so it round-trips losslessly through the
// location getters.
fn resolve_location(current: &str, next: &str) -> String {
    if next.contains("://") {
        return next.to_string();
    }
    let Some(origin) = origin_of(current) else {
        // No base URL bound yet — best effort, store as-is.
        return next.to_string();
    };
    if let Some(authority_and_path) = next.strip_prefix("//") {
        // Scheme-relative: keeps only the current scheme.
        let scheme = &origin[..origin.find("://").unwrap_or(0)];
        return format!("{scheme}://{authority_and_path}");
    }
    let rest = &current[origin.len()..]; // "/path?q#h" or ""
    let (cur_path, cur_search, _cur_hash) = split_path_search_hash(rest);
    if next.starts_with('/') {
        format!("{origin}{next}")
    } else if next.starts_with('?') {
        format!("{origin}{cur_path}{next}")
    } else if next.starts_with('#') {
        format!("{origin}{cur_path}{cur_search}{next}")
    } else {
        // Relative to the current directory.
        let dir = match cur_path.rfind('/') {
            Some(i) => &cur_path[..=i],
            None => "/",
        };
        format!("{origin}{dir}{next}")
    }
}

// Compares the `scheme://authority` prefixes, case-insensitively.
fn same_origin(a: &str, b: &str) -> bool {
    match (origin_of(a), origin_of(b)) {
        (Some(x), Some(y)) => x.eq_ignore_ascii_case(y),
        _ => false,
    }
}

// The `scheme://authority` prefix of an href, or None if there's no
// scheme. Everything from the first '/' after the authority is the
// path/search/hash.
fn origin_of(href: &str) -> Option<&str> {
    let after_scheme = href.find("://")? + 3;
    match href[after_scheme..].find('/') {
        Some(i) => Some(&href[..after_scheme + i]),
        None => Some(href),
    }
}
