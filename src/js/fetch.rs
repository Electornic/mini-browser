// Browser-style `fetch(url, init?)` global. Wraps `crate::net` which
// is synchronous in a Promise so the JS surface mirrors a real browser:
// `fetch(url).then(r => r.text())` and `await fetch(url)` both work.
//
// The async-ness is approximated — we run the HTTP exchange on the main
// thread inside the `fetch()` call and resolve the returned Promise
// with an already-built Response. That blocks the UI for the duration
// of a request, which is fine for a toy and avoids the worker-pool
// machinery a real engine maintains.
//
// `init` (second argument) is an optional object. The toy understands
// three of its WHATWG-spec fields:
//   - `method` (string)  — defaults to "GET"
//   - `headers` (object) — plain `{name: value}` map; the Headers
//     class is not implemented, but real-world callers
//     (`fetch(u, { headers: { 'X-Foo': 'bar' } })`) hand in a plain
//     object literal anyway, which round-trips through here.
//   - `body` (string)    — sent verbatim, with a `Content-Length`
//     header tacked on by the network layer. Blob/FormData/URLSearchParams
//     are not implemented yet.
//
// Errors (URL parse failure, network/TLS failure, redirect-limit) reject
// the Promise as a TypeError so the standard `try { await fetch(...) }
// catch (e) { ... }` pattern catches them. A non-2xx HTTP status is NOT
// an error — it lands as a Response with `ok = false`, matching the spec.
//
// Response surface (`Response`):
//   - `ok` (bool)        — true iff status is 200..299
//   - `status` (number)  — HTTP status code
//   - `statusText` (str) — HTTP reason phrase
//   - `url` (str)        — the final URL after any redirects
//   - `text()`           — Promise<string> body
//   - `json()`           — Promise<any> body parsed via JSON.parse

use boa_engine::{
    Context, JsError, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction,
    js_string,
    object::{ObjectInitializer, builtins::JsPromise},
    property::{Attribute, PropertyKey},
};

use crate::net::{self, FetchResult};

use super::util::first_arg_as_string;

pub(super) fn register_fetch(context: &mut Context) {
    let _ = context.register_global_builtin_callable(
        js_string!("fetch"),
        2,
        NativeFunction::from_fn_ptr(fetch_global),
    );
}

// `fetch(url, init?)` — sync HTTP exchange wrapped in a Promise. Always
// returns a Promise: success branches resolve with a Response; URL
// parse / network failures reject with a TypeError so callers can
// `.catch(...)` or `try { await ... } catch`.
fn fetch_global(
    _this: &JsValue,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let url_string = first_arg_as_string(args, context)?;

    let url = match net::Url::parse(&url_string) {
        Ok(u) => u,
        Err(parse_err) => {
            let err = JsError::from_native(
                JsNativeError::typ()
                    .with_message(format!("fetch: invalid URL: {parse_err:?}")),
            );
            return Ok(JsValue::from(JsPromise::reject(err, context)));
        }
    };

    // Parse the optional `init` object once, up front. A bad `init`
    // (non-object, non-stringifiable header value, etc.) propagates
    // synchronously — the spec also rejects these synchronously
    // rather than as a Promise rejection.
    let request_init = match parse_request_init(args.get(1), context) {
        Ok(init) => init,
        Err(err) => return Ok(JsValue::from(JsPromise::reject(err, context))),
    };

    match net::fetch_with_request(
        &url,
        &request_init.method,
        &request_init.headers,
        &request_init.body,
    ) {
        Ok(result) => {
            let response = build_response_object(result, context);
            Ok(JsValue::from(JsPromise::resolve(JsValue::from(response), context)))
        }
        Err(net_err) => {
            let err = JsError::from_native(
                JsNativeError::typ()
                    .with_message(format!("fetch: network error: {net_err:?}")),
            );
            Ok(JsValue::from(JsPromise::reject(err, context)))
        }
    }
}

