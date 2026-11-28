// Block flow layout — the default layout mode for any element that isn't
// explicitly inline / inline-block / flex / grid.

use crate::{css::Value, style::StyledNode};

use super::flex::{is_flex_container, layout_flex_children};
use super::grid::{is_grid_container, layout_grid_children};
use super::inline::{
    BlockChild, group_block_children, inline_align_for, inline_block_shrink_to_fit_width,
    layout_inline_children, layout_inline_run, uses_inline_flow,
};
use super::table::{is_table_container, layout_table_children};
use super::{
    Dimensions, EdgeSizes, LayoutBox, Rect, apply_relative_offset, child_height, clamp_height,
    clamp_width, container_box_type, content_width_value, definite_height, edge_sizes,
    intrinsic_height, intrinsic_width, is_auto, is_display_none, is_float_left, is_float_right,
    is_layout_whitespace_text, is_out_of_flow, length_value, outer_rect, shift_layout_subtree,
    with_containing_block_height,
};

pub(super) fn layout_node(
    node: &StyledNode,
    parent_x: f32,
    cursor_y: &mut f32,
    parent_width: f32,
) -> LayoutBox {
    layout_node_sized(node, parent_x, cursor_y, parent_width, None)
}

/// [`layout_node`] with a used content width decided by the caller — a
/// float's shrink-to-fit width (step 3d). Such a box ignores `auto`
/// margins (they are 0 on floats).
fn layout_node_sized(
    node: &StyledNode,
    parent_x: f32,
    cursor_y: &mut f32,
    parent_width: f32,
    used_width: Option<f32>,
) -> LayoutBox {
    let raw_margin = edge_sizes(node, "margin", parent_width);
    let padding = edge_sizes(node, "padding", parent_width);
    let border = edge_sizes(node, "border", parent_width);

    // CSS auto-margin centering only applies when a width is specified.
    let specified_width = used_width
        .or_else(|| content_width_value(node, "width", parent_width))
        .or_else(|| intrinsic_width(node));
    let left_auto = used_width.is_none() && is_auto(node, "margin-left");
    let right_auto = used_width.is_none() && is_auto(node, "margin-right");
    // min-/max-width (CSS 2.1 §10.4, step 5m): clamp the tentative width —
    // the specified one, or what `auto` would fill (auto margins as 0). A
    // clamped `auto` width is then used as if specified, so auto margins
    // share what's left: `max-width: 1200px; margin: 0 auto` centres.
    let tentative = specified_width.unwrap_or_else(|| {
        let margins = if left_auto { 0.0 } else { raw_margin.left }
            + if right_auto { 0.0 } else { raw_margin.right };
        (parent_width - margins - padding.left - padding.right - border.left - border.right)
            .max(0.0)
    });
    let clamped = clamp_width(node, tentative, parent_width);
    let explicit_width = (specified_width.is_some() || clamped != tentative).then_some(clamped);

    let (content_width, margin_left, margin_right) = if let Some(width) = explicit_width {
        let used = padding.left + padding.right + border.left + border.right + width;
        let total_margin_space = (parent_width - used).max(0.0);
        let (ml, mr) = match (left_auto, right_auto) {
            (true, true) => (total_margin_space / 2.0, total_margin_space / 2.0),
            (true, false) => (
                (total_margin_space - raw_margin.right).max(0.0),
                raw_margin.right,
            ),
            (false, true) => (
                raw_margin.left,
                (total_margin_space - raw_margin.left).max(0.0),
            ),
            (false, false) => (raw_margin.left, raw_margin.right),
        };
        (width, ml, mr)
    } else {
        // With width: auto, an auto horizontal margin collapses to 0 and the
        // content stretches to fill the parent.
        let ml = if left_auto { 0.0 } else { raw_margin.left };
        let mr = if right_auto { 0.0 } else { raw_margin.right };
        let horizontal_non_content =
            ml + mr + padding.left + padding.right + border.left + border.right;
        let width = (parent_width - horizontal_non_content).max(0.0);
        (width, ml, mr)
    };

    let margin = EdgeSizes {
        left: margin_left,
        right: margin_right,
        top: raw_margin.top,
        bottom: raw_margin.bottom,
    };

    let content_x = parent_x + margin.left + border.left + padding.left;
    let content_y = *cursor_y + margin.top + border.top + padding.top;

    // Flex / Grid containers run their own child-placement algorithms and
    // bypass the inline-flow/block-flow paths entirely. Both ignore margin
    // collapse and floats per spec.
    // Otherwise: parents with only inline children lay them out left-to-right;
    // everything else stays block.
    // This box's definite content height (its `height` against our
    // containing block) is what its children's percentages resolve
    // against — known before they are laid out, or never (auto).
    let own_height = definite_height(node);
    let (children, auto_content_height) = with_containing_block_height(own_height, || {
        if is_flex_container(node) {
            layout_flex_children(node, &node.children, content_x, content_y, content_width)
        } else if is_grid_container(node) {
            layout_grid_children(node, &node.children, content_x, content_y, content_width)
        } else if is_table_container(node) {
            layout_table_children(node, &node.children, content_x, content_y, content_width)
        } else if uses_inline_flow(node) {
            let align = inline_align_for(node);
            layout_inline_children(&node.children, content_x, content_y, content_width, align)
        } else {
            // Block flow: stack children top-to-bottom while collapsing the
            // previous in-flow child's margin-bottom against the next child's
            // margin-top. Out-of-flow children skip both the cursor advance and
            // the collapse chain — they neither push siblings down nor break
            // adjacency between the in-flow neighbours that surround them.
            // Floats are also out of flow but they DO take horizontal space at
            // the current cursor and let `clear` push later siblings past them.
            let mut child_cursor_y = content_y;
            let mut prev_margin_bottom: f32 = 0.0;
            let mut next_left_float_x = content_x;
            let mut next_right_float_right = content_x + content_width;
            let mut float_bottom_left: f32 = content_y;
            let mut float_bottom_right: f32 = content_y;
            let mut children: Vec<LayoutBox> = Vec::with_capacity(node.children.len());
            let align = inline_align_for(node);
            for group in group_block_children(&node.children) {
                let child = match group {
                    BlockChild::Single(i) => &node.children[i],
                    BlockChild::InlineRun(range) => {
                        // Anonymous block: no margins of its own, so the previous
                        // child's margin-bottom (already in the cursor) stands
                        // and ends the collapse chain.
                        let (boxes, height) = layout_inline_run(
                            &node.children[range],
                            content_x,
                            child_cursor_y,
                            content_width,
                            align,
                        );
                        children.extend(boxes);
                        child_cursor_y += height;
                        prev_margin_bottom = 0.0;
                        continue;
                    }
                };
                if is_display_none(child) {
                    continue;
                }
                // Pure-whitespace text children come from the HTML parser's
                // inter-element whitespace preservation (so inline runs keep
                // their separating spaces). In block flow that whitespace
                // would otherwise lay out as a font-size-tall full-width
                // line — a visible vertical gap between every pair of block
                // siblings on the page. Real browsers anonymous-inline this
                // whitespace and then collapse it to nothing in pure-block
                // context; the toy approximates the same observable result
                // by dropping the child outright.
                if is_layout_whitespace_text(child) {
                    continue;
                }
                if is_out_of_flow(child) {
                    let mut frozen = child_cursor_y;
                    children.push(layout_node(child, content_x, &mut frozen, content_width));
                    continue;
                }

                if is_float_left(child) {
                    // Place at the next available x in the left-float column. The
                    // float's outer top sits at the current in-flow cursor, but
                    // we never write back to the cursor — siblings flow past it.
                    let mut throwaway = child_cursor_y;
                    let width = float_width(child, content_width);
                    let mut float_box = layout_node_sized(
                        child,
                        next_left_float_x,
                        &mut throwaway,
                        content_width,
                        width,
                    );
                    let outer = outer_rect(&float_box);
                    // No room beside the floats already on this row (step
                    // 3d): start a new row below them, at the left edge.
                    if next_left_float_x > content_x
                        && next_left_float_x + outer.width > next_right_float_right + 0.01
                    {
                        let row_y = float_bottom_left.max(child_cursor_y);
                        shift_layout_subtree(
                            &mut float_box,
                            content_x - next_left_float_x,
                            row_y - child_cursor_y,
                        );
                        throwaway += row_y - child_cursor_y;
                        next_left_float_x = content_x;
                        if row_y >= float_bottom_right {
                            next_right_float_right = content_x + content_width;
                        }
                    }
                    next_left_float_x += outer.width;
                    float_bottom_left = float_bottom_left.max(throwaway);
                    children.push(float_box);
                    continue;
                }
                if is_float_right(child) {
                    // Right floats need their outer width before placement, so we
                    // lay out at the left edge first to measure, then shift the
                    // whole subtree to (right_edge - outer_width).
                    let mut throwaway = child_cursor_y;
                    let width = float_width(child, content_width);
                    let mut float_box =
                        layout_node_sized(child, content_x, &mut throwaway, content_width, width);
                    let outer_width = outer_rect(&float_box).width;
                    let mut dy = 0.0;
                    if next_right_float_right < content_x + content_width
                        && next_right_float_right - outer_width < next_left_float_x - 0.01
                    {
                        // Same as a left float: a new row below.
                        let row_y = float_bottom_right.max(child_cursor_y);
                        dy = row_y - child_cursor_y;
                        next_right_float_right = content_x + content_width;
                        if row_y >= float_bottom_left {
                            next_left_float_x = content_x;
                        }
                    }
                    let dx = (next_right_float_right - outer_width) - content_x;
                    if dx != 0.0 || dy != 0.0 {
                        shift_layout_subtree(&mut float_box, dx, dy);
                    }
                    throwaway += dy;
                    next_right_float_right -= outer_width;
                    float_bottom_right = float_bottom_right.max(throwaway);
                    children.push(float_box);
                    continue;
                }

                // `clear` jumps the cursor past prior floats on the named side(s)
                // and resets the float-stack column for that side because no
                // earlier floats are adjacent at the new cursor anymore.
                let cleared = clear_target_y(child, float_bottom_left, float_bottom_right);
                if cleared > child_cursor_y {
                    child_cursor_y = cleared;
                    prev_margin_bottom = 0.0;
                    if cleared >= float_bottom_left {
                        next_left_float_x = content_x;
                    }
                    if cleared >= float_bottom_right {
                        next_right_float_right = content_x + content_width;
                    }
                }

                // The cursor at this point already includes prev_margin_bottom from
                // the previous in-flow child's tail. Subtracting `(sum - combined)`
                // collapses it against the next margin-top before the child uses
                // the cursor as its own starting position.
                let next_margin_top =
                    length_value(child, "margin-top", content_width).unwrap_or(0.0);
                let combined = collapse_margins(prev_margin_bottom, next_margin_top);
                child_cursor_y += combined - (prev_margin_bottom + next_margin_top);
                let laid_out = layout_node(child, content_x, &mut child_cursor_y, content_width);
                prev_margin_bottom = laid_out.dimensions.margin.bottom;
                children.push(laid_out);
            }
            // Parent height needs to cover both the in-flow cursor and the
            // tallest float — without this, floats would spill below the parent's
            // background.
            let in_flow_height = child_height(node, content_y, child_cursor_y);
            let float_height = (float_bottom_left.max(float_bottom_right) - content_y).max(0.0);
            (children, in_flow_height.max(float_height))
        }
    });

    let content_height = own_height
        .unwrap_or_else(|| clamp_height(node, auto_content_height.max(intrinsic_height(node))));

    let dimensions = Dimensions {
        content: Rect {
            x: content_x,
            y: content_y,
            width: content_width,
            height: content_height,
        },
        padding,
        border,
        margin,
    };

    // Sibling cursor advances based on the *unoffset* outer bottom: a relative
    // element only shifts its own subtree, never its in-flow neighbors.
    *cursor_y = content_y + content_height + padding.bottom + border.bottom + margin.bottom;

    let mut layout_box = LayoutBox {
        box_type: container_box_type(node),
        dimensions,
        children,
    };
    apply_relative_offset(&mut layout_box, node, parent_width);
    layout_box
}

