// Synchronous-under-the-hood `XMLHttpRequest` global. The toy keeps
// the public surface jQuery-shaped — `new XMLHttpRequest()`, `open`,
// `setRequestHeader`, `send`, `onreadystatechange` / `onload` /
// `onerror` — but performs the HTTP exchange inline inside `send()`.
// Once `send` returns, the readyState transitions
// (HEADERS_RECEIVED → LOADING → DONE) have already happened and every
// event has fired.
//
// Implementation split (mirrors fetch.rs):
//   - Rust hook `__mb_xhr_send_sync(method, url, headers_flat, body)`
//     calls `net::fetch_with_request`. URL parse failures throw; a
//     network error after open returns a sentinel 6-tuple
//     `(false, 0, "", "", "", [])` so the JS side can fire `error`
//     instead of throwing — same "request error" steps the boa bridge
//     followed.
//   - JS-side `XMLHttpRequest` class: per-instance closure state for
//     readyState / headers / response, fires events through both
//     property-style handlers (`xhr.onload = ...`) and
//     `addEventListener` registrations.

use rquickjs::{Ctx, Exception, Result, convert::List, prelude::Func};

use crate::net;

// Same shape as fetch.rs's return tuple — `List` surfaces as a JS Array
// the JS-side bootstrap destructures.
type XhrSendTuple = List<(bool, u16, String, String, String, Vec<String>)>;

pub(super) fn register_xhr(ctx: &Ctx<'_>) -> Result<()> {
    ctx.globals().set(
        "__mb_xhr_send_sync",
        Func::from(
            move |ctx: Ctx<'_>,
                  method: String,
                  url: String,
                  headers_flat: Vec<String>,
                  body: String|
                  -> Result<XhrSendTuple> {
                let parsed_url = net::Url::parse(&url).map_err(|err| {
                    Exception::throw_type(
                        &ctx,
                        &format!("XMLHttpRequest.send: invalid URL: {err:?}"),
                    )
                })?;
                let headers: Vec<(String, String)> = headers_flat
                    .chunks(2)
                    .filter_map(|c| {
                        if c.len() == 2 {
                            Some((c[0].clone(), c[1].clone()))
                        } else {
                            None
                        }
                    })
                    .collect();
                match net::fetch_with_request(&parsed_url, &method, &headers, body.as_bytes()) {
                    Ok(result) => {
                        let ok = (200..300).contains(&result.response.status_code);
                        let status_text = result.response.reason_phrase.clone();
                        let response_text =
                            String::from_utf8_lossy(&result.response.body).into_owned();
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
                    }
                    // "Request error" sentinel — JS side maps this to
                    // status=0 + DONE state + 'error' event, no throw.
                    Err(_) => Ok(List((
                        false,
                        0u16,
                        String::new(),
                        String::new(),
                        String::new(),
                        Vec::new(),
                    ))),
                }
            },
        ),
    )?;
    ctx.eval::<(), _>(XHR_BOOT)?;
    Ok(())
}

