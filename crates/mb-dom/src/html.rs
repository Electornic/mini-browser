// Phase 4.1: html5ever drives the actual tokeniser + tree-construction; this
// module is the thin glue that pumps the parser, then walks the resulting
// `markup5ever_rcdom::RcDom` tree into our `Document` arena.
//
// We pick between two of html5ever's entry points based on what the source
// explicitly wrote:
//   * If the source contains `<html>`, `<head>`, or `<body>` openers, we use
//     `parse_document` so those wrappers survive intact (tests style the
//     real `<body>` and read `document.body` / `document.head`).
//   * Otherwise we use `parse_fragment` with a `<body>` context. The tree
//     builder is already in "in body" insertion mode, so siblings written
//     at the top of the source — including `<script>` and `<style>` — stay
//     siblings instead of getting hoisted into a synthetic `<head>`.

use crate::dom::{AttrMap, Document, NodeId};

use html5ever::tendril::TendrilSink;
use html5ever::tree_builder::TreeBuilderOpts;
use html5ever::{
    LocalName, ParseOpts, QualName, ns, parse_document as h5e_document,
    parse_fragment as h5e_fragment,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom};

/// Parser error — kept for source compatibility with the previous hand-rolled
/// parser. html5ever's whole point is permissive recovery, so we never produce
/// `Err` in practice; the type still exists because `navigation.rs` and
/// `js/element.rs` formatted error fields and we'd rather not churn callers
/// just to delete an unused branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub position: usize,
    pub message: String,
}

/// The deepest an element can sit in a parsed tree (counting the arena root
/// as 1). Deeper content is attached to the element at this depth's parent
/// instead — it becomes a run of siblings at the cap, in document order —
/// the same idea as Blink/WebKit's `kMaximumHTMLParserDOMTreeDepth` (512).
///
/// Ours is lower because our passes recurse per level and layout is the
/// hungriest: measured with nested `<div>`s, the full pipeline plus the
/// agent's observation overflowed at ~540 levels in a debug build on an
/// 8 MB stack (the browser's main thread) and ~980 in release on a 2 MB
/// thread (docs/compat-round3.md D1). 256 leaves ≥ 2× headroom on both.
/// A stack overflow is an abort, not a panic — `catch_unwind` can't save
/// the session. Trees built by script (`appendChild` in a loop) are not
/// capped.
pub const MAX_DOM_DEPTH: usize = 256;

pub fn parse(source: &str) -> Result<Document, ParseError> {
    let mut document = Document::new();

    let saw_html = contains_tag_open(source, "html");
    let saw_head = contains_tag_open(source, "head");
    let saw_body = contains_tag_open(source, "body");

    if saw_html || saw_head || saw_body {
        let dom = h5e_document(RcDom::default(), parser_opts()).one(source);
        // `parse_document` always builds  #Document → <html> → [<head>, <body>],
        // synthesising any wrapper the user didn't write. We expose only the
        // wrappers the source actually contained — that keeps `roots()[0]` ==
        // `<body>` for the common `<body>...</body>` test fixture, while still
        // letting `document.body` / `document.head` resolve to the real
        // elements when the source spelt them out.
        if let Some(html_handle) = first_element_named(&dom.document.children.borrow(), "html") {
            for child in html_handle.children.borrow().iter() {
                if let NodeData::Element { name, .. } = &child.data {
                    let include = match name.local.as_ref() {
                        "head" => saw_head,
                        "body" => saw_body,
                        _ => true,
                    };
                    if include && let Some(id) = build_into_arena(&mut document, child) {
                        document.append_root(id);
                    }
                }
            }
        }
    } else {
        let roots = parse_fragment(source, &mut document)?;
        for root in roots {
            document.append_root(root);
        }
    }
    Ok(document)
}

