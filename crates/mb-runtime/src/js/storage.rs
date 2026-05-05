// `localStorage` and `sessionStorage` shims. Both stores live for the
// lifetime of the JsRuntime — every navigation rebuilds the runtime, so
// neither survives a reload. That matches the contract scripts care
// about within a single page (read what you wrote earlier in the same
// script) without committing to a disk-backed store.
//
// The whole implementation runs JS-side: there's no Rust state to
// share, no HRTB closure traps to dodge, just a plain factory closure
// kept in the global scope. Spec subset matches the boa version:
// length / getItem / setItem / removeItem / clear / key.

use rquickjs::{Ctx, Result};

pub(super) fn register_storage(ctx: &Ctx<'_>) -> Result<()> {
    ctx.eval::<(), _>(STORAGE_BOOT)
}

const STORAGE_BOOT: &str = r#"
(function () {
    function makeStorage() {
        var entries = []; // [[key, value], ...] preserves insertion order.
        var find = function (k) {
            for (var i = 0; i < entries.length; i++) {
                if (entries[i][0] === k) return i;
            }
            return -1;
        };
        var store = {
            getItem: function (k) {
                var i = find(String(k));
                return i < 0 ? null : entries[i][1];
            },
            setItem: function (k, v) {
                var key = String(k);
                var val = String(v);
                var i = find(key);
                if (i < 0) {
                    entries.push([key, val]);
                } else {
                    entries[i][1] = val;
                }
            },
            removeItem: function (k) {
                var i = find(String(k));
                if (i >= 0) entries.splice(i, 1);
            },
            clear: function () { entries.length = 0; },
            key: function (idx) {
                idx = +idx | 0;
                if (idx < 0 || idx >= entries.length) return null;
                return entries[idx][0];
            },
        };
        Object.defineProperty(store, 'length', {
            get: function () { return entries.length; },
            configurable: true, enumerable: true,
        });
        return store;
    }
    globalThis.localStorage = makeStorage();
    globalThis.sessionStorage = makeStorage();
})();
"#;
