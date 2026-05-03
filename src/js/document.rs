// `document` global. Each method captures its own `Rc` clone of the shared
// Document handle so they stay valid after `register_document` returns. The
// closures use `unsafe from_closure` because our captures
// (Rc<RefCell<Document>>) are pure host data — no JS values hide inside, so
// Boa's GC has nothing to trace through them.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsNativeError, JsResult, JsValue, NativeFunction, js_string,
    object::ObjectInitializer, property::Attribute,
};

use crate::{
    css::{self, Combinator, Selector, SimpleSelector, SimpleSelectorKind},
    dom::{AttrMap, Document, NodeId, NodeType},
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

    let dom_for_text = dom;
    let create_text_node = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let text = first_arg_as_string(args, ctx)?;
            let new_id = dom_for_text.borrow_mut().create_text(text);
            Ok(JsValue::from(make_text(new_id, dom_for_text.clone(), ctx)))
        })
    };

    let document = ObjectInitializer::new(context)
        .function(get_element_by_id, js_string!("getElementById"), 1)
        .function(query_selector, js_string!("querySelector"), 1)
        .function(create_element, js_string!("createElement"), 1)
        .function(create_text_node, js_string!("createTextNode"), 1)
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

fn find_first_match(document: &Document, selector: &Selector) -> Option<NodeId> {
    let mut ancestors: Vec<NodeId> = Vec::new();
    for &root in document.roots() {
        if let Some(found) = walk_for_match(document, root, selector, &mut ancestors) {
            return Some(found);
        }
    }
    None
}

fn walk_for_match(
    document: &Document,
    node_id: NodeId,
    selector: &Selector,
    ancestors: &mut Vec<NodeId>,
) -> Option<NodeId> {
    if matches_static_selector(document, node_id, ancestors, selector) {
        return Some(node_id);
    }
    // Snapshot children before recursing so a lookup against the
    // arena doesn't conflict with the recursive borrows.
    let children: Vec<NodeId> = match document.get(node_id) {
        Some(node) => node.children.clone(),
        None => return None,
    };
    ancestors.push(node_id);
    for child in &children {
        if let Some(found) = walk_for_match(document, *child, selector, ancestors) {
            ancestors.pop();
            return Some(found);
        }
    }
    ancestors.pop();
    None
}

// Walk parent links from `node_id` up to a root and return the chain
// outermost-first (the document root sits at index 0, the immediate parent
// of `node_id` is the last element). The receiver itself is excluded — that
// matches what `matches_static_selector` expects in its `ancestors` slice.
// Used by Element.matches / Element.closest so a parsed selector that uses
// descendant or child combinators can be resolved against the live tree.
pub(super) fn ancestors_outermost_first(document: &Document, node_id: NodeId) -> Vec<NodeId> {
    let mut chain: Vec<NodeId> = Vec::new();
    let mut cur = document.get(node_id).and_then(|n| n.parent);
    while let Some(id) = cur {
        chain.push(id);
        cur = document.get(id).and_then(|n| n.parent);
    }
    chain.reverse();
    chain
}

// Mirrors style::matches_selector but skips pseudo-class state — querySelector
// is a static lookup against the parsed Document, no hover/focus context to
// thread through. Pseudo-classes parse-but-ignore here: `.btn:hover` matches
// the same set as `.btn`.
pub(super) fn matches_static_selector(
    document: &Document,
    node_id: NodeId,
    ancestors: &[NodeId],
    selector: &Selector,
) -> bool {
    let Some((target, leading)) = selector.parts.split_last() else {
        return false;
    };
    if !matches_simple_static(document, node_id, target) {
        return false;
    }
    let mut iter = ancestors.iter().rev();
    for (j, part) in leading.iter().enumerate().rev() {
        let combinator = selector.combinators[j];
        match combinator {
            Combinator::Descendant => loop {
                match iter.next() {
                    Some(ancestor) if matches_simple_static(document, *ancestor, part) => break,
                    Some(_) => continue,
                    None => return false,
                }
            },
            Combinator::Child => match iter.next() {
                Some(ancestor) if matches_simple_static(document, *ancestor, part) => {}
                _ => return false,
            },
        }
    }
    true
}

fn matches_simple_static(document: &Document, node_id: NodeId, simple: &SimpleSelector) -> bool {
    let element = match document.get(node_id).map(|n| &n.node_type) {
        Some(NodeType::Element(e)) => e,
        _ => return false,
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
