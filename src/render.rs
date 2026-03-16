use crate::{
    css::{Color, Unit, Value},
    dom::NodeType,
    layout::{Dimensions, LayoutBox, Rect},
};

#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCommand {
    SolidRect(Color, Rect),
    Text(TextCommand),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextCommand {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub color: Color,
    pub font_size: f32,
}

pub fn build_display_list(layout_root: &LayoutBox) -> Vec<DisplayCommand> {
    let mut commands = Vec::new();
    paint_layout_box(layout_root, &mut commands);
    commands
}

pub fn rasterize(commands: &[DisplayCommand], width: usize, height: usize) -> Vec<u32> {
    let mut buffer = vec![rgb_u32(Color::WHITE); width * height];

    for command in commands {
        match command {
            DisplayCommand::SolidRect(color, rect) => {
                fill_rect(&mut buffer, width, height, *color, *rect)
            }
            DisplayCommand::Text(text) => draw_text(&mut buffer, width, height, text),
        }
    }

    buffer
}

fn paint_layout_box(layout_box: &LayoutBox, commands: &mut Vec<DisplayCommand>) {
    if let Some(command) = background_command(layout_box) {
        commands.push(command);
    }

    if let Some(command) = text_command(layout_box) {
        commands.push(command);
    }

    for child in &layout_box.children {
        paint_layout_box(child, commands);
    }
}

fn background_command(layout_box: &LayoutBox) -> Option<DisplayCommand> {
    let node = layout_box.styled_node()?;
    let color = match node.value("background-color") {
        Some(Value::Color(color)) => *color,
        _ => return None,
    };

    Some(DisplayCommand::SolidRect(
        color,
        layout_box.dimensions.padding_box(),
    ))
}

fn text_command(layout_box: &LayoutBox) -> Option<DisplayCommand> {
    let node = layout_box.styled_node()?;
    let text = match &node.node.node_type {
        NodeType::Text(text) => text.clone(),
        NodeType::Element(_) => return None,
    };

    Some(DisplayCommand::Text(TextCommand {
        text,
        x: layout_box.dimensions.content.x,
        y: layout_box.dimensions.content.y,
        color: text_color(node),
        font_size: font_size(node),
    }))
}

fn text_color(node: &crate::style::StyledNode) -> Color {
    match node.value("color") {
        Some(Value::Color(color)) => *color,
        _ => Color::BLACK,
    }
}

fn font_size(node: &crate::style::StyledNode) -> f32 {
    match node.value("font-size") {
        Some(Value::Length(value, Unit::Px)) => *value,
        _ => 16.0,
    }
}

impl LayoutBox {
    fn styled_node(&self) -> Option<&crate::style::StyledNode> {
        match &self.box_type {
            crate::layout::BoxType::BlockNode(node) => Some(node),
            crate::layout::BoxType::AnonymousBlock => None,
        }
    }
}

impl Dimensions {
    fn padding_box(&self) -> Rect {
        Rect {
            x: self.content.x - self.padding.left,
            y: self.content.y - self.padding.top,
            width: self.content.width + self.padding.left + self.padding.right,
            height: self.content.height + self.padding.top + self.padding.bottom,
        }
    }
}

impl Color {
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };

    pub const BLACK: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
}

fn fill_rect(buffer: &mut [u32], width: usize, height: usize, color: Color, rect: Rect) {
    let x_start = rect.x.max(0.0).floor() as usize;
    let y_start = rect.y.max(0.0).floor() as usize;
    let x_end = (rect.x + rect.width).ceil().max(0.0) as usize;
    let y_end = (rect.y + rect.height).ceil().max(0.0) as usize;
    let x_end = x_end.min(width);
    let y_end = y_end.min(height);
    let pixel = rgb_u32(color);

    for y in y_start..y_end {
        let row = y * width;
        for x in x_start..x_end {
            buffer[row + x] = pixel;
        }
    }
}

fn draw_text(buffer: &mut [u32], width: usize, height: usize, text: &TextCommand) {
    let scale = (text.font_size / 8.0).max(1.0).round() as usize;
    let mut cursor_x = text.x.round() as i32;
    let baseline_y = text.y.round() as i32;

    for ch in text.text.chars() {
        if ch == ' ' {
            cursor_x += (4 * scale) as i32;
            continue;
        }

        let glyph = glyph_pattern(ch);
        for (row_index, row) in glyph.iter().enumerate() {
            for (column_index, pixel) in row.chars().enumerate() {
                if pixel == ' ' {
                    continue;
                }

                let x = cursor_x + (column_index * scale) as i32;
                let y = baseline_y + (row_index * scale) as i32;
                fill_rect(
                    buffer,
                    width,
                    height,
                    text.color,
                    Rect {
                        x: x as f32,
                        y: y as f32,
                        width: scale as f32,
                        height: scale as f32,
                    },
                );
            }
        }

        cursor_x += (6 * scale) as i32;
    }
}

