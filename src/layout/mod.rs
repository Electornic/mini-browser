// Layout uses a single rectangular box model for both block and simple inline flow.
// Every node becomes a box with a content rect plus margin/padding/border around it.
//
// The algorithm split lives in sibling submodules (`block`, `inline`, `flex`,
// `grid`) and each owns its own paint-recursive entry. mod.rs keeps the box
// types, `layout_tree` (the public entry) and the cross-cutting helpers every
// algorithm needs (containing-block math, edge sizes, intrinsic sizes, …).

use std::cell::Cell;

use crate::{
    css::{Unit, Value},
    dom::{ElementData, NodeType},
    style::StyledNode,
};

mod block;
mod flex;
mod grid;
mod inline;
mod table;
mod taffy_bridge;

// The fontdue font slice currently in use for the running layout pass.
// `layout_tree_with_fonts` writes this for the duration of one call so
// inline-measurement helpers (deep inside the layout call tree) can
// reach the real font metrics without every layout function growing a
// `&[fontdue::Font]` parameter. The slot is null between layout passes
// (the bare `layout_tree` wrapper restores it before returning), so any
// caller — including tests — that bypasses `_with_fonts` observes an
// empty slice and falls back to the legacy `font_size * 0.75` estimate.
thread_local! {
    static LAYOUT_FONTS_PTR: Cell<*const [fontdue::Font]> = const {
        Cell::new(std::ptr::null::<[fontdue::Font; 0]>() as *const [fontdue::Font])
    };
}

// Scope guard that restores the previous fonts pointer when dropped, so
// a panic inside a layout call doesn't leak a dangling pointer to the
// next layout pass (or the next test in the same thread).
struct LayoutFontsGuard {
    previous: *const [fontdue::Font],
}

impl Drop for LayoutFontsGuard {
    fn drop(&mut self) {
        LAYOUT_FONTS_PTR.with(|cell| cell.set(self.previous));
    }
}

/// Read the layout pass's currently-installed fontdue font slice. Returns
/// an empty slice when no `_with_fonts` call is active. Sibling layout
/// modules (`inline`, etc.) call this to get real font metrics for text
/// width measurement.
pub(super) fn current_fonts() -> &'static [fontdue::Font] {
    LAYOUT_FONTS_PTR.with(|cell| {
        let ptr = cell.get();
        if ptr.is_null() {
            &[]
        } else {
            // SAFETY: the pointer is only ever set by
            // `layout_tree_with_fonts` to a borrow that outlives the
            // entire layout pass, and the guard clears it on return /
            // panic. Layout helpers only iterate the slice for the
            // duration of a single call — never store the reference —
            // so the lifetime extension to 'static is sound in
            // practice even though it is a compile-time fiction.
            unsafe { &*ptr }
        }
    })
}

use block::layout_node;
use flex::is_flex_container;
use grid::is_grid_container;
use table::is_table_container;

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

pub fn layout_tree(root: &StyledNode, viewport_width: f32) -> LayoutBox {
    layout_tree_with_fonts(root, viewport_width, &[])
}

pub fn layout_tree_with_fonts(
    root: &StyledNode,
    viewport_width: f32,
    fonts: &[fontdue::Font],
) -> LayoutBox {
    let previous =
        LAYOUT_FONTS_PTR.with(|cell| cell.replace(fonts as *const [fontdue::Font]));
    let _guard = LayoutFontsGuard { previous };
    // Element roots route through taffy (the bridge's measure-callback
    // boundary handles every shape taffy itself doesn't understand). Only
    // a non-element root (e.g. a stand-alone text node — never produced by
    // current callers) falls back to the legacy block path.
    let mut layout_box = match taffy_bridge::layout_via_taffy(root, viewport_width) {
        Some(layout) => layout,
        None => {
            let mut cursor_y = 0.0;
            layout_node(root, 0.0, &mut cursor_y, viewport_width)
        }
    };
    // Pass 2: walk the tree and move every `position: absolute` subtree to
    // its final spot relative to its containing block. The initial
    // containing block is the viewport; we only know its width, so we use
    // the laid-out root's own outer height as the height base for the
    // initial CB — close enough for `bottom`/`%` resolution at the root.
    let initial_cb_height = outer_rect(&layout_box).height;
    let initial_cb = ContainingBlock {
        x: 0.0,
        y: 0.0,
        width: viewport_width,
        height: initial_cb_height,
    };
    reposition_absolutes(&mut layout_box, initial_cb, initial_cb);
    layout_box
}


