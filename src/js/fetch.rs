// Browser-style `fetch(url)` global. Wraps `crate::net::fetch`, which is
// synchronous, in a Promise so the JS surface mirrors a real browser:
// `fetch(url).then(r => r.text())` and `await fetch(url)` both work.
//
// The async-ness is approximated — we run the HTTP exchange on the main
// thread inside the `fetch()` call and resolve the returned Promise
// with an already-built Response. That blocks the UI for the duration
// of a request, which is fine for a toy and avoids the worker-pool
// machinery a real engine maintains.
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
    property::Attribute,
};

use crate::net::{self, FetchResult};

use super::util::first_arg_as_string;

pub(super) fn register_fetch(context: &mut Context) {
    let _ = context.register_global_builtin_callable(
        js_string!("fetch"),
        1,
        NativeFunction::from_fn_ptr(fetch_global),
    );
}

// `fetch(url)` — sync HTTP exchange wrapped in a Promise. Always
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

    match net::fetch(&url) {
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
