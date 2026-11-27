// Phase 4.1: html5ever drives the actual tokeniser + tree-construction; this
// module is the thin glue that pumps the parser, then walks the resulting
// `markup5ever_rcdom::RcDom` tree into our `Document` arena.
//
// We pick between two of html5ever's entry points based on what the source
// explicitly wrote:
//   * If the source contains `<html>`, `<head>`, or `<body>` openers, we use
//     `parse_document` so those wrappers survive intact (tests style the
//     real `<body>` and read `document.body` / `document.head`). With
//     `<html` (every real page) the `<html>` element is the single root,
//     as in the spec tree; with only `<head>`/`<body>` those are the roots.
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

/// The deepest a node can sit in a parsed tree (counting the tree's root
/// as 1) — the document-wide cap, defined with its rationale in
/// [`crate::dom::MAX_DOM_DEPTH`]. The parser **flattens** at it: deeper
/// content is attached to the capped element's parent instead — a run of
/// siblings at the cap, in document order — the same idea as Blink/WebKit's
/// `kMaximumHTMLParserDOMTreeDepth` (512). Script mutations are refused
/// instead (`dom.rs`).
pub use crate::dom::MAX_DOM_DEPTH;

pub fn parse(source: &str) -> Result<Document, ParseError> {
    let mut document = Document::new();

    let saw_html = contains_tag_open(source, "html");
    let saw_head = contains_tag_open(source, "head");
    let saw_body = contains_tag_open(source, "body");

    if saw_html || saw_head || saw_body {
        let dom = h5e_document(RcDom::default(), parser_opts()).one(source);
        // `parse_document` always builds  #Document → <html> → [<head>, <body>],
        // synthesising any wrapper the source didn't write.
        let document_kids = dom.document.children.borrow();
        let html_handle = first_element_named(&document_kids, "html");
        if saw_html {
            // A real page: the `<html>` element (attributes kept, for
            // `html[data-theme]` / `html.dark`) is the single root with the
            // head and body html5ever built under it — the spec tree, so
            // `:root`, inheritance from `<html>` and rem work (compat round
            // 3 step 1r).
            if let Some(html_handle) = html_handle
                && let Some(id) = build_into_arena(&mut document, html_handle)
            {
                document.append_root(id);
            }
        } else if let Some(html_handle) = html_handle {
            // Only `<head>`/`<body>` were spelt out (test fixtures): expose
            // just those wrappers as roots — `roots()[0]` == `<body>` for the
            // common `<body>...</body>` fixture, and `document.body` /
            // `document.head` still resolve to the real elements.
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
/// can splice them under any existing element via `append_child`. The
/// top-level nodes count as depth 1 for the cap; to insert under an
/// element, use [`parse_fragment_at`] with that element's depth + 1.
pub fn parse_fragment(source: &str, document: &mut Document) -> Result<Vec<NodeId>, ParseError> {
    parse_fragment_at(source, document, 1)
}

/// [`parse_fragment`] for nodes that will sit at `depth` (the context
/// element's depth + 1) — the JS `innerHTML` setter. Content that would go
/// past [`MAX_DOM_DEPTH`] is flattened as in a document parse; a fragment
/// whose top level itself would be past the cap yields no nodes at all
/// (the caller refuses).
pub fn parse_fragment_at(
    source: &str,
    document: &mut Document,
    depth: usize,
) -> Result<Vec<NodeId>, ParseError> {
    if depth > MAX_DOM_DEPTH {
        return Ok(Vec::new());
    }
    let context = QualName::new(None, ns!(html), LocalName::from("body"));
    // Last arg = scripting_enabled. We run scripts ourselves on top of the
    // arena (`BrowserState::run_scripts`), so the parser must leave inline
    // <script> textContent intact rather than swallowing it the way it would
    // for a host that runs scripts itself.
    let dom = h5e_fragment(RcDom::default(), parser_opts(), context, Vec::new(), false).one(source);

    // `parse_fragment` builds:
    //   #Document → <html> (synthetic context wrapper) → ...real siblings...
    let document_kids = dom.document.children.borrow();
    let Some(context_root) = document_kids.first() else {
        return Ok(Vec::new());
    };
    let top_level: Vec<Handle> = context_root.children.borrow().clone();
    Ok(build_nodes(document, &top_level, depth))
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

/// Translate one rcdom `Handle` and its subtree into our arena (a document
/// root: depth 1). `None` for nodes we don't model.
fn build_into_arena(document: &mut Document, handle: &Handle) -> Option<NodeId> {
    build_nodes(document, std::slice::from_ref(handle), 1)
        .into_iter()
        .next()
}

/// Translate rcdom `handles` (siblings) and their subtrees into our arena;
/// returns the top-level NodeIds, detached, in document order. The handles
/// sit at `top_depth` (≤ [`MAX_DOM_DEPTH`]). Nodes we don't model (Document,
/// Doctype, Comment, ProcessingInstruction) are skipped with their subtree.
///
/// Iterative, and depth-capped at [`MAX_DOM_DEPTH`]: the children of a node
/// at the cap are attached to that node's parent instead, right after it
/// (pre-order = document order) — for a top-level node at the cap, they
/// become top-level nodes too. rcdom itself drops iteratively, so a
/// 100 000-deep source is fine up to here. Fresh nodes and the cap make
/// the unchecked attach safe.
fn build_nodes(document: &mut Document, handles: &[Handle], top_depth: usize) -> Vec<NodeId> {
    let mut top = Vec::new();
    // (handle, arena parent — None = top level, depth the handle will sit at)
    let mut stack: Vec<(Handle, Option<NodeId>, usize)> = handles
        .iter()
        .rev()
        .map(|h| (h.clone(), None, top_depth))
        .collect();
    while let Some((handle, parent, depth)) = stack.pop() {
        let Some(id) = create_node(document, &handle) else {
            continue;
        };
        match parent {
            Some(parent) => document.attach_unchecked(parent, id),
            None => top.push(id),
        }
        if depth < MAX_DOM_DEPTH {
            push_children(&mut stack, &handle, Some(id), depth + 1);
        } else {
            push_children(&mut stack, &handle, parent, depth);
        }
    }
    top
}

/// Queue `handle`'s children (reversed, so they pop in document order) to
/// be attached under `parent` (None = top level) at `depth`.
fn push_children(
    stack: &mut Vec<(Handle, Option<NodeId>, usize)>,
    handle: &Handle,
    parent: Option<NodeId>,
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
    use super::{MAX_DOM_DEPTH, parse, parse_fragment, parse_fragment_at};
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

    fn tag(document: &Document, id: NodeId) -> String {
        match &document.get(id).unwrap().node_type {
            NodeType::Element(e) => e.tag_name.clone(),
            NodeType::Text(t) => format!("#text {t:?}"),
        }
    }

    #[test]
    fn document_with_html_tag_has_the_html_element_as_its_single_root() {
        // Compat round 3 step 1r: `:root`, `html[data-*]` and rem need the
        // real document element (Chrome: #document → <html> → head, body).
        let document = parse(
            r#"<!DOCTYPE html><html lang="ko" data-theme="dark" class="js"><head><title>t</title></head><body><p>x</p></body></html>"#,
        )
        .unwrap();
        assert_eq!(document.roots().len(), 1);
        let html = document.document_element().expect("document element");
        assert_eq!(html, document.roots()[0]);
        assert_eq!(tag(&document, html), "html");
        let attrs = &document.element_data(html).unwrap().attributes;
        assert_eq!(attrs.get("data-theme").map(String::as_str), Some("dark"));
        assert_eq!(attrs.get("lang").map(String::as_str), Some("ko"));
        let kids: Vec<String> = document
            .get(html)
            .unwrap()
            .children
            .iter()
            .map(|&c| tag(&document, c))
            .collect();
        assert_eq!(kids, ["head", "body"]);
        assert_eq!(
            document.head().map(|h| tag(&document, h)).as_deref(),
            Some("head")
        );
        assert_eq!(
            document.body().map(|b| tag(&document, b)).as_deref(),
            Some("body")
        );
    }

    #[test]
    fn html_tag_without_head_or_body_gets_the_synthesised_ones() {
        let document = parse("<html><title>t</title><p>x</p></html>").unwrap();
        assert_eq!(document.roots().len(), 1);
        let html = document.document_element().unwrap();
        let kids: Vec<String> = document
            .get(html)
            .unwrap()
            .children
            .iter()
            .map(|&c| tag(&document, c))
            .collect();
        assert_eq!(kids, ["head", "body"]);
        let body = document.body().unwrap();
        let body_kids: Vec<String> = document
            .get(body)
            .unwrap()
            .children
            .iter()
            .map(|&c| tag(&document, c))
            .collect();
        assert_eq!(body_kids, ["p"]);
    }

    #[test]
    fn fragment_and_body_only_sources_keep_their_roots() {
        // No `<html`: unchanged — top-level siblings / the `<body>` are roots,
        // and there is no document element.
        let fragment = parse("<div>a</div>").unwrap();
        assert_eq!(fragment.roots().len(), 1);
        assert_eq!(tag(&fragment, fragment.roots()[0]), "div");
        assert_eq!(fragment.document_element(), None);
        let body_only = parse("<body><p>x</p></body>").unwrap();
        assert_eq!(tag(&body_only, body_only.roots()[0]), "body");
        assert_eq!(body_only.body(), Some(body_only.roots()[0]));
        assert_eq!(body_only.document_element(), None);
    }

    #[test]
    fn html_root_counts_as_depth_one_for_the_cap() {
        // <html> is depth 1, <body> 2: the cap still bounds the whole tree.
        let depth = MAX_DOM_DEPTH + 10;
        let source = format!(
            "<html><body>{}x{}</body></html>",
            "<div>".repeat(depth),
            "</div>".repeat(depth)
        );
        let document = parse(&source).unwrap();
        let (deepest, elements, _) = shape(&document, document.roots()[0]);
        assert_eq!(deepest, MAX_DOM_DEPTH);
        assert_eq!(elements, depth + 3, "html, head, body and every div");
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
    fn fragment_parsed_below_a_deep_element_flattens_within_the_cap() {
        // innerHTML at depth d parses at d + 1: the fragment's own depth
        // budget is what is left under the cap. At the cap itself the top
        // level's children become top-level nodes; past it, nothing.
        let source = "<div>".repeat(20) + "x";
        let mut document = Document::new();
        let tops = parse_fragment_at(&source, &mut document, MAX_DOM_DEPTH - 4).unwrap();
        assert_eq!(tops.len(), 1);
        let (deepest, elements, text) = shape(&document, tops[0]);
        assert_eq!(deepest, 5, "depth MAX-4 ..= MAX");
        assert_eq!((elements, text.as_str()), (20, "x"));

        let tops = parse_fragment_at(&source, &mut document, MAX_DOM_DEPTH).unwrap();
        assert_eq!(tops.len(), 21, "every node a top-level sibling at the cap");
        assert!(
            parse_fragment_at(&source, &mut document, MAX_DOM_DEPTH + 1)
                .unwrap()
                .is_empty()
        );
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
