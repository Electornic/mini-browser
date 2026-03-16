use crate::{
    css::{Unit, Value},
    dom::{ElementData, NodeType},
    style::StyledNode,
};

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
    let margin = edge_sizes(node, "margin");
    let padding = edge_sizes(node, "padding");
    let border = EdgeSizes::default();

    let horizontal_non_content =
        margin.left + margin.right + padding.left + padding.right + border.left + border.right;
    let content_width = length_value(node, "width")
        .or_else(|| intrinsic_width(node))
        .unwrap_or((parent_width - horizontal_non_content).max(0.0));
    let content_x = parent_x + margin.left + border.left + padding.left;
    let content_y = *cursor_y + margin.top + border.top + padding.top;

    let mut child_cursor_y = content_y;
    let children = node
        .children
        .iter()
        .map(|child| layout_node(child, content_x, &mut child_cursor_y, content_width))
        .collect::<Vec<_>>();

    let content_height = length_value(node, "height").unwrap_or_else(|| {
        child_height(node, content_y, child_cursor_y).max(intrinsic_height(node))
    });

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

fn child_height(node: &StyledNode, content_y: f32, child_cursor_y: f32) -> f32 {
    if matches!(node.node.node_type, NodeType::Text(_)) {
        0.0
    } else {
        child_cursor_y - content_y
    }
}

fn intrinsic_width(node: &StyledNode) -> Option<f32> {
    match &node.node.node_type {
        NodeType::Element(element) if element.tag_name == "img" => {
            attribute_length(element, "width").or(Some(200.0))
        }
        _ => None,
    }
}

fn intrinsic_height(node: &StyledNode) -> f32 {
    match &node.node.node_type {
        NodeType::Text(_) => length_value(node, "font-size").unwrap_or(16.0),
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
}
