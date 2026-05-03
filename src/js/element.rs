// Element / Text / Node wrapper factories. Every observable property on
// these wrappers resolves against the shared `Rc<RefCell<Document>>` on each
// access — multiple wrappers may exist for the same NodeId and they all
// observe each other's mutations because they all funnel through the same
// arena. The `_nodeId` hidden property round-trips through any wrapper so
// methods like `parent.appendChild(child)` can recover the receiver and
// argument NodeIds without a parallel handle table.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    Context, JsNativeError, JsObject, JsResult, JsString, JsValue, NativeFunction, js_string,
    object::{ObjectInitializer, builtins::JsArray},
    property::Attribute,
};

use crate::css;
use crate::dom::{Document, NodeId, NodeType};

use super::ListenerMap;
use super::NODE_ID_PROP;
use super::document::{ancestors_outermost_first, matches_static_selector};
use super::util::{first_arg_as_string, nth_arg_as_string, read_node_id};

// Builds an Element wrapper that resolves every observable property against
// the shared Document on each access. Multiple wrappers may exist for the
// same NodeId (e.g. one returned from getElementById, another later from
// `.children[0]`) — they're equivalent because all reads/writes funnel
// through the same `Rc<RefCell<Document>>`.
//
// `tagName` is the only static property: the DOM treats it as readonly, so a
// one-time uppercase snapshot is correct and saves a borrow per access.
// Everything else (textContent, children, getAttribute/setAttribute,
// appendChild/removeChild) is dynamic so post-mutation reads observe the
// new tree.
pub(super) fn make_element(
    node_id: NodeId,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    context: &mut Context,
) -> JsObject {
    let tag = {
        let document = dom.borrow();
        let element = document
            .element_data(node_id)
            .expect("element factory called with non-Element NodeId");
        element.tag_name.to_ascii_uppercase()
    };

    let dom_g = dom.clone();
    let text_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let document = dom_g.borrow();
            // Stale handle (the slot was tombstoned): degrade to null per the
            // Step 5.1.4 silent-degrade policy. Throwing is reserved for a
            // later commit.
            if document.get(node_id).is_none() {
                return Ok(JsValue::null());
            }
            let text = collect_text_content(&document, node_id);
            Ok(JsValue::from(JsString::from(text.as_str())))
        })
    }
    .to_js_function(context.realm());

    let dom_s = dom.clone();
    let text_set = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_text = first_arg_as_string(args, ctx)?;
            // Mutation setters on a stale handle throw — getters keep the
            // older silent-null behaviour because reading a removed node is
            // common (logging, cleanup) and shouldn't blow up scripts, but
            // *writing* through a dead handle is a genuine bug worth
            // surfacing per Step 5.1.5.
            let mut document = dom_s.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            document.replace_with_text(node_id, new_text);
            Ok(JsValue::undefined())
        })
    }
    .to_js_function(context.realm());

    let dom_c = dom.clone();
    let listeners_c = listeners.clone();
    let children_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            // Snapshot the current Element children into a Vec<NodeId> while
            // holding the borrow, then drop it before recursing into
            // make_element — make_element re-borrows to read tag names, so
            // overlapping borrows would panic. Text-only children are
            // filtered out so .children mirrors HTMLCollection (Element
            // kids only) rather than .childNodes.
            let kids: Vec<NodeId> = {
                let document = dom_c.borrow();
                match document.get(node_id) {
                    Some(node) => node
                        .children
                        .iter()
                        .copied()
                        .filter(|cid| {
                            matches!(
                                document.get(*cid).map(|n| &n.node_type),
                                Some(NodeType::Element(_))
                            )
                        })
                        .collect(),
                    None => Vec::new(),
                }
            };
            let array = JsArray::new(ctx);
            for child_id in kids {
                let child_obj = make_element(child_id, dom_c.clone(), listeners_c.clone(), ctx);
                let _ = array.push(JsValue::from(child_obj), ctx);
            }
            Ok(JsValue::from(array))
        })
    }
    .to_js_function(context.realm());

    // `Element.value` accessor: round-trips with the `value` attribute so
    // scripts can read or set the field text without going through
    // get/setAttribute. Defined on every Element wrapper rather than just
    // <input> to match how toy bridge accessors stay tag-agnostic
    // (`children`, `classList`, etc. all do the same) — real DOM only
    // hangs `.value` on form controls, but a `<div>.value = …` here just
    // writes a custom attribute, which is harmless for the toy.
    //
    // Getter returns "" for missing/stale slots (matches the empty-string
    // default real `<input>.value` exposes), while the setter throws on a
    // tombstoned receiver — same write-vs-read stale-handle split the
    // textContent / setAttribute pair already follows.
    let dom_vg = dom.clone();
    let value_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let document = dom_vg.borrow();
            let value = document
                .element_data(node_id)
                .and_then(|elem| elem.attributes.get("value").cloned())
                .unwrap_or_default();
            Ok(JsValue::from(JsString::from(value.as_str())))
        })
    }
    .to_js_function(context.realm());

    let dom_vs = dom.clone();
    let value_set = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_value = first_arg_as_string(args, ctx)?;
            let mut document = dom_vs.borrow_mut();
            match document.element_data_mut(node_id) {
                Some(elem) => {
                    elem.attributes.insert("value".into(), new_value);
                    Ok(JsValue::undefined())
                }
                None => Err(stale_node_error()),
            }
        })
    }
    .to_js_function(context.realm());

    let dom_cl = dom.clone();
    let class_list_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, ctx| {
            // Return a fresh DOMTokenList wrapper on every read. Real
            // browsers cache one per element, but every wrapper observes
            // the same `class` attribute through the shared Document, so
            // a stateless factory is observably equivalent for the toy.
            // No stale-handle check here — the returned object's methods
            // re-borrow on each call and surface the tombstone there.
            Ok(JsValue::from(make_class_list(node_id, dom_cl.clone(), ctx)))
        })
    }
    .to_js_function(context.realm());

    let dom_ga = dom.clone();
    let get_attribute = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let name = first_arg_as_string(args, ctx)?;
            let document = dom_ga.borrow();
            match document.element_data(node_id) {
                Some(elem) => match elem.attributes.get(&name) {
                    Some(value) => Ok(JsValue::from(JsString::from(value.as_str()))),
                    None => Ok(JsValue::null()),
                },
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_sa = dom.clone();
    let set_attribute = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let name = first_arg_as_string(args, ctx)?;
            let value = nth_arg_as_string(args, 1, ctx)?;
            let mut document = dom_sa.borrow_mut();
            match document.element_data_mut(node_id) {
                Some(elem) => {
                    elem.attributes.insert(name, value);
                    Ok(JsValue::undefined())
                }
                None => Err(stale_node_error()),
            }
        })
    };

    let dom_ac = dom.clone();
    let append_child = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let arg = args.first().cloned().unwrap_or_default();
            let other_id = read_node_id(&arg, ctx)?;
            {
                let mut document = dom_ac.borrow_mut();
                // Both ids must point at live slots; per Step 5.1.5 a stale
                // receiver or argument is a script bug, not a no-op.
                if document.get(node_id).is_none() || document.get(other_id).is_none() {
                    return Err(stale_node_error());
                }
                // A node can only live in one parent at a time — unhook it
                // first so we don't end up with the same NodeId in two
                // children lists.
                document.detach(other_id);
                document.append_child(node_id, other_id);
            }
            Ok(arg)
        })
    };

    let dom_rc = dom.clone();
    let remove_child = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let arg = args.first().cloned().unwrap_or_default();
            let other_id = read_node_id(&arg, ctx)?;
            let mut document = dom_rc.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            // The standard throws NotFoundError when the target isn't a
            // direct child; we surface that as a TypeError so callers see
            // a real exception rather than the previous silent-null result.
            if !document.remove_child(node_id, other_id) {
                return Err(JsNativeError::typ()
                    .with_message("removeChild: target is not a child of this node")
                    .into());
            }
            // Toy bridge convention: tombstone the removed subtree so its
            // wrappers cleanly resolve to None on stale-handle checks.
            // A future commit can park the node in a "free" pool if
            // reattachment turns out to matter.
            document.tombstone_subtree(other_id);
            Ok(arg)
        })
    };

    let dom_ib = dom.clone();
    let insert_before = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_arg = args.first().cloned().unwrap_or_default();
            let ref_arg = args.get(1).cloned().unwrap_or_default();
            let new_id = read_node_id(&new_arg, ctx)?;
            // Spec: when refNode is null, insertBefore degrades to
            // appendChild. Resolving the optional id outside the borrow
            // keeps `read_node_id` (which uses ctx) from racing the
            // dom borrow_mut below.
            let ref_id_opt = if ref_arg.is_null() || ref_arg.is_undefined() {
                None
            } else {
                Some(read_node_id(&ref_arg, ctx)?)
            };
            let mut document = dom_ib.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            match ref_id_opt {
                None => {
                    if document.get(new_id).is_none() {
                        return Err(stale_node_error());
                    }
                    document.detach(new_id);
                    document.append_child(node_id, new_id);
                }
                Some(ref_id) => {
                    if !document.insert_before(node_id, new_id, ref_id) {
                        return Err(JsNativeError::typ()
                            .with_message(
                                "insertBefore: reference node is not a child of this node",
                            )
                            .into());
                    }
                }
            }
            Ok(new_arg)
        })
    };

    let dom_rep = dom.clone();
    let replace_child = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_arg = args.first().cloned().unwrap_or_default();
            let old_arg = args.get(1).cloned().unwrap_or_default();
            let new_id = read_node_id(&new_arg, ctx)?;
            let old_id = read_node_id(&old_arg, ctx)?;
            let mut document = dom_rep.borrow_mut();
            if document.get(node_id).is_none() {
                return Err(stale_node_error());
            }
            if !document.replace_child(node_id, new_id, old_id) {
                return Err(JsNativeError::typ()
                    .with_message("replaceChild: target node is not a child of this node")
                    .into());
            }
            // Standard returns the (now-removed) old node. Our tombstoning
            // means subsequent reads on the returned wrapper observe the
            // usual stale-handle semantics — sufficient for the toy bridge.
            Ok(old_arg)
        })
    };

    let dom_cl = dom.clone();
    let listeners_cl = listeners.clone();
    let clone_node = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let deep = args.first().is_some_and(|v| v.to_boolean());
            let new_id = {
                let mut document = dom_cl.borrow_mut();
                document.clone_node(node_id, deep)
            };
            match new_id {
                Some(id) => Ok(
                    make_node(id, dom_cl.clone(), listeners_cl.clone(), ctx)
                        .map(JsValue::from)
                        .unwrap_or(JsValue::null()),
                ),
                None => Err(stale_node_error()),
            }
        })
    };

    let dom_ael = dom.clone();
    let listeners_ael = listeners.clone();
    let add_event_listener = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let event_type = first_arg_as_string(args, ctx)?;
            let handler_obj = args
                .get(1)
                .and_then(|arg| arg.as_object())
                .filter(|obj| obj.is_callable())
                .ok_or_else(|| {
                    JsNativeError::typ()
                        .with_message("addEventListener: handler must be a function")
                })?;
            // Stale receiver: writing through a removed wrapper is the same
            // bug class as the other mutation entry points, so throw rather
            // than silently pile up listeners on a tombstoned node.
            if dom_ael.borrow().get(node_id).is_none() {
                return Err(stale_node_error());
            }
            let mut map = listeners_ael.borrow_mut();
            let entry = map.entry((node_id, event_type)).or_default();
            // Whatwg dedup: same `(target, type, callback)` tuple registered
            // twice is treated as one listener. Identity-compare the
            // underlying JsObject so two distinct `function () {}` literals
            // (different objects, identical bodies) still count as two.
            if !entry
                .iter()
                .any(|existing| JsObject::equals(existing, &handler_obj))
            {
                entry.push(handler_obj);
            }
            Ok(JsValue::undefined())
        })
    };

    let listeners_rel = listeners.clone();
    let remove_event_listener = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let event_type = first_arg_as_string(args, ctx)?;
            // A non-callable / missing second arg is a no-op per spec —
            // there's nothing to match in the registry.
            let Some(handler_obj) = args.get(1).and_then(|arg| arg.as_object()) else {
                return Ok(JsValue::undefined());
            };
            let mut map = listeners_rel.borrow_mut();
            if let Some(entry) = map.get_mut(&(node_id, event_type)) {
                entry.retain(|existing| !JsObject::equals(existing, &handler_obj));
            }
            Ok(JsValue::undefined())
        })
    };

    // Element.matches(selector): does this element itself satisfy the
    // selector? Reuses the same static matcher querySelector funnels
    // through, so the parser's combinator/pseudo handling stays in lock-
    // step across both entry points. Stale receiver returns false (read
    // path, lenient like getAttribute) rather than throwing — matching a
    // detached node legitimately yields "no, it does not match".
    let dom_matches = dom.clone();
    let matches_fn = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let selector_text = first_arg_as_string(args, ctx)?;
            let selector = css::parse_selector(&selector_text).map_err(|err| {
                JsNativeError::syntax().with_message(format!(
                    "invalid selector `{selector_text}`: {} (at byte {})",
                    err.message, err.position
                ))
            })?;
            let document = dom_matches.borrow();
            if document.get(node_id).is_none() {
                return Ok(JsValue::from(false));
            }
            let ancestors = ancestors_outermost_first(&document, node_id);
            Ok(JsValue::from(matches_static_selector(
                &document, node_id, &ancestors, &selector,
            )))
        })
    };

    // Element.closest(selector): walk self-then-ancestors and return the
    // first element wrapper that satisfies the selector, or null. This is
    // the inverse direction of querySelector (down-the-tree) — same
    // matcher, parent chain instead of subtree recursion. The ancestors
    // slice is shrunk by one each step so that descendant/child
    // combinators in the selector are evaluated against the candidate's
    // own ancestor chain on every iteration.
    let dom_closest = dom.clone();
    let listeners_closest = listeners.clone();
    let closest = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let selector_text = first_arg_as_string(args, ctx)?;
            let selector = css::parse_selector(&selector_text).map_err(|err| {
                JsNativeError::syntax().with_message(format!(
                    "invalid selector `{selector_text}`: {} (at byte {})",
                    err.message, err.position
                ))
            })?;
            let found = {
                let document = dom_closest.borrow();
                if document.get(node_id).is_none() {
                    return Ok(JsValue::null());
                }
                let mut ancestors = ancestors_outermost_first(&document, node_id);
                let mut current = Some(node_id);
                let mut hit: Option<NodeId> = None;
                while let Some(id) = current {
                    // Only Element nodes can match a CSS selector — a stray
                    // text/document parent quietly fails the simple-selector
                    // check inside `matches_static_selector`, so the loop
                    // naturally skips non-elements without an explicit guard.
                    if matches_static_selector(&document, id, &ancestors, &selector) {
                        hit = Some(id);
                        break;
                    }
                    let parent = document.get(id).and_then(|n| n.parent);
                    // Drop the candidate's own immediate parent off the
                    // ancestor stack so the next iteration treats it as the
                    // new candidate (its own ancestors are everything above).
                    ancestors.pop();
                    current = parent;
                }
                hit
            };
            match found {
                Some(id) => Ok(JsValue::from(make_element(
                    id,
                    dom_closest.clone(),
                    listeners_closest.clone(),
                    ctx,
                ))),
                None => Ok(JsValue::null()),
            }
        })
    };

    ObjectInitializer::new(context)
        .property(
            js_string!(NODE_ID_PROP),
            JsValue::from(node_id.raw()),
            Attribute::all(),
        )
        .property(
            js_string!("nodeType"),
            JsValue::from(1i32),
            Attribute::all(),
        )
        .property(
            js_string!("tagName"),
            JsString::from(tag.as_str()),
            Attribute::all(),
        )
        .accessor(
            js_string!("textContent"),
            Some(text_get),
            Some(text_set),
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("children"),
            Some(children_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("classList"),
            Some(class_list_get),
            None,
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .accessor(
            js_string!("value"),
            Some(value_get),
            Some(value_set),
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .function(get_attribute, js_string!("getAttribute"), 1)
        .function(set_attribute, js_string!("setAttribute"), 2)
        .function(append_child, js_string!("appendChild"), 1)
        .function(remove_child, js_string!("removeChild"), 1)
        .function(insert_before, js_string!("insertBefore"), 2)
        .function(replace_child, js_string!("replaceChild"), 2)
        .function(clone_node, js_string!("cloneNode"), 1)
        .function(add_event_listener, js_string!("addEventListener"), 2)
        .function(remove_event_listener, js_string!("removeEventListener"), 2)
        .function(matches_fn, js_string!("matches"), 1)
        .function(closest, js_string!("closest"), 1)
        .build()
}

// Wrapper for a Text node — much thinner than Element since text nodes
// only carry a string. The single accessor (`textContent`) doubles as the
// `data` / `nodeValue` getter and setter; the toy bridge skips those alias
// names rather than cloning the closures three times.
//
// `_nodeId` round-trips just like the Element wrapper so methods like
// `parent.appendChild(textNode)` and `parent.replaceChild(text, oldNode)`
// can recover the NodeId without any extra dispatch.
pub(super) fn make_text(
    node_id: NodeId,
    dom: Rc<RefCell<Document>>,
    context: &mut Context,
) -> JsObject {
    let dom_g = dom.clone();
    let text_get = unsafe {
        NativeFunction::from_closure(move |_this, _args, _ctx| {
            let document = dom_g.borrow();
            match document.text(node_id) {
                Some(t) => Ok(JsValue::from(JsString::from(t))),
                None => Ok(JsValue::null()),
            }
        })
    }
    .to_js_function(context.realm());

    let dom_s = dom;
    let text_set = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let new_text = first_arg_as_string(args, ctx)?;
            // `set_text` returns false when the slot is gone OR when it
            // points at an Element. Both are stale-handle scenarios from
            // the script's perspective: throw rather than silently lose
            // the write.
            if !dom_s.borrow_mut().set_text(node_id, new_text) {
                return Err(stale_node_error());
            }
            Ok(JsValue::undefined())
        })
    }
    .to_js_function(context.realm());

    ObjectInitializer::new(context)
        .property(
            js_string!(NODE_ID_PROP),
            JsValue::from(node_id.raw()),
            Attribute::all(),
        )
        .property(
            js_string!("nodeType"),
            JsValue::from(3i32),
            Attribute::all(),
        )
        .accessor(
            js_string!("textContent"),
            Some(text_get),
            Some(text_set),
            Attribute::ENUMERABLE | Attribute::CONFIGURABLE,
        )
        .build()
}

