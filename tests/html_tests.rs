// Phase 4.1: behavioural smoke for the html5ever bridge in `src/html.rs`.
//
// These were originally in-file unit tests for the hand-rolled parser. We
// keep the ones that lock in cross-cutting parser contracts (entities,
// implicit-close, attribute quoting) because Phase 4 swapped the engine
// underneath without reworking the rest of the pipeline; if html5ever ever
// regresses one of these we want to notice it here, not via subtle layout
// drift on real pages.
//
// The "returns_error_for_stray_closing_tag_at_top_level" test from the
// original suite was deliberately dropped — html5ever recovers per WHATWG
// spec, matching real browsers, so the old "trailing input" error contract
// no longer applies.

use mini_browser::dom::NodeType;
use mini_browser::html;

#[test]
fn parses_nested_elements_and_text() {
    let document = html::parse("<div id='root'><p>Hello</p><span>world</span></div>").unwrap();

    assert_eq!(document.roots().len(), 1);

    let root_id = document.roots()[0];
    let root = document.get(root_id).unwrap();
    let root_element = match &root.node_type {
        NodeType::Element(data) => data,
        NodeType::Text(_) => panic!("expected element node"),
    };

    assert_eq!(root_element.tag_name, "div");
    assert_eq!(
        root_element.attributes.get("id").map(String::as_str),
        Some("root")
    );

    // html5ever may inject whitespace text nodes between siblings depending
    // on how the source is laid out; this input has no inter-element gap so
    // the only children are the two real elements.
    let elem_kids: Vec<_> = root
        .children
        .iter()
        .filter(|id| matches!(&document.get(**id).unwrap().node_type, NodeType::Element(_)))
        .collect();
    assert_eq!(elem_kids.len(), 2);

    let paragraph = document.get(*elem_kids[0]).unwrap();
    let paragraph_element = match &paragraph.node_type {
        NodeType::Element(data) => data,
        NodeType::Text(_) => panic!("expected paragraph element"),
    };
    assert_eq!(paragraph_element.tag_name, "p");
    assert_eq!(paragraph.children.len(), 1);
    assert_eq!(document.text(paragraph.children[0]), Some("Hello"));
}

