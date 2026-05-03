// Synchronous-under-the-hood `XMLHttpRequest` global. The toy keeps the
// public surface jQuery-shaped — `new XMLHttpRequest()`, `open`,
// `setRequestHeader`, `send`, `onreadystatechange` / `onload` / `onerror`
// — but performs the HTTP exchange inline inside `send()`. Once `send`
// returns, the readyState transitions (HEADERS_RECEIVED → LOADING → DONE)
// have already happened and every event has fired. That's enough for
// every $.ajax-style caller, since they all read `xhr.responseText` /
// `xhr.status` from the DONE listener — they never observe the
// intermediate states asynchronously.
//
// State lives on a per-instance `Rc<RefCell<XhrInner>>` shared between
// every accessor and method closure. Listener registration uses an
// instance-local map (separate from the DOM listener registry on
// JsRuntime, which is keyed by NodeId).
//
// Errors that block the request before the network round-trip
// (`open` with an invalid URL, `setRequestHeader`/`send` called out of
// the OPENED state) throw synchronously. A failed network exchange
// surfaces as readyState=DONE plus an `error` event — `status=0`,
// `responseText=""`, no `load` event — matching what the spec calls
// the "request error" steps.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    object::ObjectInitializer,
    property::Attribute,
};

use crate::net;

use super::util::{first_arg_as_string, nth_arg_as_string};

// readyState values, mirroring the WHATWG XHR constants exposed on every
// instance. Production JS uses both literal numbers and `xhr.DONE` —
// we expose them as own properties on the instance so either form works.
const READY_STATE_UNSENT: u32 = 0;
const READY_STATE_OPENED: u32 = 1;
const READY_STATE_HEADERS_RECEIVED: u32 = 2;
const READY_STATE_LOADING: u32 = 3;
const READY_STATE_DONE: u32 = 4;

// Per-instance backing store. Every XHR accessor and method closure
// captures a clone of the same Rc, so reads and writes from any entry
// point observe the same buffer.
#[derive(Default)]
struct XhrInner {
    method: String,
    url: Option<net::Url>,
    request_headers: Vec<(String, String)>,
    ready_state: u32,
    status: u16,
    status_text: String,
    response_text: String,
    response_url: String,
    response_headers: Vec<(String, String)>,
    // Listeners registered via `addEventListener`. Property-style
    // handlers (`onload`, `onreadystatechange`, …) live on the JS
    // object itself; `fire_event` reads them off the `this` reference
    // it gets passed at dispatch time.
    listeners: HashMap<String, Vec<JsObject>>,
}

pub(super) fn register_xmlhttprequest(context: &mut Context) {
    // Constructor (length=0; spec says zero formal parameters).
    // `register_global_callable` marks the function as `[[Construct]]`-able
    // so `new XMLHttpRequest()` works the way every $.ajax-shaped library
    // expects. Calling it without `new` also returns an instance — the toy
    // doesn't enforce the "must be invoked as constructor" check the spec
    // mandates, since real callers always use `new` and the no-new path
    // is harmless either way.
    let _ = context.register_global_callable(
        js_string!("XMLHttpRequest"),
        0,
        NativeFunction::from_fn_ptr(xhr_constructor),
    );
}

fn xhr_constructor(
    _this: &JsValue,
    _args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let state = Rc::new(RefCell::new(XhrInner::default()));
    Ok(JsValue::from(build_xhr_object(state, context)))
}