const XHR_BOOT: &str = r#"
(function () {
    var UNSENT = 0;
    var OPENED = 1;
    var HEADERS_RECEIVED = 2;
    var LOADING = 3;
    var DONE = 4;

    function XMLHttpRequest() {
        var self = {
            readyState: UNSENT,
            status: 0,
            statusText: '',
            responseText: '',
            response: '',
            responseURL: '',
            UNSENT: UNSENT,
            OPENED: OPENED,
            HEADERS_RECEIVED: HEADERS_RECEIVED,
            LOADING: LOADING,
            DONE: DONE,
        };
        var requestMethod = '';
        var requestUrl = null;
        var requestHeaders = [];
        var responseHeaders = [];
        var listeners = Object.create(null);

        function fireEvent(type) {
            var event = { type: type, target: self, currentTarget: self };
            // Property-style first (matches browser order), then the
            // addEventListener registrations.
            var prop = 'on' + type;
            if (typeof self[prop] === 'function') {
                try { self[prop].call(self, event); } catch (e) { /* swallow */ }
            }
            var arr = listeners[type];
            if (!arr) return;
            var snapshot = arr.slice();
            for (var i = 0; i < snapshot.length; i++) {
                try { snapshot[i].call(self, event); } catch (e) { /* swallow */ }
            }
        }

        function setStateAndFire(s) {
            self.readyState = s;
            fireEvent('readystatechange');
        }

        self.open = function (method, url) {
            requestMethod = String(method).toUpperCase();
            requestUrl = String(url);
            // open() resets request-side fields per spec — repeat
            // calls on the same instance are legal, with later open()
            // winning.
            requestHeaders = [];
            self.readyState = OPENED;
        };

        self.setRequestHeader = function (name, value) {
            if (self.readyState !== OPENED) {
                throw new TypeError(
                    'XMLHttpRequest.setRequestHeader: state must be OPENED'
                );
            }
            requestHeaders.push(String(name));
            requestHeaders.push(String(value));
        };

        self.send = function (body) {
            if (self.readyState !== OPENED) {
                throw new TypeError('XMLHttpRequest.send: state must be OPENED');
            }
            if (requestUrl == null) {
                throw new TypeError('XMLHttpRequest.send: open() not called');
            }
            var bodyStr = (body == null) ? '' : String(body);
            // Hook throws on URL parse failure; let it propagate so the
            // caller sees the same TypeError shape boa surfaced. A
            // network-error response comes back as the (false,0,...)
            // sentinel below.
            var t = globalThis.__mb_xhr_send_sync(
                requestMethod, requestUrl, requestHeaders, bodyStr
            );
            var ok = t[0];
            var status = t[1];
            var statusText = t[2];
            var responseURL = t[3];
            var responseText = t[4];
            var headers = t[5] || [];
            // Sentinel: status=0 AND empty final URL ⇒ network error
            // before any headers were received.
            if (status === 0 && responseURL === '') {
                self.status = 0;
                self.statusText = '';
                self.responseText = '';
                self.response = '';
                self.responseURL = '';
                responseHeaders = [];
                setStateAndFire(DONE);
                fireEvent('error');
                return;
            }
            self.status = status;
            self.statusText = statusText;
            self.responseText = responseText;
            self.response = responseText;
            self.responseURL = responseURL;
            responseHeaders = headers;
            setStateAndFire(HEADERS_RECEIVED);
            setStateAndFire(LOADING);
            setStateAndFire(DONE);
            fireEvent('load');
        };

        // No-op: send() is synchronous, so by the time JS could call
        // abort the request has already returned. Real browsers fire
        // an `abort` event here — keeping the call shape compatible
        // without paying for the never-hit branch.
        self.abort = function () {};

        self.getResponseHeader = function (name) {
            var needle = String(name).toLowerCase();
            for (var i = 0; i < responseHeaders.length; i += 2) {
                if (responseHeaders[i].toLowerCase() === needle) {
                    return responseHeaders[i + 1];
                }
            }
            return null;
        };

        self.getAllResponseHeaders = function () {
            var buf = '';
            for (var i = 0; i < responseHeaders.length; i += 2) {
                buf += responseHeaders[i] + ': ' + responseHeaders[i + 1] + '\r\n';
            }
            return buf;
        };

        self.addEventListener = function (type, fn) {
            if (typeof fn !== 'function') return;
            var arr = listeners[type] || (listeners[type] = []);
            if (arr.indexOf(fn) === -1) arr.push(fn);
        };
        self.removeEventListener = function (type, fn) {
            if (typeof fn !== 'function') return;
            var arr = listeners[type];
            if (!arr) return;
            var idx = arr.indexOf(fn);
            if (idx >= 0) arr.splice(idx, 1);
        };

        return self;
    }

    XMLHttpRequest.UNSENT = UNSENT;
    XMLHttpRequest.OPENED = OPENED;
    XMLHttpRequest.HEADERS_RECEIVED = HEADERS_RECEIVED;
    XMLHttpRequest.LOADING = LOADING;
    XMLHttpRequest.DONE = DONE;
    globalThis.XMLHttpRequest = XMLHttpRequest;
})();
"#;
