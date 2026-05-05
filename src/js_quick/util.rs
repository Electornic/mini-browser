// Argument-shape helpers shared across host-API closures. 4.8b will add
// `read_node_id` here once the Element wrapper factory lands; for 4.8a
// the file holds the small pieces console / window already need.

use rquickjs::{Ctx, Function, Result, Value};

/// Coerce a JS value through the global `String(v)` constructor. Returns
/// the empty string when coercion fails — same lossy fallback boa's
/// bridge used so a misbehaving `toString` can't crash a host API.
pub(super) fn coerce_string<'js>(ctx: &Ctx<'js>, value: &Value<'js>) -> Result<String> {
    if let Some(js_str) = value.as_string() {
        return js_str.to_string();
    }
    let string_ctor: Function<'js> = ctx.globals().get("String")?;
    string_ctor.call::<_, String>((value.clone(),))
}

/// Pluck the nth positional argument and coerce it to a Rust String.
/// Missing arguments coerce to "undefined" — matches what real browsers
/// surface for `getItem()` / `setAttribute(undefined)` etc.
#[allow(dead_code)]
pub(super) fn nth_arg_as_string<'js>(
    args: &[Value<'js>],
    n: usize,
    ctx: &Ctx<'js>,
) -> Result<String> {
    match args.get(n) {
        Some(v) => coerce_string(ctx, v),
        None => Ok("undefined".to_string()),
    }
}

#[allow(dead_code)]
pub(super) fn first_arg_as_string<'js>(args: &[Value<'js>], ctx: &Ctx<'js>) -> Result<String> {
    nth_arg_as_string(args, 0, ctx)
}