/// Parse `source` as a fragment — zero or more sibling nodes — into the
/// existing `document`. Returns the freshly created top-level NodeIds; they
/// are detached (not in `document.roots()` and have no parent) so callers
/// can splice them under any existing element via `append_child`. Used by
/// the JS `innerHTML` setter to swap an element's children without
/// destroying the rest of the tree.
pub fn parse_fragment(source: &str, document: &mut Document) -> Result<Vec<NodeId>, ParseError> {
    let context = QualName::new(None, ns!(html), LocalName::from("body"));
    // Last arg = scripting_enabled. We run scripts ourselves on top of the
    // arena (`BrowserState::run_scripts`), so the parser must leave inline
    // <script> textContent intact rather than swallowing it the way it would
    // for a host that runs scripts itself.
    let dom = h5e_fragment(RcDom::default(), parser_opts(), context, Vec::new(), false).one(source);

    // `parse_fragment` builds:
    //   #Document → <html> (synthetic context wrapper) → ...real siblings...
    let mut roots = Vec::new();
    let document_kids = dom.document.children.borrow();
    if let Some(context_root) = document_kids.first() {
        for child in context_root.children.borrow().iter() {
            if let Some(node_id) = build_into_arena(document, child) {
                roots.push(node_id);
            }
        }
    }
    Ok(roots)
}

fn parser_opts() -> ParseOpts {
    ParseOpts {
        tree_builder: TreeBuilderOpts {
            scripting_enabled: false,
            drop_doctype: true,
            ..TreeBuilderOpts::default()
        },
        ..ParseOpts::default()
    }
}

fn first_element_named<'a>(handles: &'a [Handle], local: &str) -> Option<&'a Handle> {
    handles.iter().find(|h| {
        matches!(
            &h.data,
            NodeData::Element { name, .. } if &*name.local == local
        )
    })
}

/// Cheap heuristic: did the source spell out `<tag` followed by a non-name
/// character? Catches `<body>`, `<body ...>`, `<BODY/>` (case-insensitive).
/// Misses content inside HTML comments, but our test inputs don't put real
/// `<body>` strings inside comments and a false positive only forces the
/// heavier `parse_document` path — still produces correct output.
fn contains_tag_open(source: &str, tag: &str) -> bool {
    let needle = format!("<{tag}");
    let bytes = source.as_bytes();
    let needle_bytes = needle.as_bytes();
    let mut i = 0;
    while i + needle_bytes.len() <= bytes.len() {
        if bytes[i..i + needle_bytes.len()].eq_ignore_ascii_case(needle_bytes) {
            let next = bytes.get(i + needle_bytes.len()).copied().unwrap_or(b' ');
            if !next.is_ascii_alphanumeric() && next != b'-' && next != b'_' {
                return true;
            }
        }
        i += 1;
    }
    false
}

/// Translate one rcdom `Handle` and its subtree into our arena. Returns
/// `None` for nodes we don't model (Document, Doctype, Comment,
/// ProcessingInstruction) — those are simply skipped along with their
/// subtree.
///
/// Iterative, and depth-capped at [`MAX_DOM_DEPTH`]: the children of an
/// element at the cap are attached to that element's parent instead, right
/// after it (pre-order = document order). rcdom itself drops iteratively,
/// so a 100 000-deep source is fine up to here.
fn build_into_arena(document: &mut Document, handle: &Handle) -> Option<NodeId> {
    let root = create_node(document, handle)?;
    // (handle, arena parent, depth the handle will sit at)
    let mut stack: Vec<(Handle, NodeId, usize)> = Vec::new();
    push_children(&mut stack, handle, root, 2);
    while let Some((handle, parent, depth)) = stack.pop() {
        let Some(id) = create_node(document, &handle) else {
            continue;
        };
        document.append_child(parent, id);
        if depth < MAX_DOM_DEPTH {
            push_children(&mut stack, &handle, id, depth + 1);
        } else {
            push_children(&mut stack, &handle, parent, depth);
        }
    }
    Some(root)
}

