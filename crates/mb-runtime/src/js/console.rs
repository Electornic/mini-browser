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

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use rquickjs::{Ctx, Result, prelude::Func};

/// One `console.*` call, kept for the agent's observation (the line also
/// still goes to stderr — never stdout, which belongs to MCP).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleMessage {
    pub level: &'static str,
    pub text: String,
}

/// Oldest messages are dropped past this, so a chatty page can't grow the
/// buffer without bound between two agent observations.
const MAX_BUFFERED: usize = 200;

pub(super) type ConsoleBuffer = Rc<RefCell<Vec<ConsoleMessage>>>;

/// Script label (`EvalOptions::filename`) → source text; see `JsRuntime::sources`.
pub(super) type ScriptSources = Rc<RefCell<HashMap<String, Rc<str>>>>;

pub(super) fn push(buffer: &ConsoleBuffer, level: &'static str, text: String) {
    let mut buf = buffer.borrow_mut();
    if buf.len() >= MAX_BUFFERED {
        buf.remove(0);
    }
    buf.push(ConsoleMessage { level, text });
}

fn sink(buffer: &ConsoleBuffer, level: &'static str) -> impl Fn(String) + 'static {
    let buffer = buffer.clone();
    move |s: String| {
        eprintln!("[console.{level}] {s}");
        push(&buffer, level, s);
    }
}

// `Uncaught in {where}: {message} (near "x") at {url}:{line}:{col}` for
// listener / timer / rAF errors — same locator as top-level script errors.
fn error_reporter(
    buffer: &ConsoleBuffer,
    sources: &ScriptSources,
) -> impl Fn(String, String, String) + 'static {
    let buffer = buffer.clone();
    let sources = sources.clone();
    move |place: String, message: String, stack: String| {
        let line = {
            let sources = sources.borrow();
            let stack = Some(stack.as_str()).filter(|s| !s.is_empty());
            super::error_location::describe_error(&message, stack, |url| {
                sources.get(url).map(|s| &**s)
            })
        };
        let text = format!("Uncaught in {place}: {line}");
        eprintln!("[console.error] {text}");
        push(&buffer, "error", text);
    }
}

pub(super) fn register_console(
    ctx: &Ctx<'_>,
    buffer: ConsoleBuffer,
    sources: ScriptSources,
) -> Result<()> {
    let globals = ctx.globals();
    globals.set("__mb_console_log", Func::from(sink(&buffer, "log")))?;
    globals.set("__mb_console_warn", Func::from(sink(&buffer, "warn")))?;
    globals.set("__mb_console_error", Func::from(sink(&buffer, "error")))?;
    globals.set(
        "__mb_report_error_native",
        Func::from(error_reporter(&buffer, &sources)),
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
    var reportNative = globalThis.__mb_report_error_native;
    var make = function (sink) {
        return function () { sink(join(arguments)); };
    };
    globalThis.console = {
        log: make(sinks.log),
        warn: make(sinks.warn),
        error: make(sinks.error),
    };
    // Uncaught exceptions in listeners / timers: browsers keep going and
    // report them to the console, so do we (the agent sees them as
    // `console:` errors). Bound to the captured sink, so a page that
    // replaces `console` can't hide them. `err.stack` goes to Rust so the
    // line gets `at url:line:col` like a top-level script error. Every
    // read is guarded: `err` is whatever the page threw.
    Object.defineProperty(globalThis, '__mb_report_error', {
        value: function (where, err) {
            var message = 'exception', stack = '';
            try { message = String(err); } catch (e) {}
            try { if (err && typeof err.stack === 'string') stack = err.stack; } catch (e) {}
            reportNative('' + where, message, stack);
        },
        enumerable: false,
    });
    delete globalThis.__mb_console_log;
    delete globalThis.__mb_console_warn;
    delete globalThis.__mb_console_error;
    delete globalThis.__mb_report_error_native;
})();
"#;
