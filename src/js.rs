// Thin wrapper around boa_engine so the rest of the browser does not depend
// on Boa types directly. The runtime owns a single `Context` whose globals
// (var bindings, declared functions) persist across `execute` calls within
// the same document — that lets `<script>` tags later in the page see what
// earlier ones defined, matching real browser semantics.
//
// Boa's `Context` is `!Send`, so JS execution must stay on the main thread.
// Resource fetching uses `thread::scope`; keep `JsRuntime` calls out of those
// scopes.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, Source,
    js_string,
    object::{ObjectInitializer, builtins::JsArray},
    property::Attribute,
};

use crate::{
    css::{self, Combinator, Selector, SimpleSelector, SimpleSelectorKind},
    dom::{self, NodeType},
};

pub struct JsRuntime {
    context: Context,
    // Snapshot of the parsed DOM exposed to JS through `document.*`. Held
    // behind Rc<RefCell<…>> so the native closures registered on the
    // `document` global can each carry an independent handle without
    // re-registering when the page changes — `bind_document` overwrites the
    // inner Vec and every closure observes the new tree on its next call.
    dom: Rc<RefCell<Vec<dom::Node>>>,
}

impl JsRuntime {
    pub fn new() -> Self {
        let mut context = Context::default();
        register_console(&mut context);
        let dom: Rc<RefCell<Vec<dom::Node>>> = Rc::new(RefCell::new(Vec::new()));
        register_document(&mut context, dom.clone());
        Self { context, dom }
    }

    /// Snapshot the parsed document into the runtime so JS can read it via
    /// `document.getElementById` / `document.querySelector`. Cloned eagerly:
    /// the bridge is read-only in Step 4, so nothing inside the engine will
    /// mutate this Vec — and copying lets us drop the caller's borrow before
    /// script execution starts.
    pub fn bind_document(&mut self, nodes: &[dom::Node]) {
        *self.dom.borrow_mut() = nodes.to_vec();
    }

    // Returns the displayed form of the result on success, or a stringified
    // error on failure. Both branches are surface-level — callers that need
    // structured access to JsValue should reach into `self.context` directly.
    pub fn execute(&mut self, source: &str) -> Result<String, String> {
        self.context
            .eval(Source::from_bytes(source))
            .map(|value| value.display().to_string())
            .map_err(|err| err.to_string())
    }
}

impl Default for JsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

// Boa's Context is not Debug; surface a placeholder so containing structs can
// keep deriving Debug for diagnostics.
impl std::fmt::Debug for JsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsRuntime").finish_non_exhaustive()
    }
}

