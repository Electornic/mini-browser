// `document` global for the rquickjs bridge. Like `element.rs`, the
// shape is "Rust hooks under `__mb_doc_*`, JS-side bootstrap assembles
// the wrapper". Hooks return raw NodeIds (`u32`) or `Option<u32>`; the
// JS bootstrap maps those through `__mb_make_element` (installed by
// `element::run_dom_bootstrap`) so the wrapper population is a single
// concern.

use std::cell::RefCell;
use std::rc::Rc;

use rquickjs::{Ctx, Exception, Result, prelude::Func};

use crate::css;
use crate::dom::{Document, NodeId, NodeType};

use super::element::{fresh_attr_map, matches_static_selector};
use crate::dom_select::MatchingState;

use selectors::context::{
    MatchingContext, MatchingForInvalidation, MatchingMode, NeedsSelectorFlags, QuirksMode,
    SelectorCaches,
};

pub(super) fn register_document(ctx: &Ctx<'_>, dom: Rc<RefCell<Document>>) -> Result<()> {
    let globals = ctx.globals();

    let dom_g = dom.clone();
    globals.set(
        "__mb_doc_get_element_by_id",
        Func::from(move |id: String| -> Option<u32> {
            let document = dom_g.borrow();
            find_by_id(&document, &id).map(|n| n.raw())
        }),
    )?;

    let dom_g = dom.clone();
    globals.set(
        "__mb_doc_query_selector",
        Func::from(
            move |ctx: Ctx<'_>, selector_text: String| -> Result<Option<u32>> {
                let selector = css::parse_selector(&selector_text).map_err(|err| {
                    Exception::throw_syntax(
                        &ctx,
                        &format!(
                            "invalid selector `{selector_text}`: {} (at byte {})",
                            err.message, err.position
                        ),
                    )
                })?;
                let document = dom_g.borrow();
                Ok(find_first_match(&document, &selector).map(|n| n.raw()))
            },
        ),
    )?;

    let dom_g = dom.clone();
    globals.set(
        "__mb_doc_get_elements_by_class_name",
        Func::from(move |raw: String| -> Vec<u32> {
            let tokens: Vec<String> = raw.split_whitespace().map(String::from).collect();
            if tokens.is_empty() {
                return Vec::new();
            }
            let document = dom_g.borrow();
            collect_by_class(&document, &tokens)
                .into_iter()
                .map(|n| n.raw())
                .collect()
        }),
    )?;

    let dom_g = dom.clone();
    globals.set(
        "__mb_doc_create_element",
        Func::from(move |tag: String| -> u32 {
            let lower = tag.to_ascii_lowercase();
            dom_g
                .borrow_mut()
                .create_element(lower, fresh_attr_map())
                .raw()
        }),
    )?;

    let dom_g = dom.clone();
    globals.set(
        "__mb_doc_create_text_node",
        Func::from(move |text: String| -> u32 {
            dom_g.borrow_mut().create_text(text).raw()
        }),
    )?;

    let dom_g = dom.clone();
    globals.set(
        "__mb_doc_body",
        Func::from(move || -> Option<u32> {
            let document = dom_g.borrow();
            find_first_tag(&document, "body").map(|n| n.raw())
        }),
    )?;

    let dom_g = dom.clone();
    globals.set(
        "__mb_doc_head",
        Func::from(move || -> Option<u32> {
            let document = dom_g.borrow();
            find_first_tag(&document, "head").map(|n| n.raw())
        }),
    )?;

    // The `document` object itself — assembled JS-side so its methods
    // delegate through the `__mb_make_*` factories the element bootstrap
    // installs. Run AFTER `run_dom_bootstrap` so the factories exist.
    ctx.eval::<(), _>(DOCUMENT_BOOT)?;

    Ok(())
}

const DOCUMENT_BOOT: &str = r#"
(function () {
    var doc = {};

    doc.getElementById = function (id) {
        var nid = globalThis.__mb_doc_get_element_by_id(String(id));
        return nid == null ? null : globalThis.__mb_make_element(nid);
    };

    doc.querySelector = function (selector) {
        var nid = globalThis.__mb_doc_query_selector(String(selector));
        return nid == null ? null : globalThis.__mb_make_element(nid);
    };

    doc.getElementsByClassName = function (className) {
        var ids = globalThis.__mb_doc_get_elements_by_class_name(String(className));
        var out = [];
        for (var i = 0; i < ids.length; i++) out.push(globalThis.__mb_make_element(ids[i]));
        return out;
    };

    doc.createElement = function (tag) {
        var nid = globalThis.__mb_doc_create_element(String(tag));
        return globalThis.__mb_make_element(nid);
    };

    doc.createTextNode = function (text) {
        var nid = globalThis.__mb_doc_create_text_node(String(text));
        return globalThis.__mb_make_text(nid);
    };

    Object.defineProperty(doc, 'body', {
        get: function () {
            var nid = globalThis.__mb_doc_body();
            return nid == null ? null : globalThis.__mb_make_element(nid);
        },
        configurable: true, enumerable: true,
    });

    Object.defineProperty(doc, 'head', {
        get: function () {
            var nid = globalThis.__mb_doc_head();
            return nid == null ? null : globalThis.__mb_make_element(nid);
        },
        configurable: true, enumerable: true,
    });

    // Document-level addEventListener / removeEventListener stubs (4.8c
    // promotes these to real registrations on a synthetic document
    // target).
    doc.addEventListener = function () {};
    doc.removeEventListener = function () {};

    globalThis.document = doc;
})();
"#;

// ---- Pure DOM walk helpers (lifted from boa version) ------------------

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
        if tokens.iter().all(|t| classes.iter().any(|c| *c == t)) {
            hits.push(node_id);
        }
    }
    let children = node.children.clone();
    for child in children {
        walk_collect_by_class(document, child, tokens, hits);
    }
}

fn find_first_match(document: &Document, selector: &css::Selector) -> Option<NodeId> {
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
    selector: &css::Selector,
    state: &MatchingState,
    ctx: &mut MatchingContext<'_, crate::css::MiniBrowserSelectorImpl>,
) -> Option<NodeId> {
    if matches_static_selector(document, node_id, selector, state, ctx) {
        return Some(node_id);
    }
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
