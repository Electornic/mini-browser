// Browsers expose `window` and `self` as aliases of the global object —
// scripts in the wild rely on either name being defined (`window.foo`,
// `self.addEventListener`, `typeof window === 'object'` feature checks).
// Boa already provides `globalThis` per spec; we just bind the two extra
// names to the same object so `window === globalThis === self` and a
// `var x` at top level shows up as `window.x` like every other engine.

use boa_engine::{Context, JsValue, js_string, property::Attribute};

pub(super) fn register_window_aliases(context: &mut Context) {
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
}
