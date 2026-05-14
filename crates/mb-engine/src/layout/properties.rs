// Property/predicate helpers shared by every layout algorithm: percent-or-px
// length resolution against a base, boolean checks for `position` / `float` /
// `display: none` keywords, the relative-positioning offset pair, the
// subtree-shifter that applies it, and the HTML-attribute → f32 reader.
//
// Everything here is `pub(crate)` so block/flex/grid/inline/table/absolute can
// pull them in via mod.rs's `pub(super) use` re-export. None of these touch
// box-tree mutation except
// `shift_layout_subtree` and `apply_relative_offset`, which are the two
// helpers paired with `relative_offset` for the in-flow `position: relative`
// path.

use crate::{
    css::{Unit, Value},
    dom::{ElementData, NodeType},
    style::StyledNode,
};

use super::{EdgeSizes, LayoutBox};

pub(crate) fn edge_sizes(node: &StyledNode, prefix: &str, base: f32) -> EdgeSizes {
    // CSS resolves percent margin/padding against the containing block's *width*, even
    // for the top and bottom sides — a common gotcha worth keeping in mind here.
    EdgeSizes {
        left: length_value(node, &format!("{prefix}-left"), base).unwrap_or(0.0),
        right: length_value(node, &format!("{prefix}-right"), base).unwrap_or(0.0),
        top: length_value(node, &format!("{prefix}-top"), base).unwrap_or(0.0),
        bottom: length_value(node, &format!("{prefix}-bottom"), base).unwrap_or(0.0),
    }
}

pub(crate) fn length_value(node: &StyledNode, name: &str, base: f32) -> Option<f32> {
    // `base` is the containing-block dimension a Percent length resolves against. For
    // properties that should never see a percent (font-size after style resolution, etc.)
    // callers can safely pass any value.
    match node.value(name) {
        Some(Value::Length(value, Unit::Px)) => Some(*value),
        Some(Value::Length(value, Unit::Percent)) => Some(*value / 100.0 * base),
        _ => None,
    }
}

pub(crate) fn is_auto(node: &StyledNode, name: &str) -> bool {
    matches!(node.value(name), Some(Value::Keyword(keyword)) if keyword == "auto")
}

pub(crate) fn is_position_relative(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "relative")
}

pub(crate) fn is_position_absolute(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "absolute")
}

pub(crate) fn is_position_fixed(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "fixed")
}

pub(crate) fn is_out_of_flow(node: &StyledNode) -> bool {
    // Both `absolute` and `fixed` skip in-flow placement during pass 1; they
    // differ only in which containing block pass 2 resolves them against.
    is_position_absolute(node) || is_position_fixed(node)
}

pub(crate) fn is_float_left(node: &StyledNode) -> bool {
    matches!(node.value("float"), Some(Value::Keyword(k)) if k == "left")
}

pub(crate) fn is_float_right(node: &StyledNode) -> bool {
    matches!(node.value("float"), Some(Value::Keyword(k)) if k == "right")
}

pub(crate) fn has_float(node: &StyledNode) -> bool {
    is_float_left(node) || is_float_right(node)
}

pub(crate) fn is_display_none(node: &StyledNode) -> bool {
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
pub(crate) fn is_layout_whitespace_text(node: &StyledNode) -> bool {
    matches!(
        &node.node_type,
        NodeType::Text(text) if text.chars().all(char::is_whitespace)
    )
}

pub(crate) fn relative_offset(node: &StyledNode, base: f32) -> Option<(f32, f32)> {
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

pub(crate) fn apply_relative_offset(layout_box: &mut LayoutBox, node: &StyledNode, base: f32) {
    if let Some((dx, dy)) = relative_offset(node, base) {
        shift_layout_subtree(layout_box, dx, dy);
    }
}

pub(crate) fn shift_layout_subtree(layout_box: &mut LayoutBox, dx: f32, dy: f32) {
    // Relative positioning shifts the visual rect of the box and *every*
    // descendant — siblings and cursors keep using the unshifted geometry, so
    // we only mutate this subtree.
    layout_box.dimensions.content.x += dx;
    layout_box.dimensions.content.y += dy;
    for child in &mut layout_box.children {
        shift_layout_subtree(child, dx, dy);
    }
}

pub(crate) fn attribute_length(element: &ElementData, name: &str) -> Option<f32> {
    element
        .attributes
        .get(name)
        .and_then(|value| value.parse::<f32>().ok())
}
