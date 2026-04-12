use crate::{
    css::{Color, Unit, Value},
    dom::NodeType,
    layout::{Dimensions, LayoutBox, Rect},
};

// Rendering is two-stage: layout boxes become display commands, then commands rasterize to pixels.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCommand {
    SolidRect(Color, Rect),
    Text(TextCommand),
    Image(ImageCommand),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextCommand {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub color: Color,
    pub font_size: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ImageCommand {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub source_width: usize,
    pub source_height: usize,
    pub pixels: Vec<u32>,
}

pub fn build_display_list(layout_root: &LayoutBox) -> Vec<DisplayCommand> {
    let mut commands = Vec::new();
    paint_layout_box(layout_root, &mut commands);
    commands
}

// Chrome UI and scrolling both reuse this helper to move already-built commands around.
pub fn translate(mut commands: Vec<DisplayCommand>, dx: f32, dy: f32) -> Vec<DisplayCommand> {
    for command in &mut commands {
        match command {
            DisplayCommand::SolidRect(_, rect) => {
                rect.x += dx;
                rect.y += dy;
            }
            DisplayCommand::Text(text) => {
                text.x += dx;
                text.y += dy;
            }
            DisplayCommand::Image(image) => {
                image.x += dx;
                image.y += dy;
            }
        }
    }

    commands
}

pub fn rasterize(
    commands: &[DisplayCommand],
    width: usize,
    height: usize,
    fonts: &[fontdue::Font],
) -> Vec<u32> {
    let mut buffer = vec![rgb_u32(Color::WHITE); width * height];

    for command in commands {
        match command {
            DisplayCommand::SolidRect(color, rect) => {
                fill_rect(&mut buffer, width, height, *color, *rect)
            }
            DisplayCommand::Text(text) => draw_text(&mut buffer, width, height, text, fonts),
            DisplayCommand::Image(image) => draw_image(&mut buffer, width, height, image),
        }
    }

    buffer
}

fn paint_layout_box(layout_box: &LayoutBox, commands: &mut Vec<DisplayCommand>) {
    // The paint order is background -> border -> content so children appear on top.
    if let Some(command) = background_command(layout_box) {
        commands.push(command);
    }

    commands.extend(border_commands(layout_box));

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

fn border_commands(layout_box: &LayoutBox) -> Vec<DisplayCommand> {
    let node = match layout_box.styled_node() {
        Some(node) => node,
        None => return Vec::new(),
    };
    let color = match node.value("border-color") {
        Some(Value::Color(color)) => *color,
        _ => return Vec::new(),
    };
    let border = layout_box.dimensions.border;
    if border.left == 0.0 && border.right == 0.0 && border.top == 0.0 && border.bottom == 0.0 {
        return Vec::new();
    }

    let border_box = layout_box.dimensions.border_box();
    let mut commands = Vec::new();

    if border.top > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x,
                y: border_box.y,
                width: border_box.width,
                height: border.top,
            },
        ));
    }

    if border.bottom > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x,
                y: border_box.y + border_box.height - border.bottom,
                width: border_box.width,
                height: border.bottom,
            },
        ));
    }

    if border.left > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x,
                y: border_box.y,
                width: border.left,
                height: border_box.height,
            },
        ));
    }

    if border.right > 0.0 {
        commands.push(DisplayCommand::SolidRect(
            color,
            Rect {
                x: border_box.x + border_box.width - border.right,
                y: border_box.y,
                width: border.right,
                height: border_box.height,
            },
        ));
    }

    commands
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

    fn border_box(&self) -> Rect {
        let padding_box = self.padding_box();
        Rect {
            x: padding_box.x - self.border.left,
            y: padding_box.y - self.border.top,
            width: padding_box.width + self.border.left + self.border.right,
            height: padding_box.height + self.border.top + self.border.bottom,
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

fn draw_text(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    text: &TextCommand,
    fonts: &[fontdue::Font],
) {
    if fonts.is_empty() {
        draw_text_bitmap(buffer, width, height, text);
        return;
    }

    let font_size = text.font_size.max(8.0);
    let ascent = fonts[0]
        .horizontal_line_metrics(font_size)
        .map(|m| m.ascent)
        .unwrap_or(font_size * 0.8);
    let mut cursor_x = text.x;

    for ch in text.text.chars() {
        // Find a font that contains this glyph.
        let font_match = fonts
            .iter()
            .find(|f| f.lookup_glyph_index(ch) != 0 || ch == ' ');

        let Some(font) = font_match else {
            // No font has this glyph — use the bitmap fallback for this character.
            draw_bitmap_char(buffer, width, height, ch, cursor_x, text.y, text.color, text.font_size);
            cursor_x += text.font_size * 0.75;
            continue;
        };

        let (metrics, bitmap) = font.rasterize(ch, font_size);
        let glyph_y = text.y + ascent - metrics.height as f32 - metrics.ymin as f32;

        for row in 0..metrics.height {
            for col in 0..metrics.width {
                let alpha = bitmap[row * metrics.width + col];
                if alpha == 0 {
                    continue;
                }

                let px = (cursor_x + metrics.xmin as f32 + col as f32).round() as i32;
                let py = (glyph_y + row as f32).round() as i32;

                if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                    continue;
                }

                let idx = py as usize * width + px as usize;
                if alpha >= 128 {
                    buffer[idx] = rgb_u32(text.color);
                } else if alpha >= 32 {
                    // Simple alpha blend for anti-aliased edges.
                    let bg = buffer[idx];
                    let a = alpha as u32;
                    let inv = 255 - a;
                    let r = (a * text.color.r as u32 + inv * ((bg >> 16) & 0xFF)) / 255;
                    let g = (a * text.color.g as u32 + inv * ((bg >> 8) & 0xFF)) / 255;
                    let b = (a * text.color.b as u32 + inv * (bg & 0xFF)) / 255;
                    buffer[idx] = (r << 16) | (g << 8) | b;
                }
            }
        }

        cursor_x += metrics.advance_width;
    }
}

fn draw_text_bitmap(buffer: &mut [u32], width: usize, height: usize, text: &TextCommand) {
    let mut cursor_x = text.x;

    for ch in text.text.chars() {
        draw_bitmap_char(buffer, width, height, ch, cursor_x, text.y, text.color, text.font_size);
        let scale = (text.font_size / 8.0).max(1.0).round();
        cursor_x += if ch == ' ' { 4.0 * scale } else { 6.0 * scale };
    }
}

fn draw_bitmap_char(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    ch: char,
    x: f32,
    y: f32,
    color: Color,
    font_size: f32,
) {
    let scale = (font_size / 8.0).max(1.0).round() as usize;
    let cursor_x = x.round() as i32;
    let baseline_y = y.round() as i32;

    if ch == ' ' {
        return;
    }

    let glyph = glyph_pattern(ch);
    for (row_index, row) in glyph.iter().enumerate() {
        for (column_index, pixel) in row.chars().enumerate() {
            if pixel == ' ' {
                continue;
            }
            let px = cursor_x + (column_index * scale) as i32;
            let py = baseline_y + (row_index * scale) as i32;
            fill_rect(
                buffer,
                width,
                height,
                color,
                Rect {
                    x: px as f32,
                    y: py as f32,
                    width: scale as f32,
                    height: scale as f32,
                },
            );
        }
    }
}

fn draw_image(buffer: &mut [u32], width: usize, height: usize, image: &ImageCommand) {
    let x_start = image.x.max(0.0).floor() as usize;
    let y_start = image.y.max(0.0).floor() as usize;
    let x_end = (image.x + image.width).ceil().max(0.0) as usize;
    let y_end = (image.y + image.height).ceil().max(0.0) as usize;
    let x_end = x_end.min(width);
    let y_end = y_end.min(height);

    if image.source_width == 0 || image.source_height == 0 {
        return;
    }

    // Images are scaled with nearest-neighbor sampling to keep the implementation small.
    for y in y_start..y_end {
        let source_y = (((y as f32 - image.y) / image.height.max(1.0)) * image.source_height as f32)
            .floor()
            .clamp(0.0, (image.source_height - 1) as f32) as usize;
        let row = y * width;

        for x in x_start..x_end {
            let source_x = (((x as f32 - image.x) / image.width.max(1.0))
                * image.source_width as f32)
                .floor()
                .clamp(0.0, (image.source_width - 1) as f32) as usize;
            let pixel = image.pixels[source_y * image.source_width + source_x];
            buffer[row + x] = pixel;
        }
    }
}

fn rgb_u32(color: Color) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

fn glyph_pattern(ch: char) -> [&'static str; 7] {
    match ch.to_ascii_lowercase() {
        '0' => [
            " ### ", "#   #", "#  ##", "# # #", "##  #", "#   #", " ### ",
        ],
        '1' => [
            "  #  ", " ##  ", "# #  ", "  #  ", "  #  ", "  #  ", "#####",
        ],
        '2' => [
            " ### ", "#   #", "    #", "   # ", "  #  ", " #   ", "#####",
        ],
        '3' => [
            " ### ", "#   #", "    #", " ### ", "    #", "#   #", " ### ",
        ],
        '4' => [
            "   # ", "  ## ", " # # ", "#  # ", "#####", "   # ", "   # ",
        ],
        '5' => [
            "#####", "#    ", "#    ", "#### ", "    #", "#   #", " ### ",
        ],
        '6' => [
            " ### ", "#   #", "#    ", "#### ", "#   #", "#   #", " ### ",
        ],
        '7' => [
            "#####", "    #", "   # ", "  #  ", " #   ", " #   ", " #   ",
        ],
        '8' => [
            " ### ", "#   #", "#   #", " ### ", "#   #", "#   #", " ### ",
        ],
        '9' => [
            " ### ", "#   #", "#   #", " ####", "    #", "#   #", " ### ",
        ],
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
        ':' => [
            "     ", " ### ", " ### ", "     ", " ### ", " ### ", "     ",
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
        '<' => [
            "   # ", "  #  ", " #   ", "#    ", " #   ", "  #  ", "   # ",
        ],
        '>' => [
            " #   ", "  #  ", "   # ", "    #", "   # ", "  #  ", " #   ",
        ],
        '/' => [
            "    #", "   # ", "   # ", "  #  ", " #   ", " #   ", "#    ",
        ],
        _ => [
            "#####", "#   #", "   # ", "  #  ", "  #  ", "     ", "  #  ",
        ],
    }
}

#[cfg(test)]
mod tests {
    use crate::{css, html, layout, render, style};

    use super::{Color, DisplayCommand, ImageCommand, TextCommand, rasterize, translate};

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
                y: 12.0,
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
            &[],
        );

        assert_eq!(pixels[5], 0xFF0000);
        assert_eq!(pixels[10], 0xFF0000);
        assert_eq!(pixels[0], 0xFFFFFF);
    }

    #[test]
    fn translates_display_commands() {
        let commands = translate(
            vec![
                DisplayCommand::SolidRect(
                    Color::BLACK,
                    crate::layout::Rect {
                        x: 1.0,
                        y: 2.0,
                        width: 3.0,
                        height: 4.0,
                    },
                ),
                DisplayCommand::Text(TextCommand {
                    text: "hello".into(),
                    x: 5.0,
                    y: 6.0,
                    color: Color::BLACK,
                    font_size: 8.0,
                }),
                DisplayCommand::Image(ImageCommand {
                    x: 7.0,
                    y: 8.0,
                    width: 9.0,
                    height: 10.0,
                    source_width: 1,
                    source_height: 1,
                    pixels: vec![0x112233],
                }),
            ],
            10.0,
            20.0,
        );

        assert_eq!(
            commands[0],
            DisplayCommand::SolidRect(
                Color::BLACK,
                crate::layout::Rect {
                    x: 11.0,
                    y: 22.0,
                    width: 3.0,
                    height: 4.0,
                },
            )
        );
        assert_eq!(
            commands[1],
            DisplayCommand::Text(TextCommand {
                text: "hello".into(),
                x: 15.0,
                y: 26.0,
                color: Color::BLACK,
                font_size: 8.0,
            })
        );
        assert_eq!(
            commands[2],
            DisplayCommand::Image(ImageCommand {
                x: 17.0,
                y: 28.0,
                width: 9.0,
                height: 10.0,
                source_width: 1,
                source_height: 1,
                pixels: vec![0x112233],
            })
        );
    }

    #[test]
    fn rasterizes_image_pixels() {
        let pixels = rasterize(
            &[DisplayCommand::Image(ImageCommand {
                x: 0.0,
                y: 0.0,
                width: 2.0,
                height: 2.0,
                source_width: 2,
                source_height: 2,
                pixels: vec![0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF],
            })],
            2,
            2,
            &[],
        );

        assert_eq!(pixels, vec![0xFF0000, 0x00FF00, 0x0000FF, 0xFFFFFF]);
    }

    #[test]
    fn paints_borders_when_color_and_width_are_present() {
        let commands = display_list(
            r#"<div class="panel"></div>"#,
            r#"
                .panel {
                    width: 20px;
                    height: 10px;
                    border-left: 2px;
                    border-right: 2px;
                    border-top: 1px;
                    border-bottom: 3px;
                    border-color: #112233;
                }
            "#,
        );

        assert_eq!(
            commands,
            vec![
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 24.0,
                        height: 1.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 11.0,
                        width: 24.0,
                        height: 3.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 2.0,
                        height: 14.0,
                    },
                ),
                DisplayCommand::SolidRect(
                    Color {
                        r: 17,
                        g: 34,
                        b: 51,
                        a: 255,
                    },
                    crate::layout::Rect {
                        x: 22.0,
                        y: 0.0,
                        width: 2.0,
                        height: 14.0,
                    },
                ),
            ]
        );
    }
}