// Decoded `init` argument for the network layer. Keeping the parsed
// shape behind a struct (instead of three separate locals) makes the
// fetch_global body read top-down and keeps the spec field names
// visible at the call site.
struct RequestInit {
    method: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

// Pulls the spec-significant fields off the `init` argument. A
// missing / undefined / null `init` collapses to a plain GET with no
// body and no extra headers — same default a single-arg fetch would
// have produced. Anything else must be an Object; primitives raise
// TypeError so callers don't accidentally send a stringified number.
fn parse_request_init(init: Option<&JsValue>, context: &mut Context) -> JsResult<RequestInit> {
    let mut parsed = RequestInit {
        method: "GET".to_string(),
        headers: Vec::new(),
        body: Vec::new(),
    };
    let Some(init_value) = init else {
        return Ok(parsed);
    };
    if init_value.is_undefined() || init_value.is_null() {
        return Ok(parsed);
    }
    let Some(init_obj) = init_value.as_object() else {
        return Err(JsError::from_native(
            JsNativeError::typ().with_message("fetch: init argument must be an object"),
        ));
    };

    // method is read first; an upper-cased copy keeps the toy's
    // outgoing request line consistent regardless of how the JS
    // caller spelled it (`'post'`, `'POST'`, `'PoSt'` all become
    // `POST`). Empty / undefined falls back to GET.
    let method_value = init_obj.get(js_string!("method"), context)?;
    if !method_value.is_undefined() && !method_value.is_null() {
        parsed.method = method_value
            .to_string(context)?
            .to_std_string_escaped()
            .to_uppercase();
    }

    let headers_value = init_obj.get(js_string!("headers"), context)?;
    if !headers_value.is_undefined() && !headers_value.is_null() {
        let Some(headers_obj) = headers_value.as_object() else {
            return Err(JsError::from_native(
                JsNativeError::typ().with_message("fetch: headers must be an object"),
            ));
        };
        // Walk own keys in source order so handlers that care about
        // ordering (some servers do) see the same sequence the JS
        // author wrote. Symbol keys are skipped — HTTP header names
        // are strings.
        for key in headers_obj.own_property_keys(context)? {
            let name = match &key {
                PropertyKey::String(s) => s.to_std_string_escaped(),
                PropertyKey::Index(n) => n.get().to_string(),
                PropertyKey::Symbol(_) => continue,
            };
            let value = headers_obj
                .get(key, context)?
                .to_string(context)?
                .to_std_string_escaped();
            parsed.headers.push((name, value));
        }
    }

    let body_value = init_obj.get(js_string!("body"), context)?;
    if !body_value.is_undefined() && !body_value.is_null() {
        parsed.body = body_value
            .to_string(context)?
            .to_std_string_escaped()
            .into_bytes();
    }

    Ok(parsed)
}

// Builds the Response wrapper with the per-fetch metadata locked in
// via `move` closures. The body Vec<u8> is materialised into UTF-8
// once (lossy on invalid sequences) and cloned into both text() and
// json() closures so a caller can read whichever they prefer without
// the other consuming the buffer.
fn build_response_object(result: FetchResult, context: &mut Context) -> JsObject {
    let body_text = String::from_utf8_lossy(&result.response.body).into_owned();
    let status = f64::from(result.response.status_code);
    let ok = (200..300).contains(&result.response.status_code);
    let url_str = result.final_url.to_string();
    let status_text = result.response.reason_phrase.clone();

    let body_for_text = body_text.clone();
    let text_fn = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            // text() always succeeds (we already lossy-decoded UTF-8
            // when constructing the Response) and returns the body
            // wrapped in a resolved Promise.
            let value = JsValue::from(JsString::from(body_for_text.as_str()));
            Ok(JsValue::from(JsPromise::resolve(value, ctx)))
        })
    };

    let body_for_json = body_text;
    let json_fn = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            // Delegate to `JSON.parse` so we get the same TypeError
            // behaviour real `Response.json()` exposes: invalid JSON
            // rejects the Promise; valid JSON resolves with the value.
            let body_value = JsValue::from(JsString::from(body_for_json.as_str()));
            match call_json_parse(body_value, ctx) {
                Ok(parsed) => Ok(JsValue::from(JsPromise::resolve(parsed, ctx))),
                Err(parse_err) => Ok(JsValue::from(JsPromise::reject(parse_err, ctx))),
            }
        })
    };

    ObjectInitializer::new(context)
        .property(js_string!("ok"), JsValue::from(ok), Attribute::all())
        .property(
            js_string!("status"),
            JsValue::from(status),
            Attribute::all(),
        )
        .property(
            js_string!("statusText"),
            JsString::from(status_text.as_str()),
            Attribute::all(),
        )
        .property(
            js_string!("url"),
            JsString::from(url_str.as_str()),
            Attribute::all(),
        )
        .function(text_fn, js_string!("text"), 0)
        .function(json_fn, js_string!("json"), 0)
        .build()
}

// Calls the global `JSON.parse(body)` from Rust. Going through the
// real built-in (rather than serde_json) means the parsed value uses
// Boa's own object/array/number representations, which are what
// Response.json() callers expect to receive on the JS side.
fn call_json_parse(body: JsValue, context: &mut Context) -> JsResult<JsValue> {
    let global = context.global_object();
    let json_obj_value = global.get(js_string!("JSON"), context)?;
    let json_obj = json_obj_value.as_object().ok_or_else(|| {
        JsError::from_native(JsNativeError::typ().with_message("JSON global is not an object"))
    })?;
    let parse_value = json_obj.get(js_string!("parse"), context)?;
    let parse_callable = parse_value.as_callable().ok_or_else(|| {
        JsError::from_native(JsNativeError::typ().with_message("JSON.parse is not callable"))
    })?;
    parse_callable.call(&JsValue::undefined(), &[body], context)
}
