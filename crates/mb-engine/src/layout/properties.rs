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

// ---- Percentage heights (CSS 2.1 §10.5; compat round 3 step 1p) --------
//
// A `height` / `min-height` / `max-height` percentage resolves against the
// containing block's *height*, and only when that height is definite;
// otherwise it behaves as `auto` / `0` / `none`. Heights are computed
// bottom-up, so the definite height is handed *down*: a box resolves its own
// definite content height before laying out its children and installs it
// for them ([`with_containing_block_height`]). Inside a container's child
// layout, [`containing_block_height`] is therefore that container's own
// definite height — flex/grid/table read it as such. Pass-scoped like the
// inline-size memo; nothing outside a layout pass sees it.

thread_local! {
    static CONTAINING_BLOCK_HEIGHT: std::cell::Cell<Option<f32>> =
        const { std::cell::Cell::new(None) };
}

/// The definite content height of the box whose children are being laid
/// out, or `None` when it is auto (indefinite).
pub(crate) fn containing_block_height() -> Option<f32> {
    CONTAINING_BLOCK_HEIGHT.with(std::cell::Cell::get)
}

/// Run `layout` with `height` as the containing block's definite height
/// (`None` = indefinite); the previous value comes back afterwards, also on
/// a panic.
pub(crate) fn with_containing_block_height<R>(
    height: Option<f32>,
    layout: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<f32>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CONTAINING_BLOCK_HEIGHT.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(CONTAINING_BLOCK_HEIGHT.with(|cell| cell.replace(height)));
    layout()
}

/// A vertical size property (`height`, `min-height`, `max-height`) in px:
/// a length, or a percentage of the definite containing-block height.
/// `None` = auto / none / a percentage against an indefinite height.
pub(crate) fn height_value(node: &StyledNode, name: &str) -> Option<f32> {
    match node.value(name) {
        Some(Value::Length(value, Unit::Px)) => Some(*value),
        Some(Value::Length(value, Unit::Percent)) => {
            containing_block_height().map(|base| *value / 100.0 * base)
        }
        _ => None,
    }
}

/// `height` clamped by `max-height` then `min-height` (min wins, CSS 2.1
/// §10.7). Used for both a definite `height` and the auto content height.
pub(crate) fn clamp_height(node: &StyledNode, height: f32) -> f32 {
    let mut height = height;
    if let Some(max) = height_value(node, "max-height") {
        height = height.min(max);
    }
    if let Some(min) = height_value(node, "min-height") {
        height = height.max(min);
    }
    height.max(0.0)
}

/// The box's definite content height, if it has one: an explicit or
/// definitely-resolved `height`, clamped. This is what its children's
/// percentages resolve against.
pub(crate) fn definite_height(node: &StyledNode) -> Option<f32> {
    height_value(node, "height").map(|height| clamp_height(node, height))
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