// DOMTokenList wrapper for `Element.classList`. Backs the four methods
// scripts reach for most often: add/remove/toggle/contains. Each method
// re-borrows the shared Document on call so a fresh wrapper is observably
// equivalent to a cached one — every read sees the latest class string.
//
// Stale-handle policy mirrors the rest of the bridge: mutating methods
// (add/remove/toggle) throw when the underlying slot is tombstoned;
// `contains` silently reports false (consistent with attribute getters).
fn make_class_list(
    node_id: NodeId,
    dom: Rc<RefCell<Document>>,
    context: &mut Context,
) -> JsObject {
    let dom_add = dom.clone();
    let add = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            // Validate every token first so a single bad arg leaves the
            // class string untouched (DOMTokenList is atomic per spec).
            let mut new_tokens = Vec::with_capacity(args.len());
            for arg in args {
                let token = arg.to_string(ctx)?.to_std_string_escaped();
                validate_class_token(&token)?;
                new_tokens.push(token);
            }
            let mut document = dom_add.borrow_mut();
            let elem = document
                .element_data_mut(node_id)
                .ok_or_else(stale_node_error)?;
            let mut tokens =
                parse_class_tokens(elem.attributes.get("class").map(String::as_str).unwrap_or(""));
            for token in new_tokens {
                if !tokens.iter().any(|existing| existing == &token) {
                    tokens.push(token);
                }
            }
            elem.attributes.insert("class".into(), tokens.join(" "));
            Ok(JsValue::undefined())
        })
    };

    let dom_remove = dom.clone();
    let remove = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let mut drop_tokens = Vec::with_capacity(args.len());
            for arg in args {
                let token = arg.to_string(ctx)?.to_std_string_escaped();
                validate_class_token(&token)?;
                drop_tokens.push(token);
            }
            let mut document = dom_remove.borrow_mut();
            let elem = document
                .element_data_mut(node_id)
                .ok_or_else(stale_node_error)?;
            let mut tokens =
                parse_class_tokens(elem.attributes.get("class").map(String::as_str).unwrap_or(""));
            tokens.retain(|t| !drop_tokens.iter().any(|d| d == t));
            elem.attributes.insert("class".into(), tokens.join(" "));
            Ok(JsValue::undefined())
        })
    };

    let dom_toggle = dom.clone();
    let toggle = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            // toggle(name [, force]). With `force` undefined the token is
            // flipped; with force=true it's force-added; false force-removed.
            // Returns whether the token is in the list afterwards.
            let token = first_arg_as_string(args, ctx)?;
            validate_class_token(&token)?;
            // toggle's spec says: only an explicitly-supplied non-undefined
            // value forces the outcome; an absent or `undefined` second arg
            // falls back to the flip behaviour.
            let force = match args.get(1) {
                Some(value) if !value.is_undefined() => Some(value.to_boolean()),
                _ => None,
            };
            let mut document = dom_toggle.borrow_mut();
            let elem = document
                .element_data_mut(node_id)
                .ok_or_else(stale_node_error)?;
            let mut tokens =
                parse_class_tokens(elem.attributes.get("class").map(String::as_str).unwrap_or(""));
            let already_present = tokens.iter().any(|t| t == &token);
            let should_be_present = match force {
                Some(forced) => forced,
                None => !already_present,
            };
            if should_be_present && !already_present {
                tokens.push(token);
            } else if !should_be_present && already_present {
                tokens.retain(|t| t != &token);
            }
            elem.attributes.insert("class".into(), tokens.join(" "));
            Ok(JsValue::from(should_be_present))
        })
    };

    let dom_contains = dom;
    let contains = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let token = first_arg_as_string(args, ctx)?;
            // Empty / whitespace-bearing argument is a SyntaxError per spec
            // even on the read side — keeps add/remove/toggle/contains
            // contracts symmetric.
            validate_class_token(&token)?;
            let document = dom_contains.borrow();
            let Some(elem) = document.element_data(node_id) else {
                // Stale read: silent false, same lenient policy as the
                // attribute getters — reads on detached nodes shouldn't
                // crash cleanup code.
                return Ok(JsValue::from(false));
            };
            let tokens =
                parse_class_tokens(elem.attributes.get("class").map(String::as_str).unwrap_or(""));
            Ok(JsValue::from(tokens.iter().any(|t| t == &token)))
        })
    };

    ObjectInitializer::new(context)
        .function(add, js_string!("add"), 1)
        .function(remove, js_string!("remove"), 1)
        .function(toggle, js_string!("toggle"), 1)
        .function(contains, js_string!("contains"), 1)
        .build()
}

