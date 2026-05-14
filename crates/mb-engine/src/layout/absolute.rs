// Pass-2 of layout: walk the laid-out tree and reposition every
// `position: absolute` / `position: fixed` subtree against its
// containing block. Pass-1 (block/flex/grid) ran with absolute boxes
// still in static flow position; this module is what moves them to
// their final spot using the `top`/`right`/`bottom`/`left` values.
//
// `ContainingBlock` lives here because the only callers that build one
// are the entry shim in mod.rs and this module itself.

use super::{
    LayoutBox, Rect, box_is_absolute, box_is_fixed, box_is_positioned, box_styled_node,
    length_value, outer_rect, shift_layout_subtree,
};

#[derive(Debug, Clone, Copy)]
pub(super) struct ContainingBlock {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) width: f32,
    pub(super) height: f32,
}

pub(super) fn reposition_absolutes(
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
    let outer: Rect = outer_rect(layout_box);
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