#[derive(Debug, Clone, Copy)]
struct ContainingBlock {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

fn reposition_absolutes(
    layout_box: &mut LayoutBox,
    cb: ContainingBlock,
    initial_cb: ContainingBlock,
) {
    // If THIS box is positioned, descendants resolve their containing block
    // against this box's padding box. The CB inherited from above is what
    // applies to THIS box itself when it is `position: absolute`. Fixed
    // boxes ignore the inherited CB entirely and always use the viewport.
    let child_cb = if box_is_positioned(layout_box) {
        padding_box_as_cb(layout_box)
    } else {
        cb
    };

    for child in &mut layout_box.children {
        reposition_absolutes(child, child_cb, initial_cb);
    }

    let resolution_cb = if box_is_fixed(layout_box) {
        Some(initial_cb)
    } else if box_is_absolute(layout_box) {
        Some(cb)
    } else {
        None
    };
    if let Some(target_cb) = resolution_cb {
        let (delta_x, delta_y) = absolute_offset_delta(layout_box, target_cb);
        if delta_x != 0.0 || delta_y != 0.0 {
            shift_layout_subtree(layout_box, delta_x, delta_y);
        }
    }
}


fn padding_box_as_cb(layout_box: &LayoutBox) -> ContainingBlock {
    let d = &layout_box.dimensions;
    ContainingBlock {
        x: d.content.x - d.padding.left,
        y: d.content.y - d.padding.top,
        width: d.padding.left + d.content.width + d.padding.right,
        height: d.padding.top + d.content.height + d.padding.bottom,
    }
}

fn absolute_offset_delta(layout_box: &LayoutBox, cb: ContainingBlock) -> (f32, f32) {
    // Resolve `top`/`right`/`bottom`/`left` against the containing block.
    // When the start side is set we pin to it; otherwise the end side pins
    // the OUTER edge to (cb_end - end_value). Falling through to neither
    // means stay put at the static position computed in pass 1.
    let node = match box_styled_node(layout_box) {
        Some(node) => node,
        None => return (0.0, 0.0),
    };
    let outer = outer_rect(layout_box);
    let left = length_value(node, "left", cb.width);
    let right = length_value(node, "right", cb.width);
    let top = length_value(node, "top", cb.height);
    let bottom = length_value(node, "bottom", cb.height);

    let target_outer_x = if let Some(value) = left {
        cb.x + value
    } else if let Some(value) = right {
        cb.x + cb.width - value - outer.width
    } else {
        outer.x
    };
    let target_outer_y = if let Some(value) = top {
        cb.y + value
    } else if let Some(value) = bottom {
        cb.y + cb.height - value - outer.height
    } else {
        outer.y
    };

    (target_outer_x - outer.x, target_outer_y - outer.y)
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

pub(super) fn edge_sizes(node: &StyledNode, prefix: &str, base: f32) -> EdgeSizes {
    // CSS resolves percent margin/padding against the containing block's *width*, even
    // for the top and bottom sides — a common gotcha worth keeping in mind here.
    EdgeSizes {
        left: length_value(node, &format!("{prefix}-left"), base).unwrap_or(0.0),
        right: length_value(node, &format!("{prefix}-right"), base).unwrap_or(0.0),
        top: length_value(node, &format!("{prefix}-top"), base).unwrap_or(0.0),
        bottom: length_value(node, &format!("{prefix}-bottom"), base).unwrap_or(0.0),
    }
}

pub(super) fn length_value(node: &StyledNode, name: &str, base: f32) -> Option<f32> {
    // `base` is the containing-block dimension a Percent length resolves against. For
    // properties that should never see a percent (font-size after style resolution, etc.)
    // callers can safely pass any value.
    match node.value(name) {
        Some(Value::Length(value, Unit::Px)) => Some(*value),
        Some(Value::Length(value, Unit::Percent)) => Some(*value / 100.0 * base),
        _ => None,
    }
}

pub(super) fn is_auto(node: &StyledNode, name: &str) -> bool {
    matches!(node.value(name), Some(Value::Keyword(keyword)) if keyword == "auto")
}

pub(super) fn is_position_relative(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "relative")
}

pub(super) fn is_position_absolute(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "absolute")
}

pub(super) fn is_position_fixed(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "fixed")
}

pub(super) fn is_out_of_flow(node: &StyledNode) -> bool {
    // Both `absolute` and `fixed` skip in-flow placement during pass 1; they
    // differ only in which containing block pass 2 resolves them against.
    is_position_absolute(node) || is_position_fixed(node)
}

pub(super) fn is_display_none(node: &StyledNode) -> bool {
    // `display: none` removes the element (and its subtree) from the box tree
    // entirely — no layout, no paint, no hit test. Every algorithm's child
    // iteration filters on this so a hidden node never contributes to flow,
    // line packing, flex tracks, grid placement, or inline-flow detection.
    matches!(node.value("display"), Some(Value::Keyword(keyword)) if keyword == "none")
}

/// Whether `node` is a text node consisting purely of HTML whitespace.
/// The HTML parser preserves inter-element whitespace as `" "` text nodes
/// so inline runs keep their separating spaces; in non-inline layout modes
/// (block flow, flex item placement, grid placement, table cell stacking)
/// that whitespace would otherwise become a visible empty box / phantom
/// item. Inline layout intentionally does NOT filter on this — there the
/// whitespace text contributes the space the author wrote between
/// adjacent inline elements.
pub(super) fn is_layout_whitespace_text(node: &StyledNode) -> bool {
    matches!(
        &node.node_type,
        NodeType::Text(text) if text.chars().all(char::is_whitespace)
    )
}

