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
    layout_node(root, 0.0, &mut cursor_y, viewport_width)
}

fn layout_node(
    node: &StyledNode,
    parent_x: f32,
    cursor_y: &mut f32,
    parent_width: f32,
) -> LayoutBox {
    let raw_margin = edge_sizes(node, "margin");
    let padding = edge_sizes(node, "padding");
    let border = edge_sizes(node, "border");

    // CSS auto-margin centering only applies when a width is specified.
    let explicit_width = length_value(node, "width").or_else(|| intrinsic_width(node));
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
        let mut child_cursor_y = content_y;
        let children = node
            .children
            .iter()
            .map(|child| layout_node(child, content_x, &mut child_cursor_y, content_width))
            .collect::<Vec<_>>();
        (children, child_height(node, content_y, child_cursor_y))
    };

    let content_height = length_value(node, "height")
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

    *cursor_y = content_y + content_height + padding.bottom + border.bottom + margin.bottom;

    LayoutBox {
        box_type: BoxType::BlockNode(node.clone()),
        dimensions,
        children,
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
    // information to offset the line for non-left alignments.
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut line_widths: Vec<f32> = Vec::new();
    let mut current_line: Vec<usize> = Vec::new();
    let mut current_width: f32 = 0.0;

    for (idx, child) in children.iter().enumerate() {
        let child_w = inline_total_size(child).width;
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

    // Second pass: place each line at its alignment-corrected offset.
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
            let child_size = inline_total_size(child);
            let child_box = layout_inline_node(child, line_x, line_y);
            line_x += child_size.width;
            line_height = line_height.max(child_size.height);
            boxes.push(child_box);
        }
        max_bottom = max_bottom.max(line_y + line_height);
        line_y += line_height;
    }

    (boxes, max_bottom - content_y)
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

fn layout_inline_node(node: &StyledNode, x: f32, y: f32) -> LayoutBox {
    let margin = edge_sizes(node, "margin");
    let padding = edge_sizes(node, "padding");
    let border = edge_sizes(node, "border");
    let content_width = inline_content_width(node);
    let content_height = inline_content_height(node);
    let content_x = x + margin.left + border.left + padding.left;
    let content_y = y + margin.top + border.top + padding.top;

    // Nested inline children are positioned relative to their inline parent's content box.
    let children = if matches!(&node.node.node_type, NodeType::Element(element) if element.tag_name != "img")
    {
        layout_inline_sequence_no_wrap(&node.children, content_x, content_y)
    } else {
        Vec::new()
    };

    LayoutBox {
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
    }
}

fn layout_inline_sequence_no_wrap(children: &[StyledNode], x: f32, y: f32) -> Vec<LayoutBox> {
    let mut cursor_x = x;
    let mut boxes = Vec::new();

    for child in children {
        let child_box = layout_inline_node(child, cursor_x, y);
        cursor_x += inline_total_size(child).width;
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
        Some(Value::Keyword(keyword)) if keyword == "inline" => return true,
        _ => {}
    }

    match &node.node.node_type {
        NodeType::Text(_) => true,
        // Keep the inline set small and predictable instead of trying to emulate full HTML layout.
        NodeType::Element(element) => matches!(element.tag_name.as_str(), "a" | "span" | "img"),
    }
}

fn inline_total_size(node: &StyledNode) -> Rect {
    let margin = edge_sizes(node, "margin");
    let padding = edge_sizes(node, "padding");
    let border = edge_sizes(node, "border");
    let width = margin.left
        + border.left
        + padding.left
        + inline_content_width(node)
        + padding.right
        + border.right
        + margin.right;
    let height = margin.top
        + border.top
        + padding.top
        + inline_content_height(node)
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

fn inline_content_width(node: &StyledNode) -> f32 {
    // Text width is approximated from character count because this toy renderer does not do
    // real font shaping or glyph measurement.
    length_value(node, "width")
        .or_else(|| intrinsic_width(node))
        .unwrap_or_else(|| match &node.node.node_type {
            NodeType::Text(text) => text.chars().count() as f32 * inline_char_width(node),
            NodeType::Element(element) if element.tag_name == "img" => 200.0,
            NodeType::Element(_) => node
                .children
                .iter()
                .map(|child| inline_total_size(child).width)
                .sum(),
        })
}

fn inline_content_height(node: &StyledNode) -> f32 {
    length_value(node, "height").unwrap_or_else(|| match &node.node.node_type {
        NodeType::Text(_) => inline_font_size(node),
        NodeType::Element(element) if element.tag_name == "img" => intrinsic_height(node),
        NodeType::Element(_) => node
            .children
            .iter()
            .map(|child| inline_total_size(child).height)
            .fold(0.0, f32::max)
            .max(intrinsic_height(node)),
    })
}

fn inline_font_size(node: &StyledNode) -> f32 {
    length_value(node, "font-size").unwrap_or(16.0)
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
        NodeType::Text(_) => length_value(node, "font-size").unwrap_or(16.0),
        // Images also get a default height so the renderer has an area to paint into.
        NodeType::Element(element) if element.tag_name == "img" => {
            attribute_length(element, "height").unwrap_or(150.0)
        }
        NodeType::Element(_) => 0.0,
    }
}

fn edge_sizes(node: &StyledNode, prefix: &str) -> EdgeSizes {
    EdgeSizes {
        left: length_value(node, &format!("{prefix}-left")).unwrap_or(0.0),
        right: length_value(node, &format!("{prefix}-right")).unwrap_or(0.0),
        top: length_value(node, &format!("{prefix}-top")).unwrap_or(0.0),
        bottom: length_value(node, &format!("{prefix}-bottom")).unwrap_or(0.0),
    }
}

fn length_value(node: &StyledNode, name: &str) -> Option<f32> {
    match node.value(name) {
        Some(Value::Length(value, Unit::Px)) => Some(*value),
        _ => None,
    }
}

fn is_auto(node: &StyledNode, name: &str) -> bool {
    matches!(node.value(name), Some(Value::Keyword(keyword)) if keyword == "auto")
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
        assert_eq!(second.dimensions.content.y, 37.0);
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
