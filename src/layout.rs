use crate::{
    css::{TrackSize, Unit, Value},
    dom::{ElementData, NodeType},
    style::StyledNode,
};

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
    AnonymousBlock,
}

pub fn layout_tree(root: &StyledNode, viewport_width: f32) -> LayoutBox {
    let mut cursor_y = 0.0;
    let mut layout_box = layout_node(root, 0.0, &mut cursor_y, viewport_width);
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

fn box_styled_node(layout_box: &LayoutBox) -> Option<&StyledNode> {
    match &layout_box.box_type {
        BoxType::BlockNode(node) | BoxType::FlexNode(node) | BoxType::GridNode(node) => Some(node),
        BoxType::AnonymousBlock => None,
    }
}

fn box_position_keyword(layout_box: &LayoutBox) -> Option<&str> {
    match box_styled_node(layout_box).and_then(|node| node.value("position"))? {
        Value::Keyword(keyword) => Some(keyword.as_str()),
        _ => None,
    }
}

fn box_is_positioned(layout_box: &LayoutBox) -> bool {
    matches!(
        box_position_keyword(layout_box),
        Some("relative" | "absolute" | "fixed")
    )
}

fn box_is_absolute(layout_box: &LayoutBox) -> bool {
    matches!(box_position_keyword(layout_box), Some("absolute"))
}

fn box_is_fixed(layout_box: &LayoutBox) -> bool {
    matches!(box_position_keyword(layout_box), Some("fixed"))
}

fn outer_rect(layout_box: &LayoutBox) -> Rect {
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

fn layout_node(
    node: &StyledNode,
    parent_x: f32,
    cursor_y: &mut f32,
    parent_width: f32,
) -> LayoutBox {
    let raw_margin = edge_sizes(node, "margin", parent_width);
    let padding = edge_sizes(node, "padding", parent_width);
    let border = edge_sizes(node, "border", parent_width);

    // CSS auto-margin centering only applies when a width is specified.
    let explicit_width =
        length_value(node, "width", parent_width).or_else(|| intrinsic_width(node));
    let left_auto = is_auto(node, "margin-left");
    let right_auto = is_auto(node, "margin-right");

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
    let (children, auto_content_height) = if is_flex_container(node) {
        layout_flex_children(node, &node.children, content_x, content_y, content_width)
    } else if is_grid_container(node) {
        layout_grid_children(node, &node.children, content_x, content_y, content_width)
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
        for child in &node.children {
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
                let float_box =
                    layout_node(child, next_left_float_x, &mut throwaway, content_width);
                let outer = outer_rect(&float_box);
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
                let mut float_box = layout_node(child, content_x, &mut throwaway, content_width);
                let outer_width = outer_rect(&float_box).width;
                let dx = (next_right_float_right - outer_width) - content_x;
                if dx != 0.0 {
                    shift_layout_subtree(&mut float_box, dx, 0.0);
                }
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
            let next_margin_top = length_value(child, "margin-top", content_width).unwrap_or(0.0);
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
    };

    // Percent-on-height technically resolves against the parent's height in CSS, but the
    // layout walk does not yet track parent height (heights are computed bottom-up). For
    // now we use parent_width as the base, which is wrong only for explicit `height: x%`
    // declarations — none of our toy pages exercise that path.
    let content_height = length_value(node, "height", parent_width)
        .unwrap_or_else(|| auto_content_height.max(intrinsic_height(node)));

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlexDirection {
    Row,
    Column,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JustifyContent {
    FlexStart,
    Center,
    FlexEnd,
    SpaceBetween,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlignItems {
    Stretch,
    FlexStart,
    Center,
    FlexEnd,
}

fn layout_flex_children(
    container: &StyledNode,
    children: &[StyledNode],
    content_x: f32,
    content_y: f32,
    content_width: f32,
) -> (Vec<LayoutBox>, f32) {
    // Two-pass placement. Pass 1: lay out every in-flow item at the container's
    // content origin so we can measure each item's outer main/cross size
    // without committing to a final position. Pass 2: read justify-content
    // (main axis) and align-items (cross axis), shift each item by its
    // computed offset on each axis. For align-items: stretch, items without
    // an explicit cross size also have their content cross size grown to fill
    // the container.
    //
    // Sizing comes from the inline-block path (explicit width wins, otherwise
    // shrink-to-fit). Margin collapse and floats are skipped on flex items
    // per spec.
    let direction = flex_direction(container);
    let justify = justify_content(container);
    let align = align_items(container);

    let mut boxes: Vec<LayoutBox> = Vec::with_capacity(children.len());
    // Track (boxes index, source styled node) for each in-flow item so pass 2
    // can read the styled node again to decide whether stretch should grow
    // the item.
    let mut in_flow: Vec<(usize, &StyledNode)> = Vec::new();

    for child in children {
        if is_out_of_flow(child) {
            // Static-position approximation, same trick as inline flow: drop
            // the absolute child at the container's content origin and let
            // the absolute reposition pass at the tree root move it.
            let abs_box = layout_inline_or_inline_block(child, content_x, content_y, content_width);
            boxes.push(abs_box);
            continue;
        }
        in_flow.push((boxes.len(), child));
        boxes.push(layout_flex_item(child, content_x, content_y, content_width));
    }

    // Apply explicit `flex-basis: <length>` as a content-axis override on
    // top of pass-1 sizing. CSS spec: flex-basis takes precedence over the
    // item's `width`/`height` along the main axis, so the override happens
    // after pass 1 even though it would also have been valid to substitute
    // the basis upstream.
    for &(i, child) in &in_flow {
        if let Some(basis) = flex_basis_content_main(child) {
            force_item_content_main(&mut boxes[i], basis, direction);
        }
    }

    let total_basis: f32 = in_flow
        .iter()
        .map(|&(i, _)| main_axis_outer(&boxes[i], direction))
        .sum();

    // Container's main-axis content size. Row direction is anchored to the
    // already-resolved content_width. Column direction needs an explicit
    // height for `justify-content`/grow to have any leftover to distribute —
    // height: auto falls back to total_basis so free space is zero.
    let container_main_size = match direction {
        FlexDirection::Row => content_width,
        FlexDirection::Column => {
            length_value(container, "height", content_width).unwrap_or(total_basis)
        }
    };

    // Distribute free space along the main axis. Positive free space goes to
    // flex-grow (proportional to grow weight). Negative free space goes to
    // flex-shrink (proportional to shrink weight × basis, per spec, so larger
    // items absorb more shrinkage). Items with zero weight are skipped on
    // their respective passes.
    let free = container_main_size - total_basis;
    if free > 0.0 {
        let total_grow: f32 = in_flow.iter().map(|&(_, child)| flex_grow(child)).sum();
        if total_grow > 0.0 {
            for &(i, child) in &in_flow {
                let grow = flex_grow(child);
                if grow > 0.0 {
                    let delta = free * grow / total_grow;
                    grow_item_main(&mut boxes[i], delta, direction);
                }
            }
        }
    } else if free < 0.0 {
        let total_shrink_weight: f32 = in_flow
            .iter()
            .map(|&(i, child)| flex_shrink(child) * main_axis_outer(&boxes[i], direction))
            .sum();
        if total_shrink_weight > 0.0 {
            for &(i, child) in &in_flow {
                let weight = flex_shrink(child) * main_axis_outer(&boxes[i], direction);
                if weight > 0.0 {
                    // free is negative; delta is the (negative) size delta to
                    // apply to this item, so larger weights shrink more.
                    let delta = free * weight / total_shrink_weight;
                    grow_item_main(&mut boxes[i], delta, direction);
                }
            }
        }
    }

    // After grow/shrink, total_used reflects the actually-occupied main size.
    // When grow consumed all positive free space (or shrink absorbed all
    // negative free space) the leftover is zero and justify-content has
    // nothing to distribute.
    let total_used: f32 = in_flow
        .iter()
        .map(|&(i, _)| main_axis_outer(&boxes[i], direction))
        .sum();
    let leftover_main = (container_main_size - total_used).max(0.0);
    let item_count = in_flow.len();

    let (start_offset, between_gap) = match justify {
        JustifyContent::FlexStart => (0.0, 0.0),
        JustifyContent::Center => (leftover_main / 2.0, 0.0),
        JustifyContent::FlexEnd => (leftover_main, 0.0),
        JustifyContent::SpaceBetween if item_count > 1 => {
            (0.0, leftover_main / (item_count - 1) as f32)
        }
        // Single-item space-between collapses to flex-start (no gap to distribute).
        JustifyContent::SpaceBetween => (0.0, 0.0),
    };

    // Container's cross size — needed before pass 2 so each item knows what
    // to align against. For row direction, height may be explicit or fall back
    // to the tallest item's outer cross size; for column direction the cross
    // axis is width, which is always already resolved.
    let max_cross_natural: f32 = in_flow
        .iter()
        .map(|&(i, _)| cross_axis_outer(&boxes[i], direction))
        .fold(0.0, f32::max);
    let container_cross_size = match direction {
        FlexDirection::Row => {
            length_value(container, "height", content_width).unwrap_or(max_cross_natural)
        }
        FlexDirection::Column => content_width,
    };

    let mut cursor = start_offset;
    for (idx_in_flow, &(i, child)) in in_flow.iter().enumerate() {
        // Stretch grows the item's content cross size to fill the container,
        // but only when the item didn't declare its own cross size — explicit
        // sizes always win over stretch per spec. The growth happens before
        // we read cross_axis_outer below so the post-stretch height feeds the
        // alignment math correctly.
        if matches!(align, AlignItems::Stretch) && !has_explicit_cross_size(child, direction) {
            stretch_item_to_cross(&mut boxes[i], container_cross_size, direction);
        }

        let main_size = main_axis_outer(&boxes[i], direction);
        let cross_size = cross_axis_outer(&boxes[i], direction);
        let cross_offset = match align {
            AlignItems::FlexStart | AlignItems::Stretch => 0.0,
            AlignItems::Center => ((container_cross_size - cross_size) / 2.0).max(0.0),
            AlignItems::FlexEnd => (container_cross_size - cross_size).max(0.0),
        };

        match direction {
            FlexDirection::Row => shift_layout_subtree(&mut boxes[i], cursor, cross_offset),
            FlexDirection::Column => shift_layout_subtree(&mut boxes[i], cross_offset, cursor),
        }
        cursor += main_size;
        if idx_in_flow + 1 < item_count {
            cursor += between_gap;
        }
    }

    // Auto height for the container depends on direction:
    // - row:    cross axis = height, so it grows to the tallest item
    //           (post-stretch). When the container has explicit height, that
    //           wins anyway in layout_node — this fallback only matters in the
    //           auto case, where container_cross_size == max_cross_natural.
    // - column: main axis  = height, so it grows to the cumulative cursor.
    let auto_content_height = match direction {
        FlexDirection::Row => container_cross_size,
        FlexDirection::Column => cursor,
    };
    (boxes, auto_content_height)
}

fn stretch_item_to_cross(
    layout_box: &mut LayoutBox,
    container_cross_size: f32,
    direction: FlexDirection,
) {
    // Grow the item's content rect on the cross axis so its outer size matches
    // the container's cross. Margins/borders/padding stay as declared, so the
    // delta lands on content size only. Children that already laid out inside
    // do not move — they stay at their original positions and any gained
    // space appears as background area at the trailing edge, which is the
    // simplest reasonable visual approximation of stretch for a toy renderer.
    let outer = outer_rect(layout_box);
    let current_outer_cross = match direction {
        FlexDirection::Row => outer.height,
        FlexDirection::Column => outer.width,
    };
    if current_outer_cross >= container_cross_size {
        return;
    }
    let delta = container_cross_size - current_outer_cross;
    match direction {
        FlexDirection::Row => layout_box.dimensions.content.height += delta,
        FlexDirection::Column => layout_box.dimensions.content.width += delta,
    }
}

fn align_items(node: &StyledNode) -> AlignItems {
    match node.value("align-items") {
        Some(Value::Keyword(keyword)) if keyword == "flex-start" => AlignItems::FlexStart,
        Some(Value::Keyword(keyword)) if keyword == "center" => AlignItems::Center,
        Some(Value::Keyword(keyword)) if keyword == "flex-end" => AlignItems::FlexEnd,
        Some(Value::Keyword(keyword)) if keyword == "stretch" => AlignItems::Stretch,
        // CSS default for align-items is `stretch`.
        _ => AlignItems::Stretch,
    }
}

fn has_explicit_cross_size(node: &StyledNode, direction: FlexDirection) -> bool {
    let prop = match direction {
        FlexDirection::Row => "height",
        FlexDirection::Column => "width",
    };
    matches!(node.value(prop), Some(Value::Length(_, _)))
}

fn flex_grow(node: &StyledNode) -> f32 {
    // CSS default is 0 — items don't grow unless the author opts in.
    match node.value("flex-grow") {
        Some(Value::Number(value)) if *value >= 0.0 => *value,
        _ => 0.0,
    }
}

fn flex_shrink(node: &StyledNode) -> f32 {
    // CSS default is 1 — items shrink to fit by default.
    match node.value("flex-shrink") {
        Some(Value::Number(value)) if *value >= 0.0 => *value,
        _ => 1.0,
    }
}

fn flex_basis_content_main(node: &StyledNode) -> Option<f32> {
    // Only the explicit `<length>` form drives the content-axis override.
    // `auto` (default) leaves layout_flex_item's width/shrink-to-fit logic in
    // charge, and percent / keyword forms are not yet implemented.
    match node.value("flex-basis") {
        Some(Value::Length(value, Unit::Px)) => Some(*value),
        _ => None,
    }
}

fn force_item_content_main(
    layout_box: &mut LayoutBox,
    content_main: f32,
    direction: FlexDirection,
) {
    let value = content_main.max(0.0);
    match direction {
        FlexDirection::Row => layout_box.dimensions.content.width = value,
        FlexDirection::Column => layout_box.dimensions.content.height = value,
    }
}

fn grow_item_main(layout_box: &mut LayoutBox, delta: f32, direction: FlexDirection) {
    // Resize is post-hoc on the item's content rect — children that were laid
    // out in pass 1 keep their original positions, so any extra space appears
    // as background area at the trailing edge. This is the same simplification
    // already used by stretch_item_to_cross.
    match direction {
        FlexDirection::Row => {
            layout_box.dimensions.content.width =
                (layout_box.dimensions.content.width + delta).max(0.0);
        }
        FlexDirection::Column => {
            layout_box.dimensions.content.height =
                (layout_box.dimensions.content.height + delta).max(0.0);
        }
    }
}

fn main_axis_outer(layout_box: &LayoutBox, direction: FlexDirection) -> f32 {
    let outer = outer_rect(layout_box);
    match direction {
        FlexDirection::Row => outer.width,
        FlexDirection::Column => outer.height,
    }
}

fn cross_axis_outer(layout_box: &LayoutBox, direction: FlexDirection) -> f32 {
    let outer = outer_rect(layout_box);
    match direction {
        FlexDirection::Row => outer.height,
        FlexDirection::Column => outer.width,
    }
}

fn layout_flex_item(node: &StyledNode, x: f32, y: f32, available_width: f32) -> LayoutBox {
    // Flex items are block-level boxes from the inside (their own children
    // still lay out as block/inline/flex), but on the outside they get
    // shrink-to-fit sizing rather than stretching to fill the parent. The
    // inline-block path already implements exactly that sizing rule, so we
    // reuse it directly.
    layout_inline_block_node(node, x, y, available_width)
}

fn is_flex_container(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(keyword)) if keyword == "flex")
}

fn is_grid_container(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(keyword)) if keyword == "grid")
}

fn container_box_type(node: &StyledNode) -> BoxType {
    if is_flex_container(node) {
        BoxType::FlexNode(node.clone())
    } else if is_grid_container(node) {
        BoxType::GridNode(node.clone())
    } else {
        BoxType::BlockNode(node.clone())
    }
}

fn layout_grid_children<'a>(
    container: &'a StyledNode,
    children: &'a [StyledNode],
    content_x: f32,
    content_y: f32,
    content_width: f32,
) -> (Vec<LayoutBox>, f32) {
    // Four-pass placement.
    //   Pass 0: lay each in-flow item out at the container origin with the
    //           full container width as available_width, just to measure
    //           natural outer widths. Auto tracks need these; Length/fr
    //           tracks ignore them. Auto-flow is row-major: item k goes to
    //           (row = k / n_cols, col = k % n_cols).
    //   Pass 1: resolve column tracks to pixel widths using the natural-width
    //           samples (auto tracks pick the column max).
    //   Pass 2: shift each item to its track's x and grow its content to
    //           fill the track when no explicit width was declared.
    //   Pass 3: compute each row's height (max outer height) and shift each
    //           item down by its row's cumulative y offset.
    //
    // Out-of-flow children skip the grid entirely — they sit at the container
    // origin during pass 0 and the absolute reposition pass at the tree root
    // moves them to their containing block.
    let track_decls = match container.value("grid-template-columns") {
        Some(Value::TrackList(tracks)) if !tracks.is_empty() => Some(tracks.as_slice()),
        _ => None,
    };
    let n_cols = track_decls.map(|t| t.len()).unwrap_or(1).max(1);

    let mut boxes: Vec<LayoutBox> = Vec::with_capacity(children.len());
    // Each entry: (row, col, boxes index, source styled node) for one in-flow item.
    let mut cell_assignments: Vec<(usize, usize, usize, &'a StyledNode)> = Vec::new();
    let mut next_cell = 0usize;

    for child in children {
        if is_out_of_flow(child) {
            let abs_box = layout_inline_or_inline_block(child, content_x, content_y, content_width);
            boxes.push(abs_box);
            continue;
        }
        let col = next_cell % n_cols;
        let row = next_cell / n_cols;
        next_cell += 1;
        let box_idx = boxes.len();
        cell_assignments.push((row, col, box_idx, child));
        // Pre-pass: lay out at the container origin so we can read the
        // item's natural outer width before knowing its track width.
        boxes.push(layout_inline_block_node(
            child,
            content_x,
            content_y,
            content_width,
        ));
    }

    // Per-column natural max outer width — feeds auto track sizing.
    let mut natural_max_per_col = vec![0.0_f32; n_cols];
    for &(_, col, box_idx, _) in &cell_assignments {
        let w = outer_rect(&boxes[box_idx]).width;
        if w > natural_max_per_col[col] {
            natural_max_per_col[col] = w;
        }
    }

    let columns = resolve_grid_columns(track_decls, content_width, &natural_max_per_col);
    let mut col_offsets: Vec<f32> = Vec::with_capacity(columns.len());
    let mut acc = 0.0;
    for w in &columns {
        col_offsets.push(acc);
        acc += w;
    }

    // Pass 2: shift each item to its track and grow content to fill.
    for &(_, col, box_idx, child) in &cell_assignments {
        let target_outer_x = content_x + col_offsets[col];
        let current_outer_x = outer_rect(&boxes[box_idx]).x;
        let dx = target_outer_x - current_outer_x;
        if dx != 0.0 {
            shift_layout_subtree(&mut boxes[box_idx], dx, 0.0);
        }
        if !matches!(child.value("width"), Some(Value::Length(_, _))) {
            let edges =
                outer_rect(&boxes[box_idx]).width - boxes[box_idx].dimensions.content.width;
            let target = (columns[col] - edges).max(0.0);
            if boxes[box_idx].dimensions.content.width < target {
                boxes[box_idx].dimensions.content.width = target;
            }
        }
    }

    // Pass 3: natural row heights = max(item outer height) per row.
    let n_rows = cell_assignments
        .iter()
        .map(|&(row, _, _, _)| row + 1)
        .max()
        .unwrap_or(0);
    let mut natural_row_heights = vec![0.0_f32; n_rows];
    for &(row, _, box_idx, _) in &cell_assignments {
        let h = outer_rect(&boxes[box_idx]).height;
        if h > natural_row_heights[row] {
            natural_row_heights[row] = h;
        }
    }

    // Resolve `grid-template-rows` against the natural heights. Rows the
    // template doesn't cover fall back to natural max.
    let row_track_decls = match container.value("grid-template-rows") {
        Some(Value::TrackList(tracks)) if !tracks.is_empty() => Some(tracks.as_slice()),
        _ => None,
    };
    let container_explicit_height = length_value(container, "height", content_width);
    let row_heights = resolve_grid_rows(
        row_track_decls,
        container_explicit_height,
        &natural_row_heights,
    );

    let mut row_offsets: Vec<f32> = Vec::with_capacity(n_rows);
    let mut acc = 0.0;
    for h in &row_heights {
        row_offsets.push(acc);
        acc += h;
    }
    for &(row, _, box_idx, _) in &cell_assignments {
        let dy = row_offsets[row];
        if dy != 0.0 {
            shift_layout_subtree(&mut boxes[box_idx], 0.0, dy);
        }
    }

    let auto_content_height: f32 = row_heights.iter().sum();
    (boxes, auto_content_height)
}

fn resolve_grid_rows(
    tracks: Option<&[TrackSize]>,
    container_height: Option<f32>,
    natural_row_heights: &[f32],
) -> Vec<f32> {
    // Rows differ from columns in two ways:
    //   - Container main-axis size (height) is often `auto`. fr rows can only
    //     distribute leftover when the container has an explicit height; under
    //     auto height they collapse to zero (matching the flex-column rule).
    //   - The template can be shorter than the implicit row count (more
    //     items than declared rows). Trailing rows beyond the template fall
    //     back to natural max — the same auto-fallback that took row sizing
    //     before this commit.
    let template = tracks.unwrap_or(&[]);
    let n_rows = natural_row_heights.len();
    if n_rows == 0 {
        return Vec::new();
    }

    let mut sizes = vec![0.0_f32; n_rows];
    let mut total_fr = 0.0_f32;
    let mut fixed_total = 0.0_f32;
    for (i, &natural_h) in natural_row_heights.iter().enumerate() {
        if let Some(track) = template.get(i) {
            match track {
                TrackSize::Length(value, Unit::Px) => {
                    sizes[i] = *value;
                    fixed_total += *value;
                }
                TrackSize::Length(value, Unit::Percent) => {
                    let resolved = *value / 100.0 * container_height.unwrap_or(0.0);
                    sizes[i] = resolved;
                    fixed_total += resolved;
                }
                TrackSize::Length(value, _) => {
                    sizes[i] = *value;
                    fixed_total += *value;
                }
                TrackSize::Auto => {
                    sizes[i] = natural_h;
                    fixed_total += natural_h;
                }
                TrackSize::Fraction(weight) => {
                    total_fr += *weight;
                    // Filled in below if container_height is known.
                }
            }
        } else {
            sizes[i] = natural_h;
            fixed_total += natural_h;
        }
    }

    if total_fr > 0.0
        && let Some(container_h) = container_height
    {
        let free = (container_h - fixed_total).max(0.0);
        for (i, track) in template.iter().enumerate().take(n_rows) {
            if let TrackSize::Fraction(weight) = track {
                sizes[i] = free * *weight / total_fr;
            }
        }
    }

    sizes
}

fn resolve_grid_columns(
    tracks: Option<&[TrackSize]>,
    content_width: f32,
    natural_max_per_col: &[f32],
) -> Vec<f32> {
    // Resolves `grid-template-columns` to a Vec of pixel widths. Length and
    // Auto tracks contribute fixed budget; Fraction tracks split the leftover
    // proportionally to their weight, like flex-grow. With no declaration,
    // behave like a single full-width track so a bare `display: grid` still
    // produces sensible single-column output.
    let tracks = match tracks {
        Some(t) if !t.is_empty() => t,
        _ => return vec![content_width],
    };

    let mut fixed_total = 0.0_f32;
    let mut total_fr = 0.0_f32;
    for (i, track) in tracks.iter().enumerate() {
        match track {
            TrackSize::Length(value, Unit::Px) => fixed_total += *value,
            TrackSize::Length(value, Unit::Percent) => {
                fixed_total += *value / 100.0 * content_width;
            }
            // em/rem are resolved to Px during style; this fallback only
            // matters if a future code path bypasses style-time resolution.
            TrackSize::Length(value, _) => fixed_total += *value,
            TrackSize::Auto => fixed_total += natural_max_per_col.get(i).copied().unwrap_or(0.0),
            TrackSize::Fraction(weight) => total_fr += *weight,
        }
    }
    let free = (content_width - fixed_total).max(0.0);

    tracks
        .iter()
        .enumerate()
        .map(|(i, track)| match track {
            TrackSize::Length(value, Unit::Px) => *value,
            TrackSize::Length(value, Unit::Percent) => *value / 100.0 * content_width,
            TrackSize::Length(value, _) => *value,
            TrackSize::Auto => natural_max_per_col.get(i).copied().unwrap_or(0.0),
            TrackSize::Fraction(weight) if total_fr > 0.0 => free * *weight / total_fr,
            TrackSize::Fraction(_) => 0.0,
        })
        .collect()
}

fn flex_direction(node: &StyledNode) -> FlexDirection {
    match node.value("flex-direction") {
        Some(Value::Keyword(keyword)) if keyword == "column" => FlexDirection::Column,
        _ => FlexDirection::Row,
    }
}

fn justify_content(node: &StyledNode) -> JustifyContent {
    match node.value("justify-content") {
        Some(Value::Keyword(keyword)) if keyword == "center" => JustifyContent::Center,
        Some(Value::Keyword(keyword)) if keyword == "flex-end" => JustifyContent::FlexEnd,
        Some(Value::Keyword(keyword)) if keyword == "space-between" => {
            JustifyContent::SpaceBetween
        }
        _ => JustifyContent::FlexStart,
    }
}

fn layout_inline_children(
    children: &[StyledNode],
    content_x: f32,
    content_y: f32,
    content_width: f32,
    align: InlineAlign,
) -> (Vec<LayoutBox>, f32) {
    // First pass: pack children into lines using their measured widths so we can know
    // each line's total width before placing any box. The placement pass uses that
    // information to offset the line for non-left alignments. Percent-based widths on
    // inline children resolve against `content_width`, which is the parent's content box.
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut line_widths: Vec<f32> = Vec::new();
    let mut current_line: Vec<usize> = Vec::new();
    let mut current_width: f32 = 0.0;

    for (idx, child) in children.iter().enumerate() {
        // Absolute children are out of flow — they neither contribute to line
        // width nor cause line breaks. They get laid out separately below.
        if is_out_of_flow(child) {
            continue;
        }
        let child_w = inline_total_size(child, content_width).width;
        if !current_line.is_empty() && current_width + child_w > content_width {
            lines.push(std::mem::take(&mut current_line));
            line_widths.push(current_width);
            current_width = 0.0;
        }
        current_line.push(idx);
        current_width += child_w;
    }
    if !current_line.is_empty() {
        lines.push(current_line);
        line_widths.push(current_width);
    }

    // Second pass: place each line at its alignment-corrected offset. We read the
    // actual outer height from the laid-out box (rather than re-measuring) so
    // inline-block children, whose auto height is only known after their own
    // layout pass, contribute the right line height here.
    let mut boxes = Vec::new();
    let mut line_y = content_y;
    let mut max_bottom = content_y;

    for (line_idx, line_children) in lines.iter().enumerate() {
        let line_width = line_widths[line_idx];
        let line_offset = match align {
            InlineAlign::Left => 0.0,
            InlineAlign::Center => ((content_width - line_width) / 2.0).max(0.0),
            InlineAlign::Right => (content_width - line_width).max(0.0),
        };
        let mut line_x = content_x + line_offset;
        let mut line_height = 0.0f32;
        for &child_idx in line_children {
            let child = &children[child_idx];
            let child_box = layout_inline_or_inline_block(child, line_x, line_y, content_width);
            let outer = outer_rect(&child_box);
            line_x += outer.width;
            line_height = line_height.max(outer.height);
            boxes.push(child_box);
        }
        max_bottom = max_bottom.max(line_y + line_height);
        line_y += line_height;
    }

    // Lay out absolute children at the parent's content origin (their static
    // position approximation). Pass 2 will replace this with the offsets
    // resolved against their containing block.
    for child in children.iter().filter(|child| is_out_of_flow(child)) {
        let abs_box = layout_inline_or_inline_block(child, content_x, content_y, content_width);
        boxes.push(abs_box);
    }

    (boxes, max_bottom - content_y)
}

fn layout_inline_or_inline_block(
    node: &StyledNode,
    x: f32,
    y: f32,
    available_width: f32,
) -> LayoutBox {
    if is_inline_block(node) {
        layout_inline_block_node(node, x, y, available_width)
    } else {
        layout_inline_node(node, x, y, available_width)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InlineAlign {
    Left,
    Center,
    Right,
}

fn inline_align_for(node: &StyledNode) -> InlineAlign {
    match node.value("text-align") {
        Some(Value::Keyword(keyword)) if keyword == "center" => InlineAlign::Center,
        Some(Value::Keyword(keyword)) if keyword == "right" => InlineAlign::Right,
        _ => InlineAlign::Left,
    }
}

fn layout_inline_node(node: &StyledNode, x: f32, y: f32, parent_width: f32) -> LayoutBox {
    let margin = edge_sizes(node, "margin", parent_width);
    let padding = edge_sizes(node, "padding", parent_width);
    let border = edge_sizes(node, "border", parent_width);
    let content_width = inline_content_width(node, parent_width);
    let content_height = inline_content_height(node, parent_width);
    let content_x = x + margin.left + border.left + padding.left;
    let content_y = y + margin.top + border.top + padding.top;

    // Nested inline children are positioned relative to their inline parent's content box,
    // honoring text-align so labels inside an inline element (e.g. <a class="tile">) can
    // be centered instead of always sticking to the left edge.
    let children = if matches!(&node.node.node_type, NodeType::Element(element) if element.tag_name != "img")
    {
        let align = inline_align_for(node);
        layout_inline_sequence_no_wrap(&node.children, content_x, content_y, content_width, align)
    } else {
        Vec::new()
    };

    let mut layout_box = LayoutBox {
        box_type: BoxType::BlockNode(node.clone()),
        dimensions: Dimensions {
            content: Rect {
                x: content_x,
                y: content_y,
                width: content_width,
                height: content_height,
            },
            padding,
            border,
            margin,
        },
        children,
    };
    apply_relative_offset(&mut layout_box, node, parent_width);
    layout_box
}

fn layout_inline_sequence_no_wrap(
    children: &[StyledNode],
    content_x: f32,
    y: f32,
    content_width: f32,
    align: InlineAlign,
) -> Vec<LayoutBox> {
    // Sum widths first so we know how much horizontal slack the line has
    // before placing boxes. Absolute children sit out of flow so they don't
    // contribute to the line.
    let total_width: f32 = children
        .iter()
        .filter(|child| !is_out_of_flow(child))
        .map(|child| inline_total_size(child, content_width).width)
        .sum();
    let line_offset = match align {
        InlineAlign::Left => 0.0,
        InlineAlign::Center => ((content_width - total_width) / 2.0).max(0.0),
        InlineAlign::Right => (content_width - total_width).max(0.0),
    };

    let mut cursor_x = content_x + line_offset;
    let mut boxes = Vec::new();

    for child in children {
        if is_out_of_flow(child) {
            // Static position approximation: at the parent's content origin.
            // Pass 2 will move it once the containing block is known.
            let abs_box = layout_inline_or_inline_block(child, content_x, y, content_width);
            boxes.push(abs_box);
            continue;
        }
        let child_box = layout_inline_or_inline_block(child, cursor_x, y, content_width);
        cursor_x += outer_rect(&child_box).width;
        boxes.push(child_box);
    }

    boxes
}

fn uses_inline_flow(node: &StyledNode) -> bool {
    // Inline flow only kicks in when all children are inline-ish.
    // Mixed block/inline trees still fall back to the simpler vertical block algorithm.
    !node.children.is_empty() && node.children.iter().all(is_inline_node)
}

fn is_inline_node(node: &StyledNode) -> bool {
    match node.value("display") {
        Some(Value::Keyword(keyword)) if keyword == "block" => return false,
        // inline-block participates in inline flow (sits on a line) but is sized
        // and laid out internally like a block — that dispatch happens later.
        Some(Value::Keyword(keyword)) if keyword == "inline" || keyword == "inline-block" => {
            return true;
        }
        _ => {}
    }

    match &node.node.node_type {
        NodeType::Text(_) => true,
        // Keep the inline set small and predictable instead of trying to emulate full HTML layout.
        NodeType::Element(element) => matches!(element.tag_name.as_str(), "a" | "span" | "img"),
    }
}

fn is_inline_block(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(keyword)) if keyword == "inline-block")
}

fn inline_total_size(node: &StyledNode, parent_width: f32) -> Rect {
    if is_inline_block(node) {
        return inline_block_outer_size(node, parent_width);
    }
    let margin = edge_sizes(node, "margin", parent_width);
    let padding = edge_sizes(node, "padding", parent_width);
    let border = edge_sizes(node, "border", parent_width);
    let width = margin.left
        + border.left
        + padding.left
        + inline_content_width(node, parent_width)
        + padding.right
        + border.right
        + margin.right;
    let height = margin.top
        + border.top
        + padding.top
        + inline_content_height(node, parent_width)
        + padding.bottom
        + border.bottom
        + margin.bottom;

    Rect {
        x: 0.0,
        y: 0.0,
        width,
        height,
    }
}

fn inline_block_outer_size(node: &StyledNode, available_width: f32) -> Rect {
    // For line packing we only need an accurate width — height ends up being
    // re-read from the laid-out box, so we approximate it from explicit
    // height plus surrounding box edges.
    let margin = edge_sizes(node, "margin", available_width);
    let padding = edge_sizes(node, "padding", available_width);
    let border = edge_sizes(node, "border", available_width);
    let content_width = inline_block_resolved_width(node, available_width);
    let content_height = length_value(node, "height", available_width).unwrap_or(0.0);
    Rect {
        x: 0.0,
        y: 0.0,
        width: margin.left
            + border.left
            + padding.left
            + content_width
            + padding.right
            + border.right
            + margin.right,
        height: margin.top
            + border.top
            + padding.top
            + content_height
            + padding.bottom
            + border.bottom
            + margin.bottom,
    }
}

fn inline_block_resolved_width(node: &StyledNode, available_width: f32) -> f32 {
    length_value(node, "width", available_width)
        .or_else(|| intrinsic_width(node))
        .unwrap_or_else(|| inline_block_shrink_to_fit_width(node, available_width))
}

fn inline_block_shrink_to_fit_width(node: &StyledNode, available_width: f32) -> f32 {
    // Toy shrink-to-fit: text uses approximate glyph width; element uses the
    // sum of inline child widths, capped to the available content width so a
    // long run still wraps rather than overflowing the container.
    let natural = match &node.node.node_type {
        NodeType::Text(text) => text.chars().count() as f32 * inline_char_width(node),
        NodeType::Element(_) => node
            .children
            .iter()
            .map(|child| inline_total_size(child, available_width).width)
            .sum(),
    };
    natural.min(available_width)
}

fn layout_inline_block_node(node: &StyledNode, x: f32, y: f32, available_width: f32) -> LayoutBox {
    // Inline-block ignores `margin: auto` (auto only collapses for in-flow
    // blocks), so we just take the raw declared margins here.
    let margin = edge_sizes(node, "margin", available_width);
    let padding = edge_sizes(node, "padding", available_width);
    let border = edge_sizes(node, "border", available_width);
    let content_width = inline_block_resolved_width(node, available_width);

    let content_x = x + margin.left + border.left + padding.left;
    let content_y = y + margin.top + border.top + padding.top;

    // Same dispatch as the regular block path with extra branches for flex
    // and grid containers: dispatch to their respective placement algorithms
    // first; else if every child is inline, run inline flow; otherwise stack
    // block children top-to-bottom inside our content box.
    let (children, auto_content_height) = if is_flex_container(node) {
        layout_flex_children(node, &node.children, content_x, content_y, content_width)
    } else if is_grid_container(node) {
        layout_grid_children(node, &node.children, content_x, content_y, content_width)
    } else if uses_inline_flow(node) {
        let align = inline_align_for(node);
        layout_inline_children(&node.children, content_x, content_y, content_width, align)
    } else {
        let mut child_cursor_y = content_y;
        let children = node
            .children
            .iter()
            .map(|child| layout_node(child, content_x, &mut child_cursor_y, content_width))
            .collect::<Vec<_>>();
        (children, child_height(node, content_y, child_cursor_y))
    };

    let content_height = length_value(node, "height", available_width)
        .unwrap_or_else(|| auto_content_height.max(intrinsic_height(node)));

    let mut layout_box = LayoutBox {
        box_type: container_box_type(node),
        dimensions: Dimensions {
            content: Rect {
                x: content_x,
                y: content_y,
                width: content_width,
                height: content_height,
            },
            padding,
            border,
            margin,
        },
        children,
    };
    apply_relative_offset(&mut layout_box, node, available_width);
    layout_box
}

fn inline_content_width(node: &StyledNode, parent_width: f32) -> f32 {
    // Text width is approximated from character count because this toy renderer does not do
    // real font shaping or glyph measurement.
    length_value(node, "width", parent_width)
        .or_else(|| intrinsic_width(node))
        .unwrap_or_else(|| match &node.node.node_type {
            NodeType::Text(text) => text.chars().count() as f32 * inline_char_width(node),
            NodeType::Element(element) if element.tag_name == "img" => 200.0,
            NodeType::Element(_) => node
                .children
                .iter()
                .map(|child| inline_total_size(child, parent_width).width)
                .sum(),
        })
}

fn inline_content_height(node: &StyledNode, parent_width: f32) -> f32 {
    // Same caveat as block height: percent on inline height should reference the parent's
    // height, but we only have parent_width on hand. Toy pages have not exercised this yet.
    length_value(node, "height", parent_width).unwrap_or_else(|| match &node.node.node_type {
        // Text contributes its line-height (not just glyph height) so a parent
        // line box stretches to fit `line-height: 1.5` even when no descendant
        // declares an explicit height.
        NodeType::Text(_) => inline_line_height_px(node),
        NodeType::Element(element) if element.tag_name == "img" => intrinsic_height(node),
        NodeType::Element(_) => node
            .children
            .iter()
            .map(|child| inline_total_size(child, parent_width).height)
            .fold(0.0, f32::max)
            .max(intrinsic_height(node)),
    })
}

fn inline_font_size(node: &StyledNode) -> f32 {
    // font-size is always Px after the style pass, so the percent base is irrelevant here.
    length_value(node, "font-size", 0.0).unwrap_or(16.0)
}

pub(crate) fn inline_line_height_px(node: &StyledNode) -> f32 {
    // CSS `line-height` resolves against the element's *own* font-size:
    // - <number>: bare multiplier (inherits as the number, applied per element)
    // - <length>: absolute (em/rem already converted to Px during style)
    // - <percent>: applied to this node's font-size
    // - keyword `normal` / unset: identity (= font-size); skip extra leading
    let font_size = inline_font_size(node);
    match node.value("line-height") {
        Some(Value::Number(multiplier)) => font_size * multiplier,
        Some(Value::Length(value, Unit::Px)) => *value,
        Some(Value::Length(value, Unit::Percent)) => font_size * value / 100.0,
        Some(Value::Length(value, _)) => *value,
        _ => font_size,
    }
}

fn inline_char_width(node: &StyledNode) -> f32 {
    inline_font_size(node) * 0.75
}

fn child_height(node: &StyledNode, content_y: f32, child_cursor_y: f32) -> f32 {
    if matches!(node.node.node_type, NodeType::Text(_)) {
        0.0
    } else {
        child_cursor_y - content_y
    }
}

fn intrinsic_width(node: &StyledNode) -> Option<f32> {
    match &node.node.node_type {
        // Images need a visible box even when no author CSS width is provided.
        NodeType::Element(element) if element.tag_name == "img" => {
            attribute_length(element, "width").or(Some(200.0))
        }
        _ => None,
    }
}

fn intrinsic_height(node: &StyledNode) -> f32 {
    match &node.node.node_type {
        // font-size is always Px after the style pass, so the percent base is irrelevant.
        NodeType::Text(_) => length_value(node, "font-size", 0.0).unwrap_or(16.0),
        // Images also get a default height so the renderer has an area to paint into.
        NodeType::Element(element) if element.tag_name == "img" => {
            attribute_length(element, "height").unwrap_or(150.0)
        }
        NodeType::Element(_) => 0.0,
    }
}

fn edge_sizes(node: &StyledNode, prefix: &str, base: f32) -> EdgeSizes {
    // CSS resolves percent margin/padding against the containing block's *width*, even
    // for the top and bottom sides — a common gotcha worth keeping in mind here.
    EdgeSizes {
        left: length_value(node, &format!("{prefix}-left"), base).unwrap_or(0.0),
        right: length_value(node, &format!("{prefix}-right"), base).unwrap_or(0.0),
        top: length_value(node, &format!("{prefix}-top"), base).unwrap_or(0.0),
        bottom: length_value(node, &format!("{prefix}-bottom"), base).unwrap_or(0.0),
    }
}

fn length_value(node: &StyledNode, name: &str, base: f32) -> Option<f32> {
    // `base` is the containing-block dimension a Percent length resolves against. For
    // properties that should never see a percent (font-size after style resolution, etc.)
    // callers can safely pass any value.
    match node.value(name) {
        Some(Value::Length(value, Unit::Px)) => Some(*value),
        Some(Value::Length(value, Unit::Percent)) => Some(*value / 100.0 * base),
        _ => None,
    }
}

fn is_auto(node: &StyledNode, name: &str) -> bool {
    matches!(node.value(name), Some(Value::Keyword(keyword)) if keyword == "auto")
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

fn is_position_relative(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "relative")
}

fn is_float_left(node: &StyledNode) -> bool {
    matches!(node.value("float"), Some(Value::Keyword(keyword)) if keyword == "left")
}

fn is_float_right(node: &StyledNode) -> bool {
    matches!(node.value("float"), Some(Value::Keyword(keyword)) if keyword == "right")
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

fn is_position_absolute(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "absolute")
}

fn is_position_fixed(node: &StyledNode) -> bool {
    matches!(node.value("position"), Some(Value::Keyword(keyword)) if keyword == "fixed")
}

fn is_out_of_flow(node: &StyledNode) -> bool {
    // Both `absolute` and `fixed` skip in-flow placement during pass 1; they
    // differ only in which containing block pass 2 resolves them against.
    is_position_absolute(node) || is_position_fixed(node)
}

fn relative_offset(node: &StyledNode, base: f32) -> Option<(f32, f32)> {
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

fn apply_relative_offset(layout_box: &mut LayoutBox, node: &StyledNode, base: f32) {
    if let Some((dx, dy)) = relative_offset(node, base) {
        shift_layout_subtree(layout_box, dx, dy);
    }
}

fn shift_layout_subtree(layout_box: &mut LayoutBox, dx: f32, dy: f32) {
    // Relative positioning shifts the visual rect of the box and *every*
    // descendant — siblings and cursors keep using the unshifted geometry, so
    // we only mutate this subtree.
    layout_box.dimensions.content.x += dx;
    layout_box.dimensions.content.y += dy;
    for child in &mut layout_box.children {
        shift_layout_subtree(child, dx, dy);
    }
}

fn attribute_length(element: &ElementData, name: &str) -> Option<f32> {
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
        let node = html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = css::parse(css_source).unwrap();
        style::style_tree(&node, &[stylesheet])
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
}