// Wires `console.log/warn/error` to stderr. Boa's default `Context` ships
// without `console`, and adding the optional `boa_runtime` crate would pull
// in extra dependencies just for this — a three-method shim is enough for the
// debug-printf use case scripts actually rely on. Each call coerces every
// argument with the standard JS ToString algorithm so that `console.log("hi")`
// prints `hi`, not `"hi"`.
fn register_console(context: &mut Context) {
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

// Builds the `document` global with read-only DOM access methods. Each method
// captures its own `Rc` clone of the shared DOM snapshot so they stay valid
// after `register_document` returns. The closures use `unsafe from_closure`
// because our captures (Rc<RefCell<Vec<Node>>>) are pure host data — no JS
// values hide inside, so Boa's GC has nothing to trace through them.
fn register_document(context: &mut Context, dom: Rc<RefCell<Vec<dom::Node>>>) {
    let dom_for_id = dom.clone();
    let get_element_by_id = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let id = first_arg_as_string(args, ctx)?;
            let nodes = dom_for_id.borrow();
            match find_by_id(&nodes, &id) {
                Some(path) => Ok(JsValue::from(make_element(&path, &nodes, ctx))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_for_qs = dom;
    let query_selector = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let selector_text = first_arg_as_string(args, ctx)?;
            let selector = match css::parse_selector(&selector_text) {
                Ok(s) => s,
                Err(err) => {
                    return Err(JsNativeError::syntax()
                        .with_message(format!(
                            "invalid selector `{selector_text}`: {} (at byte {})",
                            err.message, err.position
                        ))
                        .into());
                }
            };
            let nodes = dom_for_qs.borrow();
            match find_first_match(&nodes, &selector) {
                Some(path) => Ok(JsValue::from(make_element(&path, &nodes, ctx))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let document = ObjectInitializer::new(context)
        .function(get_element_by_id, js_string!("getElementById"), 1)
        .function(query_selector, js_string!("querySelector"), 1)
        .build();

    let _ = context.register_global_property(js_string!("document"), document, Attribute::all());
}

fn first_arg_as_string(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    let arg = args.first().cloned().unwrap_or_default();
    Ok(arg.to_string(context)?.to_std_string_escaped())
}

// Eagerly materialises a JS Element wrapper around the DOM node at `path`.
// All exposed properties (tagName, textContent, children, getAttribute) are
// captured by value at construction — Step 4 is read-only and the parsed DOM
// can't change beneath the runtime, so a one-shot snapshot is correct and
// dodges the lifetime gymnastics of carrying a borrow across native calls.
fn make_element(path: &[usize], roots: &[dom::Node], context: &mut Context) -> JsObject {
    // The walker that produced `path` only emits valid indices, so `expect`
    // surfaces regressions immediately rather than silently returning a stub.
    let node = resolve_path(roots, path).expect("path produced by tree walk must resolve");
    let element = match &node.node_type {
        NodeType::Element(e) => e,
        NodeType::Text(_) => unreachable!("element factory called with text-node path"),
    };

    // DOM `tagName` is canonically uppercase for HTML elements; our parser
    // stores lowercase tag names, so normalise at the JS boundary.
    let tag = element.tag_name.to_ascii_uppercase();
    let attrs = element.attributes.clone();
    let text_content = collect_text_content(node);

    // `.children` mirrors HTMLCollection: only Element children, indexed by
    // position among elements (Text nodes filtered out). Recursion is bounded
    // by tree depth; each child carries its own captured attribute map.
    let children = JsArray::new(context);
    for (i, child) in node.children.iter().enumerate() {
        if matches!(child.node_type, NodeType::Element(_)) {
            let mut child_path = path.to_vec();
            child_path.push(i);
            let child_obj = make_element(&child_path, roots, context);
            // push() can only fail on length overflow — irrelevant for trees.
            let _ = children.push(JsValue::from(child_obj), context);
        }
    }

    let get_attribute = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let name = first_arg_as_string(args, ctx)?;
            match attrs.get(&name) {
                Some(value) => Ok(JsValue::from(JsString::from(value.as_str()))),
                None => Ok(JsValue::null()),
            }
        })
    };

    ObjectInitializer::new(context)
        .property(
            js_string!("tagName"),
            JsString::from(tag.as_str()),
            Attribute::all(),
        )
        .property(
            js_string!("textContent"),
            JsString::from(text_content.as_str()),
            Attribute::all(),
        )
        .property(js_string!("children"), children, Attribute::all())
        .function(get_attribute, js_string!("getAttribute"), 1)
        .build()
}

fn resolve_path<'a>(roots: &'a [dom::Node], path: &[usize]) -> Option<&'a dom::Node> {
    let (first, rest) = path.split_first()?;
    let mut current = roots.get(*first)?;
    for idx in rest {
        current = current.children.get(*idx)?;
    }
    Some(current)
}

fn collect_text_content(node: &dom::Node) -> String {
    let mut buf = String::new();
    walk_text(node, &mut buf);
    buf
}

fn walk_text(node: &dom::Node, buf: &mut String) {
    match &node.node_type {
        NodeType::Text(text) => buf.push_str(text),
        NodeType::Element(_) => {
            for child in &node.children {
                walk_text(child, buf);
            }
        }
    }
}

fn find_by_id(roots: &[dom::Node], id: &str) -> Option<Vec<usize>> {
    let mut path = Vec::new();
    for (i, node) in roots.iter().enumerate() {
        path.push(i);
        if let Some(p) = walk_for_id(node, id, &mut path) {
            return Some(p);
        }
        path.pop();
    }
    None
}

fn walk_for_id(node: &dom::Node, id: &str, path: &mut Vec<usize>) -> Option<Vec<usize>> {
    if let NodeType::Element(elem) = &node.node_type
        && elem.attributes.get("id").is_some_and(|v| v == id)
    {
        return Some(path.clone());
    }
    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        if let Some(p) = walk_for_id(child, id, path) {
            return Some(p);
        }
        path.pop();
    }
    None
}

fn find_first_match(roots: &[dom::Node], selector: &Selector) -> Option<Vec<usize>> {
    let mut path = Vec::new();
    let mut ancestors: Vec<&dom::Node> = Vec::new();
    for (i, node) in roots.iter().enumerate() {
        path.push(i);
        if let Some(p) = walk_for_match(node, selector, &mut ancestors, &mut path) {
            return Some(p);
        }
        path.pop();
    }
    None
}

