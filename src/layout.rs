use crate::{
    css::{Unit, Value},
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
        BoxType::BlockNode(node) => Some(node),
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

    // Parents with only inline children lay them out left-to-right; everything else stays block.
    let (children, auto_content_height) = if uses_inline_flow(node) {
        let align = inline_align_for(node);
        layout_inline_children(&node.children, content_x, content_y, content_width, align)
    } else {
        // Block flow: stack children top-to-bottom while collapsing the
        // previous in-flow child's margin-bottom against the next child's
        // margin-top. Out-of-flow children skip both the cursor advance and
        // the collapse chain — they neither push siblings down nor break
        // adjacency between the in-flow neighbours that surround them.
        let mut child_cursor_y = content_y;
        let mut prev_margin_bottom: f32 = 0.0;
        let mut children: Vec<LayoutBox> = Vec::with_capacity(node.children.len());
        for child in &node.children {
            if is_out_of_flow(child) {
                let mut frozen = child_cursor_y;
                children.push(layout_node(child, content_x, &mut frozen, content_width));
                continue;
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
        (children, child_height(node, content_y, child_cursor_y))
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
        box_type: BoxType::BlockNode(node.clone()),
        dimensions,
        children,
    };
    apply_relative_offset(&mut layout_box, node, parent_width);
    layout_box
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

    // Same dispatch as the regular block path: if every child is inline, run
    // the inline flow; otherwise stack block children top-to-bottom inside our
    // content box.
    let (children, auto_content_height) = if uses_inline_flow(node) {
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
        NodeType::Text(_) => inline_font_size(node),
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
}
