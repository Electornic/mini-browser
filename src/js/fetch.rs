// Browser-style `fetch(url, init?)` global. Wraps `crate::net` (which
// is synchronous) inside a JS-side `Promise` so the surface mirrors a
// real browser: `fetch(url).then(r => r.text())` and `await fetch(url)`
// both work.
//
// Async-ness is approximated — the HTTP exchange runs on the main
// thread inside the call and we resolve the returned Promise with an
// already-built Response. That blocks the UI for the duration of a
// request, fine for a toy and the same trade-off the boa version made.
//
// Implementation split:
//   - Rust hook `__mb_fetch_sync(url, method, headers_flat, body)`
//     does the actual round-trip via `net::fetch_with_request`. Throws
//     on URL parse failure / network failure (rquickjs converts the
//     Err to a JS exception that the JS-side `try/catch` catches).
//     Returns a 6-tuple (rquickjs IntoJs maps it to a JS Array):
//     `(ok, status, statusText, finalUrl, body, headers_flat)`.
//   - JS-side `fetch` global: parses init, calls the hook, wraps the
//     tuple into a Response object with `.text()` / `.json()` Promise-
//     returning methods. Errors from the hook propagate through
//     `Promise.reject`.

use rquickjs::{Ctx, Exception, Result, convert::List, prelude::Func};

use crate::net;

// rquickjs IntoJs is implemented for `List<(...)>` (which surfaces as a
// JS Array), not for plain tuples — wrap the 6-field result through
// `List` so the JS wrapper can destructure positionally.
type FetchTuple = List<(bool, u16, String, String, String, Vec<String>)>;

pub(super) fn register_fetch(ctx: &Ctx<'_>) -> Result<()> {
    ctx.globals().set(
        "__mb_fetch_sync",
        Func::from(
            move |ctx: Ctx<'_>,
                  url: String,
                  method: String,
                  headers_flat: Vec<String>,
                  body: String|
                  -> Result<FetchTuple> {
                let parsed_url = net::Url::parse(&url).map_err(|err| {
                    Exception::throw_type(&ctx, &format!("fetch: invalid URL: {err:?}"))
                })?;
                let headers = pair_up(&headers_flat);
                let result = net::fetch_with_request(
                    &parsed_url,
                    &method,
                    &headers,
                    body.as_bytes(),
                )
                .map_err(|err| {
                    Exception::throw_type(&ctx, &format!("fetch: network error: {err:?}"))
                })?;
                let ok = (200..300).contains(&result.response.status_code);
                let status_text = result.response.reason_phrase.clone();
                let response_text = String::from_utf8_lossy(&result.response.body).into_owned();
                let final_url = result.final_url.to_string();
                let response_headers_flat: Vec<String> = result
                    .response
                    .headers
                    .iter()
                    .flat_map(|(n, v)| [n.clone(), v.clone()])
                    .collect();
                Ok(List((
                    ok,
                    result.response.status_code,
                    status_text,
                    final_url,
                    response_text,
                    response_headers_flat,
                )))
            },
        ),
    )?;
    ctx.eval::<(), _>(FETCH_BOOT)?;
    Ok(())
}

fn pair_up(flat: &[String]) -> Vec<(String, String)> {
    flat.chunks(2)
        .filter_map(|c| {
            if c.len() == 2 {
                Some((c[0].clone(), c[1].clone()))
            } else {
                None
            }
        })
        .collect()
}

const FETCH_BOOT: &str = r#"
(function () {
    function makeResponse(ok, status, statusText, url, body) {
        return {
            ok: ok,
            status: status,
            statusText: statusText,
            url: url,
            // Keep a single decoded body buffer; .text() / .json() each
            // resolve a fresh Promise off it so callers can read either
            // (or both) without consuming the other.
            text: function () { return Promise.resolve(body); },
            json: function () {
                try { return Promise.resolve(JSON.parse(body)); }
                catch (err) { return Promise.reject(err); }
            },
        };
    }

    globalThis.fetch = function (url, init) {
        var method = 'GET';
        var headers_flat = [];
        var body = '';
        // init must be undefined / null / object — primitives are
        // explicitly rejected per the WHATWG spec, mirroring boa's
        // parse_request_init synchronous TypeError.
        if (init != null) {
            var t = typeof init;
            if (t !== 'object' && t !== 'function') {
                return Promise.reject(new TypeError('fetch: init argument must be an object'));
            }
            if (init.method != null) method = String(init.method).toUpperCase();
            if (init.headers != null) {
                var ht = typeof init.headers;
                if (ht !== 'object') {
                    return Promise.reject(new TypeError('fetch: headers must be an object'));
                }
                var h = init.headers;
                // Walk own keys only — symbol-keyed entries are not HTTP
                // headers (matching boa) and inherited prototype slots
                // shouldn't leak through.
                var keys = Object.keys(h);
                for (var i = 0; i < keys.length; i++) {
                    var k = keys[i];
                    headers_flat.push(String(k));
                    headers_flat.push(String(h[k]));
                }
            }
            if (init.body != null) body = String(init.body);
        }
        try {
            var t = globalThis.__mb_fetch_sync(String(url), method, headers_flat, body);
            return Promise.resolve(makeResponse(t[0], t[1], t[2], t[3], t[4]));
        } catch (err) {
            return Promise.reject(err);
        }
    };
})();
"#;
