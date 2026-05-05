// Wires `console.log/warn/error` to stderr. rquickjs ships no console
// global; a three-method shim covers the debug-printf use case scripts
// rely on. Each call coerces every argument with the standard JS
// `String(v)` algorithm so `console.log("hi")` prints `hi` (not `"hi"`).
//
// Implementation note: the per-arg `String(v)` join happens in JS, not
// Rust. rquickjs closures with both `Ctx<'js>` and a lifetime-bearing
// arg can't satisfy the higher-rank `for<'js> Fn(...)` bound the
// `Func::from` signature requires (two independent lifetime placeholders
// don't unify), so we keep the Rust callback's signature down to a
// single `String` argument and do the variadic stringification in a
// JS bootstrap. Same observable behaviour as the boa version, less
// fighting with the borrow checker.

use rquickjs::{Ctx, Result, prelude::Func};

pub(super) fn register_console(ctx: &Ctx<'_>) -> Result<()> {
    let globals = ctx.globals();
    globals.set(
        "__mb_console_log",
        Func::from(|s: String| {
            eprintln!("[console.log] {s}");
        }),
    )?;
    globals.set(
        "__mb_console_warn",
        Func::from(|s: String| {
            eprintln!("[console.warn] {s}");
        }),
    )?;
    globals.set(
        "__mb_console_error",
        Func::from(|s: String| {
            eprintln!("[console.error] {s}");
        }),
    )?;
    ctx.eval::<(), _>(CONSOLE_BOOT)?;
    Ok(())
}

// Builds `console.{log,warn,error}` so each method captures its argument
// list, runs every arg through `String(v)`, and sinks the joined line
// to the matching Rust callback. The temporary `__mb_console_*` globals
// get deleted once the wrappers capture them — keeps `globalThis`
// observably clean for `Object.keys(globalThis)` style enumeration.
const CONSOLE_BOOT: &str = r#"
(function () {
    var join = function (args) {
        var parts = [];
        for (var i = 0; i < args.length; i++) parts.push(String(args[i]));
        return parts.join(' ');
    };
    var sinks = {
        log: globalThis.__mb_console_log,
        warn: globalThis.__mb_console_warn,
        error: globalThis.__mb_console_error,
    };
    var make = function (sink) {
        return function () { sink(join(arguments)); };
    };
    globalThis.console = {
        log: make(sinks.log),
        warn: make(sinks.warn),
        error: make(sinks.error),
    };
    delete globalThis.__mb_console_log;
    delete globalThis.__mb_console_warn;
    delete globalThis.__mb_console_error;
})();
"#;