fn walk_for_match<'a>(
    node: &'a dom::Node,
    selector: &Selector,
    ancestors: &mut Vec<&'a dom::Node>,
    path: &mut Vec<usize>,
) -> Option<Vec<usize>> {
    if matches_static_selector(node, ancestors, selector) {
        return Some(path.clone());
    }
    ancestors.push(node);
    for (i, child) in node.children.iter().enumerate() {
        path.push(i);
        if let Some(p) = walk_for_match(child, selector, ancestors, path) {
            return Some(p);
        }
        path.pop();
    }
    ancestors.pop();
    None
}

// Mirrors style::matches_selector but skips pseudo-class state — querySelector
// is a static lookup against the parsed DOM, no hover/focus context to thread
// through. Pseudo-classes parse-but-ignore here: `.btn:hover` matches the
// same set as `.btn`.
fn matches_static_selector(
    node: &dom::Node,
    ancestors: &[&dom::Node],
    selector: &Selector,
) -> bool {
    let Some((target, leading)) = selector.parts.split_last() else {
        return false;
    };
    if !matches_simple_static(node, target) {
        return false;
    }
    let mut iter = ancestors.iter().rev();
    for (j, part) in leading.iter().enumerate().rev() {
        let combinator = selector.combinators[j];
        match combinator {
            Combinator::Descendant => loop {
                match iter.next() {
                    Some(ancestor) if matches_simple_static(ancestor, part) => break,
                    Some(_) => continue,
                    None => return false,
                }
            },
            Combinator::Child => match iter.next() {
                Some(ancestor) if matches_simple_static(ancestor, part) => {}
                _ => return false,
            },
        }
    }
    true
}

