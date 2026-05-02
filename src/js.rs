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
    dom::{AttrMap, Document, NodeId, NodeType},
};

// Hidden property name used to round-trip a NodeId through any Element
// JsObject — methods like `appendChild(other)` read `other._nodeId` to
// recover the receiver's NodeId without an external wrapper-to-NodeId table.
// JS code shouldn't poke at this; the dynamic mutation methods all
// re-validate the recovered id against the live arena before acting on it.
const NODE_ID_PROP: &str = "_nodeId";

pub struct JsRuntime {
    context: Context,
    // Shared handle to the parsed Document. BrowserState constructs the Rc
    // and hands a clone to the runtime; that clone is then handed to every
    // native closure registered on `document` and on each Element wrapper,
    // so each closure observes the same up-to-date tree without ever
    // re-reading the field on the struct. The field is therefore live only
    // through the closures (and through `dom_handle` in tests) — `dead_code`
    // is silenced to make that intent explicit rather than have us drop the
    // canonical handle and reach into the Context for it later.
    #[allow(dead_code)]
    dom: Rc<RefCell<Document>>,
}

impl JsRuntime {
    /// Build a runtime bound to `dom`. The caller keeps a clone of the Rc so
    /// it can read the DOM back (for layout) and mutate it directly (for
    /// page swaps via `*dom.borrow_mut() = …`); JS-side mutations land in the
    /// same arena. Per Step 5.1, ownership is shared at construction time —
    /// there is no `bind_document` afterward because nothing inside the engine
    /// needs to switch Documents mid-life: a navigation rebuilds JsRuntime.
    pub fn new(dom: Rc<RefCell<Document>>) -> Self {
        let mut context = Context::default();
        register_console(&mut context);
        register_document(&mut context, dom.clone());
        Self { context, dom }
    }

    /// Returns a clone of the shared DOM handle. Mainly useful in tests where
    /// the test wants to swap the document contents under the runtime to
    /// simulate a navigation.
    #[cfg(test)]
    pub fn dom_handle(&self) -> Rc<RefCell<Document>> {
        self.dom.clone()
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

// Builds the `document` global. Each method captures its own `Rc` clone of
// the shared Document handle so they stay valid after `register_document`
// returns. The closures use `unsafe from_closure` because our captures
// (Rc<RefCell<Document>>) are pure host data — no JS values hide inside, so
// Boa's GC has nothing to trace through them.
fn register_document(context: &mut Context, dom: Rc<RefCell<Document>>) {
    let dom_for_id = dom.clone();
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
                Some(node_id) => Ok(JsValue::from(make_element(node_id, dom_for_id.clone(), ctx))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_for_qs = dom.clone();
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
                Some(node_id) => Ok(JsValue::from(make_element(node_id, dom_for_qs.clone(), ctx))),
                None => Ok(JsValue::null()),
            }
        })
    };

    let dom_for_create = dom.clone();
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
        .build();

    let _ = context.register_global_property(js_string!("document"), document, Attribute::all());
}

fn first_arg_as_string(args: &[JsValue], context: &mut Context) -> JsResult<String> {
    nth_arg_as_string(args, 0, context)
}

fn nth_arg_as_string(args: &[JsValue], n: usize, context: &mut Context) -> JsResult<String> {
    let arg = args.get(n).cloned().unwrap_or_default();
    Ok(arg.to_string(context)?.to_std_string_escaped())
}