pub(super) fn relative_offset(node: &StyledNode, base: f32) -> Option<(f32, f32)> {
    // CSS spec: top/bottom percent resolves against the containing block's height
    // and left/right against its width. The layout walk only carries width on hand,
    // so percent offsets reuse `base` for both axes — same shortcut already taken
    // for percent margin/padding.
    if !is_position_relative(node) {
        return None;
    }
    let left = length_value(node, "left", base);
    let right = length_value(node, "right", base);
    let top = length_value(node, "top", base);
    let bottom = length_value(node, "bottom", base);
    // When both sides are set, the start side wins (LTR + top-down): `left` and
    // `top` take precedence and the opposite side is ignored.
    let dx = left.unwrap_or_else(|| -right.unwrap_or(0.0));
    let dy = top.unwrap_or_else(|| -bottom.unwrap_or(0.0));
    if dx == 0.0 && dy == 0.0 {
        None
    } else {
        Some((dx, dy))
    }
}

pub(super) fn apply_relative_offset(layout_box: &mut LayoutBox, node: &StyledNode, base: f32) {
    if let Some((dx, dy)) = relative_offset(node, base) {
        shift_layout_subtree(layout_box, dx, dy);
    }
}

pub(super) fn shift_layout_subtree(layout_box: &mut LayoutBox, dx: f32, dy: f32) {
    // Relative positioning shifts the visual rect of the box and *every*
    // descendant — siblings and cursors keep using the unshifted geometry, so
    // we only mutate this subtree.
    layout_box.dimensions.content.x += dx;
    layout_box.dimensions.content.y += dy;
    for child in &mut layout_box.children {
        shift_layout_subtree(child, dx, dy);
    }
}

pub(super) fn attribute_length(element: &ElementData, name: &str) -> Option<f32> {
    element
        .attributes
        .get(name)
        .and_then(|value| value.parse::<f32>().ok())
}

#[cfg(test)]
mod tests {
    use crate::{css, html, style};

    use super::layout_tree;

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
        // not sum (12). Second block's content_y = first bottom (25) + 7 = 32.
        assert_eq!(second.dimensions.content.y, 32.0);
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
    fn text_nodes_use_font_size_as_intrinsic_height() {
        let styled = styled_root(
            r#"<p class="copy">Hello</p>"#,
            r#"
                .copy { font-size: 18px; }
            "#,
        );

        let layout = layout_tree(&styled, 400.0);
        let text = &layout.children[0];

        assert_eq!(text.dimensions.content.height, 18.0);
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
                    border-left: 4px;
                    border-right: 6px;
                    border-top: 2px;
                    border-bottom: 3px;
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
        // The toy uses parent_width as the base for both axes, matching the
        // existing percent-on-margin/padding approximation.
        let styled = styled_root(
            r#"<div id="root"><div class="box"></div></div>"#,
            r#"
                #root { width: 200px; }
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

        // 25% of 200px = 50px, 10% of 200px = 20px.
        assert_eq!(shifted.dimensions.content.x, 50.0);
        assert_eq!(shifted.dimensions.content.y, 20.0);
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
    fn line_box_stretches_to_tallest_inline_child() {
        // A line containing a 12px span and a 30px span should be 30 tall —
        // that's the max of the per-child line heights, not their sum.
        let styled = styled_root(
            r#"<p><span class="small">a</span><span class="big">b</span></p>"#,
            r#"
                p { font-size: 16px; }
                .small { font-size: 12px; }
                .big { font-size: 30px; }
            "#,
        );
        let layout = layout_tree(&styled, 400.0);

        assert_eq!(layout.dimensions.content.height, 30.0);
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
        // `flex: 2` should expand to flex-grow: 2 (with shrink: 1 default and
        // basis unset). Verifies the parser-side shorthand handler.
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

        // Free = 300, total grow = 3, .a gets 200, .b gets 100.
        assert_eq!(a.dimensions.content.width, 250.0);
        assert_eq!(b.dimensions.content.width, 150.0);
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
        assert_eq!((b.dimensions.content.x, b.dimensions.content.y), (50.0, 0.0));
        assert_eq!((first.dimensions.content.x, first.dimensions.content.y), (0.0, 30.0));
        assert_eq!((c.dimensions.content.x, c.dimensions.content.y), (50.0, 30.0));
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
        let styled = styled_root(
            r#"<pre>x</pre>"#,
            r#"pre { white-space: pre; }"#,
        );
        assert_eq!(
            super::collapsed_text(&styled, "Hello\n  world"),
            "Hello\n  world"
        );
    }
}
