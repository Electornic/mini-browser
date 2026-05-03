// Wires `console.log/warn/error` to stderr. Boa's default `Context` ships
// without `console`, and adding the optional `boa_runtime` crate would pull
// in extra dependencies just for this — a three-method shim is enough for the
// debug-printf use case scripts actually rely on. Each call coerces every
// argument with the standard JS ToString algorithm so that `console.log("hi")`
// prints `hi`, not `"hi"`.

use boa_engine::{
    Context, JsResult, JsValue, NativeFunction, js_string, object::ObjectInitializer,
    property::Attribute,
};

pub(super) fn register_console(context: &mut Context) {
    let console = ObjectInitializer::new(context)
        .function(
            NativeFunction::from_fn_ptr(console_log),
            js_string!("log"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(console_warn),
            js_string!("warn"),
            0,
        )
        .function(
            NativeFunction::from_fn_ptr(console_error),
            js_string!("error"),
            0,
        )
        .build();
    let _ = context.register_global_property(js_string!("console"), console, Attribute::all());
}

fn console_log(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_console("log", args, context);
    Ok(JsValue::undefined())
}

fn console_warn(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_console("warn", args, context);
    Ok(JsValue::undefined())
}

fn console_error(_this: &JsValue, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    write_console("error", args, context);
    Ok(JsValue::undefined())
}

fn write_console(level: &str, args: &[JsValue], context: &mut Context) {
    let parts: Vec<String> = args
        .iter()
        .map(|v| match v.to_string(context) {
            Ok(s) => s.to_std_string_escaped(),
            // ToString failed (rare — Symbol, or a custom toString that threw).
            // Fall back to the debug-style display so something useful still prints.
            Err(_) => v.display().to_string(),
        })
        .collect();
    eprintln!("[console.{level}] {}", parts.join(" "));
}
