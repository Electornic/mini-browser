// `document` global. Each method captures its own `Rc` clone of the shared
// Document handle so they stay valid after `register_document` returns. The
// closures use `unsafe from_closure` because our captures
// (Rc<RefCell<Document>>) are pure host data — no JS values hide inside, so
// Boa's GC has nothing to trace through them.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsNativeError, JsResult, JsValue, NativeFunction, js_string,
    object::{ObjectInitializer, builtins::JsArray},
    property::Attribute,
};

use crate::{
    css::{self, Selector},
    dom::{AttrMap, Document, NodeId, NodeType},
    dom_select::{MatchingElement, MatchingState},
};

use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};

use super::ListenerMap;
use super::element::{make_element, make_text};
use super::util::first_arg_as_string;

pub(super) fn register_document(
    context: &mut Context,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
) {
    let dom_for_id = dom.clone();
    let listeners_for_id = listeners.clone();
    let get_element_by_id = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let id = first_arg_as_string(args, ctx)?;
            // Borrow scoped to the lookup so make_element below can take its
            // own borrow without the two stepping on each other.
            let node_id = {
                let document = dom_for_id.borrow();
                find_by_id(&document, &id)
            };
            match node_id {
                Some(node_id) => Ok(JsValue::from(make_element(
                    node_id,
                    dom_for_id.clone(),
                    listeners_for_id.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_for_qs = dom.clone();
    let listeners_for_qs = listeners.clone();
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
            let node_id = {
                let document = dom_for_qs.borrow();
                find_first_match(&document, &selector)
            };
            match node_id {
                Some(node_id) => Ok(JsValue::from(make_element(
                    node_id,
                    dom_for_qs.clone(),
                    listeners_for_qs.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_for_class = dom.clone();
    let listeners_for_class = listeners.clone();
    let get_elements_by_class_name = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            // Spec: argument is a string of one-or-more whitespace-separated
            // class tokens; an element matches when its class list contains
            // every token. Modeled as a snapshot here (not the spec's live
            // HTMLCollection) — every call walks the current Document and
            // returns a fresh JS array, which is enough for HN-style helpers
            // like `byClass()` that just iterate once.
            let raw = first_arg_as_string(args, ctx)?;
            let tokens: Vec<String> =
                raw.split_whitespace().map(|s| s.to_string()).collect();
            // Empty token list (the spec would return everything, but real
            // call sites pass at least one class) — return [] instead of
            // every element on the page.
            let collected: Vec<NodeId> = if tokens.is_empty() {
                Vec::new()
            } else {
                let document = dom_for_class.borrow();
                collect_by_class(&document, &tokens)
            };
            let array = JsArray::new(ctx);
            for node_id in collected {
                let element = make_element(
                    node_id,
                    dom_for_class.clone(),
                    listeners_for_class.clone(),
                    ctx,
                );
                array.push(JsValue::from(element), ctx)?;
            }
            Ok(array.into())
        })
    };

    let dom_for_create = dom.clone();
    let listeners_for_create = listeners.clone();
    let create_element = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let tag = first_arg_as_string(args, ctx)?;
            // Match the parser convention: tag names live lowercase in the
            // arena, regardless of how JS spelled them. The tagName getter
            // surfaces the canonical uppercase form back to JS.
            let tag_lower = tag.to_ascii_lowercase();
            let new_id = dom_for_create
                .borrow_mut()
                .create_element(tag_lower, AttrMap::new());
            Ok(JsValue::from(make_element(
                new_id,
                dom_for_create.clone(),
                listeners_for_create.clone(),
                ctx,
            )))
        })
    };

    let dom_for_text = dom.clone();
    let create_text_node = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let text = first_arg_as_string(args, ctx)?;
            let new_id = dom_for_text.borrow_mut().create_text(text);
            Ok(JsValue::from(make_text(new_id, dom_for_text.clone(), ctx)))
        })
    };

    // `document.body` and `document.head` are spec-mandated getters: scripts
    // routinely call `document.body.appendChild(...)` at boot time, and a
    // missing accessor turns into "TypeError: Cannot read properties of
    // undefined" on the first line of the inline script. The closures walk
    // every root for the first matching element each call (no cache), so
    // post-mutation reads observe the live tree just like getElementById.
    let dom_for_body = dom.clone();
    let listeners_for_body = listeners.clone();
    let body_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            let node_id = {
                let document = dom_for_body.borrow();
                find_first_tag(&document, "body")
            };
            match node_id {
                Some(id) => Ok(JsValue::from(make_element(
                    id,
                    dom_for_body.clone(),
                    listeners_for_body.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    }
    .to_js_function(context.realm());

    let dom_for_head = dom.clone();
    let listeners_for_head = listeners.clone();
    let head_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            let node_id = {
                let document = dom_for_head.borrow();
                find_first_tag(&document, "head")
            };
            match node_id {
                Some(id) => Ok(JsValue::from(make_element(
                    id,
                    dom_for_head.clone(),
                    listeners_for_head.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    }
    .to_js_function(context.realm());

    let document = ObjectInitializer::new(context)
        .function(get_element_by_id, js_string!("getElementById"), 1)
        .function(query_selector, js_string!("querySelector"), 1)
        .function(
            get_elements_by_class_name,
            js_string!("getElementsByClassName"),
            1,
        )
        .function(create_element, js_string!("createElement"), 1)
        .function(create_text_node, js_string!("createTextNode"), 1)
        .accessor(
            js_string!("body"),
            Some(body_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("head"),
            Some(head_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        // Silent no-op stubs. `document.addEventListener('DOMContentLoaded', …)`
        // is one of the most common top-level calls on real pages; without a
        // method here the call throws and the rest of the script never runs.
        // Real dispatch (e.g. promoting click bubble to document, firing
        // DOMContentLoaded) is a follow-up — for now the registration is
        // accepted and dropped.
        .function(
            NativeFunction::from_fn_ptr(noop_event_listener),
            js_string!("addEventListener"),
            2,
        )
        .function(
            NativeFunction::from_fn_ptr(noop_event_listener),
            js_string!("removeEventListener"),
            2,
        )
        .build();

    let _ = context.register_global_property(js_string!("document"), document, Attribute::all());
}

// Shared no-op for document-level add/removeEventListener. See window.rs
// for the matching global-object stub — same shape, same rationale.
fn noop_event_listener(_this: &JsValue, _args: &[JsValue], _ctx: &mut Context) -> JsResult<JsValue> {
    Ok(JsValue::undefined())
}

fn find_by_id(document: &Document, id: &str) -> Option<NodeId> {
    for &root in document.roots() {
        if let Some(found) = walk_for_id(document, root, id) {
            return Some(found);
        }
    }
    None
}

fn walk_for_id(document: &Document, node_id: NodeId, id: &str) -> Option<NodeId> {
    let node = document.get(node_id)?;
    if let NodeType::Element(elem) = &node.node_type
        && elem.attributes.get("id").is_some_and(|v| v == id)
    {
        return Some(node_id);
    }
    for child in &node.children {
        if let Some(found) = walk_for_id(document, *child, id) {
            return Some(found);
        }
    }
    None
}

fn find_first_tag(document: &Document, tag: &str) -> Option<NodeId> {
    for &root in document.roots() {
        if let Some(found) = walk_for_tag(document, root, tag) {
            return Some(found);
        }
    }
    None
}

fn walk_for_tag(document: &Document, node_id: NodeId, tag: &str) -> Option<NodeId> {
    let node = document.get(node_id)?;
    if let NodeType::Element(elem) = &node.node_type
        && elem.tag_name == tag
    {
        return Some(node_id);
    }
    for child in &node.children {
        if let Some(found) = walk_for_tag(document, *child, tag) {
            return Some(found);
        }
    }
    None
}

fn collect_by_class(document: &Document, tokens: &[String]) -> Vec<NodeId> {
    let mut hits: Vec<NodeId> = Vec::new();
    for &root in document.roots() {
        walk_collect_by_class(document, root, tokens, &mut hits);
    }
    hits
}

fn walk_collect_by_class(
    document: &Document,
    node_id: NodeId,
    tokens: &[String],
    hits: &mut Vec<NodeId>,
) {
    let Some(node) = document.get(node_id) else {
        return;
    };
    if let NodeType::Element(elem) = &node.node_type
        && let Some(class_attr) = elem.attributes.get("class")
    {
        let classes: Vec<&str> = class_attr.split_whitespace().collect();
        // Spec: every requested token must appear in the element's class
        // list (whitespace-separated). Match is case-sensitive in standards
        // mode, which matches what we model elsewhere.
        if tokens.iter().all(|t| classes.iter().any(|c| *c == t)) {
            hits.push(node_id);
        }
    }
    let children = node.children.clone();
    for child in children {
        walk_collect_by_class(document, child, tokens, hits);
    }
}

fn find_first_match(document: &Document, selector: &Selector) -> Option<NodeId> {
    // querySelector is a static lookup against the parsed Document — no
    // live hover/focus/active state to thread through, so we hand the
    // selectors crate a default MatchingState.
    let state = MatchingState::default();
    let mut caches = SelectorCaches::default();
    let mut ctx = MatchingContext::<crate::css::MiniBrowserSelectorImpl>::new(
        MatchingMode::Normal,
        None,
        &mut caches,
        QuirksMode::NoQuirks,
        NeedsSelectorFlags::No,
        MatchingForInvalidation::No,
    );
    for &root in document.roots() {
        if let Some(found) = walk_for_match(document, root, selector, &state, &mut ctx) {
            return Some(found);
        }
    }
    None
}

fn walk_for_match(
    document: &Document,
    node_id: NodeId,
    selector: &Selector,
    state: &MatchingState,
    ctx: &mut MatchingContext<'_, crate::css::MiniBrowserSelectorImpl>,
) -> Option<NodeId> {
    if matches_static_selector(document, node_id, selector, state, ctx) {
        return Some(node_id);
    }
    // Snapshot children before recursing so a lookup against the
    // arena doesn't conflict with the recursive borrows.
    let children: Vec<NodeId> = match document.get(node_id) {
        Some(node) => node.children.clone(),
        None => return None,
    };
    for child in &children {
        if let Some(found) = walk_for_match(document, *child, selector, state, ctx) {
            return Some(found);
        }
    }
    None
}

// Run a parsed selector against an element by id, using the selectors
// crate's matcher. Reused by querySelector / Element.matches /
// Element.closest. Pseudo-class evaluation pulls live state from the
// MatchingState the caller provides — for the static (querySelector)
// case that's the default "nothing is engaged" state.
pub(super) fn matches_static_selector(
    document: &Document,
    node_id: NodeId,
    selector: &Selector,
    state: &MatchingState,
    ctx: &mut MatchingContext<'_, crate::css::MiniBrowserSelectorImpl>,
) -> bool {
    if !matches!(
        document.get(node_id).map(|n| &n.node_type),
        Some(NodeType::Element(_))
    ) {
        // Text / detached nodes never match a CSS selector — short-circuit
        // before constructing the wrapper to keep the matcher's invariants
        // happy (it expects element-shaped inputs).
        return false;
    }
    let element = MatchingElement::new(node_id, document, state);
    selector
        .list()
        .slice()
        .iter()
        .any(|sel| selectors::matching::matches_selector(sel, 0, None, &element, ctx))
}