#[test]
fn parses_multiple_attributes_with_mixed_quotes() {
    let document = html::parse(r#"<img src="hero.png" alt='Hero' data-id=abc />"#).unwrap();

    assert_eq!(document.roots().len(), 1);
    let root_id = document.roots()[0];
    let element = match &document.get(root_id).unwrap().node_type {
        NodeType::Element(data) => data,
        NodeType::Text(_) => panic!("expected element node"),
    };

    assert_eq!(element.tag_name, "img");
    assert_eq!(
        element.attributes.get("src").map(String::as_str),
        Some("hero.png")
    );
    assert_eq!(
        element.attributes.get("alt").map(String::as_str),
        Some("Hero")
    );
    assert_eq!(
        element.attributes.get("data-id").map(String::as_str),
        Some("abc")
    );
}

#[test]
fn implicitly_closes_inner_element_on_outer_closer() {
    // `<div><p>Hello</div>` — the unmatched `</div>` closes the open `<p>`
    // first and is consumed as the `<div>`'s own closer. This is the tree
    // every browser produces.
    let document = html::parse("<div><p>Hello</div>").unwrap();
    let div = document.roots()[0];
    let div_node = document.get(div).unwrap();
    let NodeType::Element(div_elem) = &div_node.node_type else {
        panic!("root must be an element");
    };
    assert_eq!(div_elem.tag_name, "div");
    assert_eq!(div_node.children.len(), 1);

    let p = div_node.children[0];
    let p_node = document.get(p).unwrap();
    let NodeType::Element(p_elem) = &p_node.node_type else {
        panic!("expected <p> as div's child");
    };
    assert_eq!(p_elem.tag_name, "p");

    let text_id = p_node.children[0];
    assert_eq!(document.text(text_id), Some("Hello"));
}

#[test]
fn implicitly_closes_li_when_sibling_li_opens() {
    // Real-world lists usually omit `</li>`; `<li>a<li>b` is two sibling
    // `<li>`s under the `<ul>`.
    let document = html::parse("<ul><li>a<li>b</ul>").unwrap();
    let ul = document.roots()[0];
    let ul_node = document.get(ul).unwrap();
    let lis: Vec<_> = ul_node
        .children
        .iter()
        .filter_map(|id| {
            let node = document.get(*id)?;
            match &node.node_type {
                NodeType::Element(el) if el.tag_name == "li" => Some(*id),
                _ => None,
            }
        })
        .collect();
    assert_eq!(lis.len(), 2);
    let first = document.get(lis[0]).unwrap().children[0];
    let second = document.get(lis[1]).unwrap().children[0];
    assert_eq!(document.text(first), Some("a"));
    assert_eq!(document.text(second), Some("b"));
}

#[test]
fn implicitly_closes_p_when_block_opener_starts() {
    // `<p>` cannot legally contain a block-level element; `<p>foo<div>bar</div>`
    // produces two siblings. (Note: under html5ever — and per the HTML5 spec —
    // these still come out as siblings, but a `<body>` may now be auto-synthesised
    // as the parser's insertion-mode wrapper. We pull the `<p>` and `<div>` out
    // by tag name rather than positional index to stay independent of that.)
    let document = html::parse("<p>foo<div>bar</div>").unwrap();
    let p_id = document
        .roots()
        .iter()
        .find(|id| matches!(&document.get(**id).unwrap().node_type, NodeType::Element(e) if e.tag_name == "p"))
        .copied()
        .expect("expected a <p> root");
    let div_id = document
        .roots()
        .iter()
        .find(|id| matches!(&document.get(**id).unwrap().node_type, NodeType::Element(e) if e.tag_name == "div"))
        .copied()
        .expect("expected a <div> root");

    let p_text = document.get(p_id).unwrap().children[0];
    assert_eq!(document.text(p_text), Some("foo"));
    let div_text = document.get(div_id).unwrap().children[0];
    assert_eq!(document.text(div_text), Some("bar"));
}

#[test]
fn implicitly_closes_table_cells_and_rows_when_closer_missing() {
    // `<table><tr><td>x</tr></table>` — html5ever may also auto-insert a
    // `<tbody>` per HTML5 rules; we walk by tag name so the test stays
    // shape-agnostic.
    let document = html::parse("<table><tr><td>x</tr></table>").unwrap();
    let table = document.roots()[0];

    fn first_descendant_with_tag(
        document: &mini_browser::dom::Document,
        node: mini_browser::dom::NodeId,
        tag: &str,
    ) -> Option<mini_browser::dom::NodeId> {
        let n = document.get(node)?;
        if let NodeType::Element(el) = &n.node_type
            && el.tag_name == tag
        {
            return Some(node);
        }
        for child in &n.children {
            if let Some(found) = first_descendant_with_tag(document, *child, tag) {
                return Some(found);
            }
        }
        None
    }

    let tr = first_descendant_with_tag(&document, table, "tr").expect("tr should exist");
    let td = first_descendant_with_tag(&document, tr, "td").expect("td should exist");
    let text = document.get(td).unwrap().children[0];
    assert_eq!(document.text(text), Some("x"));
}

#[test]
fn decodes_named_and_numeric_entities_in_text() {
    // The big five plus typography (`&hellip;`) plus decimal (`&#39;`) and
    // hex (`&#x27;`) numeric forms.
    let document = html::parse(
        "<p>&amp;&lt;&gt;&quot;&#39;&#x27;&hellip;&nbsp;&copy;</p>",
    )
    .unwrap();
    let p = document.roots()[0];
    let text_id = document.get(p).unwrap().children[0];
    assert_eq!(
        document.text(text_id),
        Some("&<>\"\'\'\u{2026}\u{00A0}\u{00A9}")
    );
}

#[test]
fn decodes_entities_inside_attribute_values() {
    // Query strings encode `&` as `&amp;`; the live attribute must be the
    // decoded form so links work and JS comparisons against the URL match.
    let document = html::parse(r#"<a href="?x=1&amp;y=2&#x3D;ok">go</a>"#).unwrap();
    let a = document.roots()[0];
    let element = match &document.get(a).unwrap().node_type {
        NodeType::Element(e) => e,
        _ => panic!("expected <a>"),
    };
    assert_eq!(
        element.attributes.get("href").map(String::as_str),
        Some("?x=1&y=2=ok")
    );
}

#[test]
fn does_not_decode_entities_inside_script_or_style_bodies() {
    // `<script>` / `<style>` are raw-text elements; their body is consumed
    // verbatim. A JS comparison like `if (a < b)` written as `&lt;` must
    // round-trip with `&lt;` left alone.
    let document = html::parse("<script>if (a&lt;b) {}</script>").unwrap();
    let script = document.roots()[0];
    let body = document.get(script).unwrap().children[0];
    assert_eq!(document.text(body), Some("if (a&lt;b) {}"));
}