fn rgb_u32(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

fn glyph_pattern(ch: char) -> [&'static str; 7] {
    match ch.to_ascii_lowercase() {
        'a' => [
            " ### ", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'b' => [
            "#### ", "#   #", "#   #", "#### ", "#   #", "#   #", "#### ",
        ],
        'c' => [
            " ####", "#    ", "#    ", "#    ", "#    ", "#    ", " ####",
        ],
        'd' => [
            "#### ", "#   #", "#   #", "#   #", "#   #", "#   #", "#### ",
        ],
        'e' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####",
        ],
        'f' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#    ",
        ],
        'g' => [
            " ####", "#    ", "#    ", "#  ##", "#   #", "#   #", " ####",
        ],
        'h' => [
            "#   #", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'i' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "#####",
        ],
        'j' => [
            "#####", "   # ", "   # ", "   # ", "   # ", "#  # ", " ##  ",
        ],
        'k' => [
            "#   #", "#  # ", "# #  ", "##   ", "# #  ", "#  # ", "#   #",
        ],
        'l' => [
            "#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####",
        ],
        'm' => [
            "#   #", "## ##", "# # #", "#   #", "#   #", "#   #", "#   #",
        ],
        'n' => [
            "#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #",
        ],
        'o' => [
            " ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'p' => [
            "#### ", "#   #", "#   #", "#### ", "#    ", "#    ", "#    ",
        ],
        'q' => [
            " ### ", "#   #", "#   #", "#   #", "# # #", "#  # ", " ## #",
        ],
        'r' => [
            "#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #",
        ],
        's' => [
            " ####", "#    ", "#    ", " ### ", "    #", "    #", "#### ",
        ],
        't' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        'u' => [
            "#   #", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'v' => [
            "#   #", "#   #", "#   #", "#   #", "#   #", " # # ", "  #  ",
        ],
        'w' => [
            "#   #", "#   #", "#   #", "# # #", "# # #", "## ##", "#   #",
        ],
        'x' => [
            "#   #", "#   #", " # # ", "  #  ", " # # ", "#   #", "#   #",
        ],
        'y' => [
            "#   #", "#   #", " # # ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        'z' => [
            "#####", "    #", "   # ", "  #  ", " #   ", "#    ", "#####",
        ],
        '.' => [
            "     ", "     ", "     ", "     ", "     ", " ### ", " ### ",
        ],
        '!' => [
            " ### ", " ### ", " ### ", " ### ", " ### ", "     ", " ### ",
        ],
        '?' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", "     ", "  #  ",
        ],
        '-' => [
            "     ", "     ", "     ", "#####", "     ", "     ", "     ",
        ],
        _ => [
            "#####", "#   #", "   # ", "  #  ", "  #  ", "     ", "  #  ",
        ],
    }
}

#[cfg(test)]
mod tests {
    use crate::{css, html, layout, render, style};

    use super::{Color, DisplayCommand, TextCommand, rasterize};

    fn display_list(html_source: &str, css_source: &str) -> Vec<DisplayCommand> {
        let node = html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = css::parse(css_source).unwrap();
        let styled = style::style_tree(&node, &[stylesheet]);
        let layout = layout::layout_tree(&styled, 400.0);
        render::build_display_list(&layout)
    }

    #[test]
    fn paints_background_rect_from_padding_box() {
        let commands = display_list(
            r#"<div id="card"></div>"#,
            r#"
                #card {
                    width: 100px;
                    height: 40px;
                    padding-left: 5px;
                    padding-right: 7px;
                    padding-top: 3px;
                    padding-bottom: 9px;
                    background-color: #336699;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![DisplayCommand::SolidRect(
                Color {
                    r: 51,
                    g: 102,
                    b: 153,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 112.0,
                    height: 52.0,
                }
            )]
        );
    }

    #[test]
    fn paints_text_nodes_with_inherited_style() {
        let commands = display_list(
            r#"<p class="copy">Hello</p>"#,
            r#"
                .copy {
                    color: #0f0;
                    font-size: 18px;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![DisplayCommand::Text(TextCommand {
                text: "Hello".into(),
                x: 0.0,
                y: 0.0,
                color: Color {
                    r: 0,
                    g: 255,
                    b: 0,
                    a: 255,
                },
                font_size: 18.0,
            })]
        );
    }

    #[test]
    fn paints_rect_before_descendant_text() {
        let commands = display_list(
            r#"<div id="card"><p>Hello</p></div>"#,
            r#"
                #card {
                    background-color: #111111;
                }

                p {
                    font-size: 20px;
                }
            "#,
        );

        assert!(matches!(commands[0], DisplayCommand::SolidRect(_, _)));
        assert!(matches!(commands[1], DisplayCommand::Text(_)));
    }

    #[test]
    fn rasterizes_background_pixels() {
        let pixels = rasterize(
            &[DisplayCommand::SolidRect(
                Color {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255,
                },
                crate::layout::Rect {
                    x: 1.0,
                    y: 1.0,
                    width: 2.0,
                    height: 2.0,
                },
            )],
            4,
            4,
        );

        assert_eq!(pixels[5], 0xFF0000);
        assert_eq!(pixels[10], 0xFF0000);
        assert_eq!(pixels[0], 0xFFFFFF);
    }
}
