// Inline / inline-block flow. Inline atoms are laid out left-to-right with
// optional shrink-to-fit width; inline-block boxes get full block layout
// inside but participate in the surrounding inline run.

use crate::{
    css::{Unit, Value},
    dom::NodeType,
    style::StyledNode,
};

use super::{
    BoxType, Dimensions, LayoutBox, Rect, apply_relative_offset, child_height,
    container_box_type, edge_sizes, intrinsic_height, intrinsic_width, is_display_none,
    is_out_of_flow, length_value, outer_rect,
};
use super::block::layout_node;
use super::flex::{is_flex_container, layout_flex_children};
use super::grid::{is_grid_container, layout_grid_children};

pub(super) fn layout_inline_children(
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
        // `display: none` removes the child entirely — no width contribution,
        // no slot in the second-pass placement. Skipped before the out-of-flow
        // check so a hidden absolute also drops out.
        if is_display_none(child) {
            continue;
        }
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
    for child in children
        .iter()
        .filter(|child| is_out_of_flow(child) && !is_display_none(child))
    {
        let abs_box = layout_inline_or_inline_block(child, content_x, content_y, content_width);
        boxes.push(abs_box);
    }

    (boxes, max_bottom - content_y)
}

pub(super) fn layout_inline_or_inline_block(
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
pub(super) enum InlineAlign {
    Left,
    Center,
    Right,
}

pub(super) fn inline_align_for(node: &StyledNode) -> InlineAlign {
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
    let children = if matches!(&node.node_type, NodeType::Element(element) if element.tag_name != "img")
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
    // contribute to the line, and `display: none` children don't exist as
    // far as layout is concerned.
    let total_width: f32 = children
        .iter()
        .filter(|child| !is_out_of_flow(child) && !is_display_none(child))
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
        if is_display_none(child) {
            continue;
        }
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

pub(super) fn uses_inline_flow(node: &StyledNode) -> bool {
    // Inline flow only kicks in when all visible children are inline-ish.
    // `display: none` siblings are invisible to flow detection — a hidden
    // <script> alongside inline text should not flip the parent into block
    // flow. Mixed block/inline trees still fall back to the simpler
    // vertical block algorithm.
    let mut visible = node
        .children
        .iter()
        .filter(|child| !is_display_none(child))
        .peekable();
    visible.peek().is_some() && visible.all(is_inline_node)
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

    match &node.node_type {
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
    let natural = match &node.node_type {
        NodeType::Text(text) => text.chars().count() as f32 * inline_char_width(node),
        NodeType::Element(_) => node
            .children
            .iter()
            .map(|child| inline_total_size(child, available_width).width)
            .sum(),
    };
    natural.min(available_width)
}

pub(super) fn layout_inline_block_node(node: &StyledNode, x: f32, y: f32, available_width: f32) -> LayoutBox {
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
        .unwrap_or_else(|| match &node.node_type {
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
    length_value(node, "height", parent_width).unwrap_or_else(|| match &node.node_type {
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