fn matches_simple_static(node: &dom::Node, simple: &SimpleSelector) -> bool {
    let element = match &node.node_type {
        NodeType::Element(e) => e,
        NodeType::Text(_) => return false,
    };
    match &simple.kind {
        SimpleSelectorKind::Tag(tag) => element.tag_name == *tag,
        SimpleSelectorKind::Class(class) => element
            .attributes
            .get("class")
            .is_some_and(|v| v.split_whitespace().any(|c| c == class)),
        SimpleSelectorKind::Id(id) => element.attributes.get("id").is_some_and(|v| v == id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    fn runtime_with(html: &str) -> JsRuntime {
        let mut runtime = JsRuntime::new();
        let nodes = html::parse(html).unwrap();
        runtime.bind_document(&nodes);
        runtime
    }

    #[test]
    fn evaluates_arithmetic() {
        let mut runtime = JsRuntime::new();
        assert_eq!(runtime.execute("1 + 2 * 3").unwrap(), "7");
    }

    #[test]
    fn preserves_global_state_between_calls() {
        let mut runtime = JsRuntime::new();
        runtime.execute("var page = 41;").unwrap();
        assert_eq!(runtime.execute("page + 1").unwrap(), "42");
    }

    #[test]
    fn surfaces_runtime_errors() {
        let mut runtime = JsRuntime::new();
        let err = runtime.execute("missing.prop").unwrap_err();
        assert!(
            err.to_lowercase().contains("missing"),
            "error should reference the missing identifier, got: {err}"
        );
    }

    #[test]
    fn evaluates_string_concatenation() {
        let mut runtime = JsRuntime::new();
        assert_eq!(
            runtime.execute("'hello, ' + 'world'").unwrap(),
            "\"hello, world\""
        );
    }

    #[test]
    fn console_object_is_registered_with_log_warn_error() {
        let mut runtime = JsRuntime::new();
        assert_eq!(runtime.execute("typeof console").unwrap(), "\"object\"");
        assert_eq!(runtime.execute("typeof console.log").unwrap(), "\"function\"");
        assert_eq!(runtime.execute("typeof console.warn").unwrap(), "\"function\"");
        assert_eq!(runtime.execute("typeof console.error").unwrap(), "\"function\"");
    }

    #[test]
    fn console_log_returns_undefined_and_does_not_throw() {
        let mut runtime = JsRuntime::new();
        // Multiple args + mixed types — exercises the ToString coercion path
        // and confirms the binding accepts variadic invocation.
        assert_eq!(
            runtime.execute("console.log('hi', 42, true)").unwrap(),
            "undefined"
        );
    }

    #[test]
    fn document_global_exposes_get_element_by_id_and_query_selector() {
        let mut runtime = runtime_with("<p>hi</p>");
        assert_eq!(runtime.execute("typeof document").unwrap(), "\"object\"");
        assert_eq!(
            runtime.execute("typeof document.getElementById").unwrap(),
            "\"function\""
        );
        assert_eq!(
            runtime.execute("typeof document.querySelector").unwrap(),
            "\"function\""
        );
    }

    #[test]
    fn get_element_by_id_returns_null_for_missing() {
        let mut runtime = runtime_with(r#"<div id="x"></div>"#);
        assert_eq!(
            runtime.execute("document.getElementById('absent')").unwrap(),
            "null"
        );
    }

    #[test]
    fn get_element_by_id_returns_uppercase_tag_name() {
        // Parser stores `div` lowercase; tagName must surface uppercase to
        // match how every real browser exposes the attribute.
        let mut runtime = runtime_with(r#"<div id="x">hi</div>"#);
        assert_eq!(
            runtime.execute("document.getElementById('x').tagName").unwrap(),
            "\"DIV\""
        );
    }

    #[test]
    fn text_content_concatenates_descendant_text() {
        // Each text node is wrapped in its own element so the inter-element
        // whitespace stripping the parser performs (consumed before each tag)
        // doesn't change what survives — the test stays focused on the JS
        // bridge's tree-walk concatenation behavior.
        let mut runtime = runtime_with(
            r#"<section id="s"><p>hello </p><span>and <b>world</b></span></section>"#,
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('s').textContent")
                .unwrap(),
            "\"hello and world\""
        );
    }

    #[test]
    fn get_attribute_reads_raw_value_or_returns_null() {
        let mut runtime = runtime_with(r#"<a id="link" href="/about" data-x="42">about</a>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('href')")
                .unwrap(),
            "\"/about\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('data-x')")
                .unwrap(),
            "\"42\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('missing')")
                .unwrap(),
            "null"
        );
    }

    #[test]
    fn children_lists_only_element_kids_in_document_order() {
        let mut runtime = runtime_with(
            r#"<ul id="list">leading <li>a</li>between<li>b</li> trailing<li>c</li></ul>"#,
        );
        // Text siblings filtered out so .children mirrors HTMLCollection
        // semantics rather than .childNodes.
        assert_eq!(
            runtime
                .execute("document.getElementById('list').children.length")
                .unwrap(),
            "3"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('list').children[0].tagName")
                .unwrap(),
            "\"LI\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('list').children[2].textContent")
                .unwrap(),
            "\"c\""
        );
    }

    #[test]
    fn query_selector_matches_tag_class_and_id() {
        let mut runtime = runtime_with(
            r#"<div><p class="hit" id="target">hi</p><p class="miss">x</p></div>"#,
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('p').textContent")
                .unwrap(),
            "\"hi\""
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('.hit').getAttribute('id')")
                .unwrap(),
            "\"target\""
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('#target').tagName")
                .unwrap(),
            "\"P\""
        );
    }

    #[test]
    fn query_selector_supports_descendant_and_child_combinators() {
        let mut runtime = runtime_with(
            r#"<section><div><span class="t">deep</span></div><span class="t">shallow</span></section>"#,
        );
        // Descendant must reach through `<div>`; child combinator must skip it.
        assert_eq!(
            runtime
                .execute("document.querySelector('section .t').textContent")
                .unwrap(),
            "\"deep\""
        );
        assert_eq!(
            runtime
                .execute("document.querySelector('section > .t').textContent")
                .unwrap(),
            "\"shallow\""
        );
    }

    #[test]
    fn query_selector_returns_null_for_no_match() {
        let mut runtime = runtime_with("<p>hi</p>");
        assert_eq!(
            runtime.execute("document.querySelector('.absent')").unwrap(),
            "null"
        );
    }

    #[test]
    fn query_selector_throws_on_invalid_selector() {
        let mut runtime = runtime_with("<p>hi</p>");
        let err = runtime.execute("document.querySelector('!!')").unwrap_err();
        assert!(
            err.to_lowercase().contains("selector"),
            "error should mention the bad selector, got: {err}"
        );
    }

    #[test]
    fn re_binding_document_replaces_dom_for_subsequent_calls() {
        // The closure captures an Rc<RefCell<…>>, not a Vec snapshot — so a
        // later bind_document must redirect the next getElementById call.
        let mut runtime = runtime_with(r#"<p id="a">first</p>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('a').textContent")
                .unwrap(),
            "\"first\""
        );
        let next = html::parse(r#"<p id="b">second</p>"#).unwrap();
        runtime.bind_document(&next);
        assert_eq!(
            runtime.execute("document.getElementById('a')").unwrap(),
            "null"
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('b').textContent")
                .unwrap(),
            "\"second\""
        );
    }
}
