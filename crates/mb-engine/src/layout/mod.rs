// Layout uses a single rectangular box model for both block and simple inline flow.
// Every node becomes a box with a content rect plus margin/padding/border around it.
//
// The algorithm split lives in sibling submodules (`block`, `inline`, `flex`,
// `grid`) and each owns its own paint-recursive entry. mod.rs keeps the box
// types, `layout_tree` (the public entry) and the cross-cutting helpers every
// algorithm needs (containing-block math, edge sizes, intrinsic sizes, …).

use crate::{css::Value, dom::NodeType, style::StyledNode};

mod absolute;
mod block;
pub mod clip;
mod flex;
mod grid;
mod inline;
mod properties;
mod table;
mod taffy_bridge;

use absolute::{ContainingBlock, reposition_absolutes};
use block::layout_node;
use flex::is_flex_container;
use grid::is_grid_container;
pub(super) use properties::{
    apply_relative_offset, attribute_length, clamp_height, clamp_width, containing_block_height,
    content_width_value, definite_height, edge_sizes, has_float, has_height_limits, has_percent,
    has_width_limits, in_intrinsic_probe, is_auto, is_display_none, is_float_left, is_float_right,
    is_layout_whitespace_text, is_out_of_flow, length_like, length_value, shift_layout_subtree,
    with_containing_block_height, with_intrinsic_probe,
};
use table::is_table_container;

pub use clip::{ClipTable, compute_clips};
pub use inline::inline_line_height_px as line_height_px;

pub(crate) use properties::font_family_name;

// Layout uses a single rectangular box model for both block and simple inline flow.
// Every node becomes a box with a content rect plus margin/padding/border around it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeSizes {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Dimensions {
    pub content: Rect,
    pub padding: EdgeSizes,
    pub border: EdgeSizes,
    pub margin: EdgeSizes,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LayoutBox {
    pub box_type: BoxType,
    pub dimensions: Dimensions,
    pub children: Vec<LayoutBox>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoxType {
    BlockNode(StyledNode),
    // A flex container's outer box behaves like a block (its width/margin/padding
    // resolve the same way), but its children lay out along a main axis instead
    // of stacking vertically. The variant is distinct so render and hit-test
    // code can identify flex containers when needed; child placement happens in
    // `layout_flex_children`.
    FlexNode(StyledNode),
    // A grid container: outer box resolves like a block, but children get
    // placed into a 2D track grid resolved from `grid-template-columns` /
    // `grid-template-rows`. Auto-flow is row-major. Layout dispatch happens
    // in `layout_grid_children`.
    GridNode(StyledNode),
    // A table container (`display: table`): outer box behaves like a block
    // (the parent block flow stacks it normally), but its children are
    // placed by `layout_table_children` — rows get harvested from any
    // intermediate row groups (thead/tbody/tfoot), columns are sized from
    // cell intrinsic widths, and every cell in the same column shares an x
    // and width. Rows themselves are flattened into the children vector so
    // the renderer doesn't need a separate row box type.
    TableNode(StyledNode),
    AnonymousBlock,
}

/// The largest magnitude a layout coordinate or size can have (Blink's
/// `LayoutUnit` range, the same bound the cascade puts on lengths). See
/// [`saturate_layout`].
pub const MAX_LAYOUT_COORDINATE: f32 = mb_dom::style::MAX_CSS_LENGTH;

pub fn layout_tree(root: &StyledNode, viewport_width: f32) -> LayoutBox {
    layout_tree_with_viewport(root, viewport_width, None)
}

/// [`layout_tree`] with the viewport height as the initial containing
/// block's definite height, so the root's `height: N%` (and a chain of
/// them, `html, body {height: 100%}`) resolves as in Chrome (standards
/// mode; the quirks-mode body/html percentage quirk is not modelled).
/// `layout_tree` leaves it indefinite — the root's percentage is `auto`.
pub fn layout_tree_with_viewport(
    root: &StyledNode,
    viewport_width: f32,
    viewport_height: Option<f32>,
) -> LayoutBox {
    inline::with_inline_size_memo(|| {
        with_containing_block_height(viewport_height, || {
            layout_tree_pass(root, viewport_width, viewport_height)
        })
    })
}

fn layout_tree_pass(
    root: &StyledNode,
    viewport_width: f32,
    viewport_height: Option<f32>,
) -> LayoutBox {
    // Element roots route through taffy (the bridge's measure-callback
    // boundary handles every shape taffy itself doesn't understand). Only
    // a non-element root (e.g. a stand-alone text node — never produced by
    // current callers) falls back to the legacy block path.
    let mut layout_box = mb_dom::profile::time_block("layout::via_taffy_or_legacy", || {
        match taffy_bridge::layout_via_taffy(root, viewport_width, viewport_height) {
            Some(layout) => layout,
            None => {
                let mut cursor_y = 0.0;
                layout_node(root, 0.0, &mut cursor_y, viewport_width)
            }
        }
    });
    // Pass 2: walk the tree and move every `position: absolute` subtree to
    // its final spot relative to its containing block. The initial
    // containing block is the viewport (CSS 2.1 §10.1) — its height when the
    // caller knows it (step 1p); otherwise the laid-out root's own outer
    // height stands in, as before.
    let initial_cb_height = viewport_height.unwrap_or_else(|| outer_rect(&layout_box).height);
    let initial_cb = ContainingBlock {
        x: 0.0,
        y: 0.0,
        width: viewport_width,
        height: initial_cb_height,
    };
    mb_dom::profile::time_block("layout::reposition_absolutes", || {
        reposition_absolutes(&mut layout_box, initial_cb, initial_cb);
    });
    saturate_layout(&mut layout_box);
    layout_box
}

/// Clamp every coordinate and size into ±[`MAX_LAYOUT_COORDINATE`], NaN → 0.
///
/// The cascade already bounds each length (`style::MAX_CSS_LENGTH`), but
/// legal CSS still compounds past f32 — `width: 1000%` nested 40 deep, taffy
/// adding saturated paddings — and `inf - inf` turns into NaN. Blink avoids
/// it by saturating every `LayoutUnit` operation; we saturate the result
/// once, so paint, hit testing, `document_height` (which sizes buffers) and
/// the agent's rects never see a non-finite number. Iterative: the tree can
/// be as deep as the parser lets it (docs/compat-round3.md D1).
fn saturate_layout(root: &mut LayoutBox) {
    fn clamp(v: &mut f32) {
        *v = if v.is_nan() {
            0.0
        } else {
            v.clamp(-MAX_LAYOUT_COORDINATE, MAX_LAYOUT_COORDINATE)
        };
    }
    let mut stack = vec![root];
    while let Some(layout_box) = stack.pop() {
        let d = &mut layout_box.dimensions;
        for v in [
            &mut d.content.x,
            &mut d.content.y,
            &mut d.content.width,
            &mut d.content.height,
        ] {
            clamp(v);
        }
        for edges in [&mut d.padding, &mut d.border, &mut d.margin] {
            for v in [
                &mut edges.left,
                &mut edges.right,
                &mut edges.top,
                &mut edges.bottom,
            ] {
                clamp(v);
            }
        }
        stack.extend(layout_box.children.iter_mut());
    }
}

pub(super) fn box_styled_node(layout_box: &LayoutBox) -> Option<&StyledNode> {
    match &layout_box.box_type {
        BoxType::BlockNode(node)
        | BoxType::FlexNode(node)
        | BoxType::GridNode(node)
        | BoxType::TableNode(node) => Some(node),
        BoxType::AnonymousBlock => None,
    }
}

pub(super) fn box_position_keyword(layout_box: &LayoutBox) -> Option<&str> {
    match box_styled_node(layout_box).and_then(|node| node.value("position"))? {
        Value::Keyword(keyword) => Some(keyword.as_str()),
        _ => None,
    }
}

pub(super) fn box_is_positioned(layout_box: &LayoutBox) -> bool {
    matches!(
        box_position_keyword(layout_box),
        Some("relative" | "absolute" | "fixed")
    )
}

pub(super) fn box_is_absolute(layout_box: &LayoutBox) -> bool {
    matches!(box_position_keyword(layout_box), Some("absolute"))
}

pub(super) fn box_is_fixed(layout_box: &LayoutBox) -> bool {
    matches!(box_position_keyword(layout_box), Some("fixed"))
}

pub(super) fn outer_rect(layout_box: &LayoutBox) -> Rect {
    let d = &layout_box.dimensions;
    Rect {
        x: d.content.x - d.padding.left - d.border.left - d.margin.left,
        y: d.content.y - d.padding.top - d.border.top - d.margin.top,
        width: d.margin.left
            + d.border.left
            + d.padding.left
            + d.content.width
            + d.padding.right
            + d.border.right
            + d.margin.right,
        height: d.margin.top
            + d.border.top
            + d.padding.top
            + d.content.height
            + d.padding.bottom
            + d.border.bottom
            + d.margin.bottom,
    }
}

pub(super) fn container_box_type(node: &StyledNode) -> BoxType {
    if is_flex_container(node) {
        BoxType::FlexNode(node.clone())
    } else if is_grid_container(node) {
        BoxType::GridNode(node.clone())
    } else if is_table_container(node) {
        BoxType::TableNode(node.clone())
    } else {
        BoxType::BlockNode(node.clone())
    }
}

pub(super) fn child_height(node: &StyledNode, content_y: f32, child_cursor_y: f32) -> f32 {
    if matches!(node.node_type, NodeType::Text(_)) {
        0.0
    } else {
        child_cursor_y - content_y
    }
}

/// Apply CSS whitespace processing to a raw text-node string. With the
/// default `white-space: normal` (or `nowrap`), runs of any whitespace
/// (space, tab, newline, CR) collapse to a single ASCII space — which
/// is what turns source markup like `<p>Hello\n  world</p>` into the
/// expected single-spaced output. `pre`, `pre-wrap`, and `pre-line`
/// keep the input verbatim (the spec says `pre-line` collapses non-
/// newline whitespace, but our renderer doesn't honor newlines yet, so
/// the looser fallback gives the same visible result).
///
/// Layout and render both call this on their text reads so widths and
/// painted glyphs stay aligned. The original buffer in the DOM stays
/// untouched, so JS observers (textContent, innerHTML) keep seeing the
/// authored source.
pub fn collapsed_text(node: &StyledNode, raw: &str) -> String {
    let preserve = matches!(
        node.value("white-space"),
        Some(crate::css::Value::Keyword(kw)) if kw == "pre" || kw == "pre-wrap" || kw == "pre-line"
    );
    if preserve {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut prev_was_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(ch);
            prev_was_space = false;
        }
    }
    out
}

pub(super) fn intrinsic_width(node: &StyledNode) -> Option<f32> {
    match &node.node_type {
        // Images need a visible box even when no author CSS width is provided.
        NodeType::Element(element) if element.tag_name == "img" => {
            attribute_length(element, "width").or(Some(200.0))
        }
        NodeType::Element(element) if element.tag_name == "svg" => {
            Some(properties::svg_intrinsic_size(node, element).0)
        }
        _ => None,
    }
}

