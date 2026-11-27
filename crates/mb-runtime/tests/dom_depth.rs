//! Script-built DOM depth (docs/compat-round3.md Appendix C R3).
//!
//! The HTML parser caps nesting at `html::MAX_DOM_DEPTH`, but a page script
//! can build any depth with `appendChild` in a loop — or nest a detached
//! chain and attach it in one call, or `innerHTML` at an already-deep
//! element. Style, layout and paint recurse per level, so that is a stack
//! overflow: an **abort** no `catch_unwind` stops, which kills the whole
//! agent / MCP session. The case therefore runs in a child process (this
//! test binary, filtered to this test) and the parent checks the exit
//! status. The child uses an 8 MB thread — the stack the browser runs on.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use mb_runtime::dom::{Document, NodeId};
use mb_runtime::html::{self, MAX_DOM_DEPTH};
use mb_runtime::js::JsRuntime;
use mb_runtime::{css, style, view};

const CHILD_ENV: &str = "MB_DOM_DEPTH_CHILD";

#[test]
fn script_built_depth_cannot_overflow_the_stack() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let worker = std::thread::Builder::new()
            .stack_size(8 << 20)
            .spawn(run_child)
            .expect("spawn 8 MB thread");
        let report = worker.join().expect("child case panicked");
        println!("[depth] ok {report}");
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let output = std::process::Command::new(exe)
        .args([
            "script_built_depth_cannot_overflow_the_stack",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_ENV, "1")
        .output()
        .expect("run the child test process");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("[depth] ok"),
        "child failed ({:?}):\n{stderr}\n{stdout}",
        output.status
    );
    if let Some(line) = stdout.lines().find(|l| l.contains("[depth] ok")) {
        eprintln!("{line}");
    }
}

/// Deepest node below any root, iteratively (the point is trees too deep
/// to recurse over). Roots are depth 1.
fn deepest(document: &Document) -> usize {
    let mut deepest = 0;
    let mut budget = document.live_node_count() + 1; // a cycle would loop forever
    let mut stack: Vec<(NodeId, usize)> = document.roots().iter().map(|r| (*r, 1)).collect();
    while let Some((id, depth)) = stack.pop() {
        budget = budget.checked_sub(1).expect("the tree has a cycle");
        deepest = deepest.max(depth);
        if let Some(node) = document.get(id) {
            stack.extend(node.children.iter().map(|c| (*c, depth + 1)));
        }
    }
    deepest
}

fn run_child() -> String {
    let document = html::parse(
        r#"<!DOCTYPE html><html><body><div id="chain"></div><div id="ins"></div><div id="html"></div></body></html>"#,
    )
    .unwrap();
    let dom = Rc::new(RefCell::new(document));
    let mut rt = JsRuntime::new(dom.clone());
    let result = rt
        .execute(
            r#"
            var refused = { append: 0, insert: 0, attach: 0, html: 0, cycle: 0 };
            // 1. appendChild chain.
            var n = document.getElementById('chain');
            for (var i = 0; i < 10000; i++) {
                var c = document.createElement('div');
                try { n.appendChild(c); n = c; } catch (e) { refused.append++; }
            }
            // 2. insertBefore chain (reference = an existing first child).
            n = document.getElementById('ins');
            for (var i = 0; i < 10000; i++) {
                var c = document.createElement('span');
                c.appendChild(document.createTextNode ? document.createTextNode('t') : document.createElement('b'));
                try { n.insertBefore(c, n.firstChild || null); n = c; } catch (e) { refused.insert++; }
            }
            // 3. A detached chain, attached in one call.
            var top = document.createElement('div');
            var cur = top;
            for (var i = 0; i < 10000; i++) {
                var d = document.createElement('div');
                try { cur.appendChild(d); cur = d; } catch (e) { refused.attach++; break; }
            }
            try { document.body.appendChild(top); } catch (e) { refused.attach++; }
            // 4. innerHTML at the deepest element of chain 1.
            var deep = document.getElementById('chain');
            while (deep.firstElementChild || deep.children.length) { deep = deep.children[0]; }
            try { deep.parentElement.innerHTML = '<div>'.repeat(1000) + 'x'; } catch (e) { refused.html++; }
            // 5. A cycle: an ancestor into its own descendant.
            try { deep.parentElement.appendChild(document.getElementById('chain')); } catch (e) { refused.cycle++; }
            JSON.stringify(refused);
            "#,
        )
        .expect("script runs");
    let deepest = deepest(&dom.borrow());
    assert!(
        deepest <= MAX_DOM_DEPTH,
        "tree reached depth {deepest}: {result}"
    );
    let document = dom.borrow();
    let view = view::build_document_view(
        &document,
        &[css::parse("div { padding-left: 1px }").unwrap()],
        800,
        None,
        &HashMap::new(),
        style::InteractionState::default(),
    )
    .expect("view builds");
    format!(
        "{result}, deepest {deepest}, {} paint commands",
        view.commands.len()
    )
}