/// Queue `handle`'s children (reversed, so they pop in document order) to
/// be attached under `parent` at `depth`.
fn push_children(
    stack: &mut Vec<(Handle, NodeId, usize)>,
    handle: &Handle,
    parent: NodeId,
    depth: usize,
) {
    for child in handle.children.borrow().iter().rev() {
        stack.push((child.clone(), parent, depth));
    }
}

/// The arena node for one rcdom node, without its children.
fn create_node(document: &mut Document, handle: &Handle) -> Option<NodeId> {
    match &handle.data {
        NodeData::Element { name, attrs, .. } => {
            let mut attrmap = AttrMap::new();
            for attr in attrs.borrow().iter() {
                // Flatten namespaced attribute names to their local part —
                // our AttrMap is `BTreeMap<String, String>`, so we'd lose
                // any prefix anyway, and the rest of the engine only ever
                // reads attributes by their local name.
                attrmap.insert(attr.name.local.to_string(), attr.value.to_string());
            }
            Some(document.create_element(name.local.to_string(), attrmap))
        }
        NodeData::Text { contents } => Some(document.create_text(contents.borrow().to_string())),
        _ => None,
    }
}

/// HTML5 void elements — see <https://html.spec.whatwg.org/#void-elements>.
/// The JS `outerHTML` getter consults this to decide whether to emit a
/// closing tag for an element that html5ever's tokenizer treats as
/// self-closing.
pub fn is_void_element(tag_name: &str) -> bool {
    matches!(
        tag_name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "source"
            | "track"
            | "wbr"
    )
}

#[cfg(test)]
mod tests {
    use super::{MAX_DOM_DEPTH, parse, parse_fragment};
    use crate::dom::{Document, NodeId, NodeType};

    /// (deepest element depth, element count, text) — iteratively, because
    /// the point is trees too deep to recurse over.
    fn shape(document: &Document, root: NodeId) -> (usize, usize, String) {
        let (mut deepest, mut elements, mut text) = (0, 0, String::new());
        let mut stack = vec![(root, 1)];
        while let Some((id, depth)) = stack.pop() {
            let node = document.get(id).expect("live node");
            match &node.node_type {
                NodeType::Element(_) => {
                    elements += 1;
                    deepest = deepest.max(depth);
                }
                NodeType::Text(t) => text.push_str(t),
            }
            for child in node.children.iter().rev() {
                stack.push((*child, depth + 1));
            }
        }
        (deepest, elements, text)
    }

    #[test]
    fn deeply_nested_source_is_flattened_at_the_depth_cap() {
        // Every pass over the tree (style, layout, paint, the agent's index)
        // recurses per level; ~10 000 nested `<div>`s overflowed the stack —
        // an abort that `catch_unwind` can't catch. Like Blink/WebKit's
        // parser cap, content deeper than the cap becomes siblings at the
        // cap's level: nothing is dropped, document order is kept.
        let depth = MAX_DOM_DEPTH + 44;
        let mut source = String::new();
        for i in 0..depth {
            source.push_str(&format!("<div>{}", i % 10));
        }
        source.push_str(&"</div>".repeat(depth));
        let document = parse(&source).unwrap();
        let root = document.roots()[0];
        let (deepest, elements, text) = shape(&document, root);
        assert_eq!(deepest, MAX_DOM_DEPTH);
        assert_eq!(elements, depth, "no element is lost");
        let expected: String = (0..depth)
            .map(|i| char::from(b'0' + (i % 10) as u8))
            .collect();
        assert_eq!(text, expected, "document order is kept");
    }

    #[test]
    fn fragment_parsing_has_the_same_depth_cap() {
        let source = "<span>".repeat(MAX_DOM_DEPTH * 2);
        let mut document = Document::new();
        let roots = parse_fragment(&source, &mut document).unwrap();
        let (deepest, elements, _) = shape(&document, roots[0]);
        assert_eq!(deepest, MAX_DOM_DEPTH);
        assert_eq!(elements, MAX_DOM_DEPTH * 2);
    }
}
