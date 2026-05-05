// Event-listener registry + Event factory + bubble dispatcher. Lives
// entirely JS-side: `__mb_listener_add` / `__mb_listener_remove` /
// `__mb_listener_prune` mutate a closure-scoped registry keyed by
// `nodeId`, and `__mb_dispatch_chain` walks the bubble chain Rust hands
// in (target-first → root) firing every registered handler with a
// freshly-built Event. Returns `defaultPrevented` to the caller.
//
// Why JS-side: stashing `rquickjs::Persistent<Function>` Rust-side would
// re-introduce the multi-lifetime closure trap (Function<'js> + the
// surrounding `Ctx<'js>` for storing/restoring), and listener identity
// for dedup / removal still has to compare via `===` anyway. Keeping
// the registry in JS gets `===` semantics for free and leaves Rust to
// compute the bubble chain (which needs the live `Document`).

use rquickjs::{Ctx, Result};

pub(super) fn register_events(ctx: &Ctx<'_>) -> Result<()> {
    ctx.eval::<(), _>(EVENT_BOOT)
}

const EVENT_BOOT: &str = r#"
(function () {
    // { nodeId: { type: [fn, ...] } }. Identity-based dedup via
    // Array.prototype.indexOf; same `(target, type, callback)` triple
    // registered twice counts as one listener (WHATWG spec).
    var registry = Object.create(null);

    globalThis.__mb_listener_add = function (nodeId, type, fn) {
        if (typeof fn !== 'function') return;
        var byNode = registry[nodeId];
        if (!byNode) byNode = registry[nodeId] = Object.create(null);
        var arr = byNode[type];
        if (!arr) arr = byNode[type] = [];
        if (arr.indexOf(fn) === -1) arr.push(fn);
    };

    globalThis.__mb_listener_remove = function (nodeId, type, fn) {
        if (typeof fn !== 'function') return;
        var byNode = registry[nodeId];
        if (!byNode) return;
        var arr = byNode[type];
        if (!arr) return;
        var idx = arr.indexOf(fn);
        if (idx >= 0) arr.splice(idx, 1);
    };

    // Drop every entry for the listed `nodeId`s. Used by the
    // `innerHTML` setter to reap listeners that lived on the soon-to-be-
    // tombstoned subtree — matches the boa bridge's listener prune.
    globalThis.__mb_listener_prune = function (nodeIds) {
        if (!nodeIds || !nodeIds.length) return;
        for (var i = 0; i < nodeIds.length; i++) delete registry[nodeIds[i]];
    };

    function makeEvent(type, targetId, key, isKeyboard) {
        var event = {
            type: type,
            target: globalThis.__mb_make_element(targetId),
            currentTarget: null,
            defaultPrevented: false,
            // Hidden flags — not part of the public Event surface but
            // observed by the dispatcher between handlers / ancestors.
            __propStopped: false,
            __immediateStopped: false,
            preventDefault: function () { this.defaultPrevented = true; },
            stopPropagation: function () { this.__propStopped = true; },
            stopImmediatePropagation: function () {
                this.__propStopped = true;
                this.__immediateStopped = true;
            },
        };
        if (isKeyboard) event.key = key == null ? '' : String(key);
        return event;
    }

    // Walks `ancestorIds` (target-first → root) firing every registered
    // handler with a fresh Event. Returns `defaultPrevented` so the
    // Rust caller can decide whether to skip the default action (link
    // navigate, form submit, default text-input handling).
    globalThis.__mb_dispatch_chain = function (
        targetId, type, key, isKeyboard, ancestorIds
    ) {
        var event = makeEvent(type, targetId, key, isKeyboard);
        for (var i = 0; i < ancestorIds.length; i++) {
            if (event.__propStopped) break;
            var nid = ancestorIds[i];
            event.currentTarget = globalThis.__mb_make_element(nid);
            var byNode = registry[nid];
            if (!byNode) continue;
            var arr = byNode[type];
            if (!arr || arr.length === 0) continue;
            // Snapshot so a handler that calls removeEventListener on
            // itself mid-iteration doesn't shorten the slice we're
            // walking.
            var snapshot = arr.slice();
            for (var j = 0; j < snapshot.length; j++) {
                if (event.__immediateStopped) break;
                try { snapshot[j].call(event.currentTarget, event); }
                catch (err) { /* swallow handler errors — match boa */ }
            }
        }
        return event.defaultPrevented;
    };
})();
"#;