// Whitespace-separated tokenisation of the `class` attribute. The HTML
// spec calls this an "ordered set of unique space-separated tokens", but
// callers feed in raw author strings that may contain duplicates — the
// caller dedupes when re-inserting. Empty input -> empty Vec.
fn parse_class_tokens(value: &str) -> Vec<String> {
    value.split_whitespace().map(String::from).collect()
}

// DOMTokenList rejects empty strings and any token containing ASCII
// whitespace as a SyntaxError. The toy enforces both so author code that
// relies on the throw (e.g. validation flows that catch the error) sees
// the same shape.
fn validate_class_token(token: &str) -> JsResult<()> {
    if token.is_empty() {
        return Err(JsNativeError::syntax()
            .with_message("classList: token must not be empty")
            .into());
    }
    if token.chars().any(|c| c.is_ascii_whitespace()) {
        return Err(JsNativeError::syntax()
            .with_message("classList: token must not contain whitespace")
            .into());
    }
    Ok(())
}

// Dispatch helper: hand back the right wrapper for whatever kind of node
// `node_id` happens to be. Used by `cloneNode` (whose result mirrors the
// source's kind) and by anything else that needs to surface a not-yet-typed
// NodeId to JS without the caller pre-computing the variant.
//
// Returns `None` only when the slot is already tombstoned — callers either
// propagate that as a stale-handle throw or fall back to `JsValue::null()`.
fn make_node(
    node_id: NodeId,
    dom: Rc<RefCell<Document>>,
    listeners: Rc<RefCell<ListenerMap>>,
    context: &mut Context,
) -> Option<JsObject> {
    let is_element = {
        let document = dom.borrow();
        document
            .get(node_id)
            .map(|n| matches!(n.node_type, NodeType::Element(_)))
    };
    match is_element {
        Some(true) => Some(make_element(node_id, dom, listeners, context)),
        Some(false) => Some(make_text(node_id, dom, context)),
        None => None,
    }
}

// Standard error returned by every mutation entry point when the receiver
// or argument refers to a slot that's been tombstoned. Step 5.1.5 promoted
// this from the previous silent-null degrade because writing through a
// dead handle is a script bug worth surfacing — getters keep the lenient
// behaviour since reading after removal is a common cleanup pattern.
fn stale_node_error() -> boa_engine::JsError {
    JsNativeError::typ()
        .with_message("operation on detached or removed node")
        .into()
}

fn collect_text_content(document: &Document, node_id: NodeId) -> String {
    let mut buf = String::new();
    walk_text(document, node_id, &mut buf);
    buf
}

fn walk_text(document: &Document, node_id: NodeId, buf: &mut String) {
    let Some(node) = document.get(node_id) else {
        return;
    };
    match &node.node_type {
        NodeType::Text(text) => buf.push_str(text),
        NodeType::Element(_) => {
            for child in &node.children {
                walk_text(document, *child, buf);
            }
        }
    }
}