fn collapse_margins(prev: f32, next: f32) -> f32 {
    // CSS adjacent-margin collapse rules:
    // - both non-negative → max
    // - both non-positive → most negative (min)
    // - mixed signs → algebraic sum
    if prev >= 0.0 && next >= 0.0 {
        prev.max(next)
    } else if prev <= 0.0 && next <= 0.0 {
        prev.min(next)
    } else {
        prev + next
    }
}

/// The used content width of a float with `width: auto` — shrink-to-fit
/// (CSS 2.1 §10.3.5), measured like an inline-block against what's left of
/// the containing block after the float's own margins, borders and padding.
/// `None` when the float has a width of its own (or an intrinsic one).
/// Before step 3d an auto-width float stretched to the whole container,
/// so floated `dt`/`dd` pairs marched off to the right one container-width
/// apart.
fn float_width(node: &StyledNode, containing_width: f32) -> Option<f32> {
    if content_width_value(node, "width", containing_width).is_some()
        || intrinsic_width(node).is_some()
    {
        return None;
    }
    let margin = edge_sizes(node, "margin", containing_width);
    let padding = edge_sizes(node, "padding", containing_width);
    let border = edge_sizes(node, "border", containing_width);
    let available = (containing_width
        - margin.left
        - margin.right
        - padding.left
        - padding.right
        - border.left
        - border.right)
        .max(0.0);
    Some(clamp_width(
        node,
        inline_block_shrink_to_fit_width(node, available),
        containing_width,
    ))
}

fn clear_target_y(node: &StyledNode, float_bottom_left: f32, float_bottom_right: f32) -> f32 {
    // CSS `clear` makes the box's outer top jump down past preceding floats
    // on the named side(s). Returning -∞ means the cursor is left untouched.
    match node.value("clear") {
        Some(Value::Keyword(keyword)) if keyword == "left" => float_bottom_left,
        Some(Value::Keyword(keyword)) if keyword == "right" => float_bottom_right,
        Some(Value::Keyword(keyword)) if keyword == "both" => {
            float_bottom_left.max(float_bottom_right)
        }
        _ => f32::NEG_INFINITY,
    }
}