// Recovers a NodeId from any Element wrapper by reading the hidden `_nodeId`
// data property the wrapper factory stored. Returns Err for non-Element
// arguments (foreign objects, primitives) — that's the TypeError the DOM
// methods report.
fn read_node_id(arg: &JsValue, context: &mut Context) -> JsResult<NodeId> {
    let object = arg.as_object().ok_or_else(|| {
        JsNativeError::typ().with_message("expected an Element-like argument")
    })?;
    let raw = object
        .get(js_string!(NODE_ID_PROP), context)?
        .to_u32(context)?;
    Ok(NodeId::from_raw(raw))
}

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
fn make_element(node_id: NodeId, dom: Rc<RefCell<Document>>, context: &mut Context) -> JsObject {
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
                let child_obj = make_element(child_id, dom_c.clone(), ctx);
                let _ = array.push(JsValue::from(child_obj), ctx);
            }
            Ok(JsValue::from(array))
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
    let clone_node = unsafe {
        NativeFunction::from_closure(move |_this, args, ctx| {
            let deep = args.first().is_some_and(|v| v.to_boolean());
            let new_id = {
                let mut document = dom_cl.borrow_mut();
                document.clone_node(node_id, deep)
            };
            match new_id {
                Some(id) => Ok(make_node(id, dom_cl.clone(), ctx)
                    .map(JsValue::from)
                    .unwrap_or(JsValue::null())),
                None => Err(stale_node_error()),
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
        .function(get_attribute, js_string!("getAttribute"), 1)
        .function(set_attribute, js_string!("setAttribute"), 2)
        .function(append_child, js_string!("appendChild"), 1)
        .function(remove_child, js_string!("removeChild"), 1)
        .function(insert_before, js_string!("insertBefore"), 2)
        .function(replace_child, js_string!("replaceChild"), 2)
        .function(clone_node, js_string!("cloneNode"), 1)
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
fn make_text(node_id: NodeId, dom: Rc<RefCell<Document>>, context: &mut Context) -> JsObject {
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
    context: &mut Context,
) -> Option<JsObject> {
    let is_element = {
        let document = dom.borrow();
        document
            .get(node_id)
            .map(|n| matches!(n.node_type, NodeType::Element(_)))
    };
    match is_element {
        Some(true) => Some(make_element(node_id, dom, context)),
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

// Mirrors style::matches_selector but skips pseudo-class state — querySelector
// is a static lookup against the parsed Document, no hover/focus context to
// thread through. Pseudo-classes parse-but-ignore here: `.btn:hover` matches
// the same set as `.btn`.
fn matches_static_selector(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    fn runtime_with(html: &str) -> JsRuntime {
        let document = html::parse(html).unwrap();
        let dom = Rc::new(RefCell::new(document));
        JsRuntime::new(dom)
    }

    #[test]
    fn evaluates_arithmetic() {
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("1 + 2 * 3").unwrap(), "7");
    }

    #[test]
    fn preserves_global_state_between_calls() {
        let mut runtime = runtime_with("");
        runtime.execute("var page = 41;").unwrap();
        assert_eq!(runtime.execute("page + 1").unwrap(), "42");
    }

    #[test]
    fn surfaces_runtime_errors() {
        let mut runtime = runtime_with("");
        let err = runtime.execute("missing.prop").unwrap_err();
        assert!(
            err.to_lowercase().contains("missing"),
            "error should reference the missing identifier, got: {err}"
        );
    }

    #[test]
    fn evaluates_string_concatenation() {
        let mut runtime = runtime_with("");
        assert_eq!(
            runtime.execute("'hello, ' + 'world'").unwrap(),
            "\"hello, world\""
        );
    }

    #[test]
    fn console_object_is_registered_with_log_warn_error() {
        let mut runtime = runtime_with("");
        assert_eq!(runtime.execute("typeof console").unwrap(), "\"object\"");
        assert_eq!(runtime.execute("typeof console.log").unwrap(), "\"function\"");
        assert_eq!(runtime.execute("typeof console.warn").unwrap(), "\"function\"");
        assert_eq!(runtime.execute("typeof console.error").unwrap(), "\"function\"");
    }

    #[test]
    fn console_log_returns_undefined_and_does_not_throw() {
        let mut runtime = runtime_with("");
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
        assert_eq!(
            runtime.execute("typeof document.createElement").unwrap(),
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
    fn swapping_dom_under_runtime_redirects_subsequent_lookups() {
        // The closures capture an Rc<RefCell<…>>, not a Document snapshot —
        // so replacing the inner Document under the runtime must redirect
        // the next getElementById call. This is the contract BrowserState
        // relies on for navigation: a fresh JsRuntime is built per page,
        // but the test exercises the in-place swap path that the production
        // arena now also uses for read-only DOM updates.
        let mut runtime = runtime_with(r#"<p id="a">first</p>"#);
        assert_eq!(
            runtime
                .execute("document.getElementById('a').textContent")
                .unwrap(),
            "\"first\""
        );
        let next = html::parse(r#"<p id="b">second</p>"#).unwrap();
        *runtime.dom_handle().borrow_mut() = next;
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

    // ---- Step 5.1 mutation API ----

    #[test]
    fn text_content_setter_replaces_descendants_with_text() {
        let mut runtime = runtime_with(r#"<div id="host"><span>old</span><b>more</b></div>"#);
        runtime
            .execute("document.getElementById('host').textContent = 'fresh';")
            .unwrap();
        // Subsequent reads observe the new text; the old element children
        // are gone (children list now empty since the only child is text).
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"fresh\""
        );
        assert_eq!(
            runtime
                .execute("document.getElementById('host').children.length")
                .unwrap(),
            "0"
        );
    }

    #[test]
    fn set_attribute_round_trips_through_get_attribute() {
        let mut runtime = runtime_with(r#"<a id="link">x</a>"#);
        runtime
            .execute("document.getElementById('link').setAttribute('href', '/about');")
            .unwrap();
        assert_eq!(
            runtime
                .execute("document.getElementById('link').getAttribute('href')")
                .unwrap(),
            "\"/about\""
        );
    }

    #[test]
    fn append_child_attaches_freshly_created_element_into_parent() {
        let mut runtime = runtime_with(r#"<div id="host"></div>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var p = document.createElement('p');\
                 p.textContent = 'inserted';\
                 host.appendChild(p);",
            )
            .unwrap();
        assert_eq!(runtime.execute("host.children.length").unwrap(), "1");
        assert_eq!(
            runtime.execute("host.children[0].tagName").unwrap(),
            "\"P\""
        );
        assert_eq!(
            runtime.execute("host.children[0].textContent").unwrap(),
            "\"inserted\""
        );
    }

    #[test]
    fn append_child_reparents_existing_node_rather_than_duplicating() {
        let mut runtime = runtime_with(
            r#"<div id="src"><span id="movable">m</span></div><div id="dst"></div>"#,
        );
        runtime
            .execute(
                "var src = document.getElementById('src');\
                 var dst = document.getElementById('dst');\
                 var m = document.getElementById('movable');\
                 dst.appendChild(m);",
            )
            .unwrap();
        // The node moved — src is empty and dst owns it now.
        assert_eq!(runtime.execute("src.children.length").unwrap(), "0");
        assert_eq!(runtime.execute("dst.children.length").unwrap(), "1");
        assert_eq!(
            runtime
                .execute("dst.children[0].getAttribute('id')")
                .unwrap(),
            "\"movable\""
        );
    }

    #[test]
    fn element_and_text_wrappers_expose_node_type() {
        // 1 = ELEMENT_NODE, 3 = TEXT_NODE per the standard. The toy bridge
        // exposes just these two — the rest (comment, document, etc.) aren't
        // produced by the parser or by any createX() factory.
        let mut runtime = runtime_with(r#"<div id="x">y</div>"#);
        assert_eq!(
            runtime.execute("document.getElementById('x').nodeType").unwrap(),
            "1"
        );
        assert_eq!(
            runtime.execute("document.createTextNode('hi').nodeType").unwrap(),
            "3"
        );
    }

    #[test]
    fn create_text_node_returns_text_wrapper_appendable_into_a_parent() {
        let mut runtime = runtime_with(r#"<p id="host"></p>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var t = document.createTextNode('first ');\
                 host.appendChild(t);\
                 host.appendChild(document.createTextNode('second'));",
            )
            .unwrap();
        // textContent walks all descendants so two adjacent text nodes
        // surface as the concatenated string.
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"first second\""
        );
    }

    #[test]
    fn text_node_text_content_is_writable() {
        // The Text wrapper's textContent doubles as `data`/`nodeValue` —
        // setting it edits the text in place rather than replacing the node.
        let mut runtime = runtime_with(r#"<p id="host"></p>"#);
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var t = document.createTextNode('initial');\
                 host.appendChild(t);\
                 t.textContent = 'updated';",
            )
            .unwrap();
        assert_eq!(
            runtime
                .execute("document.getElementById('host').textContent")
                .unwrap(),
            "\"updated\""
        );
    }

    #[test]
    fn insert_before_places_node_at_ref_child_position() {
        let mut runtime = runtime_with(
            r#"<ul id="list"><li id="a">a</li><li id="c">c</li></ul>"#,
        );
        runtime
            .execute(
                "var list = document.getElementById('list');\
                 var c = document.getElementById('c');\
                 var b = document.createElement('li');\
                 b.textContent = 'b';\
                 list.insertBefore(b, c);",
            )
            .unwrap();
        // Final order is a, b, c.
        assert_eq!(runtime.execute("list.children.length").unwrap(), "3");
        assert_eq!(
            runtime.execute("list.children[0].textContent").unwrap(),
            "\"a\""
        );
        assert_eq!(
            runtime.execute("list.children[1].textContent").unwrap(),
            "\"b\""
        );
        assert_eq!(
            runtime.execute("list.children[2].textContent").unwrap(),
            "\"c\""
        );
    }

    #[test]
    fn insert_before_with_null_ref_appends_to_end() {
        // Spec: insertBefore(node, null) === appendChild(node). Useful for
        // generic insertion code that doesn't special-case the empty list.
        let mut runtime = runtime_with(r#"<ul id="list"><li>first</li></ul>"#);
        runtime
            .execute(
                "var list = document.getElementById('list');\
                 var x = document.createElement('li');\
                 x.textContent = 'tail';\
                 list.insertBefore(x, null);",
            )
            .unwrap();
        assert_eq!(runtime.execute("list.children.length").unwrap(), "2");
        assert_eq!(
            runtime.execute("list.children[1].textContent").unwrap(),
            "\"tail\""
        );
    }

    #[test]
    fn insert_before_throws_when_ref_is_not_a_child() {
        let mut runtime = runtime_with(
            r#"<div id="a"><p id="kid">k</p></div><div id="b"></div>"#,
        );
        let err = runtime
            .execute(
                "var a = document.getElementById('a');\
                 var b = document.getElementById('b');\
                 var kid = document.getElementById('kid');\
                 var x = document.createElement('span');\
                 b.insertBefore(x, kid);",
            )
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("insertbefore"),
            "expected insertBefore TypeError, got: {err}"
        );
    }

    #[test]
    fn replace_child_swaps_node_and_tombstones_the_old_subtree() {
        let mut runtime = runtime_with(
            r#"<section id="host"><p id="old">old</p></section>"#,
        );
        runtime
            .execute(
                "var host = document.getElementById('host');\
                 var oldNode = document.getElementById('old');\
                 var fresh = document.createElement('p');\
                 fresh.textContent = 'fresh';\
                 host.replaceChild(fresh, oldNode);",
            )
            .unwrap();
        // The replacement is in place …
        assert_eq!(runtime.execute("host.children.length").unwrap(), "1");
        assert_eq!(
            runtime.execute("host.children[0].textContent").unwrap(),
            "\"fresh\""
        );
        // … and the old node is gone from the document entirely.
        assert_eq!(
            runtime.execute("document.getElementById('old')").unwrap(),
            "null"
        );
    }

    #[test]
    fn clone_node_shallow_drops_descendants_and_does_not_attach() {
        let mut runtime = runtime_with(
            r#"<div id="src"><span>kid</span></div>"#,
        );
        runtime
            .execute(
                "var src = document.getElementById('src');\
                 var dup = src.cloneNode(false);",
            )
            .unwrap();
        // Same tag, no children, not yet in the document.
        assert_eq!(runtime.execute("dup.tagName").unwrap(), "\"DIV\"");
        assert_eq!(runtime.execute("dup.children.length").unwrap(), "0");
        // Original is untouched.
        assert_eq!(runtime.execute("src.children.length").unwrap(), "1");
    }

    #[test]
    fn clone_node_deep_duplicates_subtree_into_fresh_handles() {
        let mut runtime = runtime_with(
            r#"<ul id="src"><li>one</li><li>two</li></ul>"#,
        );
        // Mutating the original after the clone confirms independence —
        // a shared subtree would let the textContent= rewrite collapse the
        // clone too, surfacing as a length=0 in the next assertion.
        runtime
            .execute(
                "var src = document.getElementById('src');\
                 var dup = src.cloneNode(true);\
                 src.textContent = 'wiped';",
            )
            .unwrap();
        // The clone keeps both <li> children with their text.
        assert_eq!(runtime.execute("dup.children.length").unwrap(), "2");
        assert_eq!(
            runtime.execute("dup.children[0].textContent").unwrap(),
            "\"one\""
        );
        assert_eq!(
            runtime.execute("dup.children[1].textContent").unwrap(),
            "\"two\""
        );
    }

    #[test]
    fn mutation_setter_throws_on_a_stale_handle() {
        // Step 5.1.5: writing through a removed wrapper raises rather than
        // silently dropping the write. Reading (textContent get on a stale
        // handle) keeps the previous null-degrade behaviour — that path is
        // exercised by `remove_child_unhooks_node_and_invalidates_its_handle`.
        let mut runtime = runtime_with(r#"<ul id="list"><li id="kid">a</li></ul>"#);
        runtime
            .execute("var kid = document.getElementById('kid'); document.getElementById('list').removeChild(kid);")
            .unwrap();
        let err = runtime.execute("kid.textContent = 'x';").unwrap_err();
        assert!(
            err.to_lowercase().contains("detached") || err.to_lowercase().contains("removed"),
            "expected stale-handle TypeError, got: {err}"
        );
    }

    #[test]
    fn append_child_throws_when_receiver_is_stale() {
        let mut runtime = runtime_with(
            r#"<div id="parent"><div id="host"><p>old</p></div></div>"#,
        );
        runtime
            .execute(
                "var parent = document.getElementById('parent');\
                 var host = document.getElementById('host');\
                 parent.removeChild(host);",
            )
            .unwrap();
        let err = runtime
            .execute("host.appendChild(document.createElement('span'));")
            .unwrap_err();
        assert!(
            err.to_lowercase().contains("detached") || err.to_lowercase().contains("removed"),
            "expected stale-handle TypeError, got: {err}"
        );
    }

    #[test]
    fn remove_child_unhooks_node_and_invalidates_its_handle() {
        let mut runtime = runtime_with(r#"<ul id="list"><li id="kid">a</li></ul>"#);
        runtime
            .execute(
                "var list = document.getElementById('list');\
                 var kid = document.getElementById('kid');\
                 list.removeChild(kid);",
            )
            .unwrap();
        // Parent observes the removal.
        assert_eq!(runtime.execute("list.children.length").unwrap(), "0");
        // Stale handle: textContent on the removed wrapper degrades to null
        // per the Step 5.1.4 silent-degrade policy.
        assert_eq!(runtime.execute("kid.textContent").unwrap(), "null");
        // Re-querying the document confirms the node is gone, not just
        // unhooked from `list`.
        assert_eq!(
            runtime.execute("document.getElementById('kid')").unwrap(),
            "null"
        );
    }
}