pub(super) fn intrinsic_height(node: &StyledNode) -> f32 {
    match &node.node_type {
        // font-size is always Px after the style pass, so the percent base is irrelevant.
        NodeType::Text(_) => length_value(node, "font-size", 0.0).unwrap_or(16.0),
        // Images also get a default height so the renderer has an area to paint into.
        NodeType::Element(element) if element.tag_name == "img" => {
            attribute_length(element, "height").unwrap_or(150.0)
        }
        NodeType::Element(element) if element.tag_name == "svg" => {
            properties::svg_intrinsic_size(node, element).1
        }
        // <input> is an atomic widget with no children, so its content height
        // has to come from somewhere — use the font-size so a single-line
        // text field is exactly tall enough for one glyph row. Authoring
        // `height: …` still overrides because explicit height takes
        // precedence over intrinsic in `layout_inline_block_node`.
        NodeType::Element(element) if element.tag_name == "input" => {
            length_value(node, "font-size", 0.0).unwrap_or(16.0)
        }
        // <textarea> is the same atomic widget as <input> but tall:
        // one font-size row per `rows` attribute (default 2, matching
        // the HTML spec's textarea reflection default). The value text
        // wraps purely on `\n` characters — author content with long
        // unwrapped lines still over-runs the right edge for now,
        // which is the same trade-off the rest of the toy makes for
        // single-line `<input>`.
        NodeType::Element(element) if element.tag_name == "textarea" => {
            let font_size = length_value(node, "font-size", 0.0).unwrap_or(16.0);
            let rows = element
                .attributes
                .get("rows")
                .and_then(|s| s.parse::<u32>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(2);
            font_size * rows as f32
        }
        NodeType::Element(_) => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use crate::{css, html, style};

    use super::{box_styled_node, layout_tree, outer_rect};

    fn styled_root(html_source: &str, css_source: &str) -> style::StyledNode {
        let document = html::parse(html_source).unwrap();
        let root = document.roots()[0];
        let stylesheet = css::parse(css_source).unwrap();
        style::style_tree(&document, root, &[stylesheet])
    }

    #[test]
    fn stacks_block_children_vertically() {
        let styled = styled_root(
            r#"<div id="root"><p>One</p><p>Two</p></div>"#,
            r#"
                #root { width: 300px; }
                p { margin-top: 5px; margin-bottom: 7px; font-size: 20px; }
            "#,
        );

        let layout = layout_tree(&styled, 800.0);
        let first = &layout.children[0];
        let second = &layout.children[1];

        assert_eq!(layout.dimensions.content.width, 300.0);
        assert_eq!(first.dimensions.content.y, 5.0);
        // Adjacent vertical margins collapse: gap between blocks is max(7, 5) = 7,
        // not sum (12). First <p> contains one line at 20px font-size; with
        // `line-height: normal` ≈ 1.2× (Phase 6.B) the line box is 24, so first
        // bottom = 5 + 24 = 29 and second.content_y = 29 + 7 = 36.
        assert_eq!(second.dimensions.content.y, 36.0);
    }

    #[test]
    fn display_none_children_drop_out_of_the_box_tree() {
        // <script>/<style> picked up `display: none` from the UA defaults, and
        // every flow algorithm now skips display:none children. The visible
        // <p> should be the root's only laid-out child, and its y position
        // should be the same with or without the hidden siblings around it.
        let styled = styled_root(
            r#"<div id="root"><script>var x = 1;</script><p>visible</p><style>p{color:red}</style></div>"#,
            r#"
                #root { width: 300px; }
                p { margin-top: 5px; font-size: 20px; }
            "#,
        );

        let layout = layout_tree(&styled, 800.0);
        // Root has exactly one child after the filter; <script> and <style>
        // contribute no boxes. Their absence also means the surviving <p>
        // starts at the top of the parent (margin: 5).
        assert_eq!(layout.children.len(), 1);
        assert_eq!(layout.children[0].dimensions.content.y, 5.0);
    }

    #[test]
    fn uses_available_width_after_margin_and_padding() {
        let styled = styled_root(
            r#"<div id="root"><section class="card"></section></div>"#,
            r#"
                #root { width: 200px; }
                .card {
                    margin-left: 10px;
                    margin-right: 10px;
                    padding-left: 5px;
                    padding-right: 5px;
                }
            "#,
        );

        let layout = layout_tree(&styled, 500.0);
        let card = &layout.children[0];

        assert_eq!(card.dimensions.content.x, 15.0);
        assert_eq!(card.dimensions.content.width, 170.0);
    }

    #[test]
    fn text_nodes_use_line_height_as_intrinsic_height() {
        // A single-line text run reserves one line box of vertical space.
        // Phase 6.B made `line-height: normal` resolve to font-size × 1.2,
        // so an 18px font-size gives a 21.6 line box (the spec's
        // metric-driven default that Chrome's body copy clusters around).
        let styled = styled_root(
            r#"<p class="copy">Hello</p>"#,
            r#"
                .copy { font-size: 18px; }
            "#,
        );

        let layout = layout_tree(&styled, 400.0);
        let text = &layout.children[0];

        assert_eq!(text.dimensions.content.height, 21.6);
    }

    #[test]
    fn input_uses_widget_defaults_and_renders_atomically() {
        // The UA stylesheet (style::default_values) gives <input> a 200px
        // width, 1px border on all sides, and 4×2 padding. With 16px default
        // font-size driving intrinsic_height, the input's content box ends
        // up 200×16 and its border box (= what other inline-blocks see for
        // line packing) ends up 210×22. Wrap in a div so layout_tree picks
        // the parent as root and we can pluck the input as a child.
        let styled = styled_root(r#"<div><input type="text"/></div>"#, "");
        let layout = layout_tree(&styled, 400.0);
        let input = &layout.children[0];

        assert_eq!(input.dimensions.content.width, 200.0);
        assert_eq!(input.dimensions.content.height, 16.0);
        assert_eq!(input.dimensions.padding.left, 4.0);
        assert_eq!(input.dimensions.padding.right, 4.0);
        assert_eq!(input.dimensions.padding.top, 2.0);
        assert_eq!(input.dimensions.padding.bottom, 2.0);
        assert_eq!(input.dimensions.border.left, 1.0);
        assert_eq!(input.dimensions.border.top, 1.0);
        // Atomic widget: even if the parser somehow handed us children,
        // <input> shouldn't recurse into them. Void-element parsing already
        // guarantees this in practice; the assertion locks the contract.
        assert!(input.children.is_empty());
    }

    #[test]
    fn img_uses_attribute_size_or_defaults() {
        let styled = styled_root(r#"<img src="/photo.png" width="64" height="48" />"#, "");
        let layout = layout_tree(&styled, 400.0);

        assert_eq!(layout.dimensions.content.width, 64.0);
        assert_eq!(layout.dimensions.content.height, 48.0);

        let fallback = styled_root(r#"<img src="/photo.png" />"#, "");
        let fallback_layout = layout_tree(&fallback, 400.0);
        assert_eq!(fallback_layout.dimensions.content.width, 200.0);
        assert_eq!(fallback_layout.dimensions.content.height, 150.0);
    }

    #[test]
    fn border_widths_reduce_available_content_width() {
        let styled = styled_root(
            r#"<div class="panel"></div>"#,
            r#"
                .panel {
                    width: 100px;
                    /* A side shorthand needs a style to have a width
                       (step 1b: an omitted style is none → 0). */
                    border-left: 4px solid;
                    border-right: 6px solid;
                    border-top: 2px solid;
                    border-bottom: 3px solid;
                }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);

        assert_eq!(layout.dimensions.border.left, 4.0);
        assert_eq!(layout.dimensions.border.right, 6.0);
        assert_eq!(layout.dimensions.border.top, 2.0);
        assert_eq!(layout.dimensions.border.bottom, 3.0);
        assert_eq!(layout.dimensions.content.width, 100.0);
        assert_eq!(layout.dimensions.content.x, 4.0);
        assert_eq!(layout.dimensions.content.y, 2.0);
    }

    #[test]
    fn inline_children_flow_horizontally() {
        let styled = styled_root(r#"<p><a href="/next">Go</a><span>Now</span></p>"#, "");
        let layout = layout_tree(&styled, 400.0);
        let link = &layout.children[0];
        let span = &layout.children[1];

        assert_eq!(link.dimensions.content.x, 0.0);
        assert!(span.dimensions.content.x > link.dimensions.content.x);
        assert_eq!(link.dimensions.content.y, span.dimensions.content.y);
    }

    #[test]
    fn inline_run_beside_block_sibling_shares_one_line() {
        // A block parent mixing a block child with label + input + button
        // (the todo demo's form row): the inline siblings must share one
        // anonymous line instead of each stacking as a full-width block.
        let styled = styled_root(
            r#"<div id="root"><h1>Todo</h1><label>New</label> <input id="new"> <button>Add</button><p>after</p></div>"#,
            "#root { width: 600px; } h1 { font-size: 20px; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let tags: Vec<_> = layout
            .children
            .iter()
            .map(|b| match box_styled_node(b).map(|n| &n.node_type) {
                Some(crate::dom::NodeType::Element(e)) => e.tag_name.clone(),
                Some(crate::dom::NodeType::Text(_)) => "#text".to_string(),
                None => "anon".to_string(),
            })
            .collect();
        let find = |tag: &str| {
            let i = tags.iter().position(|t| t == tag).unwrap_or_else(|| {
                panic!("no {tag} among {tags:?}");
            });
            &layout.children[i]
        };
        let (h1, label, input, button, p) = (
            find("h1"),
            find("label"),
            find("input"),
            find("button"),
            find("p"),
        );

        // Same line = same line-box top (the toy aligns outer tops, not
        // baselines, so compare outer rects — input has border+padding).
        let y = outer_rect(label).y;
        assert!(y > h1.dimensions.content.y, "row sits below the heading");
        assert_eq!(outer_rect(input).y, y, "input on the label's line");
        assert_eq!(outer_rect(button).y, y, "button on the label's line");
        assert!(input.dimensions.content.x > label.dimensions.content.x);
        assert!(button.dimensions.content.x > input.dimensions.content.x);
        assert!(
            button.dimensions.content.width < 200.0,
            "button shrink-wraps, got {}",
            button.dimensions.content.width
        );
        let row_bottom = outer_rect(input).y + outer_rect(input).height;
        assert!(
            p.dimensions.content.y >= row_bottom,
            "block after the run starts below it"
        );
    }

    #[test]
    fn inline_run_groups_on_the_legacy_block_path_too() {
        // A float child makes the parent a taffy boundary leaf, so the
        // legacy `block::layout_node` lays it out — it must group too.
        let styled = styled_root(
            r#"<div id="root"><div id="f"></div><h1>Todo</h1><input id="new"> <button>Add</button></div>"#,
            "#root { width: 600px; } #f { float: right; width: 10px; height: 10px; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let n = layout.children.len();
        let (input, button) = (&layout.children[n - 3], &layout.children[n - 1]);
        assert_eq!(outer_rect(input).y, outer_rect(button).y);
        assert!(button.dimensions.content.x > input.dimensions.content.x);
        assert!(button.dimensions.content.width < 200.0);
    }

    #[test]
    fn text_run_beside_block_sibling_flows_inline() {
        // `<div>Intro <b>bold</b><div>block</div></div>`: the leading text
        // and <b> share a line; the block child goes below.
        let styled = styled_root(
            r#"<div id="root">Intro <b>bold</b><div>block</div></div>"#,
            "#root { width: 400px; }",
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(
            layout.children.len(),
            3,
            "text, b, div — whitespace kept inline"
        );
        let (text, bold, block) = (
            &layout.children[0],
            &layout.children[1],
            &layout.children[2],
        );
        assert_eq!(text.dimensions.content.y, bold.dimensions.content.y);
        assert!(bold.dimensions.content.x > text.dimensions.content.x);
        assert!(block.dimensions.content.y > bold.dimensions.content.y);
        assert_eq!(block.dimensions.content.width, 400.0);
    }

    #[test]
    fn margin_auto_centers_block_horizontally() {
        let styled = styled_root(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    margin-left: auto;
                    margin-right: auto;
                }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);

        // 400 viewport - 100 width = 300 leftover, split evenly across both margins.
        assert_eq!(layout.dimensions.content.width, 100.0);
        assert_eq!(layout.dimensions.content.x, 150.0);
        assert_eq!(layout.dimensions.margin.left, 150.0);
        assert_eq!(layout.dimensions.margin.right, 150.0);
    }

    #[test]
    fn max_width_in_ch_with_margin_auto_produces_centered_reading_column() {
        // The Haskell-blog-style `article { max-width: 65ch; margin: 0 auto }`
        // pattern — wide viewport, `ch`-bounded reading width, auto-margin
        // centering. Confirms 5.2's three pieces work together end to end:
        // `ch` resolves at style time, taffy clamps the width via
        // max-width, and taffy's block algorithm distributes leftover into
        // the auto margins.
        //
        // The container holds a block child (`<p>`) so it routes through the
        // native taffy block path — boundary leaves with inline-only content
        // can clamp width but their cached LayoutBox carries pre-taffy
        // margins, so auto-margin centering would be lost on that path. Real
        // articles in the wild always have block children, which is what
        // this test exercises.
        //
        // Default font-size is 16px → 65ch ≈ 65 * 16 * 0.5 = 520px.
        // 1200 viewport → leftover 680, half on each side = 340px margins.
        let styled = styled_root(
            r#"<article class="copy"><p>x</p></article>"#,
            r#"
                .copy {
                    max-width: 65ch;
                    margin-left: auto;
                    margin-right: auto;
                }
            "#,
        );
        let layout = layout_tree(&styled, 1200.0);

        assert_eq!(layout.dimensions.content.width, 520.0);
        assert_eq!(layout.dimensions.content.x, 340.0);
        assert_eq!(layout.dimensions.margin.left, 340.0);
        assert_eq!(layout.dimensions.margin.right, 340.0);
    }

    #[test]
    fn max_width_clamps_boundary_leaf_with_inline_only_content() {
        // Inline-only article goes through the boundary measure-callback
        // path. With max-width forwarded to the boundary leaf's taffy
        // style, taffy gives the measure callback a width already capped at
        // 65ch ≈ 520px instead of the full viewport. Auto-margin centering
        // is NOT re-applied on this path (the cached LayoutBox preserves
        // legacy's resolved margins) — that gap is logged as boundary-path
        // auto-margin limitation; the width clamp alone is what 5.2 needs
        // for the reading-width target.
        let styled = styled_root(
            r#"<article class="copy">just text</article>"#,
            r#"
                .copy {
                    max-width: 65ch;
                }
            "#,
        );
        let layout = layout_tree(&styled, 1200.0);

        assert_eq!(layout.dimensions.content.width, 520.0);
    }

    #[test]
    fn right_float_inside_inline_flow_lands_at_container_right_edge() {
        // Tufte sidenote pattern: container with inline copy + a `float:
        // right` sibling. Before 5.3 the float was packed into the inline
        // run at line_x = text_width, so the note overlapped the body copy
        // as if it were another word. After 5.3, the float is filtered out
        // of line packing and placed at the parent's right edge at
        // content_y — the visual outcome real sidenotes need.
        //
        // We use `<div>` (not `<p>`) for the container because HTML5's <p>
        // auto-closes on flow-content children like <aside>, which would
        // hoist the float out of the paragraph entirely and defeat the test.
        let styled = styled_root(
            r#"<div class="para"><span class="copy">body</span><span class="note">side</span><span class="copy">tail</span></div>"#,
            r#"
                .para { width: 600px; }
                .note { float: right; width: 200px; height: 30px; display: block; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        // The float is laid out AFTER the line packing pass and appended to
        // the box list, so identify it by its 200px content width.
        let aside = layout
            .children
            .iter()
            .find(|child| (child.dimensions.content.width - 200.0).abs() < 0.001)
            .expect("expected the 200px-wide float child to land somewhere");

        // 600 container content - 200 aside width = 400.
        assert_eq!(aside.dimensions.content.x, 400.0);
        // Float sits at content_y, not at the bottom of the inline run.
        assert_eq!(aside.dimensions.content.y, 0.0);
    }

    #[test]
    fn left_float_inside_inline_flow_lands_at_container_left_edge() {
        // Mirror of the right-float case: float lands at content_x, not at
        // line_x (which would put it at the end of the preceding inline run).
        let styled = styled_root(
            r#"<div class="para"><span class="copy">body</span><span class="note">side</span></div>"#,
            r#"
                .para { width: 600px; }
                .note { float: left; width: 150px; height: 30px; display: block; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let aside = layout
            .children
            .iter()
            .find(|child| (child.dimensions.content.width - 150.0).abs() < 0.001)
            .expect("expected the 150px-wide float child to land somewhere");

        assert_eq!(aside.dimensions.content.x, 0.0);
        assert_eq!(aside.dimensions.content.y, 0.0);
    }

    #[test]
    fn float_inside_inline_flow_does_not_collapse_parent_height_below_float() {
        // The inline run is short (one word) but the float is tall; the
        // container must enclose the taller of the two so the float doesn't
        // visually spill below its parent's background box.
        let styled = styled_root(
            r#"<div class="para"><span>x</span><span class="note">note</span></div>"#,
            r#"
                .para { width: 400px; font-size: 16px; }
                .note { float: right; width: 100px; height: 200px; display: block; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert!(
            layout.dimensions.content.height >= 200.0,
            "container content height was {} but should enclose the 200px-tall float",
            layout.dimensions.content.height
        );
    }

    #[test]
    fn one_sided_margin_auto_pushes_content_to_far_side() {
        let styled = styled_root(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    margin-left: auto;
                    margin-right: 20px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);

        // 400 - 100 = 300 leftover, minus the explicit 20px right margin = 280 left margin.
        assert_eq!(layout.dimensions.margin.left, 280.0);
        assert_eq!(layout.dimensions.margin.right, 20.0);
        assert_eq!(layout.dimensions.content.x, 280.0);
    }

    #[test]
    fn text_align_center_offsets_inline_line() {
        let styled = styled_root(
            r#"<p><a href="/x">Go</a></p>"#,
            r#"
                p { width: 200px; text-align: center; }
                a { width: 40px; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);
        let link = &layout.children[0];

        // Line width is 40, container is 200, so the line offsets by (200-40)/2 = 80.
        assert_eq!(link.dimensions.content.x, 80.0);
    }

    #[test]
    fn text_align_left_keeps_default_layout() {
        let styled = styled_root(
            r#"<p><a href="/x">Go</a></p>"#,
            r#"
                p { width: 200px; }
                a { width: 40px; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);
        let link = &layout.children[0];

        // No alignment override means the line still starts at content_x = 0.
        assert_eq!(link.dimensions.content.x, 0.0);
    }

    #[test]
    fn text_align_center_offsets_inline_children_inside_inline_element() {
        // text-align is inherited, so the <span> inside the <a> picks up the centered
        // alignment from <p> and offsets within the <a>'s own content box.
        let styled = styled_root(
            r#"<p><a href="/x"><span>Go</span></a></p>"#,
            r#"
                p { width: 200px; text-align: center; }
                a { width: 100px; }
                span { width: 40px; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);
        let link = &layout.children[0];
        let span = &link.children[0];

        // <a> centers within <p>: (200 - 100) / 2 = 50.
        assert_eq!(link.dimensions.content.x, 50.0);
        // <span> centers within <a>: (100 - 40) / 2 = 30, plus the link's content_x = 80.
        assert_eq!(span.dimensions.content.x, 80.0);
    }

    #[test]
    fn percent_width_resolves_against_parent_content_width() {
        let styled = styled_root(
            r#"<div id="root"><section class="card"></section></div>"#,
            r#"
                #root { width: 400px; }
                .card { width: 50%; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let card = &layout.children[0];

        // 50% of #root's 400px content width = 200px.
        assert_eq!(card.dimensions.content.width, 200.0);
    }

    #[test]
    fn percent_padding_uses_parent_width_even_for_vertical_sides() {
        // CSS spec quirk: percent padding/margin on top and bottom resolves against the
        // containing block's width, not its height.
        let styled = styled_root(
            r#"<div id="root"><div class="card"></div></div>"#,
            r#"
                #root { width: 200px; }
                .card { padding-top: 25%; padding-bottom: 10%; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let card = &layout.children[0];

        // 25% and 10% of 200 = 50 and 20 respectively.
        assert_eq!(card.dimensions.padding.top, 50.0);
        assert_eq!(card.dimensions.padding.bottom, 20.0);
    }

    #[test]
    fn em_widths_compose_with_inherited_font_size() {
        // 1em width on the inner element should equal the parent's resolved font-size,
        // proving the style-time em resolution feeds layout correctly.
        let styled = styled_root(
            r#"<div id="root"><div class="inner"></div></div>"#,
            r#"
                #root { font-size: 24px; }
                .inner { width: 5em; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let inner = &layout.children[0];

        // Inner inherits 24px font-size, so 5em = 120px.
        assert_eq!(inner.dimensions.content.width, 120.0);
    }

    #[test]
    fn inline_block_flows_horizontally_with_explicit_size() {
        // Two inline-block siblings should stack on a single line and respect
        // their explicit width/height instead of stretching to the container.
        let styled = styled_root(
            r#"<div id="row"><span class="chip">A</span><span class="chip">B</span></div>"#,
            r#"
                #row { width: 400px; }
                .chip {
                    display: inline-block;
                    width: 80px;
                    height: 30px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let first = &layout.children[0];
        let second = &layout.children[1];

        assert_eq!(first.dimensions.content.x, 0.0);
        assert_eq!(first.dimensions.content.y, 0.0);
        assert_eq!(first.dimensions.content.width, 80.0);
        assert_eq!(first.dimensions.content.height, 30.0);
        // Second box sits to the right of the first with the same baseline.
        assert_eq!(second.dimensions.content.x, 80.0);
        assert_eq!(second.dimensions.content.y, 0.0);
    }

    #[test]
    fn inline_block_wraps_to_next_line_when_overflowing() {
        // Three 80px chips into a 200px row: third one wraps below the first two.
        let styled = styled_root(
            r#"<div id="row"><span class="chip">A</span><span class="chip">B</span><span class="chip">C</span></div>"#,
            r#"
                #row { width: 200px; }
                .chip {
                    display: inline-block;
                    width: 80px;
                    height: 30px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let third = &layout.children[2];

        assert_eq!(third.dimensions.content.x, 0.0);
        assert_eq!(third.dimensions.content.y, 30.0);
    }

    #[test]
    fn inline_block_padding_and_margin_count_toward_outer_width() {
        // Outer width = margin(5+5) + padding(10+10) + width(40) = 70.
        let styled = styled_root(
            r#"<div id="row"><span class="chip"></span><span class="chip"></span></div>"#,
            r#"
                #row { width: 400px; }
                .chip {
                    display: inline-block;
                    width: 40px;
                    height: 20px;
                    margin-left: 5px;
                    margin-right: 5px;
                    padding-left: 10px;
                    padding-right: 10px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let first = &layout.children[0];
        let second = &layout.children[1];

        // First chip's content_x = 0 + margin-left(5) + padding-left(10) = 15.
        assert_eq!(first.dimensions.content.x, 15.0);
        assert_eq!(first.dimensions.content.width, 40.0);
        // Second chip's content_x = first outer end (70) + own margin/padding offsets.
        assert_eq!(second.dimensions.content.x, 70.0 + 15.0);
    }

    #[test]
    fn inline_block_runs_block_layout_for_inner_block_children() {
        // An inline-block with two block children should stack them vertically inside
        // its own content box and report a height equal to their combined heights.
        let styled = styled_root(
            r#"<div id="row"><span class="card"><div class="row"></div><div class="row"></div></span></div>"#,
            r#"
                #row { width: 400px; }
                .card {
                    display: inline-block;
                    width: 100px;
                }
                .row { height: 25px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let card = &layout.children[0];
        let inner_first = &card.children[0];
        let inner_second = &card.children[1];

        assert_eq!(card.dimensions.content.width, 100.0);
        assert_eq!(card.dimensions.content.height, 50.0);
        assert_eq!(inner_first.dimensions.content.y, 0.0);
        assert_eq!(inner_second.dimensions.content.y, 25.0);
        // Inner block children fill the inline-block's content width.
        assert_eq!(inner_first.dimensions.content.width, 100.0);
    }

    #[test]
    fn inline_block_taller_sibling_sets_line_height() {
        // The line height should pick up the tallest inline-block on the line so
        // that the next line starts below the tallest box, not the first one.
        let styled = styled_root(
            r#"<div id="row"><span class="short">A</span><span class="tall">B</span><span class="short">C</span><span class="short">D</span></div>"#,
            r#"
                #row { width: 200px; }
                .short {
                    display: inline-block;
                    width: 60px;
                    height: 20px;
                }
                .tall {
                    display: inline-block;
                    width: 60px;
                    height: 50px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        // Three 60px chips fit on the first line (180/200), the fourth wraps.
        let fourth = &layout.children[3];
        // Wrap row should clear the tallest box on the previous line (50px), not 20px.
        assert_eq!(fourth.dimensions.content.y, 50.0);
    }

    #[test]
    fn position_relative_offsets_box_without_shifting_siblings() {
        // The relative box visually moves by (left, top), but the next sibling
        // still starts where the relative box would have ended in normal flow.
        let styled = styled_root(
            r#"<div id="root"><div class="shifted"></div><div class="next"></div></div>"#,
            r#"
                #root { width: 300px; }
                .shifted {
                    position: relative;
                    left: 20px;
                    top: 30px;
                    height: 40px;
                }
                .next { height: 25px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let shifted = &layout.children[0];
        let next = &layout.children[1];

        // Visual position picks up the offset.
        assert_eq!(shifted.dimensions.content.x, 20.0);
        assert_eq!(shifted.dimensions.content.y, 30.0);
        // Sibling still stacks at the unoffset bottom (40px), not 70px.
        assert_eq!(next.dimensions.content.x, 0.0);
        assert_eq!(next.dimensions.content.y, 40.0);
    }

    #[test]
    fn percent_top_uses_height_basis() {
        // Step 3c: a relative box's `top`/`bottom` percentage is of the
        // containing block's *height* (here 200px), not its width (400px).
        let styled = styled_root(
            r#"<div id="root"><div class="top"></div><div class="bottom"></div></div>"#,
            r#"
                #root { width: 400px; height: 200px; }
                .top { position: relative; top: 10%; height: 10px; }
                .bottom { position: relative; bottom: 25%; height: 10px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.children[0].dimensions.content.y, 20.0);
        // In flow at y = 10, moved up by 25% of 200.
        assert_eq!(layout.children[1].dimensions.content.y, 10.0 - 50.0);
    }

    #[test]
    fn percent_top_with_an_auto_height_parent_is_auto() {
        // An indefinite containing-block height makes the percentage `auto`:
        // no shift, or the other side takes over.
        let styled = styled_root(
            r#"<div id="root"><div class="top"></div><div class="both"></div></div>"#,
            r#"
                #root { width: 400px; }
                .top { position: relative; top: 10%; height: 10px; }
                .both { position: relative; top: 50%; bottom: 4px; height: 10px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.children[0].dimensions.content.y, 0.0);
        assert_eq!(layout.children[1].dimensions.content.y, 10.0 - 4.0);
    }

    #[test]
    fn position_relative_propagates_offset_to_descendants() {
        // Children should visually shift by the same amount as the relative
        // ancestor: their on-screen rects are computed by translating the whole
        // subtree, not by re-laying out the children.
        let styled = styled_root(
            r#"<div id="root"><div class="outer"><div class="inner"></div></div></div>"#,
            r#"
                #root { width: 300px; }
                .outer {
                    position: relative;
                    left: 15px;
                    top: 25px;
                    height: 80px;
                }
                .inner { height: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let outer = &layout.children[0];
        let inner = &outer.children[0];

        assert_eq!(outer.dimensions.content.x, 15.0);
        assert_eq!(outer.dimensions.content.y, 25.0);
        // Inner sits flush inside the outer's content box, then shares the shift.
        assert_eq!(inner.dimensions.content.x, 15.0);
        assert_eq!(inner.dimensions.content.y, 25.0);
    }

    #[test]
    fn position_relative_with_right_and_bottom_uses_negative_offset() {
        // `right`/`bottom` push the box away from those edges, which is just a
        // negative shift along the normal-flow axes for a relative element.
        let styled = styled_root(
            r#"<div id="root"><div class="floater"></div><div class="after"></div></div>"#,
            r#"
                #root { width: 300px; }
                .floater {
                    position: relative;
                    right: 10px;
                    bottom: 5px;
                    height: 20px;
                }
                .after { height: 15px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let floater = &layout.children[0];
        let after = &layout.children[1];

        // right: 10 → dx = -10, bottom: 5 → dy = -5.
        assert_eq!(floater.dimensions.content.x, -10.0);
        assert_eq!(floater.dimensions.content.y, -5.0);
        // Sibling cursor ignores the shift; flow continues at unoffset bottom.
        assert_eq!(after.dimensions.content.y, 20.0);
    }

    #[test]
    fn position_relative_left_wins_over_right() {
        // CSS spec for LTR: when both `left` and `right` are set on a relative
        // box, `left` wins and `right` is ignored.
        let styled = styled_root(
            r#"<div id="root"><div class="box"></div></div>"#,
            r#"
                #root { width: 300px; }
                .box {
                    position: relative;
                    left: 12px;
                    right: 50px;
                    height: 10px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let shifted = &layout.children[0];

        assert_eq!(shifted.dimensions.content.x, 12.0);
    }

    #[test]
    fn position_relative_works_on_inline_block() {
        // Relative shift should compose on top of inline-block placement so the
        // chip moves visually but does not change where the next chip sits.
        let styled = styled_root(
            r#"<div id="row"><span class="chip"></span><span class="chip shifted"></span><span class="chip"></span></div>"#,
            r#"
                #row { width: 400px; }
                .chip {
                    display: inline-block;
                    width: 60px;
                    height: 20px;
                }
                .shifted {
                    position: relative;
                    left: 100px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let first = &layout.children[0];
        let middle = &layout.children[1];
        let third = &layout.children[2];

        // First sits at the left edge.
        assert_eq!(first.dimensions.content.x, 0.0);
        // Middle would be at 60, then shifts +100 visually.
        assert_eq!(middle.dimensions.content.x, 160.0);
        // Third sits at 120 — the inline cursor advanced as if middle were unshifted.
        assert_eq!(third.dimensions.content.x, 120.0);
    }

    #[test]
    fn position_relative_resolves_percent_offsets_against_parent_width() {
        // `left` % is of the containing block's width; `top` % of its height
        // (step 3c — this test used to pin the old width-for-both shortcut).
        let styled = styled_root(
            r#"<div id="root"><div class="box"></div></div>"#,
            r#"
                #root { width: 200px; height: 100px; }
                .box {
                    position: relative;
                    left: 25%;
                    top: 10%;
                    height: 10px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let shifted = &layout.children[0];

        // 25% of 200px = 50px, 10% of 100px = 10px.
        assert_eq!(shifted.dimensions.content.x, 50.0);
        assert_eq!(shifted.dimensions.content.y, 10.0);
    }

    #[test]
    fn position_relative_zero_offsets_keep_box_in_place() {
        // `position: relative` with no offsets is a no-op for layout (the only
        // visible effect is becoming a containing block for absolutes, which we
        // do not yet support). The box should land exactly where a static box
        // would.
        let styled = styled_root(
            r#"<div id="root"><div class="box"></div></div>"#,
            r#"
                #root { width: 200px; }
                .box {
                    position: relative;
                    height: 30px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let shifted = &layout.children[0];

        assert_eq!(shifted.dimensions.content.x, 0.0);
        assert_eq!(shifted.dimensions.content.y, 0.0);
    }

    #[test]
    fn position_absolute_is_removed_from_in_flow_cursor() {
        // Sibling after the absolute box should layout where the absolute
        // would have been, since absolutes do not advance the block cursor.
        let styled = styled_root(
            r#"<div id="root"><div class="spacer"></div><div class="abs"></div><div class="next"></div></div>"#,
            r#"
                #root { width: 400px; }
                .spacer { height: 30px; }
                .abs {
                    position: absolute;
                    width: 100px;
                    height: 50px;
                }
                .next { height: 25px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let abs = &layout.children[1];
        let next = &layout.children[2];

        // With no offsets, the absolute keeps its static position (under spacer).
        assert_eq!(abs.dimensions.content.y, 30.0);
        // .next sits flush below .spacer, ignoring the absolute box.
        assert_eq!(next.dimensions.content.y, 30.0);
    }

    #[test]
    fn position_absolute_uses_initial_containing_block_when_no_positioned_ancestor() {
        // Without a positioned ancestor, the containing block is the viewport
        // (origin 0,0). top/left land the outer edge there.
        let styled = styled_root(
            r#"<div id="root"><div class="abs"></div></div>"#,
            r#"
                #root { width: 400px; }
                .abs {
                    position: absolute;
                    left: 50px;
                    top: 80px;
                    width: 100px;
                    height: 30px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let abs = &layout.children[0];

        assert_eq!(abs.dimensions.content.x, 50.0);
        assert_eq!(abs.dimensions.content.y, 80.0);
    }

    #[test]
    fn position_absolute_resolves_against_nearest_positioned_ancestor_padding_box() {
        // The .container is `position: relative` so it becomes the CB. The
        // CB is its padding box, so left/top land relative to that — including
        // its own padding offset on the inside.
        let styled = styled_root(
            r#"<div id="root"><div class="container"><div class="abs"></div></div></div>"#,
            r#"
                #root { width: 400px; }
                .container {
                    position: relative;
                    margin-top: 100px;
                    padding-left: 20px;
                    padding-top: 20px;
                    padding-right: 20px;
                    padding-bottom: 20px;
                    height: 200px;
                }
                .abs {
                    position: absolute;
                    left: 30px;
                    top: 40px;
                    width: 50px;
                    height: 25px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let container = &layout.children[0];
        let abs = &container.children[0];

        // .container starts at margin-top 100. CB origin = padding-box top-left
        // = (0, 100). Offsets land outer edge at (30, 140).
        assert_eq!(abs.dimensions.content.x, 30.0);
        assert_eq!(abs.dimensions.content.y, 140.0);
    }

    #[test]
    fn position_absolute_right_and_bottom_pin_to_far_edges_of_cb() {
        // right/bottom anchor the OUTER far edges to (cb.right - right) and
        // (cb.bottom - bottom). Outer width/height get subtracted so the box
        // sits inside the cb, not flush against the edge.
        let styled = styled_root(
            r#"<div id="root"><div class="container"><div class="abs"></div></div></div>"#,
            r#"
                #root { width: 400px; }
                .container {
                    position: relative;
                    width: 200px;
                    height: 100px;
                }
                .abs {
                    position: absolute;
                    right: 10px;
                    bottom: 20px;
                    width: 30px;
                    height: 25px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let container = &layout.children[0];
        let abs = &container.children[0];

        // CB = (0, 0, 200, 100). x = 200 - 10 - 30 = 160. y = 100 - 20 - 25 = 55.
        assert_eq!(abs.dimensions.content.x, 160.0);
        assert_eq!(abs.dimensions.content.y, 55.0);
    }

    #[test]
    fn position_absolute_keeps_static_position_when_no_offsets_set() {
        // Auto on every offset means the absolute box stays where it would
        // have been laid out in normal flow — useful as a containing block
        // marker without actually moving the box.
        let styled = styled_root(
            r#"<div id="root"><div class="spacer"></div><div class="abs"></div></div>"#,
            r#"
                #root { width: 400px; }
                .spacer { height: 75px; }
                .abs {
                    position: absolute;
                    width: 100px;
                    height: 30px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let abs = &layout.children[1];

        // Static position lands directly below the 75px spacer.
        assert_eq!(abs.dimensions.content.x, 0.0);
        assert_eq!(abs.dimensions.content.y, 75.0);
    }

    #[test]
    fn position_absolute_resolves_percent_against_cb_dimensions() {
        // Percent left/right resolves against cb width, top/bottom against cb
        // height — unlike most other percent properties in our toy that share
        // the width base.
        let styled = styled_root(
            r#"<div id="root"><div class="container"><div class="abs"></div></div></div>"#,
            r#"
                #root { width: 400px; }
                .container {
                    position: relative;
                    width: 200px;
                    height: 100px;
                }
                .abs {
                    position: absolute;
                    left: 25%;
                    top: 50%;
                    width: 30px;
                    height: 20px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let container = &layout.children[0];
        let abs = &container.children[0];

        // 25% of 200 = 50, 50% of 100 = 50.
        assert_eq!(abs.dimensions.content.x, 50.0);
        assert_eq!(abs.dimensions.content.y, 50.0);
    }

    #[test]
    fn nested_absolute_compounds_through_each_containing_block() {
        // Inner absolute resolves against outer's padding box, then outer's
        // own offsets shift the whole subtree (including inner) by another
        // delta. Both shifts compose in the natural top-down order.
        let styled = styled_root(
            r#"<div id="root"><div class="outer"><div class="inner"></div></div></div>"#,
            r#"
                #root { width: 400px; }
                .outer {
                    position: absolute;
                    left: 50px;
                    top: 100px;
                    width: 200px;
                    height: 150px;
                }
                .inner {
                    position: absolute;
                    left: 20px;
                    top: 30px;
                    width: 30px;
                    height: 20px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let outer = &layout.children[0];
        let inner = &outer.children[0];

        // Inner gets shifted to (20, 30) within outer's CB (originally at 0,0),
        // then outer's own (50, 100) shift carries inner along.
        assert_eq!(outer.dimensions.content.x, 50.0);
        assert_eq!(outer.dimensions.content.y, 100.0);
        assert_eq!(inner.dimensions.content.x, 70.0);
        assert_eq!(inner.dimensions.content.y, 130.0);
    }

    #[test]
    fn position_absolute_inside_inline_flow_does_not_break_lines() {
        // An absolute child inside an inline parent must not contribute to
        // line packing — three normal chips should still fit on one line of
        // a 200px row even with an absolute chip mixed in between.
        let styled = styled_root(
            r#"<div id="row"><span class="chip"></span><span class="chip abs"></span><span class="chip"></span><span class="chip"></span></div>"#,
            r#"
                #row { width: 200px; position: relative; height: 50px; }
                .chip {
                    display: inline-block;
                    width: 60px;
                    height: 20px;
                }
                .abs {
                    position: absolute;
                    top: 5px;
                    left: 5px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        // children: chip0, abs (out of flow → pushed to end), chip1, chip2.
        let chip0 = &layout.children[0];
        let chip1 = &layout.children[1];
        let chip2 = &layout.children[2];
        let abs = &layout.children[3];

        // Three in-flow chips occupy 0, 60, 120 on the same line.
        assert_eq!(chip0.dimensions.content.x, 0.0);
        assert_eq!(chip0.dimensions.content.y, 0.0);
        assert_eq!(chip1.dimensions.content.x, 60.0);
        assert_eq!(chip1.dimensions.content.y, 0.0);
        assert_eq!(chip2.dimensions.content.x, 120.0);
        assert_eq!(chip2.dimensions.content.y, 0.0);
        // Absolute chip lands at #row's CB origin + (5, 5).
        assert_eq!(abs.dimensions.content.x, 5.0);
        assert_eq!(abs.dimensions.content.y, 5.0);
    }

    #[test]
    fn phrasing_tags_keep_their_parent_in_inline_flow() {
        // The HN nav bar packs `<b><a>Hacker News</a></b>` next to plain
        // `<a>new</a>` and pipe-separated text inside one `<span>`. With a
        // narrow inline whitelist, `<b>` would be classified as block and
        // flip the span into block flow, stacking every link on its own
        // row — exactly the visual regression that motivated widening the
        // whitelist. The assertion here is that every visible child of
        // the span shares the same y coordinate (one line) on a wide
        // viewport. The toy HTML parser drops pure-whitespace text
        // between elements, so the four element siblings sit directly
        // next to each other in the child vector; a fifth text-bearing
        // sibling proves text rides the same line too.
        let styled = styled_root(
            r#"<span><b>One</b><em>Two</em><a>Three</a><strong>Four</strong>tail</span>"#,
            r#""#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.children.len(), 5);
        let line_y = layout.children[0].dimensions.content.y;
        for child in &layout.children {
            assert_eq!(
                child.dimensions.content.y, line_y,
                "every inline child must sit on the same line"
            );
        }
        // Failure mode pre-fix would have second child at line_y +
        // line_height (vertical stacking). After the fix the children
        // are packed left-to-right, so each sibling's x advances over
        // the previous one's width.
        for window in layout.children.windows(2) {
            assert!(
                window[1].dimensions.content.x > window[0].dimensions.content.x,
                "siblings must advance horizontally, not stack vertically"
            );
        }
    }

    #[test]
    fn position_fixed_is_removed_from_in_flow_cursor() {
        // Same out-of-flow semantics as absolute: a fixed sibling should not
        // shift the next in-flow box down.
        let styled = styled_root(
            r#"<div id="root"><div class="spacer"></div><div class="fix"></div><div class="next"></div></div>"#,
            r#"
                #root { width: 400px; }
                .spacer { height: 30px; }
                .fix {
                    position: fixed;
                    width: 100px;
                    height: 50px;
                }
                .next { height: 25px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let next = &layout.children[2];

        assert_eq!(next.dimensions.content.y, 30.0);
    }

    #[test]
    fn position_fixed_ignores_positioned_ancestor_and_uses_viewport() {
        // Even with a `position: relative` container that would normally be the
        // CB for an absolute descendant, fixed boxes resolve against the
        // viewport (initial CB).
        let styled = styled_root(
            r#"<div id="root"><div class="container"><div class="fix"></div></div></div>"#,
            r#"
                #root { width: 400px; }
                .container {
                    position: relative;
                    margin-top: 100px;
                    padding-left: 30px;
                    padding-top: 30px;
                    padding-right: 30px;
                    padding-bottom: 30px;
                    height: 200px;
                }
                .fix {
                    position: fixed;
                    left: 50px;
                    top: 80px;
                    width: 100px;
                    height: 30px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let container = &layout.children[0];
        let fix = &container.children[0];

        // If this were `position: absolute`, the CB would be the container's
        // padding box at (0, 100), placing the box at (50, 180). Fixed lands
        // at the viewport origin instead: (50, 80).
        assert_eq!(fix.dimensions.content.x, 50.0);
        assert_eq!(fix.dimensions.content.y, 80.0);
    }

    #[test]
    fn position_fixed_resolves_percent_against_viewport_size() {
        // Initial CB width is the viewport width passed to layout_tree; height
        // falls back to the laid-out root's outer height. Setting an explicit
        // height on the root pins both axes to known values.
        let styled = styled_root(
            r#"<div id="root"><div class="fix"></div></div>"#,
            r#"
                #root { width: 400px; height: 600px; }
                .fix {
                    position: fixed;
                    left: 25%;
                    top: 50%;
                    width: 30px;
                    height: 20px;
                }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let fix = &layout.children[0];

        // 25% of 800 viewport width = 200. 50% of 600 root outer height = 300.
        assert_eq!(fix.dimensions.content.x, 200.0);
        assert_eq!(fix.dimensions.content.y, 300.0);
    }

    #[test]
    fn adjacent_positive_margins_collapse_to_max() {
        // .a's margin-bottom (30) and .b's margin-top (10) collapse to the
        // larger of the two: gap = 30, not 40.
        let styled = styled_root(
            r#"<div id="root"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #root { width: 300px; }
                .a { height: 20px; margin-bottom: 30px; }
                .b { height: 15px; margin-top: 10px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let b = &layout.children[1];

        // a's content ends at 20; gap = max(30, 10) = 30 → b.y = 50.
        assert_eq!(b.dimensions.content.y, 50.0);
    }

    #[test]
    fn adjacent_negative_margins_collapse_to_min() {
        // Two non-positive margins collapse to the most negative: gap pulls
        // siblings closer by the larger absolute value, not by the sum.
        let styled = styled_root(
            r#"<div id="root"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #root { width: 300px; }
                .a { height: 20px; margin-bottom: -10px; }
                .b { height: 15px; margin-top: -5px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let b = &layout.children[1];

        // a content ends at 20. min(-10, -5) = -10 from that bottom: b.y = 10.
        assert_eq!(b.dimensions.content.y, 10.0);
    }

    #[test]
    fn mixed_sign_margins_sum_algebraically() {
        // CSS spec: when one margin is positive and the other negative, they
        // combine by simple addition.
        let styled = styled_root(
            r#"<div id="root"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #root { width: 300px; }
                .a { height: 20px; margin-bottom: 30px; }
                .b { height: 15px; margin-top: -10px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let b = &layout.children[1];

        // a content ends at 20; gap = 30 + (-10) = 20 → b.y = 40.
        assert_eq!(b.dimensions.content.y, 40.0);
    }

    #[test]
    fn absolute_child_does_not_break_margin_collapse_chain() {
        // Out-of-flow children should not reset the in-flow margin-collapse
        // chain — .a and .b are still considered adjacent for collapse even
        // with an absolute box between them in the DOM.
        let styled = styled_root(
            r#"<div id="root"><div class="a"></div><div class="abs"></div><div class="b"></div></div>"#,
            r#"
                #root { width: 300px; }
                .a { height: 20px; margin-bottom: 30px; }
                .abs { position: absolute; width: 50px; height: 50px; }
                .b { height: 15px; margin-top: 10px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let b = &layout.children[2];

        // Same outcome as if .abs were not there: gap = max(30, 10) = 30.
        assert_eq!(b.dimensions.content.y, 50.0);
    }

    #[test]
    fn line_height_normal_uses_metric_driven_ratio() {
        // Phase 6.B: `line-height: normal` (or unset) should resolve to a
        // metric-driven multiplier ≈ 1.2× the font-size, not the bare 1.0
        // identity that left HN body looking 17% tighter than Chrome.
        // Tested via two equivalent declarations to lock both code paths
        // (explicit `normal` keyword and missing declaration).
        let unset = styled_root(r#"<p>Hello</p>"#, r#"p { font-size: 20px; }"#);
        let normal = styled_root(
            r#"<p>Hello</p>"#,
            r#"p { font-size: 20px; line-height: normal; }"#,
        );
        let unset_text = &layout_tree(&unset, 400.0).children[0];
        let normal_text = &layout_tree(&normal, 400.0).children[0];

        assert_eq!(unset_text.dimensions.content.height, 24.0);
        assert_eq!(normal_text.dimensions.content.height, 24.0);
    }

    #[test]
    fn line_height_number_multiplies_font_size() {
        // Unitless line-height applies as a multiplier of the element's own
        // font-size at every level — 16px × 1.5 = 24px tall text.
        let styled = styled_root(
            r#"<p>Hello</p>"#,
            r#"
                p { font-size: 16px; line-height: 1.5; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);
        let text = &layout.children[0];

        assert_eq!(text.dimensions.content.height, 24.0);
    }

    #[test]
    fn line_height_length_uses_absolute_value() {
        // A length value pins the line height regardless of the local
        // font-size — text is 16px tall but its line box stretches to 30.
        let styled = styled_root(
            r#"<p>Hello</p>"#,
            r#"
                p { font-size: 16px; line-height: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);
        let text = &layout.children[0];

        assert_eq!(text.dimensions.content.height, 30.0);
    }

    #[test]
    fn line_height_percent_resolves_against_own_font_size() {
        // 150% of 20px font-size = 30px line height.
        let styled = styled_root(
            r#"<p>Hi</p>"#,
            r#"
                p { font-size: 20px; line-height: 150%; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);
        let text = &layout.children[0];

        assert_eq!(text.dimensions.content.height, 30.0);
    }

    #[test]
    fn line_height_number_inherits_and_reapplies_per_descendant_font_size() {
        // Per CSS spec, a unitless line-height inherits as the bare number,
        // so descendants apply it against their *own* font-size — span's
        // 24px font × 1.5 multiplier = 36px line box, even though p itself
        // is 16px.
        let styled = styled_root(
            r#"<p><span>X</span></p>"#,
            r#"
                p { font-size: 16px; line-height: 1.5; }
                span { font-size: 24px; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);
        let span = &layout.children[0];

        assert_eq!(span.dimensions.content.height, 36.0);
    }

    #[test]
    fn sup_inline_shifts_baseline_up_relative_to_sibling_text() {
        // Phase 6.D: a `<sup>` inside a paragraph should sit above the
        // baseline of the sibling text run that lives on the same line.
        // The exact offset depends on the cascaded font-size (which
        // `<sup>` shrinks via `font-size: smaller`), so the test checks
        // the relative ordering rather than a specific pixel.
        let styled = styled_root(r#"<p>X<sup>1</sup></p>"#, r#""#);
        let layout = layout_tree(&styled, 400.0);

        let plain_text = &layout.children[0];
        let sup_element = &layout.children[1];

        assert!(
            sup_element.dimensions.content.y < plain_text.dimensions.content.y,
            "sup element should sit above the plain text baseline (sup y = {}, text y = {})",
            sup_element.dimensions.content.y,
            plain_text.dimensions.content.y,
        );
    }

    #[test]
    fn line_box_stretches_to_tallest_inline_child() {
        // A line containing a 12px span and a 30px span should pick up the
        // taller child's line box, not the sum. With Phase 6.B's
        // metric-driven `line-height: normal` ≈ 1.2× the line heights are
        // 14.4 and 36 → max = 36.
        let styled = styled_root(
            r#"<p><span class="small">a</span><span class="big">b</span></p>"#,
            r#"
                p { font-size: 16px; }
                .small { font-size: 12px; }
                .big { font-size: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);

        assert_eq!(layout.dimensions.content.height, 36.0);
    }

    #[test]
    fn left_floats_stack_horizontally_at_same_y() {
        // Two `float: left` siblings should line up side by side at the
        // current cursor (y = 0), and the parent should grow to the float's
        // height even though no in-flow child contributes any height.
        let styled = styled_root(
            r#"<div id="root"><div class="f"></div><div class="f"></div></div>"#,
            r#"
                #root { width: 400px; }
                .f { float: left; width: 100px; height: 50px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let first = &layout.children[0];
        let second = &layout.children[1];

        assert_eq!(first.dimensions.content.x, 0.0);
        assert_eq!(first.dimensions.content.y, 0.0);
        assert_eq!(second.dimensions.content.x, 100.0);
        assert_eq!(second.dimensions.content.y, 0.0);
        // Parent height extends to cover the floats even with zero in-flow content.
        assert_eq!(layout.dimensions.content.height, 50.0);
    }

    #[test]
    fn auto_width_floats_shrink_to_fit_their_content() {
        // Step 3d (d2's post meta line: `dt, dd {float: left}` in a 690px
        // `dl`): a float without a width is as wide as its content, so the
        // floats sit side by side at the start of the line — before, each
        // took the whole 690px and they marched off to x = 690, 1380, …
        let styled = styled_root(
            r#"<dl id="root"><dt>by</dt><dd>2026.11.26</dd><dd>509</dd></dl>"#,
            r#"
                #root { width: 690px; margin: 0; }
                dt, dd { float: left; margin: 0 5px 0 0; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let boxes: Vec<_> = layout.children.iter().map(outer_rect).collect();
        assert_eq!(boxes.len(), 3, "{boxes:?}");
        let mut x = 0.0;
        for b in &boxes {
            assert_eq!(b.y, 0.0, "one row: {boxes:?}");
            assert_eq!(b.x, x, "packed left to right: {boxes:?}");
            assert!(b.width < 150.0, "shrunk to its text: {boxes:?}");
            x += b.width;
        }
    }

    #[test]
    fn floats_that_do_not_fit_wrap_below_earlier_floats() {
        let styled = styled_root(
            r#"<div id="root"><div class="f"></div><div class="f"></div><div class="f"></div></div>"#,
            r#"
                #root { width: 100px; }
                .f { float: left; width: 40px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let at = |i: usize| {
            let d = layout.children[i].dimensions.content;
            (d.x, d.y)
        };
        assert_eq!(at(0), (0.0, 0.0));
        assert_eq!(at(1), (40.0, 0.0));
        assert_eq!(at(2), (0.0, 20.0), "no room for 40px after 80px");
        assert_eq!(layout.dimensions.content.height, 40.0);
    }

    #[test]
    fn right_float_with_auto_width_shrinks_to_its_content() {
        let styled = styled_root(
            r#"<div id="root"><span class="f">ok</span></div>"#,
            r#"
                #root { width: 400px; }
                .f { float: right; margin: 0 auto; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let f = outer_rect(&layout.children[0]);
        assert!(f.width > 0.0 && f.width < 100.0, "{f:?}");
        // `auto` margins on a float are 0: flush with the right edge.
        assert_eq!(f.x + f.width, 400.0, "{f:?}");
    }

    #[test]
    fn right_float_pins_to_parent_right_edge() {
        // The right float's outer right edge should land at parent's content
        // right edge — measured then shifted into place.
        let styled = styled_root(
            r#"<div id="root"><div class="f"></div></div>"#,
            r#"
                #root { width: 400px; }
                .f { float: right; width: 80px; height: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let f = &layout.children[0];

        // 400 - 80 = 320: float starts there.
        assert_eq!(f.dimensions.content.x, 320.0);
        assert_eq!(f.dimensions.content.y, 0.0);
    }

    #[test]
    fn float_does_not_advance_cursor_for_following_block_sibling() {
        // Without `clear`, an in-flow block sibling that follows a float
        // sits at the same y as the float — it does not get pushed below.
        let styled = styled_root(
            r#"<div id="root"><div class="f"></div><div class="b"></div></div>"#,
            r#"
                #root { width: 400px; }
                .f { float: left; width: 100px; height: 80px; }
                .b { height: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let block = &layout.children[1];

        assert_eq!(block.dimensions.content.x, 0.0);
        assert_eq!(block.dimensions.content.y, 0.0);
        // Parent height covers the float (80) since the in-flow block (30) is shorter.
        assert_eq!(layout.dimensions.content.height, 80.0);
    }

    #[test]
    fn clear_both_pushes_block_below_all_preceding_floats() {
        // `clear: both` jumps the cursor past the tallest float on either
        // side so the block lands cleanly below them.
        let styled = styled_root(
            r#"<div id="root"><div class="left"></div><div class="right"></div><div class="cleared"></div></div>"#,
            r#"
                #root { width: 400px; }
                .left { float: left; width: 100px; height: 80px; }
                .right { float: right; width: 80px; height: 50px; }
                .cleared { clear: both; height: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let cleared = &layout.children[2];

        // Tallest float bottom = max(80, 50) = 80 → clear lands here.
        assert_eq!(cleared.dimensions.content.y, 80.0);
        // Parent height = 80 (clear pos) + 30 (cleared block).
        assert_eq!(layout.dimensions.content.height, 110.0);
    }

    #[test]
    fn float_does_not_break_margin_collapse_chain() {
        // A float between two in-flow blocks behaves like an out-of-flow
        // box for margin collapse — it neither contributes to nor breaks
        // the collapse between its non-floated neighbours.
        let styled = styled_root(
            r#"<div id="root"><div class="a"></div><div class="f"></div><div class="b"></div></div>"#,
            r#"
                #root { width: 400px; }
                .a { height: 20px; margin-bottom: 30px; }
                .f { float: left; width: 50px; height: 40px; }
                .b { height: 15px; margin-top: 10px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let b = &layout.children[2];

        // Same outcome as if .f were a regular out-of-flow box: gap = max(30, 10) = 30.
        assert_eq!(b.dimensions.content.y, 50.0);
    }

    #[test]
    fn clear_resets_float_stack_column_for_following_floats() {
        // After `clear: left`, the next left float starts at content_x again
        // (not stacked beside the cleared-out float), because the cleared
        // cursor is below all preceding left floats.
        let styled = styled_root(
            r#"<div id="root"><div class="f1"></div><div class="block"></div><div class="f2"></div></div>"#,
            r#"
                #root { width: 400px; }
                .f1 { float: left; width: 100px; height: 60px; }
                .block { clear: left; height: 20px; }
                .f2 { float: left; width: 100px; height: 40px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let f1 = &layout.children[0];
        let block = &layout.children[1];
        let f2 = &layout.children[2];

        assert_eq!(f1.dimensions.content.x, 0.0);
        assert_eq!(f1.dimensions.content.y, 0.0);
        // .block clears past f1 (60), then is laid out with height 20 → cursor=80.
        assert_eq!(block.dimensions.content.y, 60.0);
        // f2 lays out at the new cursor (80), restarting the left column at x=0.
        assert_eq!(f2.dimensions.content.x, 0.0);
        assert_eq!(f2.dimensions.content.y, 80.0);
    }

    #[test]
    fn margin_auto_collapses_when_width_is_auto() {
        let styled = styled_root(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    margin-left: auto;
                    margin-right: auto;
                }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);

        // CSS spec: with width: auto, auto margins collapse to 0 and content fills.
        assert_eq!(layout.dimensions.margin.left, 0.0);
        assert_eq!(layout.dimensions.margin.right, 0.0);
        assert_eq!(layout.dimensions.content.width, 400.0);
    }

    #[test]
    fn flex_container_box_type_is_flex_node() {
        // The container itself becomes a FlexNode so render/hit-test code can
        // tell it apart from a plain block. Children stay as BlockNodes — only
        // the container changes box_type.
        let styled = styled_root(
            r#"<div id="row"><div class="item"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; }
                .item { width: 100px; height: 50px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert!(matches!(layout.box_type, super::BoxType::FlexNode(_)));
        assert!(matches!(
            layout.children[0].box_type,
            super::BoxType::BlockNode(_)
        ));
    }

    #[test]
    fn native_flex_row_with_inline_content_block_children_does_not_squash_to_zero() {
        // HN/Haskell-blog-style nav: a flex row containing children that
        // each hold inline text. Forcing `display: block` on the children
        // pushes the parent through the *native* taffy flex path (it would
        // otherwise route through the boundary path because all children
        // are inline-displayed by default). The block children themselves
        // become boundary leaves because their content is inline.
        //
        // Before 5.4 the MinContent probe in `measure_boundary` ran the
        // legacy block algorithm at `parent_width = 0`, which collapsed
        // the inline shrink-to-fit width to 0. taffy then saw items with
        // zero min-content size and could legally shrink them all the way
        // down — the whole nav collapsed.
        //
        // After 5.4 the measure callback returns the legacy box width even
        // for the MinContent probe (using a finite non-zero width that
        // mirrors the unconstrained natural-text size), so taffy refuses
        // to shrink each item below its content and the row stays visible.
        let styled = styled_root(
            r#"<header id="nav"><a class="link">Posts</a><a class="link">Categories</a><a class="link">About</a><a class="link">Search</a></header>"#,
            r#"
                #nav { display: flex; }
                .link { display: block; font-size: 16px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(
            layout.children.len(),
            4,
            "all four nav items should survive flex sizing"
        );
        for (i, child) in layout.children.iter().enumerate() {
            assert!(
                child.dimensions.content.width > 0.0,
                "child {i} squashed to zero width — got {}",
                child.dimensions.content.width
            );
            assert!(
                child.dimensions.content.height > 0.0,
                "child {i} squashed to zero height — got {}",
                child.dimensions.content.height
            );
        }
        let first = &layout.children[0];
        let second = &layout.children[1];
        let first_right = first.dimensions.content.x + first.dimensions.content.width;
        assert!(
            second.dimensions.content.x >= first_right,
            "second item should sit to the right of the first, got x={} after right={}",
            second.dimensions.content.x,
            first_right
        );
    }

    #[test]
    fn native_flex_row_with_nested_inline_children_keeps_natural_widths() {
        // Closer to real-world nav: each flex item is itself an `<a>`
        // wrapping nested inline spans, and the link is forced to
        // `display: block` so the container goes through the native taffy
        // flex path. The nested structure exercises the inline-flow branch
        // of `measure_boundary` more thoroughly than a single text child.
        let styled = styled_root(
            r#"<header id="nav"><a class="link"><span>Posts</span></a><a class="link"><span>Categories</span></a></header>"#,
            r#"
                #nav { display: flex; }
                .link { display: block; font-size: 16px; padding: 4px 8px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        for child in layout.children.iter() {
            assert!(
                child.dimensions.content.width > 0.0,
                "nested-inline link child squashed: got width {}",
                child.dimensions.content.width
            );
        }
    }

    #[test]
    fn native_flex_row_with_padded_inline_block_children_preserves_widths() {
        // Some pages render nav links as inline-block with padding. Forcing
        // a native flex row over inline-block children stresses the
        // boundary measure path because inline-block items also flow
        // through `layout_inline_block_node` shrink-to-fit, where a
        // parent_width of 0 used to collapse the natural text size.
        let styled = styled_root(
            r#"<header id="nav"><a class="link">Posts</a><a class="link">Categories</a><div></div></header>"#,
            r#"
                #nav { display: flex; }
                .link { display: inline-block; padding: 6px 12px; font-size: 16px; }
                div { width: 1px; height: 1px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        // Two link items + the spacer div.
        assert_eq!(layout.children.len(), 3);
        for (i, child) in layout.children.iter().take(2).enumerate() {
            assert!(
                child.dimensions.content.width > 0.0,
                "inline-block link {i} squashed: got width {}",
                child.dimensions.content.width
            );
        }
    }

    #[test]
    fn flex_row_lays_children_horizontally_at_flex_start() {
        // Three explicit-width items in a flex row should sit shoulder-to-shoulder
        // starting at the container's content_x, not stacked vertically.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div><div class="c"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; }
                .a { width: 60px; height: 40px; }
                .b { width: 80px; height: 30px; }
                .c { width: 100px; height: 50px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];
        let c = &layout.children[2];

        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 60.0);
        assert_eq!(c.dimensions.content.x, 140.0);
        // All sit on the same baseline (commit 1 has no align-items, so they
        // all start at content_y = 0).
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 0.0);
        assert_eq!(c.dimensions.content.y, 0.0);
        // Container's auto height = tallest child outer height = 50.
        assert_eq!(layout.dimensions.content.height, 50.0);
    }

    #[test]
    fn flex_direction_column_stacks_children_vertically() {
        // With flex-direction: column the main axis flips to y. Items still
        // pack at flex-start by default, so they stack at increasing content_y
        // and share content_x. Container's auto height becomes the cumulative
        // main-axis size (sum of children), not the max.
        let styled = styled_root(
            r#"<div id="col"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #col { display: flex; flex-direction: column; width: 200px; }
                .a { width: 80px; height: 30px; }
                .b { width: 60px; height: 40px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 0.0);
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 30.0);
        // Auto height in column flow = sum of children outer heights = 70.
        assert_eq!(layout.dimensions.content.height, 70.0);
    }

    #[test]
    fn justify_content_center_offsets_items_by_half_leftover() {
        // 3 items totaling 180px in a 400px row → 220px leftover. center
        // pushes the start of the run by half (110px) so the cluster sits
        // centered; items remain shoulder-to-shoulder within the cluster.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div><div class="c"></div></div>"#,
            r#"
                #row { display: flex; justify-content: center; width: 400px; }
                .a { width: 60px; height: 20px; }
                .b { width: 60px; height: 20px; }
                .c { width: 60px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.children[0].dimensions.content.x, 110.0);
        assert_eq!(layout.children[1].dimensions.content.x, 170.0);
        assert_eq!(layout.children[2].dimensions.content.x, 230.0);
    }

    #[test]
    fn justify_content_flex_end_pins_run_to_main_axis_end() {
        // 100 + 80 + 60 = 240 used; 400 - 240 = 160 leftover all up front so
        // the run ends at the container's right edge.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div><div class="c"></div></div>"#,
            r#"
                #row { display: flex; justify-content: flex-end; width: 400px; }
                .a { width: 100px; height: 20px; }
                .b { width: 80px; height: 20px; }
                .c { width: 60px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.children[0].dimensions.content.x, 160.0);
        assert_eq!(layout.children[1].dimensions.content.x, 260.0);
        assert_eq!(layout.children[2].dimensions.content.x, 340.0);
    }

    #[test]
    fn justify_content_space_between_distributes_leftover_into_n_minus_1_gaps() {
        // 3 items × 60px = 180 used; 400 - 180 = 220 leftover; n-1 = 2 gaps;
        // each gap = 110. First item pinned to start, last to end.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div><div class="c"></div></div>"#,
            r#"
                #row { display: flex; justify-content: space-between; width: 400px; }
                .a { width: 60px; height: 20px; }
                .b { width: 60px; height: 20px; }
                .c { width: 60px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.children[0].dimensions.content.x, 0.0);
        assert_eq!(layout.children[1].dimensions.content.x, 170.0);
        assert_eq!(layout.children[2].dimensions.content.x, 340.0);
    }

    #[test]
    fn justify_content_center_works_in_column_direction_with_explicit_height() {
        // Column flex needs an explicit container height for justify-content
        // to mean anything — without it, container height = total used and
        // there is no leftover to distribute. With height: 200 and total = 100,
        // leftover = 100, center offsets the run by 50.
        let styled = styled_root(
            r#"<div id="col"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #col {
                    display: flex;
                    flex-direction: column;
                    justify-content: center;
                    width: 200px;
                    height: 200px;
                }
                .a { width: 50px; height: 40px; }
                .b { width: 50px; height: 60px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.children[0].dimensions.content.y, 50.0);
        assert_eq!(layout.children[1].dimensions.content.y, 90.0);
    }

    #[test]
    fn align_items_default_stretches_items_to_container_cross_size() {
        // align-items defaults to stretch. The shorter item (height: 20) grows
        // to match the container's cross size. Container has explicit height
        // 100, so both items end up at outer_height = 100.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; height: 100px; }
                .a { width: 60px; }
                .b { width: 60px; height: 40px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        // Item .a has no explicit height → stretched to fill 100.
        assert_eq!(a.dimensions.content.height, 100.0);
        // Item .b had explicit height 40 → stretch leaves it alone.
        assert_eq!(b.dimensions.content.height, 40.0);
        // Both items align at content_y = 0 (stretch and flex-start both pin
        // the cross-start to the container start).
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 0.0);
    }

    #[test]
    fn align_items_center_offsets_each_item_by_half_cross_leftover() {
        // Items have different heights (40, 60). Container height = 100.
        // center: each item shifts down by (100 - item_height) / 2.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row {
                    display: flex;
                    align-items: center;
                    width: 400px;
                    height: 100px;
                }
                .a { width: 60px; height: 40px; }
                .b { width: 60px; height: 60px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.y, 30.0);
        assert_eq!(b.dimensions.content.y, 20.0);
        // Heights stay as declared (no stretch when align is not stretch).
        assert_eq!(a.dimensions.content.height, 40.0);
        assert_eq!(b.dimensions.content.height, 60.0);
    }

    #[test]
    fn align_items_flex_end_pins_each_item_to_cross_end() {
        // Each item shifts down by (container_cross - item_cross), so both
        // bottoms land at the container's content-bottom (y = 100).
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row {
                    display: flex;
                    align-items: flex-end;
                    width: 400px;
                    height: 100px;
                }
                .a { width: 60px; height: 40px; }
                .b { width: 60px; height: 60px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.y, 60.0);
        assert_eq!(b.dimensions.content.y, 40.0);
    }

    #[test]
    fn align_items_flex_start_keeps_items_at_cross_origin() {
        // flex-start matches the original commit-1 behavior: items pinned to
        // the cross-start regardless of size differences. Crucially this
        // disables the default stretch, so the shorter item keeps its natural
        // (zero) height.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row {
                    display: flex;
                    align-items: flex-start;
                    width: 400px;
                    height: 100px;
                }
                .a { width: 60px; }
                .b { width: 60px; height: 60px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 0.0);
        // No stretch — .a's auto height stays 0 (no children, no font-size
        // intrinsic on a div).
        assert_eq!(a.dimensions.content.height, 0.0);
    }

    #[test]
    fn align_items_stretch_grows_cross_axis_in_column_direction() {
        // In column flow, cross axis = width. Stretch grows items without an
        // explicit width to fill the container's content width (200).
        let styled = styled_root(
            r#"<div id="col"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #col { display: flex; flex-direction: column; width: 200px; }
                .a { height: 30px; }
                .b { width: 80px; height: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        // Item .a stretches across the cross axis to 200; .b's explicit width
        // wins.
        assert_eq!(a.dimensions.content.width, 200.0);
        assert_eq!(b.dimensions.content.width, 80.0);
    }

    #[test]
    fn flex_grow_distributes_positive_free_space_proportionally() {
        // Container = 400px. Two items at 50px each → 100px used, 300px free.
        // .a has flex-grow: 1, .b has flex-grow: 2 → split 100 : 200, so
        // .a outer becomes 50+100 = 150, .b outer becomes 50+200 = 250.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; }
                .a { width: 50px; height: 30px; flex-grow: 1; }
                .b { width: 50px; height: 30px; flex-grow: 2; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.width, 150.0);
        assert_eq!(b.dimensions.content.width, 250.0);
        // After grow, items pack shoulder-to-shoulder again from the start.
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 150.0);
    }

    #[test]
    fn flex_grow_zero_keeps_item_at_basis() {
        // Default flex-grow is 0, so an item without an explicit flex-grow
        // should not absorb any of the 300px free space — only .b grows.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; }
                .a { width: 50px; height: 30px; }
                .b { width: 50px; height: 30px; flex-grow: 1; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.width, 50.0);
        assert_eq!(b.dimensions.content.width, 350.0);
    }

    #[test]
    fn flex_shrink_distributes_overflow_weighted_by_basis() {
        // Container = 200px but items demand 300px (3 × 100). Default shrink
        // is 1 for each, total weight = sum(1 × 100) = 300. Each item shrinks
        // by 100 × (100/300) ≈ 33.33 → final width ≈ 66.67.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div><div class="c"></div></div>"#,
            r#"
                #row { display: flex; width: 200px; }
                .a { width: 100px; height: 20px; }
                .b { width: 100px; height: 20px; }
                .c { width: 100px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];

        // 100 - (100 * (1*100) / (3*100)) = 100 - 33.333 ≈ 66.67
        let expected = 100.0 - 100.0 / 3.0;
        assert!((a.dimensions.content.width - expected).abs() < 0.01);
    }

    #[test]
    fn flex_shrink_zero_pins_item_to_basis_during_shrink() {
        // flex-shrink: 0 opts out of shrinking. .a stays at 200px and .b
        // absorbs the entire overflow. With container = 250px, .b has
        // basis = 100px and overflow = -50px; .b ends up at 100 - 50 = 50.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row { display: flex; width: 250px; }
                .a { width: 200px; height: 20px; flex-shrink: 0; }
                .b { width: 100px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.width, 200.0);
        assert_eq!(b.dimensions.content.width, 50.0);
    }

    #[test]
    fn flex_basis_overrides_explicit_width() {
        // CSS spec: flex-basis takes precedence over width on flex items. With
        // basis = 80, the item starts at 80 regardless of width = 200, so
        // free space = 400 - (80 + 50) = 270 and grow:1 makes .a = 80+270=350.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; }
                .a { width: 200px; height: 20px; flex-basis: 80px; flex-grow: 1; }
                .b { width: 50px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.width, 350.0);
        assert_eq!(b.dimensions.content.width, 50.0);
    }

    #[test]
    fn flex_shorthand_one_number_sets_grow_only() {
        // `flex: 2` = `2 1 0%` (step 1b — the basis used to stay unset,
        // i.e. the 50px width): all 400px are free space, split 2:1.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; }
                .a { width: 50px; height: 20px; flex: 2; }
                .b { width: 50px; height: 20px; flex: 1; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert!((a.dimensions.content.width - 800.0 / 3.0).abs() < 0.01);
        assert!((b.dimensions.content.width - 400.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn flex_grow_works_in_column_direction_with_explicit_height() {
        // Column flex needs an explicit container height for grow to find any
        // free space. Container height = 200, items use 60 total → 140 free,
        // split equally between two flex-grow:1 items → +70 each.
        let styled = styled_root(
            r#"<div id="col"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #col {
                    display: flex;
                    flex-direction: column;
                    width: 200px;
                    height: 200px;
                }
                .a { width: 50px; height: 30px; flex-grow: 1; }
                .b { width: 50px; height: 30px; flex-grow: 1; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.height, 100.0);
        assert_eq!(b.dimensions.content.height, 100.0);
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 100.0);
    }

    #[test]
    fn grid_container_box_type_is_grid_node() {
        let styled = styled_root(
            r#"<div id="g"><div></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: 100px 100px; width: 200px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        assert!(matches!(layout.box_type, super::BoxType::GridNode(_)));
    }

    #[test]
    fn grid_two_fixed_columns_place_items_side_by_side() {
        // Two 100px columns → first item at x=0 width=100, second at x=100
        // width=100. With one row, container height = max child outer height.
        let styled = styled_root(
            r#"<div id="g"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: 100px 100px; width: 200px; }
                .a { height: 50px; }
                .b { height: 70px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 100.0);
        assert_eq!(a.dimensions.content.y, 0.0);
        assert_eq!(b.dimensions.content.y, 0.0);
        // Items without explicit width fill their track.
        assert_eq!(a.dimensions.content.width, 100.0);
        assert_eq!(b.dimensions.content.width, 100.0);
        // Container height = single-row max = 70.
        assert_eq!(layout.dimensions.content.height, 70.0);
    }

    #[test]
    fn grid_auto_flow_wraps_to_next_row_after_columns_full() {
        // Three 100px columns + 4 items → 4th item wraps to row 2 col 0.
        // Row 1 height = max(20, 30, 40) = 40, row 2 height = 25.
        // 4th item should land at y = 40, x = 0.
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="b"></div>
                <div class="c"></div>
                <div class="d"></div>
            </div>"#,
            r#"
                #g { display: grid; grid-template-columns: 100px 100px 100px; width: 300px; }
                .a { height: 20px; }
                .b { height: 30px; }
                .c { height: 40px; }
                .d { height: 25px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let d = &layout.children[3];

        assert_eq!(d.dimensions.content.x, 0.0);
        assert_eq!(d.dimensions.content.y, 40.0);
        // Container height = sum(row heights) = 40 + 25 = 65.
        assert_eq!(layout.dimensions.content.height, 65.0);
    }

    #[test]
    fn grid_fr_unit_distributes_free_space_proportionally() {
        // Container = 400px; tracks = 100px 1fr 3fr → fixed=100, free=300,
        // total_fr=4 → 1fr=75, 3fr=225. Columns: 100, 75, 225.
        let styled = styled_root(
            r#"<div id="g"><div class="a"></div><div class="b"></div><div class="c"></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: 100px 1fr 3fr; width: 400px; }
                .a { height: 20px; }
                .b { height: 20px; }
                .c { height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];
        let c = &layout.children[2];

        assert_eq!(a.dimensions.content.width, 100.0);
        assert_eq!(b.dimensions.content.width, 75.0);
        assert_eq!(c.dimensions.content.width, 225.0);
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 100.0);
        assert_eq!(c.dimensions.content.x, 175.0);
    }

    #[test]
    fn grid_auto_track_sizes_to_widest_column_item() {
        // 3 columns: 100px, auto, 1fr. Container = 400px.
        // Items in col 1 (the auto column) have natural widths 80 and 60 →
        // auto track = 80. Fixed budget = 100 + 80 = 180. Free = 220 → 1fr = 220.
        // So columns = [100, 80, 220], offsets = [0, 100, 180].
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="b"></div>
                <div class="c"></div>
                <div class="d"></div>
                <div class="e"></div>
                <div class="f"></div>
            </div>"#,
            r#"
                #g { display: grid; grid-template-columns: 100px auto 1fr; width: 400px; }
                .a, .d { height: 20px; }
                .b { width: 80px; height: 20px; }
                .c, .f { height: 20px; }
                .e { width: 60px; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);

        // First row: a (col 0), b (col 1, auto), c (col 2, fr)
        let a = &layout.children[0];
        let b = &layout.children[1];
        let c = &layout.children[2];
        // Second row: d, e, f
        let e = &layout.children[4];

        // Column offsets should be 0, 100, 180.
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 100.0);
        assert_eq!(c.dimensions.content.x, 180.0);
        // Auto track width = 80 (max of items in col 1) → b stays at 80,
        // and e (60) stays at 60 (post-hoc fill won't shrink below explicit width).
        assert_eq!(b.dimensions.content.width, 80.0);
        assert_eq!(e.dimensions.content.width, 60.0);
        // 1fr column = leftover = 400 - 180 = 220.
        assert_eq!(c.dimensions.content.width, 220.0);
    }

    #[test]
    fn grid_auto_track_with_no_items_collapses_to_zero() {
        // Auto track with no items in the column → natural max = 0 → track = 0.
        // Useful for testing that fr tracks still share leftover correctly.
        let styled = styled_root(
            r#"<div id="g"><div class="a"></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: auto 1fr; width: 200px; }
                .a { height: 20px; width: 60px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];

        // Item lands in col 0 (auto). Natural width = 60 → auto track = 60.
        // 1fr in col 1 takes leftover 140 (no items).
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(a.dimensions.content.width, 60.0);
        // Container width is set; child of col 1 is none, so no test there.
    }

    #[test]
    fn grid_template_rows_overrides_natural_row_heights() {
        // Two-column grid with grid-template-rows: 80px 50px. Items have
        // natural heights that would auto-fit to smaller rows, but the
        // explicit template forces row 0 = 80, row 1 = 50.
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="b"></div>
                <div class="c"></div>
                <div class="d"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 100px 100px;
                    grid-template-rows: 80px 50px;
                    width: 200px;
                }
                .a, .b, .c, .d { height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);

        // Row 1 items (c, d) should sit at y = 80 (row 0 height).
        let c = &layout.children[2];
        let d = &layout.children[3];
        assert_eq!(c.dimensions.content.y, 80.0);
        assert_eq!(d.dimensions.content.y, 80.0);
        // Container height = 80 + 50 = 130.
        assert_eq!(layout.dimensions.content.height, 130.0);
    }

    #[test]
    fn grid_template_rows_auto_keyword_sizes_to_content() {
        // Mixed template: row 0 = auto (sizes to its tallest item), row 1 =
        // 100px (fixed). Items in row 0 are 30 and 50 → row 0 = 50.
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="b"></div>
                <div class="c"></div>
                <div class="d"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 100px 100px;
                    grid-template-rows: auto 100px;
                    width: 200px;
                }
                .a { height: 30px; }
                .b { height: 50px; }
                .c, .d { height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let c = &layout.children[2];

        // Row 0 collapsed to max(30, 50) = 50; row 1 starts at y=50.
        assert_eq!(c.dimensions.content.y, 50.0);
        // Container height = 50 + 100 = 150.
        assert_eq!(layout.dimensions.content.height, 150.0);
    }

    #[test]
    fn grid_template_rows_fr_distributes_against_explicit_height() {
        // Container height = 300, two rows: 100px and 1fr → free = 200, 1fr = 200.
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="b"></div>
                <div class="c"></div>
                <div class="d"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 100px 100px;
                    grid-template-rows: 100px 1fr;
                    width: 200px;
                    height: 300px;
                }
                .a, .b, .c, .d { height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let c = &layout.children[2];

        // Row 1 starts at y = 100 (row 0 height).
        assert_eq!(c.dimensions.content.y, 100.0);
        // Container's auto height (sum) = 100 + 200 = 300.
        assert_eq!(layout.dimensions.content.height, 300.0);
    }

    #[test]
    fn grid_template_rows_falls_back_to_natural_for_extra_rows() {
        // 3 rows of items but only 2 declared → row 2 falls back to its
        // natural max height (here a single 70px item).
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="b"></div>
                <div class="c"></div>
                <div class="d"></div>
                <div class="e"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 100px 100px;
                    grid-template-rows: 50px 50px;
                    width: 200px;
                }
                .a, .b, .c, .d { height: 20px; }
                .e { height: 70px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let e = &layout.children[4];

        // Row 2 starts at y = 100 (50 + 50), and fills to 70 (natural).
        assert_eq!(e.dimensions.content.y, 100.0);
        // Container height = 50 + 50 + 70 = 170.
        assert_eq!(layout.dimensions.content.height, 170.0);
    }

    #[test]
    fn grid_column_explicit_placement_anchors_item_to_specified_track() {
        // 4-column grid, single item with grid-column: 3 → item lands at
        // col 2 (line 3 - 1), not col 0.
        let styled = styled_root(
            r#"<div id="g"><div class="a"></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: 50px 50px 50px 50px; width: 200px; }
                .a { height: 30px; }
                .a { grid-column: 3; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        // col 2 starts at x = 100 (50 + 50).
        assert_eq!(a.dimensions.content.x, 100.0);
        // Single-cell span keeps width at the track width = 50.
        assert_eq!(a.dimensions.content.width, 50.0);
    }

    #[test]
    fn grid_column_span_widens_item_across_multiple_tracks() {
        // grid-column: 1 / span 3 → cells 0..3, sum widths = 50+50+50 = 150.
        let styled = styled_root(
            r#"<div id="g"><div class="a"></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: 50px 50px 50px 50px; width: 200px; }
                .a { height: 30px; grid-column: 1 / span 3; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(a.dimensions.content.width, 150.0);
    }

    #[test]
    fn grid_row_span_grows_height_to_cover_span() {
        // 2 columns, items #1 spans both rows. Items #2 and #3 fill row 0
        // col 1 and row 1 col 1 via auto-flow.
        let styled = styled_root(
            r#"<div id="g">
                <div class="span"></div>
                <div class="b"></div>
                <div class="c"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 100px 100px;
                    grid-template-rows: 40px 60px;
                    width: 200px;
                }
                .span { grid-row: 1 / span 2; }
                .b { height: 20px; }
                .c { height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let span_box = &layout.children[0];
        let b = &layout.children[1];
        let c = &layout.children[2];

        // Spanning item lives at (0, 0) and grows to row 0 + row 1 = 100 high.
        assert_eq!(span_box.dimensions.content.x, 0.0);
        assert_eq!(span_box.dimensions.content.y, 0.0);
        assert_eq!(span_box.dimensions.content.height, 100.0);
        // Auto-flow lands b at (0, 1) and c at (1, 1) — column 0 of row 1
        // is occupied by the spanning item.
        assert_eq!(b.dimensions.content.x, 100.0);
        assert_eq!(b.dimensions.content.y, 0.0);
        assert_eq!(c.dimensions.content.x, 100.0);
        assert_eq!(c.dimensions.content.y, 40.0);
    }

    #[test]
    fn grid_auto_flow_skips_cells_occupied_by_explicit_placement() {
        // Item .first explicitly occupies col 0 / row 1. Auto-flow items go
        // around it: a → (0, 0), b → (0, 1), .first → (1, 0), c → (1, 1).
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="b"></div>
                <div class="first"></div>
                <div class="c"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 50px 50px;
                    grid-template-rows: 30px 30px;
                    width: 100px;
                }
                .a, .b, .c { height: 20px; }
                .first { grid-column: 1; grid-row: 2; height: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];
        let first = &layout.children[2];
        let c = &layout.children[3];

        assert_eq!((a.dimensions.content.x, a.dimensions.content.y), (0.0, 0.0));
        assert_eq!(
            (b.dimensions.content.x, b.dimensions.content.y),
            (50.0, 0.0)
        );
        assert_eq!(
            (first.dimensions.content.x, first.dimensions.content.y),
            (0.0, 30.0)
        );
        assert_eq!(
            (c.dimensions.content.x, c.dimensions.content.y),
            (50.0, 30.0)
        );
    }

    #[test]
    fn grid_template_areas_places_named_items() {
        // 3-column grid; header spans all 3, sidebar takes (1,0), main takes
        // (1,1) and (1,2), footer spans all 3 in row 2. Items reference areas
        // by name via grid-area.
        let styled = styled_root(
            r#"<div id="g">
                <div class="header"></div>
                <div class="sidebar"></div>
                <div class="main"></div>
                <div class="footer"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 50px 50px 50px;
                    grid-template-rows: 30px 60px 40px;
                    grid-template-areas: "h h h" "s m m" "f f f";
                    width: 150px;
                }
                .header { grid-area: h; }
                .sidebar { grid-area: s; }
                .main { grid-area: m; }
                .footer { grid-area: f; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let header = &layout.children[0];
        let sidebar = &layout.children[1];
        let main_box = &layout.children[2];
        let footer = &layout.children[3];

        // header spans the whole top row
        assert_eq!(header.dimensions.content.x, 0.0);
        assert_eq!(header.dimensions.content.y, 0.0);
        assert_eq!(header.dimensions.content.width, 150.0);
        assert_eq!(header.dimensions.content.height, 30.0);

        // sidebar = single cell at (1, 0)
        assert_eq!(sidebar.dimensions.content.x, 0.0);
        assert_eq!(sidebar.dimensions.content.y, 30.0);
        assert_eq!(sidebar.dimensions.content.width, 50.0);
        assert_eq!(sidebar.dimensions.content.height, 60.0);

        // main spans cols 1-2 of row 1
        assert_eq!(main_box.dimensions.content.x, 50.0);
        assert_eq!(main_box.dimensions.content.y, 30.0);
        assert_eq!(main_box.dimensions.content.width, 100.0);
        assert_eq!(main_box.dimensions.content.height, 60.0);

        // footer spans the whole bottom row
        assert_eq!(footer.dimensions.content.x, 0.0);
        assert_eq!(footer.dimensions.content.y, 90.0);
        assert_eq!(footer.dimensions.content.width, 150.0);
        assert_eq!(footer.dimensions.content.height, 40.0);
    }

    #[test]
    fn grid_template_areas_dot_skips_cells_for_auto_flow() {
        // template-areas leaves cell (0, 1) unnamed (`.`). An item without a
        // grid-area name should auto-flow into that empty slot.
        let styled = styled_root(
            r#"<div id="g">
                <div class="a"></div>
                <div class="filler"></div>
                <div class="b"></div>
            </div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-columns: 50px 50px 50px;
                    grid-template-rows: 30px;
                    grid-template-areas: "a . b";
                    width: 150px;
                }
                .a { grid-area: a; }
                .b { grid-area: b; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let filler = &layout.children[1];
        let b = &layout.children[2];

        // a anchored at col 0, b anchored at col 2 (both via template-areas)
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 100.0);
        // filler has no grid-area → auto-flows into the open cell at (0, 1).
        assert_eq!(filler.dimensions.content.x, 50.0);
        assert_eq!(filler.dimensions.content.y, 0.0);
    }

    #[test]
    fn display_none_children_get_no_box_in_any_container() {
        // Google nests `<style>` inside a button; its CSS text was laid out
        // and painted although `style` is `display: none`.
        fn has_style_box(layout: &super::LayoutBox) -> bool {
            let own = box_styled_node(layout).is_some_and(|node| {
                matches!(&node.node_type, crate::dom::NodeType::Element(e) if e.tag_name == "style")
            });
            own || layout.children.iter().any(has_style_box)
        }
        for display in [
            "block",
            "inline",
            "inline-block",
            "flex",
            "inline-flex",
            "grid",
            "table-cell",
        ] {
            let styled = styled_root(
                &format!(
                    r#"<div><p>x <button style="display:{display}"><style>.x{{color:red}}</style>Search</button> y</p></div>"#
                ),
                "",
            );
            let layout = layout_tree(&styled, 800.0);
            assert!(
                !has_style_box(&layout),
                "display:{display} container laid out its <style>"
            );
            // Google's shape: the button is itself a flex item.
            let styled = styled_root(
                &format!(
                    r#"<div style="display:flex"><button style="display:{display}; position:relative"><style>.x{{color:red}}</style><div><div></div></div><div></div><div><span>Search</span></div></button></div>"#
                ),
                "",
            );
            let layout = layout_tree(&styled, 800.0);
            assert!(
                !has_style_box(&layout),
                "display:{display} flex item laid out its <style>"
            );
        }
    }

    #[test]
    fn deeply_nested_inline_elements_lay_out_in_linear_time() {
        // Measuring an inline box walked its children once for the width and
        // again for the height, and each child did the same: 2^depth work.
        // Wikipedia's nested `<span>`s took minutes. 28 levels would be
        // ~268M visits the old way; one walk is a few hundred.
        let depth = 28;
        let html = format!(
            "<div>{}x{}</div>",
            "<span>".repeat(depth),
            "</span>".repeat(depth)
        );
        let styled = styled_root(&html, "");
        let started = std::time::Instant::now();
        let _ = layout_tree(&styled, 800.0);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    // Vector 2022 (Wikipedia) desktop shape, synthetic: an areas grid (the
    // legacy path) with `grid-template: <rows> / <columns>`, `minmax()` and
    // a column gap. Chrome: side 200, gap 20, main = 1000 - 220 = 780.
    const VECTOR_LIKE_CSS: &str = r#"
        #g {
            display: grid;
            width: 1000px;
            column-gap: 20px;
            grid-template: min-content 1fr min-content / 200px minmax(0,1fr);
            grid-template-areas: 'note note' 'side main' 'foot foot';
        }
        .note { grid-area: note; height: 10px; }
        .side { grid-area: side; }
        .main { grid-area: main; }
        .foot { grid-area: foot; height: 10px; }
    "#;

    fn vector_like_html(main: &str) -> String {
        format!(
            r#"<div id="g"><div class="note"></div><div class="side">menu</div><div class="main">{main}</div><div class="foot"></div></div>"#
        )
    }

    #[test]
    fn grid_areas_item_is_laid_out_at_its_track_width() {
        // A long paragraph in the `minmax(0,1fr)` column used to be laid
        // out at the container's full width and then shifted right, so it
        // overflowed by the side column's width.
        let long = "word ".repeat(400);
        let styled = styled_root(
            &vector_like_html(&format!("<p>{long}</p>")),
            VECTOR_LIKE_CSS,
        );
        let layout = layout_tree(&styled, 1200.0);
        let side = &layout.children[1];
        let main = &layout.children[2];
        assert_eq!(side.dimensions.content.x, 0.0);
        assert_eq!(side.dimensions.content.width, 200.0);
        assert_eq!(main.dimensions.content.x, 220.0, "after the 20px gap");
        assert_eq!(main.dimensions.content.width, 780.0);
        let paragraph = &main.children[0];
        assert!(
            paragraph.dimensions.content.width <= 780.0,
            "content fits its column: {}",
            paragraph.dimensions.content.width
        );
    }

    #[test]
    fn grid_areas_table_in_a_minmax_column_keeps_a_width() {
        // The enwiki symptom: a navbox table under the main column ended
        // up 0px wide, so every link in it was a sliver.
        let styled = styled_root(
            &vector_like_html(
                r##"<table style="width:100%"><tr><th>Web browsers</th></tr><tr><td><a href="#b">Bookmarks</a></td></tr></table>"##,
            ),
            VECTOR_LIKE_CSS,
        );
        let layout = layout_tree(&styled, 1200.0);
        let main = &layout.children[2];
        let table = &main.children[0];
        assert_eq!(table.dimensions.content.x, 220.0);
        assert_eq!(table.dimensions.content.width, 780.0);
        fn min_cell_width(b: &super::LayoutBox) -> f32 {
            b.children
                .iter()
                .map(min_cell_width)
                .fold(b.dimensions.content.width, f32::min)
        }
        assert!(min_cell_width(table) > 100.0, "no collapsed cell");
    }

    #[test]
    fn grid_areas_minmax_capped_column_and_min_content_column() {
        // `.mw-body`: `minmax(0, 59.25rem) min-content` — the first column
        // stops at its max (948px), the second fits its content.
        let styled = styled_root(
            r#"<div id="g"><div class="c">content</div><div class="e"><div style="width:120px">x</div></div></div>"#,
            r#"
                #g {
                    display: grid;
                    width: 1200px;
                    grid-template: min-content / minmax(0,59.25rem) min-content;
                    grid-template-areas: 'c e';
                }
                .c { grid-area: c; }
                .e { grid-area: e; }
            "#,
        );
        let layout = layout_tree(&styled, 1300.0);
        let content = &layout.children[0];
        let end = &layout.children[1];
        assert_eq!(content.dimensions.content.width, 948.0);
        assert_eq!(end.dimensions.content.x, 948.0);
        assert_eq!(end.dimensions.content.width, 120.0);
    }

    #[test]
    fn grid_minmax_min_not_added_to_fr_share() {
        // css-grid-1 "find the size of an fr": `minmax(300px,1fr) 1fr` in
        // 400px is 300 / 100 — a track whose floor beats its fr share is
        // taken out and the rest re-split. Both layout paths.
        for areas in ["grid-template-areas: 'a b';", ""] {
            let styled = styled_root(
                r#"<div id="g"><div class="a">a</div><div class="b">b</div></div>"#,
                &format!(
                    "#g {{ display: grid; width: 400px; grid-template-columns: minmax(300px,1fr) 1fr; {areas} }}
                     .a {{ grid-area: a; }} .b {{ grid-area: b; }}"
                ),
            );
            let layout = layout_tree(&styled, 800.0);
            let (a, b) = (&layout.children[0], &layout.children[1]);
            assert_eq!(a.dimensions.content.width, 300.0, "{areas}");
            assert_eq!(b.dimensions.content.x, 300.0, "{areas}");
            assert_eq!(b.dimensions.content.width, 100.0, "{areas}");
        }
    }

    #[test]
    fn grid_minmax_tracks_reach_taffy() {
        // A grid without areas is laid out natively by taffy; `minmax()`
        // used to drop the whole `grid-template-columns` declaration.
        let styled = styled_root(
            r#"<div id="g"><div class="a">a</div><div class="b">b</div></div>"#,
            r#"
                #g { display: grid; width: 600px; column-gap: 10px;
                     grid-template-columns: 100px minmax(0,1fr); }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let b = &layout.children[1];
        assert_eq!(b.dimensions.content.x, 110.0);
        assert_eq!(b.dimensions.content.width, 490.0);
    }

    #[test]
    fn grid_template_areas_wider_than_column_tracks_does_not_panic() {
        // Wikipedia's Vector skin: `grid-template-areas` names two columns
        // while `grid-template-columns` uses a value we can't parse, so only
        // the areas say how wide the grid is. The area in column 2 used to
        // index past the one-column track table and panic the layout.
        let styled = styled_root(
            r#"<div id="g"><div class="side"></div><div class="main"></div></div>"#,
            r#"
                #g {
                    display: grid;
                    grid-template-areas: "side main";
                    width: 200px;
                }
                .side { grid-area: side; height: 10px; }
                .main { grid-area: main; height: 10px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let side = &layout.children[0];
        let main = &layout.children[1];
        assert!(
            main.dimensions.content.x > side.dimensions.content.x,
            "main sits right of side"
        );
    }

    #[test]
    fn grid_item_on_an_absurd_line_number_lays_out_bounded() {
        // `grid-row: 2000000000` used to size the occupancy map and the row
        // table by the line number (gigabytes). Lines clamp at 1000 now.
        let styled = styled_root(
            r#"<div id="g"><div class="far"></div><div class="wide"></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: 50px 50px; }
                .far { grid-row: 2000000000; height: 1px; }
                .wide { grid-row: span 1000000000; height: 1px; }
            "#,
        );
        let started = std::time::Instant::now();
        let layout = layout_tree(&styled, 800.0);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        for item in &layout.children {
            assert!(item.dimensions.content.y.is_finite());
        }
    }

    #[test]
    fn grid_explicit_item_width_keeps_declared_size() {
        // When the item has explicit width, the post-hoc track-fill stays out
        // of its way — the item keeps its 50px width inside the 100px track.
        let styled = styled_root(
            r#"<div id="g"><div class="a"></div></div>"#,
            r#"
                #g { display: grid; grid-template-columns: 100px; width: 100px; }
                .a { width: 50px; height: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        assert_eq!(a.dimensions.content.width, 50.0);
    }

    #[test]
    fn flex_items_skip_margin_collapse() {
        // Two flex siblings with vertical margins should not collapse — flex
        // flow ignores margin collapse entirely. Each item's margin-top
        // contributes a fresh top offset within the container.
        let styled = styled_root(
            r#"<div id="row"><div class="a"></div><div class="b"></div></div>"#,
            r#"
                #row { display: flex; width: 400px; }
                .a { width: 50px; height: 30px; margin-top: 10px; }
                .b { width: 50px; height: 30px; margin-top: 20px; }
            "#,
        );
        let layout = layout_tree(&styled, 800.0);
        let a = &layout.children[0];
        let b = &layout.children[1];

        // Each item sits at its own margin-top below the container's content
        // top. (Block flow would have collapsed these against each other; flex
        // flow keeps them independent on the cross axis.)
        assert_eq!(a.dimensions.content.y, 10.0);
        assert_eq!(b.dimensions.content.y, 20.0);
        // Main-axis stacking still works.
        assert_eq!(a.dimensions.content.x, 0.0);
        assert_eq!(b.dimensions.content.x, 50.0);
    }

    #[test]
    fn collapsed_text_normalises_runs_of_whitespace_to_single_space() {
        // Default white-space:normal — the helper reduces every run of
        // whitespace (spaces, tabs, newlines) to one ASCII space. A
        // multi-space inline like "Hello   world" must paint with a
        // single visual gap, matching what real browsers display.
        let styled = styled_root(r#"<p>x</p>"#, r#"p { font-size: 16px; }"#);
        let collapsed = super::collapsed_text(&styled, "Hello   world\n\t  again");
        assert_eq!(collapsed, "Hello world again");
    }

    #[test]
    fn collapsed_text_keeps_a_leading_or_trailing_whitespace_as_single_space() {
        // The helper doesn't trim — a leading space is preserved (just
        // collapsed) so the gap between adjacent inlines like
        // `<b>foo</b> bar` still shows.
        let styled = styled_root(r#"<p>x</p>"#, r#"p { font-size: 16px; }"#);
        assert_eq!(super::collapsed_text(&styled, "   foo"), " foo");
        assert_eq!(super::collapsed_text(&styled, "foo   "), "foo ");
    }

    #[test]
    fn collapsed_text_preserves_source_when_white_space_is_pre() {
        // `white-space: pre` opts out of the collapse — newlines, tabs,
        // and runs of spaces stay verbatim, so `<pre>` source can carry
        // ASCII art / code samples without the renderer eating gaps.
        let styled = styled_root(r#"<pre>x</pre>"#, r#"pre { white-space: pre; }"#);
        assert_eq!(
            super::collapsed_text(&styled, "Hello\n  world"),
            "Hello\n  world"
        );
    }

    #[test]
    fn nested_percent_widths_saturate_instead_of_overflowing_to_inf() {
        // `width: 1000%` per level is legal CSS; 45 levels is 800 × 10^45 px —
        // past f32. Like Blink's LayoutUnit, layout saturates instead of
        // handing paint / hit testing / the agent's rects an `inf` or `NaN`.
        let mut source = String::new();
        for _ in 0..45 {
            source.push_str(r#"<div class="w">"#);
        }
        source.push('x');
        for _ in 0..45 {
            source.push_str("</div>");
        }
        let styled = styled_root(&source, ".w { width: 1000%; padding-left: 50%; }");

        let layout = layout_tree(&styled, 800.0);
        fn check(layout_box: &super::LayoutBox) {
            let d = &layout_box.dimensions;
            for v in [
                d.content.x,
                d.content.y,
                d.content.width,
                d.content.height,
                d.padding.left,
                d.padding.right,
                d.margin.left,
                d.margin.right,
            ] {
                assert!(
                    v.is_finite() && v.abs() <= super::MAX_LAYOUT_COORDINATE,
                    "{v} in {d:?}"
                );
            }
            layout_box.children.iter().for_each(check);
        }
        check(&layout);
    }

    #[test]
    fn shrink_to_fit_width_is_never_negative_around_negative_margins() {
        // An inline box around a block with negative side margins: the
        // children's outer widths sum to -200px. Shrink-to-fit is a size —
        // floored at 0 — otherwise each wrapper with several such children
        // multiplies the negative width (fuzzer: -1e7 px at depth 10).
        let styled = styled_root(
            r#"<body><span><table style="margin: 0 -100px"></table></span></body>"#,
            "",
        );
        let layout = layout_tree(&styled, 800.0);
        let span = &layout.children[0];
        assert!(
            span.dimensions.content.width >= 0.0,
            "{:?}",
            span.dimensions
        );
        // Same for an inline-block's shrink-to-fit width.
        let styled = styled_root(
            r#"<body><span style="display: inline-block"><table style="margin: 0 -100px"></table></span></body>"#,
            "",
        );
        let layout = layout_tree(&styled, 800.0);
        let inline_block = &layout.children[0];
        assert!(
            inline_block.dimensions.content.width >= 0.0,
            "{:?}",
            inline_block.dimensions
        );

        // Two such children per level, four levels deep: stays bounded.
        let leaf = r#"<table style="margin: 0 -100px"></table>"#;
        let mut html = format!("{leaf}{leaf}");
        for _ in 0..4 {
            html = format!("<span>{html}</span><span>{html}</span>");
        }
        let styled = styled_root(&format!("<body>{html}</body>"), "");
        let layout = layout_tree(&styled, 800.0);
        fn check(b: &super::LayoutBox) {
            assert!(b.dimensions.content.width >= 0.0, "{:?}", b.dimensions);
            b.children.iter().for_each(check);
        }
        check(&layout);
    }

    #[test]
    fn percent_padding_inside_an_inline_box_resolves_against_the_block_container() {
        // An inline box is not a containing block: `50%` inside a `<span>`
        // is half of the block container's width, not half of the span. Using the span's own (content-sized) width made
        // every nested level resolve against an already-grown base — the
        // fuzzer saw 1.8e7 px ten levels down.
        let styled = styled_root(
            r#"<body><span><span class="p">x</span></span></body>"#,
            ".p { padding-left: 50%; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let body_width = layout.dimensions.content.width;
        let inner = &layout.children[0].children[0];
        assert_eq!(
            inner.dimensions.padding.left,
            body_width / 2.0,
            "{:?}",
            inner.dimensions
        );

        // Nested ten deep, nothing grows past the viewport-derived base.
        let mut html = "x".to_string();
        for _ in 0..10 {
            html = format!(r#"<span class="p">{html}</span>"#);
        }
        let styled = styled_root(&format!("<body>{html}</body>"), ".p { padding-left: 50%; }");
        let layout = layout_tree(&styled, 800.0);
        let half = layout.dimensions.content.width / 2.0;
        fn check(b: &super::LayoutBox, half: f32) {
            assert!(b.dimensions.padding.left <= half, "{:?}", b.dimensions);
            b.children.iter().for_each(|c| check(c, half));
        }
        check(&layout, half);
    }

    #[test]
    fn flex_items_with_inline_content_size_to_their_text_not_the_probe_width() {
        // A block flex item holding inline text is a legacy boundary leaf.
        // Its max-content probe laid it out against a 1e6 px stand-in and
        // reported that whole width, so the items' flex bases were all 1e6:
        // shrinking split the row evenly instead of by content, and with
        // `flex-shrink: 0` nested rows stayed 1e6 px wide (fuzzer).
        let styled = styled_root(
            r#"<body><header id="nav"><a class="link">Posts</a><a class="link">Categories</a><a class="link">About</a></header></body>"#,
            "#nav { display: flex; } .link { display: block; font-size: 16px; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let nav = &layout.children[0];
        let widths: Vec<f32> = nav
            .children
            .iter()
            .map(|c| c.dimensions.content.width)
            .collect();
        let text: Vec<f32> = nav
            .children
            .iter()
            .map(|c| c.children[0].dimensions.content.width)
            .collect();
        assert_eq!(widths, text, "items are as wide as their text");
        // Centered / right-aligned lines sit in the middle / at the end of
        // the probe width: the line's span counts, not its right edge.
        for align in ["center", "right"] {
            let styled = styled_root(
                r#"<body><header id="nav"><a class="link">Posts</a><a class="link">Categories</a></header></body>"#,
                &format!(
                    "#nav {{ display: flex; }} .link {{ display: block; font-size: 16px; text-align: {align}; flex-shrink: 0; }}"
                ),
            );
            let layout = layout_tree(&styled, 800.0);
            let widths: Vec<f32> = layout.children[0]
                .children
                .iter()
                .map(|c| c.dimensions.content.width)
                .collect();
            assert_eq!(widths, vec![60.0, 120.0], "text-align: {align}");
        }

        let mut html = "<i></i>".to_string();
        for _ in 0..4 {
            html = format!("<div><i></i>{html}</div>");
        }
        let styled = styled_root(
            &format!("<body>{html}</body>"),
            "div { display: flex; flex-shrink: 0; }",
        );
        let layout = layout_tree(&styled, 800.0);
        fn check(b: &super::LayoutBox) {
            assert!(b.dimensions.content.width <= 800.0, "{:?}", b.dimensions);
            b.children.iter().for_each(check);
        }
        check(&layout);
    }

    #[test]
    fn boundary_leaf_margins_are_subtracted_once() {
        // taffy hands a leaf's measure callback the space for its *border
        // box* — margins already taken off. The legacy layout behind a
        // boundary leaf took them off again: a `<body>` holding only inline
        // content was 768px wide in an 800px viewport instead of 784.
        let styled = styled_root(r#"<body><span>x</span></body>"#, "");
        let layout = layout_tree(&styled, 800.0);
        assert_eq!(layout.dimensions.content.width, 784.0);
        assert_eq!(layout.dimensions.content.x, 8.0);

        // Any boundary leaf (inline content) with side margins, in block flow…
        let styled = styled_root(
            r#"<body><div><blockquote class="q">quoted text</blockquote></div></body>"#,
            ".q { margin: 0 40px; padding: 0 5px; border-left: 2px solid; border-right: 2px solid; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let quote = &layout.children[0].children[0];
        assert_eq!(quote.dimensions.content.x, 8.0 + 40.0 + 2.0 + 5.0);
        assert_eq!(quote.dimensions.content.width, 784.0 - 80.0 - 4.0 - 10.0);

        // …and as a flex item.
        let styled = styled_root(
            r#"<body><div class="row"><div class="item">a</div></div></body>"#,
            ".row { display: flex; } .item { flex-grow: 1; margin: 0 10px; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let item = &layout.children[0].children[0];
        assert_eq!(item.dimensions.content.x, 18.0);
        assert_eq!(item.dimensions.content.width, 784.0 - 20.0);
    }

    #[test]
    fn flex_grow_item_with_inline_content_fills_the_row() {
        // An item with inline content is a legacy boundary leaf, and its
        // taffy style carried only margin / size — not flex-grow. Before 0c
        // F11 its max-content basis was the 1e6 probe width, so shrinking
        // to the row hid that it never grew; now it kept its text width.
        let styled = styled_root(
            r#"<body><div class="row"><div class="a">a</div><div class="b">bb</div></div></body>"#,
            ".row { display: flex; } .a { flex-grow: 1; } .b { flex-grow: 3; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let row = &layout.children[0];
        let (a, b) = (&row.children[0], &row.children[1]);
        let free = 784.0 - 12.0 - 24.0;
        assert_eq!(a.dimensions.content.width, 12.0 + free / 4.0);
        assert_eq!(b.dimensions.content.width, 24.0 + free * 3.0 / 4.0);
        assert_eq!(b.dimensions.content.x, 8.0 + a.dimensions.content.width);
    }

    #[test]
    fn grid_placement_of_an_item_with_inline_content_is_honoured() {
        // Same gap as flex-grow: `grid-column` wasn't forwarded for
        // boundary leaves, so text items were auto-placed in order.
        let styled = styled_root(
            r#"<body><div class="g"><div class="a">a</div><div class="b">b</div></div></body>"#,
            ".g { display: grid; grid-template-columns: 100px 100px 100px; } .a { grid-column: 3; } .b { grid-column: 1; }",
        );
        let layout = layout_tree(&styled, 800.0);
        let grid = &layout.children[0];
        assert_eq!(grid.children[0].dimensions.content.x, 208.0);
        assert_eq!(grid.children[1].dimensions.content.x, 8.0);
    }

    #[test]
    fn nested_inline_boxes_are_sized_once_per_layout_not_once_per_ancestor() {
        // Every inline box re-summed its whole subtree to find its own
        // size, and each descendant box did it again: O(n × inline depth) —
        // 7.8 s in release for 10 000 nested `<span>`s (flattened to depth
        // 256). Sizes are now memoised for one layout pass.
        let depth = 200;
        let html = format!(
            "<body>{}x{}</body>",
            "<span>a ".repeat(depth),
            "</span>".repeat(depth)
        );
        let styled = styled_root(&html, "");
        let nodes = 2 * depth + 1; // a span and a text node per level, + "x"
        super::inline::INLINE_SIZE_COMPUTATIONS.with(|c| c.set(0));
        let layout = layout_tree(&styled, 800.0);
        let computed = super::inline::INLINE_SIZE_COMPUTATIONS.with(|c| c.get());
        assert!(
            computed <= 4 * nodes,
            "{computed} inline size computations for {nodes} nodes"
        );
        assert!(layout.dimensions.content.width > 0.0);
    }

    // ---- 1p: percentage heights (CSS 2.1 §10.5, compat round 3 step 1p) ----

    use super::{LayoutBox, layout_tree_with_viewport};

    /// An absolutely positioned child pushes its parent onto the legacy
    /// path (taffy_bridge boundary rule); every case runs with and without.
    const LEGACY: &str = r#"<i style="position:absolute"></i>"#;

    fn page(body: &str, css: &str, viewport: (f32, f32)) -> LayoutBox {
        let document =
            html::parse(&format!("<!DOCTYPE html><html><body>{body}</body></html>")).unwrap();
        let root = *document.roots().last().unwrap();
        let sheet = css::parse(&format!("body {{ margin: 0; }} {css}")).unwrap();
        let styled = style::style_tree(&document, root, &[sheet]);
        layout_tree_with_viewport(&styled, viewport.0, Some(viewport.1))
    }

    fn by_class<'a>(layout: &'a LayoutBox, class: &str) -> &'a LayoutBox {
        fn find<'a>(b: &'a LayoutBox, class: &str) -> Option<&'a LayoutBox> {
            let hit = box_styled_node(b).is_some_and(|n| {
                matches!(&n.node_type, crate::dom::NodeType::Element(e)
                    if e.attributes.get("class").is_some_and(|c| c.split_whitespace().any(|w| w == class)))
            });
            if hit {
                return Some(b);
            }
            b.children.iter().find_map(|c| find(c, class))
        }
        find(layout, class).unwrap_or_else(|| panic!("no box with class {class}"))
    }

    /// The widest border box in the tree.
    fn widest_box(layout: &LayoutBox) -> f32 {
        let d = &layout.dimensions;
        let own =
            d.content.width + d.padding.left + d.padding.right + d.border.left + d.border.right;
        layout.children.iter().map(widest_box).fold(own, f32::max)
    }

    /// A `%` padding or margin is cyclic in an intrinsic-size probe (CSS
    /// Sizing 3 §5.2.1): it contributes 0 there and resolves against the
    /// real track / line afterwards. It used to resolve against the 1e6
    /// max-content proxy, so a `max-content` grid column (or a flex item's
    /// max-content basis) became ~820,000px wide — and each nested inline
    /// box with `%` padding compounded it (layout fuzzer, past 1e7).
    #[test]
    fn percent_padding_does_not_inflate_intrinsic_track_sizes() {
        let cases = [
            // max-content grid column around a `%`-padded item.
            (
                r#"<div style="display:grid; grid-template-columns:max-content"><div class="i" style="padding-right:82%"><img></div></div>"#,
                "",
            ),
            // Nested inline boxes with `%` padding inside the item.
            (
                r#"<div style="display:grid; grid-template-columns:max-content"><div class="i"><span><span><span><img></span></span></span></div></div>"#,
                "div, span { padding-right: 82%; }",
            ),
            // `%` margins too.
            (
                r#"<div style="display:grid; grid-template-columns:max-content"><div class="i" style="margin-left:50%"><img></div></div>"#,
                "",
            ),
            // A flex item's max-content basis.
            (
                r#"<div style="display:flex"><div class="i" style="padding-right:82%; flex-shrink:0"><img></div></div>"#,
                "",
            ),
        ];
        for (html, css) in cases {
            let layout = page(html, css, (800.0, 600.0));
            let widest = widest_box(&layout);
            assert!(widest < 5_000.0, "widest box {widest}px for {html} {css}");
        }
        // The column is the image's 200px; the padding then resolves
        // against it (82% of 200 = 164) and overflows, as in Chrome.
        let layout = page(
            r#"<div style="display:grid; grid-template-columns:max-content"><div class="i" style="padding-right:82%"><img></div></div>"#,
            "",
            (800.0, 600.0),
        );
        let item = by_class(&layout, "i");
        assert_eq!(item.dimensions.padding.right, 164.0);
    }

    fn size_of(layout: &LayoutBox, class: &str) -> (f32, f32) {
        let d = &by_class(layout, class).dimensions.content;
        (d.width, d.height)
    }

    /// Inline `<svg>` is a replaced element (follow-up F1): its size comes
    /// from `width`/`height` attributes, a missing one from the `viewBox`
    /// ratio, else 300×150 — and CSS still wins. Every layout path: inline
    /// flow, block, flex (taffy), legacy boundary.
    #[test]
    fn inline_svg_takes_its_attribute_size() {
        let svg = |attrs: &str| {
            format!(r#"<svg class="s" {attrs}><path d="M0 0h1"/><title>Icon</title></svg>"#)
        };
        for (attrs, size) in [
            (r#"width="50" height="30""#, (50.0, 30.0)),
            (r#"width="16px" height="16px""#, (16.0, 16.0)),
            (r#"width="40" viewBox="0 0 20 10""#, (40.0, 20.0)),
            (r#"height="10" viewBox="0 0 20 10""#, (20.0, 10.0)),
            (r#"width="40""#, (40.0, 150.0)),
            (r#"viewBox="0 0 24 24""#, (300.0, 150.0)),
            ("", (300.0, 150.0)),
            (r#"width="inf" height="NaN""#, (300.0, 150.0)),
            (r#"width="-5" height="1e30""#, (300.0, 150.0)),
        ] {
            for (wrap_open, wrap_close, css) in [
                ("<p>", "</p>", ""),
                ("<p>text ", " text</p>", ""),
                ("<div>", "</div>", ".s { display: block; }"),
                (r#"<div style="display:flex">"#, "</div>", ""),
                (
                    &format!("<div>{LEGACY}"),
                    "</div>",
                    ".s { display: block; }",
                ),
            ] {
                let layout = page(
                    &format!("{wrap_open}{}{wrap_close}", svg(attrs)),
                    css,
                    (800.0, 600.0),
                );
                assert_eq!(size_of(&layout, "s"), size, "{attrs} in {wrap_open}{css}");
            }
        }
        // The attributes are presentation hints (Chrome): CSS units work —
        // `1em` icons (react-icons) follow the font size.
        let layout = page(
            &format!(
                r#"<p style="font-size:20px">{}</p>"#,
                svg(r#"width="1em" height="1em""#)
            ),
            "",
            (800.0, 600.0),
        );
        assert_eq!(size_of(&layout, "s"), (20.0, 20.0));
        let layout = page(
            &format!(
                r#"<div style="width:200px">{}</div>"#,
                svg(r#"width="50%" height="10""#)
            ),
            ".s { display: block; }",
            (800.0, 600.0),
        );
        assert_eq!(size_of(&layout, "s"), (100.0, 10.0));
        // `width: auto` overrides the hint, but the intrinsic size still
        // comes from the attributes (Blink's `SVGSVGElement::IntrinsicWidth`;
        // apple.com's global-nav icons).
        let layout = page(
            &format!(
                "<p>{}</p>",
                svg(r#"width="30" height="44" viewBox="0 0 30 44""#)
            ),
            ".s { width: auto; height: auto; }",
            (800.0, 600.0),
        );
        assert_eq!(size_of(&layout, "s"), (30.0, 44.0));
        // A CSS size on one side takes the other from the viewBox ratio.
        let layout = page(
            &format!("<p>{}</p>", svg(r#"viewBox="0 0 100 26""#)),
            ".s { height: 26px; }",
            (800.0, 600.0),
        );
        assert_eq!(size_of(&layout, "s"), (100.0, 26.0));
        // CSS sizes override the attributes.
        let layout = page(
            &format!("<p>{}</p>", svg(r#"width="16" height="16""#)),
            ".s { width: 24px; height: 20px; display: inline-block; }",
            (800.0, 600.0),
        );
        assert_eq!(size_of(&layout, "s"), (24.0, 20.0));
    }

    #[test]
    fn svg_icon_button_keeps_its_size() {
        // bbc "Open menu": a borderless, paddingless button whose only
        // content is an icon.
        let layout = page(
            r#"<p><button class="b" aria-label="Open menu"><svg width="50" height="30" viewBox="0 0 50 30"><path d="M0 0"/></svg></button></p>"#,
            ".b { border: 0; padding: 0; background: none; }",
            (800.0, 600.0),
        );
        let (w, h) = size_of(&layout, "b");
        assert!(w >= 50.0 && h >= 30.0, "button {w}x{h}");
    }

    #[test]
    fn svg_children_get_no_boxes() {
        // `<title>`/`<text>` inside an icon must not lay out as page text.
        let layout = page(
            r#"<p><svg class="s" width="10" height="10"><title>Close</title><text>X</text></svg></p>"#,
            "",
            (800.0, 600.0),
        );
        assert!(by_class(&layout, "s").children.is_empty());
    }

    /// `css::supports::UNRENDERED` value entries are really unrendered:
    /// when one of these starts failing, the feature works — take its
    /// entry off the list (or `@supports` keeps choosing the fallback).
    #[test]
    fn supports_unrendered_list_matches_layout() {
        let unrendered = |property: &str, value: &str| {
            css::supports::UNRENDERED.contains(&(property, Some(value)))
        };
        // `position: sticky` stays where static flow puts it (a sticky box
        // with `top: 10px` at the top of the page would sit at y = 10).
        assert!(unrendered("position", "sticky"));
        let layout = page(
            r#"<div class="s">x</div><div style="height:2000px"></div>"#,
            ".s { position: sticky; top: 10px; }",
            (800.0, 600.0),
        );
        assert_eq!(by_class(&layout, "s").dimensions.content.y, 0.0);
        // `display: contents` still makes a box of its own (it would
        // have none, its padding gone).
        assert!(unrendered("display", "contents"));
        let layout = page(
            r#"<div class="c"><p>x</p></div>"#,
            ".c { display: contents; padding: 10px; }",
            (800.0, 600.0),
        );
        assert_eq!(by_class(&layout, "c").dimensions.padding.top, 10.0);
        // `display: -webkit-box` stacks its children (Chrome lays them in
        // a row).
        assert!(unrendered("display", "-webkit-box"));
        let layout = page(
            r#"<div class="w"><div class="a">a</div><div class="b">b</div></div>"#,
            ".w { display: -webkit-box; } .a, .b { width: 50px; }",
            (800.0, 600.0),
        );
        assert!(
            by_class(&layout, "b").dimensions.content.y
                > by_class(&layout, "a").dimensions.content.y
        );
    }

    fn height_of(layout: &LayoutBox, class: &str) -> f32 {
        by_class(layout, class).dimensions.content.height
    }

    #[test]
    fn percent_height_in_an_auto_height_parent_is_auto() {
        for extra in ["", LEGACY] {
            let layout = page(
                &format!(r#"<div class="p">{extra}<div class="c">x</div></div>"#),
                ".c { height: 50%; }",
                (800.0, 600.0),
            );
            let h = height_of(&layout, "c");
            assert!(
                h < 40.0,
                "{extra:?}: height {h} should be the content height"
            );
        }
    }

    #[test]
    fn percent_height_in_a_definite_parent_resolves_against_its_height() {
        for extra in ["", LEGACY] {
            let layout = page(
                &format!(r#"<div class="p">{extra}<div class="c">x</div></div>"#),
                ".p { height: 200px; } .c { height: 50%; }",
                (800.0, 600.0),
            );
            assert_eq!(height_of(&layout, "c"), 100.0, "{extra:?}");
        }
    }

    #[test]
    fn html_body_percent_height_chain_fills_the_viewport() {
        for extra in ["", LEGACY] {
            let layout = page(
                &format!(r#"<div class="c">{extra}x</div>"#),
                "html, body { height: 100%; } .c { height: 50%; }",
                (800.0, 600.0),
            );
            assert_eq!(layout.dimensions.content.height, 600.0, "{extra:?}: html");
            assert_eq!(
                layout.children[0].dimensions.content.height, 600.0,
                "{extra:?}: body"
            );
            assert_eq!(height_of(&layout, "c"), 300.0, "{extra:?}");
        }
    }

    #[test]
    fn percent_height_chain_breaks_at_an_auto_ancestor() {
        for extra in ["", LEGACY] {
            let layout = page(
                &format!(r#"<div class="mid">{extra}<div class="c">x</div></div>"#),
                "html, body { height: 100%; } .c { height: 50%; }",
                (800.0, 600.0),
            );
            assert!(height_of(&layout, "c") < 40.0, "{extra:?}");
        }
    }

    #[test]
    fn absolute_percent_height_resolves_against_the_containing_block_padding_box() {
        let layout = page(
            r#"<div class="rel"><div class="abs">x</div></div>
               <div class="rel2"><p>one line</p><div class="abs2"></div></div>"#,
            concat!(
                ".rel { position: relative; height: 80px; padding: 10px; } ",
                ".abs { position: absolute; top: 0; height: 50%; } ",
                ".rel2 { position: relative; } ",
                ".abs2 { position: absolute; top: 0; left: 0; width: 100%; height: 100%; }"
            ),
            (800.0, 600.0),
        );
        assert_eq!(
            height_of(&layout, "abs"),
            50.0,
            "50% of the 100px padding box"
        );
        let rel2 = by_class(&layout, "rel2").dimensions;
        let padding_box = rel2.padding.top + rel2.content.height + rel2.padding.bottom;
        assert_eq!(
            height_of(&layout, "abs2"),
            padding_box,
            "auto-height CB, known after layout"
        );
        assert!(padding_box < 60.0, "{padding_box}");
    }

    #[test]
    fn fixed_and_unanchored_absolute_percent_heights_use_the_viewport() {
        // The initial containing block is the viewport, not the root box
        // (CSS 2.1 §10.1): a tall page must not stretch them.
        let layout = page(
            r#"<div class="tall"></div><div class="fixed"></div><div class="abs"></div>"#,
            concat!(
                ".tall { height: 3000px; } ",
                ".fixed { position: fixed; top: 0px; height: 50%; } ",
                ".abs { position: absolute; bottom: 0px; height: 25%; }"
            ),
            (800.0, 600.0),
        );
        assert_eq!(height_of(&layout, "fixed"), 300.0);
        assert_eq!(height_of(&layout, "abs"), 150.0);
        let abs = by_class(&layout, "abs").dimensions.content;
        assert_eq!(abs.y + abs.height, 600.0, "bottom: 0 of the viewport");
    }

    #[test]
    fn percent_height_inside_an_absolute_box_with_a_definite_height() {
        // korea.kr's logo: an abs-pos box with a fixed height holds a link
        // with `height: 100%` whose only child is a visually-hidden span.
        let layout = page(
            r#"<header class="hd"><div class="logo"><a class="lnk" href="/"><span class="sr">Name</span></a></div></header>"#,
            concat!(
                ".hd { position: relative; height: 80px; } ",
                ".logo { position: absolute; width: 117px; height: 47px; } ",
                ".lnk { display: block; width: 100%; height: 100%; } ",
                ".sr { position: absolute; width: 1px; height: 1px; overflow: hidden; }"
            ),
            (1280.0, 800.0),
        );
        assert_eq!(height_of(&layout, "lnk"), 47.0);
    }

    #[test]
    fn absolute_child_of_a_flex_or_inline_container_sizes_as_a_block() {
        // Absolutely positioned boxes are blockified (CSS 2.1 §9.7) and sized
        // shrink-to-fit: their children resolve `width`/`height` % against
        // the box itself, not the container it sits in. korea.kr's logo link
        // (`.container{display:flex}` > abs `.logo` > `a{height:100%}`)
        // came out 1,144 wide and 0 tall, so the agent lost it.
        for container in ["display: flex;", "display: grid;", ""] {
            let layout = page(
                r#"<div class="box">text <div class="logo"><a class="lnk" href="/"><span class="sr">Name</span></a></div></div>"#,
                &format!(
                    ".box {{ {container} padding: 19px 60px; }} \
                     .logo {{ position: absolute; width: 117px; height: 47px; }} \
                     .lnk {{ display: block; width: 100%; height: 100%; }} \
                     .sr {{ position: absolute; width: 1px; height: 1px; overflow: hidden; }}"
                ),
                (1280.0, 800.0),
            );
            let lnk = by_class(&layout, "lnk").dimensions.content;
            assert_eq!((lnk.width, lnk.height), (117.0, 47.0), "{container:?}");
        }
    }

    #[test]
    fn inline_flex_with_percent_height_in_an_auto_parent_keeps_its_content_height() {
        // GitHub's breadcrumb: `a { display: inline-flex; height: 100% }`
        // in an auto-height `li`, holding an inline-block `span`. With the
        // percentage auto, the box must still be as tall as its content —
        // legacy lays inline-flex out as an inline box, whose inline-block
        // children counted 0 tall.
        let layout = page(
            r#"<ol class="ol"><li class="li"><a class="crumb" href="/x"><span class="last">tokio</span></a></li></ol>"#,
            concat!(
                ".ol { margin: 0; padding-left: 0; } .li { list-style: none; } ",
                ".crumb { display: inline-flex; align-items: center; height: 100%; padding: 4px 6px; } ",
                ".last { display: inline-block; line-height: 24px; }"
            ),
            (1280.0, 800.0),
        );
        assert_eq!(height_of(&layout, "crumb"), 24.0);
    }

    #[test]
    fn unitless_zero_offsets_pin_an_absolute_box_to_the_corner() {
        let layout = page(
            r#"<div class="gap"></div><div class="rel"><p>text</p><div class="abs">x</div></div>"#,
            concat!(
                ".gap { height: 50px; } ",
                ".rel { position: relative; margin-left: 30px; padding: 10px; } ",
                ".abs { position: absolute; top: 0; left: 0; width: 20px; }"
            ),
            (800.0, 600.0),
        );
        let rel = by_class(&layout, "rel").dimensions;
        let abs = by_class(&layout, "abs").dimensions;
        let padding_box = (
            rel.content.x - rel.padding.left,
            rel.content.y - rel.padding.top,
        );
        assert_eq!((abs.content.x, abs.content.y), padding_box);
    }

    #[test]
    fn inset_zero_pins_an_absolute_box_to_the_padding_box() {
        let layout = page(
            r#"<div class="gap"></div><div class="rel"><p>text</p><div class="abs">x</div></div>"#,
            concat!(
                ".gap { height: 50px; } ",
                ".rel { position: relative; margin-left: 30px; padding: 10px; } ",
                ".abs { position: absolute; inset: 0; width: 20px; }"
            ),
            (800.0, 600.0),
        );
        let rel = by_class(&layout, "rel").dimensions;
        let abs = by_class(&layout, "abs").dimensions;
        assert_eq!(
            (abs.content.x, abs.content.y),
            (
                rel.content.x - rel.padding.left,
                rel.content.y - rel.padding.top
            )
        );
    }

    #[test]
    fn flex_one_items_split_the_row_equally() {
        // `flex: 1` = `1 1 0%` (step 1b): the basis is 0, so the free space
        // splits evenly whatever the content widths.
        for extra in ["", LEGACY] {
            let layout = page(
                &format!(
                    r#"<div class="row">{extra}<div class="a">short</div><div class="b">a much longer piece of text</div></div>"#
                ),
                ".row { display: flex; width: 600px; } .a, .b { flex: 1; }",
                (800.0, 600.0),
            );
            let a = by_class(&layout, "a").dimensions.content.width;
            let b = by_class(&layout, "b").dimensions.content.width;
            assert!(
                (a - 300.0).abs() < 0.5 && (b - 300.0).abs() < 0.5,
                "{extra:?}: {a} / {b}"
            );
        }
    }

    #[test]
    fn min_and_max_height_percentages_need_a_definite_containing_block() {
        for extra in ["", LEGACY] {
            let layout = page(
                &format!(
                    r#"<div class="p">{extra}<div class="min">x</div><div class="max">x</div></div>
                       <div class="q">{extra}<div class="min2">x</div><div class="max2">x</div></div>"#
                ),
                ".p { height: 200px; }                  .min, .min2 { min-height: 50%; }                  .max, .max2 { height: 150px; max-height: 10%; }",
                (800.0, 600.0),
            );
            assert_eq!(
                height_of(&layout, "min"),
                100.0,
                "{extra:?}: min-height 50% of 200"
            );
            assert_eq!(
                height_of(&layout, "max"),
                20.0,
                "{extra:?}: max-height 10% of 200"
            );
            assert!(
                height_of(&layout, "min2") < 40.0,
                "{extra:?}: min-height % → 0"
            );
            assert_eq!(
                height_of(&layout, "max2"),
                150.0,
                "{extra:?}: max-height % → none"
            );
        }
    }

    #[test]
    fn percent_heights_in_an_auto_height_flex_header_stay_small() {
        // Wikipedia's Vector header: a logo with `height: 100%` and an
        // invisible full-size checkbox over a dropdown label, in auto-height
        // flex rows. Resolving % against the width made the header ~1,300px.
        let layout = page(
            r#"<header class="h"><div class="start">
                 <div class="dd"><input type="checkbox" class="cb">
                   <label class="lbl"><span>i</span><span class="sr">Main menu</span></label></div>
                 <a class="logo" href="/">Logo</a>
               </div></header><p>after</p>"#,
            concat!(
                ".h, .start { display: flex; } ",
                ".logo { display: flex; height: 100%; } ",
                ".dd { position: relative; } ",
                ".cb { position: absolute; top: 0; left: 0; width: 100%; height: 100%; } ",
                ".sr { position: absolute; width: 1px; height: 1px; overflow: hidden; }"
            ),
            (1280.0, 800.0),
        );
        for class in ["h", "logo", "cb"] {
            let h = height_of(&layout, class);
            assert!(h < 60.0, "{class}: height {h}");
        }
    }

    // ---- 5m: min-width / max-width on the legacy path (CSS 2.1 §10.4,
    // compat round 3 step 5m). Every case puts `LEGACY` first so the
    // container is a boundary leaf (the shape of every real site's
    // `<body>`, docs/compat-round3.md 5s). ----

    fn legacy(body: &str, css: &str) -> LayoutBox {
        page(&format!("{LEGACY}{body}"), css, (800.0, 600.0))
    }

    fn content(layout: &LayoutBox, class: &str) -> super::Rect {
        by_class(layout, class).dimensions.content
    }

    #[test]
    fn legacy_max_width_with_auto_margins_centres() {
        let layout = legacy(
            r#"<div class="wrap"><p>x</p></div><div class="text">only text</div>"#,
            ".wrap, .text { max-width: 300px; margin: 0 auto; }",
        );
        for class in ["wrap", "text"] {
            let c = content(&layout, class);
            assert_eq!((c.width, c.x), (300.0, 250.0), "{class}");
        }
    }

    #[test]
    fn legacy_max_width_percent_and_min_wins() {
        let layout = legacy(
            r#"<div class="half">x</div><div class="both">x</div><div class="raise">x</div>"#,
            ".half { max-width: 50%; }
             .both { max-width: 100px; min-width: 200px; }
             .raise { width: 100px; min-width: 150px; }",
        );
        assert_eq!(content(&layout, "half").width, 400.0);
        assert_eq!(content(&layout, "both").width, 200.0);
        assert_eq!(content(&layout, "raise").width, 150.0);
    }

    #[test]
    fn legacy_sizes_respect_border_box() {
        // box-sizing: border-box — `width`, `min-width` and `max-width` all
        // name the border box; the content box is what's left after
        // padding and borders.
        let layout = legacy(
            r#"<div class="w">x</div><div class="max">x</div><div class="min">x</div>"#,
            "div { box-sizing: border-box; padding: 0 20px; border: 5px solid; }
             .w { width: 300px; }
             .max { max-width: 300px; }
             .min { width: 10px; min-width: 100px; }",
        );
        assert_eq!(content(&layout, "w").width, 250.0);
        assert_eq!(content(&layout, "max").width, 250.0);
        assert_eq!(content(&layout, "min").width, 50.0);
    }

    #[test]
    fn legacy_shrink_to_fit_boxes_are_clamped() {
        // Floats, inline-blocks and absolutes size shrink-to-fit, then
        // clamp: a long text stops at max-width, a short one grows to
        // min-width.
        let long = "word ".repeat(40);
        let layout = legacy(
            &format!(
                r#"<div class="fl">{long}</div><div class="fs">x</div>
                   <span class="ib">{long}</span><span class="is">x</span>
                   <div class="ab">{long}</div>"#
            ),
            ".fl, .fs { float: left; }
             .ib, .is { display: inline-block; }
             .ab { position: absolute; }
             .fl, .ib, .ab { max-width: 120px; }
             .fs, .is { min-width: 150px; }",
        );
        for class in ["fl", "ib", "ab"] {
            assert_eq!(content(&layout, class).width, 120.0, "{class}");
        }
        for class in ["fs", "is"] {
            assert_eq!(content(&layout, class).width, 150.0, "{class}");
        }
    }

    #[test]
    fn legacy_flex_items_respect_min_and_max_width() {
        // A capped item freezes at its max and the others share the rest
        // (CSS Flexbox §9.7); a min-width item doesn't shrink below it.
        let layout = legacy(
            r#"<div class="row"><div class="a">x</div><div class="b">x</div><div class="c">x</div></div>
               <div class="tight"><div class="m">x</div><div class="n">x</div></div>"#,
            ".row, .tight { display: flex; width: 600px; }
             .row > div { flex: 1; }
             .a { max-width: 100px; }
             .tight > div { width: 400px; }
             .m { min-width: 350px; }",
        );
        assert_eq!(content(&layout, "a").width, 100.0);
        assert_eq!(content(&layout, "b").width, 250.0);
        assert_eq!(content(&layout, "c").width, 250.0);
        assert_eq!(content(&layout, "m").width, 350.0);
        assert_eq!(content(&layout, "n").width, 250.0);
    }

    #[test]
    fn legacy_grid_item_stretch_stops_at_max_width() {
        let layout = legacy(
            r#"<div class="g"><div class="cap">x</div><div class="free">x</div></div>"#,
            ".g { display: grid; grid-template-columns: 1fr 1fr; width: 600px; }
             .cap { max-width: 120px; }",
        );
        assert_eq!(content(&layout, "cap").width, 120.0);
        assert_eq!(content(&layout, "free").width, 300.0);
    }

    #[test]
    fn legacy_table_width_is_clamped() {
        let layout = legacy(
            r#"<table class="t"><tr><td>x</td></tr></table>"#,
            ".t { width: 100%; max-width: 200px; }",
        );
        assert_eq!(content(&layout, "t").width, 200.0);
    }

    // ---- 5a: calc() in layout (compat round 3 step 5a). Each case runs on
    // the taffy path (no `LEGACY`) and the legacy path; option (b) of the
    // 5s spike sends a `%` calc box (and its parent) to the legacy path,
    // so both must agree. ----

    fn both(body: &str, css: &str, viewport: f32, check: impl Fn(&LayoutBox, &str)) {
        for (label, prefix) in [("taffy", ""), ("legacy", LEGACY)] {
            let layout = page(&format!("{prefix}{body}"), css, (viewport, 600.0));
            check(&layout, label);
        }
    }

    #[test]
    fn width_calc_percent_minus_px() {
        both(
            r#"<div class="box"><div class="c">x</div><div class="d"><p>y</p></div></div>"#,
            ".box { width: 200px } .c, .d { width: calc(100% - 16px) }",
            800.0,
            |layout, label| {
                assert_eq!(content(layout, "c").width, 184.0, "{label}");
                assert_eq!(content(layout, "d").width, 184.0, "{label}");
            },
        );
    }

    #[test]
    fn margin_calc_of_vars() {
        both(
            r#"<div class="box"><div class="c"><p>x</p></div></div>"#,
            ".box { width: 200px; --g: 8px } .c { margin-left: calc(var(--g) * 2 + 10%) }",
            800.0,
            |layout, label| {
                let c = content(layout, "c");
                let x0 = content(layout, "box").x;
                assert_eq!((c.x - x0, c.width), (36.0, 164.0), "{label}");
            },
        );
    }

    #[test]
    fn clamp_respects_bounds() {
        both(
            r#"<div class="s"><div class="c a">x</div></div><div class="m"><div class="c b">x</div></div><div class="l"><div class="c z">x</div></div>"#,
            ".s { width: 100px } .m { width: 400px } .l { width: 1000px }
             .c { width: clamp(150px, 50%, 300px) }",
            1280.0,
            |layout, label| {
                assert_eq!(content(layout, "a").width, 150.0, "{label}");
                assert_eq!(content(layout, "b").width, 200.0, "{label}");
                assert_eq!(content(layout, "z").width, 300.0, "{label}");
            },
        );
    }

    #[test]
    fn flex_basis_percent_uses_main_axis() {
        both(
            r#"<div class="row"><div class="r">x</div><div>y</div></div><div class="col"><div class="k">x</div><div>y</div></div>"#,
            ".row { display: flex; width: 600px } .r { flex: 0 0 calc(50% - 10px) }
             .col { display: flex; flex-direction: column; height: 400px; width: 100px }
             .k { flex: 0 0 calc(50% + 10px) }",
            800.0,
            |layout, label| {
                assert_eq!(content(layout, "r").width, 290.0, "{label}");
                assert_eq!(content(layout, "k").height, 210.0, "{label}");
            },
        );
    }

    #[test]
    fn calc_width_on_flex_container_resolves() {
        // The 5s reference case: on the taffy path the container (it has a
        // `%` calc) goes to the legacy path, which resolves it.
        for (viewport, width, x) in [(1280.0, 1200.0, 40.0), (768.0, 736.0, 16.0)] {
            for grid in [false, true] {
                let display = if grid {
                    "display: grid; grid-template-columns: 1fr 1fr"
                } else {
                    "display: flex"
                };
                both(
                    r#"<div class="f"><div class="i">a</div><div class="j">b</div></div>"#,
                    &format!(
                        ".f {{ {display}; width: min(100% - 32px, 1200px); margin: 0 auto }}
                         .f > div {{ flex: 1 }}"
                    ),
                    viewport,
                    |layout, label| {
                        let f = content(layout, "f");
                        assert_eq!((f.width, f.x), (width, x), "{label} grid={grid} {viewport}");
                        assert_eq!(
                            content(layout, "i").width,
                            width / 2.0,
                            "{label} grid={grid}"
                        );
                    },
                );
            }
        }
    }

    #[test]
    fn explicit_width_check_accepts_calc() {
        // A grid item with a calc width is not stretched to its area, and a
        // flex column item with one is not stretched across: "has a width"
        // must not be `matches!(Value::Length)` (which reads a calc as auto).
        both(
            r#"<div class="g"><div class="gi">x</div><div>y</div></div><div class="col"><div class="ci">x</div></div>"#,
            ".g { display: grid; grid-template-columns: 1fr 1fr; width: 600px }
             .gi { width: calc(50% - 10px) }
             .col { display: flex; flex-direction: column; width: 400px }
             .ci { width: calc(25% + 4px) }",
            800.0,
            |layout, label| {
                assert_eq!(content(layout, "gi").width, 140.0, "{label}");
                assert_eq!(content(layout, "ci").width, 104.0, "{label}");
            },
        );
    }

    #[test]
    fn viewport_units_lay_out() {
        both(
            r#"<div class="v">x</div>"#,
            // `page` styles at the default 1280×800 viewport.
            ".v { width: 50vw; height: calc(50vh - 10px) }",
            1280.0,
            |layout, label| {
                let v = content(layout, "v");
                assert_eq!((v.width, v.height), (640.0, 390.0), "{label}");
            },
        );
    }

    // ---- 5b: grid track functions (compat round 3 step 5b) ----

    fn grid_items(n: usize) -> String {
        let items: String = (0..n)
            .map(|i| format!(r#"<div class="i{i}">x</div>"#))
            .collect();
        format!(r#"<div class="g">{items}</div>"#)
    }

    #[test]
    fn grid_repeat_n() {
        both(
            &grid_items(3),
            ".g { display: grid; width: 600px; grid-template-columns: repeat(3, 1fr) }",
            800.0,
            |l, label| {
                for i in 0..3 {
                    let c = content(l, &format!("i{i}"));
                    assert_eq!(
                        (c.width, c.x - content(l, "g").x),
                        (200.0, 200.0 * i as f32),
                        "{label} {i}"
                    );
                }
            },
        );
    }

    #[test]
    fn grid_repeat_auto_fill_count_includes_gap() {
        // 500px with a 20px gap: four 100px columns fit (4·100 + 3·20 =
        // 460), five don't (560) — the fifth item wraps.
        both(
            &grid_items(5),
            ".g { display: grid; width: 500px; column-gap: 20px; grid-template-columns: repeat(auto-fill, 100px) }",
            800.0,
            |l, label| {
                let x0 = content(l, "g").x;
                assert_eq!(content(l, "i3").x - x0, 360.0, "{label}");
                assert_eq!(content(l, "i4").x - x0, 0.0, "{label}");
                assert!(content(l, "i4").y > content(l, "i0").y, "{label}");
            },
        );
    }

    #[test]
    fn grid_auto_fit_collapses_empty() {
        // Five 100px-minimum tracks fit in 500px; with two items `auto-fill`
        // keeps all five (100 each), `auto-fit` collapses the three empty
        // ones so the two items share the row.
        for (kind, width) in [("auto-fill", 100.0), ("auto-fit", 250.0)] {
            both(
                &grid_items(2),
                &format!(
                    ".g {{ display: grid; width: 500px; grid-template-columns: repeat({kind}, minmax(100px, 1fr)) }}"
                ),
                800.0,
                |l, label| {
                    assert_eq!(content(l, "i0").width, width, "{label} {kind}");
                    assert_eq!(
                        content(l, "i1").x - content(l, "g").x,
                        width,
                        "{label} {kind}"
                    );
                },
            );
        }
    }

    #[test]
    fn grid_fit_content_caps_track() {
        let long = "word ".repeat(40);
        both(
            &format!(
                r#"<div class="g"><div class="a">{long}</div><div class="b">x</div></div><div class="h"><div class="c">x</div><div>y</div></div>"#
            ),
            ".g, .h { display: grid; width: 600px; grid-template-columns: fit-content(150px) 1fr }",
            800.0,
            |l, label| {
                assert_eq!(content(l, "a").width, 150.0, "{label}");
                assert_eq!(content(l, "b").width, 450.0, "{label}");
                assert!(
                    content(l, "c").width < 50.0,
                    "{label}: {}",
                    content(l, "c").width
                );
            },
        );
    }

    #[test]
    fn grid_area_name_resolves_against_line_names() {
        // mdn's layout: columns named by lines, items placed with
        // `grid-area: <name>` (its `<name>-start` / `<name>-end` lines),
        // the column list coming through a custom property.
        both(
            r#"<div class="g"><div class="c">x</div><div class="s">y</div></div>"#,
            ".g { display: grid; width: 600px;
                  --cols: [full-start side-start] 100px [side-end] 20px [content-start] 1fr [content-end full-end];
                  grid-template-columns: var(--cols) }
             .c { grid-area: content } .s { grid-area: side }",
            800.0,
            |l, label| {
                let x0 = content(l, "g").x;
                let (c, s) = (content(l, "c"), content(l, "s"));
                assert_eq!((c.x - x0, c.width), (120.0, 480.0), "{label}");
                assert_eq!((s.x - x0, s.width), (0.0, 100.0), "{label}");
                assert_eq!(c.y, s.y, "{label}: same row");
            },
        );
    }

    #[test]
    fn grid_calc_track() {
        both(
            &grid_items(2),
            ".g { display: grid; width: 600px; column-gap: 32px; grid-template-columns: repeat(2, calc(50% - 16px)) }",
            800.0,
            |l, label| {
                assert_eq!(content(l, "i0").width, 284.0, "{label}");
                assert_eq!(content(l, "i1").x - content(l, "g").x, 316.0, "{label}");
            },
        );
    }

    // ---- 5f: two bugs found in step 5a (compat round 3 step 5f) ----

    #[test]
    fn percent_width_in_a_max_content_probe_does_not_reach_the_proxy() {
        // Layout fuzzer seed 26412, shrunk by hand: a flex item that must
        // not shrink is sized by a max-content probe, which lays the legacy
        // leaf out against a 1e6 proxy width — and `width: 100%` of the
        // proxy came back as the item's content size. The page was
        // 1,000,000px wide (6,000,200 nested). A percentage width is cyclic
        // there and behaves as `auto` (CSS Sizing 3 §5.2.1, like `%`
        // padding — F1b).
        let layout = page(
            r#"<div class="f"><img class="m" width="100%"></div><div class="f"><div class="t" style="width: 50%">x</div></div><div class="f"><div class="u" style="width: calc(50% + 1px)">x</div></div>"#,
            "body, .f { display: flex; flex-shrink: 0 }",
            (800.0, 600.0),
        );
        assert!(
            widest_box(&layout) <= 800.0,
            "widest {}",
            widest_box(&layout)
        );
        for class in ["m", "t", "u"] {
            assert!(
                content(&layout, class).width < 800.0,
                "{class}: {}",
                content(&layout, class).width
            );
        }
    }

    #[test]
    fn boundary_container_max_content_is_not_the_proxy_width() {
        // The rest of seed 26412: a block-level boundary leaf (here a flex
        // container made a boundary by a child's `top: 0%`, and nested
        // tables) laid out against the 1e6 max-content proxy fills it — its
        // width *was* the proxy, reported as its max-content size. Inline
        // leaves were re-measured by their lines (F11); now any proxy-filled
        // leaf reports its shrink-to-fit content width.
        let layout = page(
            r#"<div class="f"><div class="b"><p style="top: 0%">x</p></div></div>
               <div class="f"><table><tr><td><table><tr><td>w</td></tr></table></td></tr></table></div>"#,
            "body, .f, .b { display: flex; flex-shrink: 0 }",
            (800.0, 600.0),
        );
        assert!(
            widest_box(&layout) <= 800.0,
            "widest {}",
            widest_box(&layout)
        );
        // The seed itself, shrunk: every element a non-shrinking flex box.
        let layout = page(
            r#"<table><td><form><table><span><img width="100%"></span><td><table><table><img>w</table></table>,</td> 안녕<li><ul style="clear: right"></ul></li><ul style="top: 0%"></ul></table></form></td></table>"#,
            "* { display: flex !important; flex-shrink: 0 !important }",
            (800.0, 600.0),
        );
        assert!(
            widest_box(&layout) <= 800.0,
            "widest {}",
            widest_box(&layout)
        );
    }

    #[test]
    fn boundary_leaf_percent_width_resolves_once() {
        // On the taffy path a boundary leaf (inline content) with a `%`
        // width was sized twice: taffy resolved 26% of the container, then
        // handed that width to the legacy layout as the *parent* width,
        // which took 26% again (400 → 104 → 27).
        let layout = page(
            r#"<div class="col"><div class="a">x</div></div><div class="blk"><div class="b">x</div><div class="c">x</div></div>"#,
            ".col { display: flex; flex-direction: column; width: 400px }
             .a { width: 26% }
             .blk { width: 400px }
             .b { width: 50% }
             .c { width: 50%; box-sizing: border-box; padding: 0 10px; margin-left: 10% }",
            (800.0, 600.0),
        );
        assert_eq!(content(&layout, "a").width, 104.0);
        assert_eq!(content(&layout, "b").width, 200.0);
        let c = content(&layout, "c");
        assert_eq!(c.width, 180.0);
        assert_eq!(
            c.x - content(&layout, "blk").x,
            50.0,
            "10% margin of 400 + 10px padding"
        );
    }
}