fn build_xhr_object(state: Rc<RefCell<XhrInner>>, context: &mut Context) -> JsObject {
    // -- read-only accessors ------------------------------------------------
    let s = state.clone();
    let ready_state_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            Ok(JsValue::from(s.borrow().ready_state))
        })
    }
    .to_js_function(context.realm());

    let s = state.clone();
    let status_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            Ok(JsValue::from(u32::from(s.borrow().status)))
        })
    }
    .to_js_function(context.realm());

    let s = state.clone();
    let status_text_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let v = s.borrow().status_text.clone();
            Ok(JsValue::from(JsString::from(v.as_str())))
        })
    }
    .to_js_function(context.realm());

    let s = state.clone();
    let response_text_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let v = s.borrow().response_text.clone();
            Ok(JsValue::from(JsString::from(v.as_str())))
        })
    }
    .to_js_function(context.realm());

    let s = state.clone();
    let response_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            // Default `responseType=""` returns the body as a string;
            // the toy doesn't implement arraybuffer/json/blob types,
            // so `response` and `responseText` always agree.
            let v = s.borrow().response_text.clone();
            Ok(JsValue::from(JsString::from(v.as_str())))
        })
    }
    .to_js_function(context.realm());

    let s = state.clone();
    let response_url_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let v = s.borrow().response_url.clone();
            Ok(JsValue::from(JsString::from(v.as_str())))
        })
    }
    .to_js_function(context.realm());

    // -- methods ------------------------------------------------------------
    // open(method, url[, async, user, password])
    // Async/user/password are accepted but ignored — the toy is sync, and
    // basic-auth round-trips aren't worth the credential-cache machinery.
    let s = state.clone();
    let open_fn = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let method = first_arg_as_string(args, ctx)?.to_uppercase();
            let url_str = nth_arg_as_string(args, 1, ctx)?;
            let url = net::Url::parse(&url_str).map_err(|err| {
                JsError::from_native(JsNativeError::typ().with_message(format!(
                    "XMLHttpRequest.open: invalid URL: {err:?}"
                )))
            })?;
            let mut st = s.borrow_mut();
            // open() resets request-side fields per spec — repeated calls
            // on the same instance are legal, with later open() winning.
            // Response-side fields stay zeroed at default; they'll be
            // populated by the next send() if any.
            st.method = method;
            st.url = Some(url);
            st.request_headers.clear();
            st.ready_state = READY_STATE_OPENED;
            Ok(JsValue::undefined())
        })
    };

    // setRequestHeader(name, value)
    // Spec is more elaborate (combines duplicates, blocks forbidden
    // header names, normalizes whitespace) — the toy just records the
    // pair so the network layer can emit it verbatim.
    let s = state.clone();
    let set_request_header = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let name = first_arg_as_string(args, ctx)?;
            let value = nth_arg_as_string(args, 1, ctx)?;
            let mut st = s.borrow_mut();
            if st.ready_state != READY_STATE_OPENED {
                return Err(JsError::from_native(
                    JsNativeError::typ().with_message(
                        "XMLHttpRequest.setRequestHeader: state must be OPENED",
                    ),
                ));
            }
            st.request_headers.push((name, value));
            Ok(JsValue::undefined())
        })
    };

    // send([body])
    // Performs the HTTP exchange synchronously, then fires the
    // readystatechange / load / error events before returning. Throwing
    // is reserved for state errors (open() not called, double-send) —
    // network failures land as a DONE state plus an error event so
    // `xhr.onerror` callers see them through the normal channel.
    let s = state.clone();
    let send_fn = unsafe {
        NativeFunction::from_closure(move |this, args, ctx| {
            let (url, method, headers, body) = {
                let st = s.borrow();
                if st.ready_state != READY_STATE_OPENED {
                    return Err(JsError::from_native(
                        JsNativeError::typ()
                            .with_message("XMLHttpRequest.send: state must be OPENED"),
                    ));
                }
                let url = st.url.clone().ok_or_else(|| {
                    JsError::from_native(
                        JsNativeError::typ()
                            .with_message("XMLHttpRequest.send: open() not called"),
                    )
                })?;
                let body_bytes = match args.first() {
                    Some(arg) if !arg.is_undefined() && !arg.is_null() => {
                        arg.to_string(ctx)?.to_std_string_escaped().into_bytes()
                    }
                    _ => Vec::new(),
                };
                (
                    url,
                    st.method.clone(),
                    st.request_headers.clone(),
                    body_bytes,
                )
            };

            match net::fetch_with_request(&url, &method, &headers, &body) {
                Ok(fetch_result) => {
                    {
                        let mut st = s.borrow_mut();
                        st.status = fetch_result.response.status_code;
                        st.status_text = fetch_result.response.reason_phrase.clone();
                        st.response_text =
                            String::from_utf8_lossy(&fetch_result.response.body).into_owned();
                        st.response_headers = fetch_result.response.headers.clone();
                        st.response_url = fetch_result.final_url.to_string();
                    }
                    set_state_and_fire(s.clone(), this, READY_STATE_HEADERS_RECEIVED, ctx)?;
                    set_state_and_fire(s.clone(), this, READY_STATE_LOADING, ctx)?;
                    set_state_and_fire(s.clone(), this, READY_STATE_DONE, ctx)?;
                    fire_event(s.clone(), this, "load", ctx)?;
                }
                Err(_net_err) => {
                    // Spec "request error" steps: status stays 0, response
                    // empty, jump straight to DONE then fire error. We
                    // skip the intermediate readystatechange transitions
                    // because no headers were ever received.
                    {
                        let mut st = s.borrow_mut();
                        st.status = 0;
                        st.status_text.clear();
                        st.response_text.clear();
                        st.response_headers.clear();
                    }
                    set_state_and_fire(s.clone(), this, READY_STATE_DONE, ctx)?;
                    fire_event(s.clone(), this, "error", ctx)?;
                }
            }

            Ok(JsValue::undefined())
        })
    };

    // abort() — toy stub. The spec fires `abort` and resets state, but
    // since `send()` is synchronous in this engine there is never a
    // request in flight to abort: by the time JS could call abort(),
    // send() has already returned. Implementing it as a no-op keeps
    // the call shape compatible without spending complexity on a
    // never-hit code path.
    let abort_fn = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| Ok(JsValue::undefined()))
    };

    // getResponseHeader(name) — case-insensitive lookup, null when missing.
    let s = state.clone();
    let get_response_header = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let needle = first_arg_as_string(args, ctx)?.to_ascii_lowercase();
            let st = s.borrow();
            for (name, value) in &st.response_headers {
                if name.eq_ignore_ascii_case(&needle) {
                    return Ok(JsValue::from(JsString::from(value.as_str())));
                }
            }
            Ok(JsValue::null())
        })
    };

    // getAllResponseHeaders() — `name: value\r\n` per header. Empty
    // string before send(), or after a network error. Real browsers
    // sort and lower-case header names; the toy preserves the order
    // and casing the server sent.
    let s = state.clone();
    let get_all_response_headers = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let st = s.borrow();
            let mut buf = String::new();
            for (name, value) in &st.response_headers {
                buf.push_str(name);
                buf.push_str(": ");
                buf.push_str(value);
                buf.push_str("\r\n");
            }
            Ok(JsValue::from(JsString::from(buf.as_str())))
        })
    };

    // addEventListener(type, fn) / removeEventListener(type, fn)
    // Same dedup-by-identity contract the DOM addEventListener uses,
    // so two distinct function literals with the same body still count
    // as two separate listeners.
    let s = state.clone();
    let add_event_listener = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let event_type = first_arg_as_string(args, ctx)?;
            let handler_obj = args
                .get(1)
                .and_then(|v| v.as_object())
                .filter(|o| o.is_callable())
                .ok_or_else(|| {
                    JsError::from_native(JsNativeError::typ().with_message(
                        "XMLHttpRequest.addEventListener: handler must be a function",
                    ))
                })?;
            let mut st = s.borrow_mut();
            let entry = st.listeners.entry(event_type).or_default();
            if !entry
                .iter()
                .any(|existing| JsObject::equals(existing, &handler_obj))
            {
                entry.push(handler_obj);
            }
            Ok(JsValue::undefined())
        })
    };

    let s = state.clone();
    let remove_event_listener = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let event_type = first_arg_as_string(args, ctx)?;
            let Some(handler_obj) = args.get(1).and_then(|v| v.as_object()) else {
                return Ok(JsValue::undefined());
            };
            let mut st = s.borrow_mut();
            if let Some(entry) = st.listeners.get_mut(&event_type) {
                entry.retain(|existing| !JsObject::equals(existing, &handler_obj));
            }
            Ok(JsValue::undefined())
        })
    };

    ObjectInitializer::new(context)
        .accessor(
            js_string!("readyState"),
            Some(ready_state_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("status"),
            Some(status_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("statusText"),
            Some(status_text_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("responseText"),
            Some(response_text_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("response"),
            Some(response_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("responseURL"),
            Some(response_url_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .property(
            js_string!("UNSENT"),
            JsValue::from(READY_STATE_UNSENT),
            Attribute::all(),
        )
        .property(
            js_string!("OPENED"),
            JsValue::from(READY_STATE_OPENED),
            Attribute::all(),
        )
        .property(
            js_string!("HEADERS_RECEIVED"),
            JsValue::from(READY_STATE_HEADERS_RECEIVED),
            Attribute::all(),
        )
        .property(
            js_string!("LOADING"),
            JsValue::from(READY_STATE_LOADING),
            Attribute::all(),
        )
        .property(
            js_string!("DONE"),
            JsValue::from(READY_STATE_DONE),
            Attribute::all(),
        )
        .function(open_fn, js_string!("open"), 2)
        .function(set_request_header, js_string!("setRequestHeader"), 2)
        .function(send_fn, js_string!("send"), 1)
        .function(abort_fn, js_string!("abort"), 0)
        .function(get_response_header, js_string!("getResponseHeader"), 1)
        .function(
            get_all_response_headers,
            js_string!("getAllResponseHeaders"),
            0,
        )
        .function(add_event_listener, js_string!("addEventListener"), 2)
        .function(remove_event_listener, js_string!("removeEventListener"), 2)
        .build()
}

// Bump readyState and immediately fire `readystatechange`. The two
// always travel together: every state transition triggers a
// readystatechange event, so wrapping the pair stops the send()
// flow from open-coding it three times.
fn set_state_and_fire(
    state: Rc<RefCell<XhrInner>>,
    this: &JsValue,
    new_state: u32,
    context: &mut Context,
) -> JsResult<()> {
    state.borrow_mut().ready_state = new_state;
    fire_event(state, this, "readystatechange", context)
}

// Build a minimal Event object and dispatch it through both delivery
// channels: property-style handlers (`xhr.onload = …`, registered via
// JS property assignment on the wrapper) and listener-array handlers
// (registered via `addEventListener`). Property-style fires first to
// match the order browsers use; ordering matters for `onload` chains
// that depend on a state set earlier in the same handler list.
//
// The Event carries `type`, `target`, and `currentTarget` set to the
// xhr instance — enough for callers that read `e.target.responseText`
// (the canonical jQuery readiness check). progressEvent fields
// (`loaded`, `total`, `lengthComputable`) are omitted; the toy fires
// a single `load`, no progress events.
fn fire_event(
    state: Rc<RefCell<XhrInner>>,
    this: &JsValue,
    event_type: &str,
    context: &mut Context,
) -> JsResult<()> {
    let event = ObjectInitializer::new(context)
        .property(
            js_string!("type"),
            JsString::from(event_type),
            Attribute::all(),
        )
        .property(js_string!("target"), this.clone(), Attribute::all())
        .property(
            js_string!("currentTarget"),
            this.clone(),
            Attribute::all(),
        )
        .build();
    let event_value = JsValue::from(event);

    // Property-style handler: `xhr.onload`, `xhr.onreadystatechange`, …
    let prop_name = format!("on{event_type}");
    if let Some(this_obj) = this.as_object() {
        let handler_value = this_obj.get(JsString::from(prop_name.as_str()), context)?;
        if let Some(handler_obj) = handler_value.as_object().filter(|o| o.is_callable()) {
            let _ = handler_obj
                .call(this, std::slice::from_ref(&event_value), context)
                .inspect_err(|err| {
                    eprintln!("[xhr] on{event_type} handler error: {err}")
                });
        }
    }

    // addEventListener-registered handlers, in registration order. We
    // snapshot before dispatching so a handler that calls
    // removeEventListener on itself doesn't shorten the slice we're
    // walking — same convention the DOM dispatcher uses.
    let snapshot: Vec<JsObject> = state
        .borrow()
        .listeners
        .get(event_type)
        .cloned()
        .unwrap_or_default();
    for handler in snapshot {
        if let Err(err) = handler.call(this, std::slice::from_ref(&event_value), context) {
            eprintln!("[xhr] {event_type} listener error: {err}");
        }
    }

    Ok(())
}
