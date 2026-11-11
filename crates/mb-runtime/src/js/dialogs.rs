// `alert` / `confirm` / `prompt` / `window.open` shims (docs/concept.md
// "범위: 단일 페이지"). Real browsers block on these; an agent-driven page
// can't wait for a human to dismiss a modal, so each returns immediately
// with the safe answer and the call is recorded for the agent to see:
//
//   alert(msg)        → undefined
//   confirm(msg)      → false   (never silently agree to "Delete account?")
//   prompt(msg, def)  → null    (as if cancelled)
//   window.open(url)  → null    (one page only; no popups)

use std::cell::RefCell;
use std::rc::Rc;

use rquickjs::{Ctx, Result, prelude::Func};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogRecord {
    /// "alert" | "confirm" | "prompt" | "open"
    pub kind: String,
    /// The message (or URL for `open`), as the page passed it.
    pub message: String,
}

pub(super) type DialogBuffer = Rc<RefCell<Vec<DialogRecord>>>;

const MAX_BUFFERED: usize = 50;

pub(super) fn register_dialogs(ctx: &Ctx<'_>, buffer: DialogBuffer) -> Result<()> {
    ctx.globals().set(
        "__mb_dialog_record",
        Func::from(move |kind: String, message: String| {
            eprintln!("[dialog.{kind}] {message}");
            let mut buf = buffer.borrow_mut();
            if buf.len() >= MAX_BUFFERED {
                buf.remove(0);
            }
            buf.push(DialogRecord { kind, message });
        }),
    )?;
    ctx.eval::<(), _>(DIALOGS_BOOT)
}

const DIALOGS_BOOT: &str = r#"
(function () {
    var record = globalThis.__mb_dialog_record;
    delete globalThis.__mb_dialog_record;
    var text = function (v) { return v === undefined ? '' : String(v); };
    globalThis.alert = function (msg) { record('alert', text(msg)); };
    globalThis.confirm = function (msg) { record('confirm', text(msg)); return false; };
    globalThis.prompt = function (msg) { record('prompt', text(msg)); return null; };
    globalThis.open = function (url) { record('open', text(url)); return null; };
})();
"#;
