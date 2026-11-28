//! `var()` nesting depth (compat round 3 step 1c review).
//!
//! Substitution walks nested blocks and fallbacks, and a custom property can
//! reference another on the same node — each a recursion if unbounded. A
//! page can write 50,000 levels in one declaration or a 10,000-link chain;
//! a stack overflow is an **abort** no `catch_unwind` stops. The cases run
//! in a child process (this test binary, filtered to this test) on a 2 MB
//! thread — the image / loader thread size — and the parent checks the exit
//! status. Like `mb-runtime/tests/dom_depth.rs`.

use mb_dom::css::{self, Unit, Value};
use mb_dom::{html, style};

const CHILD_ENV: &str = "MB_VAR_DEPTH_CHILD";

#[test]
fn deep_var_nesting_cannot_overflow_the_stack() {
    if std::env::var_os(CHILD_ENV).is_some() {
        let worker = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(run_child)
            .expect("spawn 2 MB thread");
        worker.join().expect("child case panicked");
        println!("[var-depth] ok");
        return;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let output = std::process::Command::new(exe)
        .args([
            "deep_var_nesting_cannot_overflow_the_stack",
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
        output.status.success() && stdout.contains("[var-depth] ok"),
        "child failed ({:?}):\n{stderr}\n{stdout}",
        output.status
    );
}

/// `p`'s styled values for `css` on a small page.
fn style_p(css: &str) -> style::StyledNode {
    let document =
        html::parse("<!DOCTYPE html><html><body><div><p>x</p></div></body></html>").unwrap();
    let root = *document.roots().last().unwrap();
    let sheet = css::parse(css).unwrap();
    let styled = style::style_tree(&document, root, std::slice::from_ref(&sheet));
    fn find(node: &style::StyledNode) -> Option<&style::StyledNode> {
        if matches!(&node.node_type, mb_dom::dom::NodeType::Element(e) if e.tag_name == "p") {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    find(&styled).unwrap().clone()
}

fn px(v: f32) -> Value {
    Value::Length(v, Unit::Px)
}

const DEEP: usize = 50_000;

fn run_child() {
    // 1. Nested blocks inside a fallback: `var(--x, ((((…))))`.
    let css = format!(
        "p {{ width: var(--x, {}1px{}); height: 2px; }}",
        "(".repeat(DEEP),
        ")".repeat(DEEP)
    );
    let p = style_p(&css);
    assert_eq!(p.value("width"), None, "too deep: invalid, not a crash");
    assert_eq!(
        p.value("height"),
        Some(&px(2.0)),
        "the rest of the rule applies"
    );

    // 2. Nested fallbacks: `var(--a, var(--a, var(--a, …)))`.
    let css = format!(
        "p {{ width: {}3px{}; height: 2px; }}",
        "var(--a, ".repeat(DEEP),
        ")".repeat(DEEP)
    );
    let p = style_p(&css);
    assert_eq!(p.value("width"), None);
    assert_eq!(p.value("height"), Some(&px(2.0)));

    // 3. A 10,000-link custom-property chain on one node.
    let mut css = String::from(":root {");
    for i in 0..10_000 {
        css.push_str(&format!(" --a{i}: var(--a{});", i + 1));
    }
    css.push_str(" --a10000: 4px; } p { width: var(--a0); height: var(--a9990); }");
    let p = style_p(&css);
    assert_eq!(p.value("width"), None, "a chain past the cap is invalid");
    assert_eq!(
        p.value("height"),
        Some(&px(4.0)),
        "a short chain from the same set still resolves (order-independent)"
    );

    // 4. Parse time: a deep value with `var(` at the bottom, and a deep
    //    custom property — the `var(` scan walks nested blocks too.
    let css = format!(
        "p {{ --deep: {}x{}; width: calc({}var(--y){}); height: 2px; }}",
        "[".repeat(DEEP),
        "]".repeat(DEEP),
        "(".repeat(DEEP),
        ")".repeat(DEEP)
    );
    let p = style_p(&css);
    assert_eq!(p.value("width"), None);
    assert_eq!(p.value("height"), Some(&px(2.0)));

    // 5. Step 5a: math functions nest by themselves — `calc(calc(…))`,
    //    parentheses, and a deep calc reached through `var()`.
    let css = format!(
        "p {{ width: {}1px{}; min-width: calc({}1px{}); --c: {}2px{}; \
         max-width: var(--c); height: 2px; }}",
        "calc(".repeat(DEEP),
        ")".repeat(DEEP),
        "(".repeat(DEEP),
        ")".repeat(DEEP),
        "min(".repeat(DEEP),
        ")".repeat(DEEP)
    );
    let p = style_p(&css);
    assert_eq!(p.value("width"), None);
    assert_eq!(p.value("min-width"), None);
    assert_eq!(p.value("max-width"), None);
    assert_eq!(p.value("height"), Some(&px(2.0)));

    // 6. Within the cap everything still works.
    let mut css = String::from(":root {");
    for i in 0..20 {
        css.push_str(&format!(" --b{i}: var(--b{});", i + 1));
    }
    css.push_str(&format!(
        " --b20: 6px; }} p {{ width: var(--b0); height: var(--n, {}7px{}); }}",
        "var(--n, ".repeat(10),
        ")".repeat(10)
    ));
    let p = style_p(&css);
    assert_eq!(p.value("width"), Some(&px(6.0)));
    assert_eq!(p.value("height"), Some(&px(7.0)));
    let p = style_p(&format!(
        "p {{ width: {}3px{}; }}",
        "calc(".repeat(20),
        ")".repeat(20)
    ));
    assert_eq!(p.value("width"), Some(&px(3.0)));
}
