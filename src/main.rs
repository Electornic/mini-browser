use std::{collections::HashMap, env};

use mini_browser::{css, dom::NodeType, html, layout, net, render, resource, style, window};

const CHROME_HEIGHT: f32 = 56.0;
const ADDRESS_TEXT_Y: f32 = 12.0;
const STATUS_TEXT_Y: f32 = 34.0;
const ADDRESS_BOX_X: f32 = 12.0;
const ADDRESS_BOX_Y: f32 = 8.0;
const ADDRESS_BOX_HEIGHT: f32 = 18.0;
const ADDRESS_TEXT_X: f32 = 16.0;
const ADDRESS_CHAR_WIDTH: f32 = 6.0;

#[derive(Debug, Clone)]
struct BrowserState {
    address_input: String,
    address_bar_focused: bool,
    address_bar_selected: bool,
    frame_index: usize,
    document_html: String,
    stylesheet: String,
    images: HashMap<String, resource::LoadedImage>,
    current_url: Option<net::Url>,
    status_text: String,
    status_color: css::Color,
    scroll_offset: f32,
}

#[derive(Debug, Clone)]
struct LinkTarget {
    href: String,
    rect: layout::Rect,
    underline: bool,
}

#[derive(Debug, Clone)]
struct DocumentView {
    commands: Vec<render::DisplayCommand>,
    links: Vec<LinkTarget>,
}

impl BrowserState {
    fn new(
        address_input: String,
        document_html: String,
        stylesheet: String,
        images: HashMap<String, resource::LoadedImage>,
        current_url: Option<net::Url>,
        status_text: impl Into<String>,
    ) -> Self {
        Self {
            address_input,
            address_bar_focused: true,
            address_bar_selected: false,
            frame_index: 0,
            document_html,
            stylesheet,
            images,
            current_url,
            status_text: status_text.into(),
            status_color: css::Color::BLACK,
            scroll_offset: 0.0,
        }
    }

    fn display_list(
        &mut self,
        viewport_width: usize,
        viewport_height: usize,
        input: &window::WindowInput,
    ) -> Vec<render::DisplayCommand> {
        self.frame_index = self.frame_index.wrapping_add(1);
        self.apply_input(input, viewport_width, viewport_height);

        let document_view = build_document_view(
            &self.document_html,
            &self.stylesheet,
            viewport_width,
            self.current_url.as_ref(),
            &self.images,
        )
        .unwrap_or_else(|build_error| {
            eprintln!("{build_error}");
            self.set_status(
                "render failed",
                css::Color {
                    r: 180,
                    g: 60,
                    b: 60,
                    a: 255,
                },
            );
            DocumentView {
                commands: Vec::new(),
                links: Vec::new(),
            }
        });

        if let Some(link_target) = self.clicked_link(input, &document_view.links) {
            self.navigate_to_link(link_target);
        }

        self.clamp_scroll(viewport_height, document_height(&document_view.commands));
        let hovered_href = self
            .hovered_link(input, &document_view.links)
            .map(|link| link.href.as_str());

        let mut commands = chrome_commands(
            viewport_width,
            &self.address_input,
            &self.status_text,
            self.status_color,
            self.address_bar_focused,
            self.address_bar_selected,
            self.show_caret(),
        );
        commands.extend(render::translate(
            document_view.commands,
            0.0,
            CHROME_HEIGHT - self.scroll_offset,
        ));
        commands.extend(render::translate(
            link_decoration_commands(&document_view.links, hovered_href),
            0.0,
            CHROME_HEIGHT - self.scroll_offset,
        ));
        commands
    }

    fn apply_input(
        &mut self,
        input: &window::WindowInput,
        viewport_width: usize,
        viewport_height: usize,
    ) {
        if input.focus_address_bar {
            self.address_bar_focused = true;
            self.address_bar_selected = true;
        }

        if input.left_mouse_pressed {
            if let Some((mouse_x, mouse_y)) = input.mouse_position {
                if point_in_rect(mouse_x, mouse_y, address_bar_rect(viewport_width as f32)) {
                    self.address_bar_focused = true;
                    self.address_bar_selected = true;
                } else {
                    self.address_bar_focused = false;
                    self.address_bar_selected = false;
                }
            }
        }

        if self.address_bar_focused {
            for ch in input.typed.chars() {
                if !ch.is_control() {
                    if self.address_bar_selected {
                        self.address_input.clear();
                        self.address_bar_selected = false;
                    }
                    self.address_input.push(ch);
                }
            }

            if input.backspace_pressed {
                if self.address_bar_selected {
                    self.address_input.clear();
                    self.address_bar_selected = false;
                } else {
                    self.address_input.pop();
                }
            }

            if input.enter_pressed {
                self.address_bar_selected = false;
                self.navigate();
                self.address_bar_focused = false;
            }
        }

        self.scroll_offset -= input.scroll_y * 24.0;
        if input.move_up {
            self.scroll_offset -= 24.0;
        }
        if input.move_down {
            self.scroll_offset += 24.0;
        }
        if input.page_up_pressed {
            self.scroll_offset -= page_step(viewport_height);
        }
        if input.page_down_pressed {
            self.scroll_offset += page_step(viewport_height);
        }
    }

    fn navigate(&mut self) {
        let target = self.address_input.trim().to_string();
        if target.is_empty() {
            self.show_error_page("enter url", "enter url then press enter");
            return;
        }

        match load_remote_document(&target) {
            Ok((document_html, stylesheet, images, resolved_url)) => {
                self.document_html = document_html;
                self.stylesheet = stylesheet;
                self.images = images;
                self.current_url = Some(resolved_url);
                self.scroll_offset = 0.0;
                self.set_status(
                    "loaded",
                    css::Color {
                        r: 40,
                        g: 120,
                        b: 40,
                        a: 255,
                    },
                );
            }
            Err(error) => {
                eprintln!("{error}");
                self.show_error_page("load failed", &error);
            }
        }
    }

    fn navigate_to_link(&mut self, link_target: &LinkTarget) {
        let resolved = match self.resolve_href(&link_target.href) {
            Ok(url) => url,
            Err(error) => {
                eprintln!("{error}");
                self.show_error_page("link failed", &error);
                return;
            }
        };

        self.address_input = resolved.to_string();
        self.address_bar_selected = false;
        self.address_bar_focused = false;
        match load_remote_document(&resolved.to_string()) {
            Ok((document_html, stylesheet, images, resolved_url)) => {
                self.document_html = document_html;
                self.stylesheet = stylesheet;
                self.images = images;
                self.current_url = Some(resolved_url);
                self.scroll_offset = 0.0;
                self.set_status(
                    "loaded",
                    css::Color {
                        r: 40,
                        g: 120,
                        b: 40,
                        a: 255,
                    },
                );
            }
            Err(error) => {
                eprintln!("{error}");
                self.show_error_page("link failed", &error);
            }
        }
    }

    fn set_status(&mut self, text: impl Into<String>, color: css::Color) {
        self.status_text = text.into();
        self.status_color = color;
    }

    fn show_error_page(&mut self, title: &str, message: &str) {
        let (document_html, stylesheet) = error_document(title, message, self.address_input.trim());
        self.document_html = document_html;
        self.stylesheet = stylesheet;
        self.images.clear();
        self.current_url = None;
        self.scroll_offset = 0.0;
        self.status_text = title.to_string();
        self.status_color = css::Color {
            r: 180,
            g: 60,
            b: 60,
            a: 255,
        };
    }

    fn show_caret(&self) -> bool {
        self.address_bar_focused
            && !self.address_bar_selected
            && (self.frame_index / 30).is_multiple_of(2)
    }

    fn clamp_scroll(&mut self, viewport_height: usize, document_height: f32) {
        let visible_height = (viewport_height as f32 - CHROME_HEIGHT).max(0.0);
        let max_scroll = (document_height - visible_height).max(0.0);
        self.scroll_offset = self.scroll_offset.clamp(0.0, max_scroll);
    }

    fn clicked_link<'a>(
        &self,
        input: &window::WindowInput,
        links: &'a [LinkTarget],
    ) -> Option<&'a LinkTarget> {
        if !input.left_mouse_pressed {
            return None;
        }

        self.hovered_link(input, links)
    }

    fn hovered_link<'a>(
        &self,
        input: &window::WindowInput,
        links: &'a [LinkTarget],
    ) -> Option<&'a LinkTarget> {
        let (mouse_x, mouse_y) = input.mouse_position?;
        if mouse_y < CHROME_HEIGHT {
            return None;
        }

        let document_y = mouse_y - CHROME_HEIGHT + self.scroll_offset;
        links
            .iter()
            .rev()
            .find(|link| point_in_rect(mouse_x, document_y, link.rect))
    }

    fn resolve_href(&self, href: &str) -> Result<net::Url, String> {
        if href.contains("://") {
            net::Url::parse(href).map_err(|error| format!("url error: {error:?}"))
        } else if let Some(base_url) = &self.current_url {
            base_url
                .resolve(href)
                .map_err(|error| format!("url error: {error:?}"))
        } else {
            Err("relative link requires a loaded base url".into())
        }
    }
}

fn build_document_view(
    document_html: &str,
    stylesheet_source: &str,
    viewport_width: usize,
    current_url: Option<&net::Url>,
    images: &HashMap<String, resource::LoadedImage>,
) -> Result<DocumentView, String> {
    let mut nodes = html::parse(document_html)
        .map_err(|error| format!("html parse error at {}: {}", error.position, error.message))?;
    let stylesheet = css::parse(stylesheet_source)
        .map_err(|error| format!("css parse error at {}: {}", error.position, error.message))?;
    let root = nodes
        .pop()
        .ok_or_else(|| "document did not produce a root node".to_string())?;
    let styled = style::style_tree(&root, &[stylesheet]);
    let layout = layout::layout_tree(&styled, viewport_width as f32);
    let mut commands = render::build_display_list(&layout);
    commands.extend(collect_image_commands(&layout, current_url, images));
    Ok(DocumentView {
        commands,
        links: collect_link_targets(&layout, None),
    })
}

fn chrome_commands(
    viewport_width: usize,
    address_input: &str,
    status_text: &str,
    status_color: css::Color,
    address_bar_focused: bool,
    address_bar_selected: bool,
    show_caret: bool,
) -> Vec<render::DisplayCommand> {
    let width = viewport_width as f32;
    let address_display = if address_input.is_empty() {
        "http://example.com".to_string()
    } else {
        address_input.to_string()
    };
    let address_box = address_bar_rect(width);
    let border_color = if address_bar_focused {
        css::Color {
            r: 54,
            g: 116,
            b: 217,
            a: 255,
        }
    } else {
        css::Color {
            r: 170,
            g: 178,
            b: 190,
            a: 255,
        }
    };

    let mut commands = vec![
        render::DisplayCommand::SolidRect(
            css::Color {
                r: 236,
                g: 239,
                b: 244,
                a: 255,
            },
            layout::Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: CHROME_HEIGHT,
            },
        ),
        render::DisplayCommand::SolidRect(border_color, address_box),
        render::DisplayCommand::SolidRect(
            css::Color {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            layout::Rect {
                x: address_box.x + 1.0,
                y: address_box.y + 1.0,
                width: (address_box.width - 2.0).max(0.0),
                height: (address_box.height - 2.0).max(0.0),
            },
        ),
        render::DisplayCommand::Text(render::TextCommand {
            text: address_display.clone(),
            x: ADDRESS_TEXT_X,
            y: ADDRESS_TEXT_Y,
            color: css::Color::BLACK,
            font_size: 8.0,
        }),
        render::DisplayCommand::Text(render::TextCommand {
            text: status_text.to_string(),
            x: 16.0,
            y: STATUS_TEXT_Y,
            color: status_color,
            font_size: 8.0,
        }),
    ];

    if address_bar_selected {
        commands.push(render::DisplayCommand::SolidRect(
            css::Color {
                r: 214,
                g: 229,
                b: 255,
                a: 255,
            },
            layout::Rect {
                x: ADDRESS_TEXT_X - 2.0,
                y: ADDRESS_BOX_Y + 4.0,
                width: (address_display.len() as f32 * ADDRESS_CHAR_WIDTH + 4.0)
                    .min((address_box.width - 8.0).max(0.0)),
                height: 10.0,
            },
        ));
        commands.push(render::DisplayCommand::Text(render::TextCommand {
            text: address_display,
            x: ADDRESS_TEXT_X,
            y: ADDRESS_TEXT_Y,
            color: css::Color::BLACK,
            font_size: 8.0,
        }));
    } else if show_caret {
        commands.push(render::DisplayCommand::SolidRect(
            css::Color::BLACK,
            layout::Rect {
                x: ADDRESS_TEXT_X + address_display.len() as f32 * ADDRESS_CHAR_WIDTH,
                y: ADDRESS_BOX_Y + 4.0,
                width: 1.0,
                height: 10.0,
            },
        ));
    }

    commands
}

fn address_bar_rect(viewport_width: f32) -> layout::Rect {
    layout::Rect {
        x: ADDRESS_BOX_X,
        y: ADDRESS_BOX_Y,
        width: (viewport_width - 24.0).max(0.0),
        height: ADDRESS_BOX_HEIGHT,
    }
}

fn document_height(commands: &[render::DisplayCommand]) -> f32 {
    commands.iter().fold(0.0, |max_bottom, command| {
        let bottom = match command {
            render::DisplayCommand::SolidRect(_, rect) => rect.y + rect.height,
            render::DisplayCommand::Text(text) => text.y + text.font_size,
            render::DisplayCommand::Image(image) => image.y + image.height,
        };
        max_bottom.max(bottom)
    })
}

fn collect_link_targets(
    layout_box: &layout::LayoutBox,
    inherited_href: Option<&str>,
) -> Vec<LinkTarget> {
    let own_href = href_for_layout_box(layout_box);
    let current_href = own_href.or(inherited_href);
    let mut targets = Vec::new();

    if let Some(href) = current_href.filter(|_| should_collect_link_target(layout_box, own_href)) {
        targets.push(LinkTarget {
            href: href.to_string(),
            rect: layout_box.dimensions.content,
            underline: own_href.is_none(),
        });
    }

    for child in &layout_box.children {
        targets.extend(collect_link_targets(child, current_href));
    }

    targets
}

fn collect_image_commands(
    layout_box: &layout::LayoutBox,
    base_url: Option<&net::Url>,
    images: &HashMap<String, resource::LoadedImage>,
) -> Vec<render::DisplayCommand> {
    let mut commands = Vec::new();

    if let Some(command) = image_command_for_layout_box(layout_box, base_url, images) {
        commands.push(command);
    }

    for child in &layout_box.children {
        commands.extend(collect_image_commands(child, base_url, images));
    }

    commands
}

fn should_collect_link_target(layout_box: &layout::LayoutBox, own_href: Option<&str>) -> bool {
    if own_href.is_some() {
        return true;
    }

    matches!(
        &layout_box.box_type,
        layout::BoxType::BlockNode(styled_node) if matches!(styled_node.node.node_type, NodeType::Text(_))
    )
}

fn href_for_layout_box(layout_box: &layout::LayoutBox) -> Option<&str> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(styled_node) => match &styled_node.node.node_type {
            NodeType::Element(element) => element.attributes.get("href").map(String::as_str),
            NodeType::Text(_) => None,
        },
        layout::BoxType::AnonymousBlock => None,
    }
}

fn src_for_layout_box(layout_box: &layout::LayoutBox) -> Option<&str> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(styled_node) => match &styled_node.node.node_type {
            NodeType::Element(element) if element.tag_name == "img" => {
                element.attributes.get("src").map(String::as_str)
            }
            _ => None,
        },
        layout::BoxType::AnonymousBlock => None,
    }
}

fn image_command_for_layout_box(
    layout_box: &layout::LayoutBox,
    base_url: Option<&net::Url>,
    images: &HashMap<String, resource::LoadedImage>,
) -> Option<render::DisplayCommand> {
    let src = src_for_layout_box(layout_box)?;
    let image_key = if src.contains("://") {
        src.to_string()
    } else {
        base_url?.resolve(src).ok()?.to_string()
    };
    let image = images.get(&image_key)?;

    Some(render::DisplayCommand::Image(render::ImageCommand {
        x: layout_box.dimensions.content.x,
        y: layout_box.dimensions.content.y,
        width: layout_box.dimensions.content.width,
        height: layout_box.dimensions.content.height,
        source_width: image.width,
        source_height: image.height,
        pixels: image.pixels.clone(),
    }))
}

fn point_in_rect(x: f32, y: f32, rect: layout::Rect) -> bool {
    x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height
}

fn link_decoration_commands(
    links: &[LinkTarget],
    hovered_href: Option<&str>,
) -> Vec<render::DisplayCommand> {
    links
        .iter()
        .filter(|link| link.underline)
        .map(|link| {
            let color = if hovered_href == Some(link.href.as_str()) {
                css::Color {
                    r: 180,
                    g: 60,
                    b: 140,
                    a: 255,
                }
            } else {
                css::Color {
                    r: 0,
                    g: 102,
                    b: 204,
                    a: 255,
                }
            };

            render::DisplayCommand::SolidRect(
                color,
                layout::Rect {
                    x: link.rect.x,
                    y: link.rect.y + link.rect.height.max(1.0) - 1.0,
                    width: link.rect.width.max(1.0),
                    height: 1.0,
                },
            )
        })
        .collect()
}

fn page_step(viewport_height: usize) -> f32 {
    (viewport_height as f32 - CHROME_HEIGHT - 24.0).max(24.0)
}

fn sample_html() -> &'static str {
    r#"
        <div id="app" class="page">
            <h1>Mini Browser</h1>
            <p>Hello from the first HTML parser milestone.</p>
        </div>
    "#
}

fn sample_css() -> &'static str {
    r#"
        #app {
            width: 320px;
            padding-top: 12px;
            padding-left: 8px;
            background-color: #f0f4f8;
        }
        h1 { font-size: 28px; margin-bottom: 8px; color: #222222; }
        p { color: #0066cc; font-size: 18px; margin-top: 4px; }
    "#
}

fn error_document(title: &str, message: &str, target: &str) -> (String, String) {
    let escaped_title = escape_html(title);
    let escaped_message = escape_html(message);
    let escaped_target = escape_html(target);

    let detail = if escaped_target.is_empty() {
        String::new()
    } else {
        format!("<p>{escaped_target}</p>")
    };

    let html = format!(
        r#"
        <div id="app" class="error">
            <h1>{escaped_title}</h1>
            <p>{escaped_message}</p>
            {detail}
        </div>
    "#
    );

    let css = r#"
        #app {
            width: 520px;
            padding-top: 16px;
            padding-left: 12px;
            background-color: #fff3f0;
        }
        h1 { font-size: 24px; margin-bottom: 8px; color: #8a1c1c; }
        p { font-size: 14px; margin-top: 6px; color: #4a2d2d; }
    "#
    .to_string();

    (html, css)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn load_remote_document(
    raw_url: &str,
) -> Result<
    (
        String,
        String,
        HashMap<String, resource::LoadedImage>,
        net::Url,
    ),
    String,
> {
    let url = net::Url::parse(raw_url).map_err(|error| format!("url error: {error:?}"))?;
    let (html, final_url) =
        net::load_html_document(&url).map_err(|error| describe_network_error(&error))?;
    let nodes =
        html::parse(&html).map_err(|error| format!("html parse error {}", error.position))?;
    let stylesheets = resource::load_stylesheets(&nodes, &final_url)
        .map_err(|error| describe_resource_error(&error))?;
    let images = resource::load_images(&nodes, &final_url)
        .map_err(|error| describe_resource_error(&error))?
        .into_iter()
        .map(|image| (image.url.to_string(), image))
        .collect();
    Ok((html, stylesheets.join("\n"), images, final_url))
}

fn describe_network_error(error: &net::NetworkError) -> String {
    match error {
        net::NetworkError::UnsupportedScheme(_) => "unsupported scheme".into(),
        net::NetworkError::InvalidUrl(_) => "invalid url".into(),
        net::NetworkError::Io(_) => "network connection failed".into(),
        net::NetworkError::Tls(_) => "tls connection failed".into(),
        net::NetworkError::InvalidResponse(_) => "invalid server response".into(),
        net::NetworkError::MissingLocationHeader => "redirect missing location".into(),
        net::NetworkError::RedirectLimitExceeded => "too many redirects".into(),
        net::NetworkError::HttpStatus(code, _) => format!("http status {code}"),
        net::NetworkError::InvalidBodyEncoding => "invalid response body encoding".into(),
        net::NetworkError::UnexpectedContentType(content_type) => {
            format!("unsupported content type {content_type}")
        }
    }
}

fn describe_resource_error(error: &resource::ResourceError) -> String {
    match error {
        resource::ResourceError::MissingHref => "stylesheet missing href".into(),
        resource::ResourceError::MissingSrc => "image missing src".into(),
        resource::ResourceError::DecodeImage(_) => "image decode failed".into(),
        resource::ResourceError::Network(network_error) => describe_network_error(network_error),
    }
}

fn load_initial_state() -> BrowserState {
    match env::args().nth(1) {
        Some(raw_url) => match load_remote_document(&raw_url) {
            Ok((document_html, stylesheet, images, current_url)) => BrowserState::new(
                raw_url,
                document_html,
                stylesheet,
                images,
                Some(current_url),
                "loaded",
            ),
            Err(error) => {
                eprintln!("{error}");
                let mut state = BrowserState::new(
                    raw_url,
                    String::new(),
                    String::new(),
                    HashMap::new(),
                    None,
                    "load failed",
                );
                state.show_error_page("load failed", &error);
                state
            }
        },
        None => BrowserState::new(
            "http://example.com".into(),
            sample_html().to_string(),
            sample_css().to_string(),
            HashMap::new(),
            None,
            "type url and press enter",
        ),
    }
}

fn main() {
    let mut browser = load_initial_state();

    if let Err(error) = window::run("mini-browser", 800, 600, |width, height, input| {
        browser.display_list(width, height, input)
    }) {
        eprintln!("window error: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        ADDRESS_BOX_HEIGHT, ADDRESS_BOX_X, ADDRESS_BOX_Y, CHROME_HEIGHT, address_bar_rect,
        LinkTarget, collect_image_commands, collect_link_targets, describe_network_error,
        document_height, error_document, link_decoration_commands, page_step, point_in_rect,
    };
    use mini_browser::{css, html, layout, render, resource, style};

    #[test]
    fn computes_document_height_from_commands() {
        let commands = vec![
            render::DisplayCommand::SolidRect(
                css::Color::BLACK,
                layout::Rect {
                    x: 0.0,
                    y: 10.0,
                    width: 20.0,
                    height: 30.0,
                },
            ),
            render::DisplayCommand::Text(render::TextCommand {
                text: "hello".into(),
                x: 0.0,
                y: 60.0,
                color: css::Color::BLACK,
                font_size: 8.0,
            }),
        ];

        assert_eq!(document_height(&commands), 68.0);
    }

    #[test]
    fn page_step_uses_visible_height() {
        let expected = 400.0 - CHROME_HEIGHT - 24.0;
        assert_eq!(page_step(400), expected);
        assert_eq!(page_step(40), 24.0);
    }

    #[test]
    fn collects_link_targets_from_layout_tree() {
        let node = html::parse(r#"<a href="/next"><span>Hello</span></a>"#)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let styled = style::style_tree(&node, &[]);
        let layout_tree = layout::layout_tree(&styled, 300.0);
        let links = collect_link_targets(&layout_tree, None);

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].href, "/next");
        assert_eq!(links[1].href, "/next");
        assert!(!links[0].underline);
        assert!(links[1].underline);
    }

    #[test]
    fn hit_testing_checks_rect_bounds() {
        assert!(point_in_rect(
            10.0,
            20.0,
            layout::Rect {
                x: 5.0,
                y: 10.0,
                width: 20.0,
                height: 20.0,
            },
        ));
        assert!(!point_in_rect(
            30.1,
            20.0,
            layout::Rect {
                x: 5.0,
                y: 10.0,
                width: 20.0,
                height: 20.0,
            },
        ));
    }

    #[test]
    fn address_bar_rect_matches_chrome_layout() {
        let rect = address_bar_rect(800.0);
        assert_eq!(rect.x, ADDRESS_BOX_X);
        assert_eq!(rect.y, ADDRESS_BOX_Y);
        assert_eq!(rect.height, ADDRESS_BOX_HEIGHT);
        assert_eq!(rect.width, 776.0);
    }

    #[test]
    fn collects_image_commands_from_layout_tree() {
        let node = html::parse(r#"<img src="/pixel.png" width="12" height="8" />"#)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let styled = style::style_tree(&node, &[]);
        let layout_tree = layout::layout_tree(&styled, 300.0);
        let mut images = HashMap::new();
        images.insert(
            "http://example.com/pixel.png".into(),
            resource::LoadedImage {
                url: mini_browser::net::Url::parse("http://example.com/pixel.png").unwrap(),
                width: 1,
                height: 1,
                pixels: vec![0xFF0000],
            },
        );

        let commands = collect_image_commands(
            &layout_tree,
            Some(&mini_browser::net::Url::parse("http://example.com/index.html").unwrap()),
            &images,
        );

        assert_eq!(
            commands,
            vec![render::DisplayCommand::Image(render::ImageCommand {
                x: 0.0,
                y: 0.0,
                width: 12.0,
                height: 8.0,
                source_width: 1,
                source_height: 1,
                pixels: vec![0xFF0000],
            })]
        );
    }

    #[test]
    fn error_document_escapes_html() {
        let (html, _) = error_document("load failed", "<bad>", "http://a.com?q=<x>");
        assert!(html.contains("&lt;bad&gt;"));
        assert!(html.contains("&lt;x&gt;"));
    }

    #[test]
    fn network_error_messages_are_human_readable() {
        assert_eq!(
            describe_network_error(&mini_browser::net::NetworkError::MissingLocationHeader),
            "redirect missing location"
        );
        assert_eq!(
            describe_network_error(&mini_browser::net::NetworkError::UnexpectedContentType(
                "application/pdf".into()
            )),
            "unsupported content type application/pdf"
        );
    }

    #[test]
    fn link_decoration_underlines_text_targets_and_highlights_hover() {
        let links = vec![
            LinkTarget {
                href: "/a".into(),
                rect: layout::Rect {
                    x: 10.0,
                    y: 20.0,
                    width: 30.0,
                    height: 12.0,
                },
                underline: false,
            },
            LinkTarget {
                href: "/a".into(),
                rect: layout::Rect {
                    x: 12.0,
                    y: 20.0,
                    width: 28.0,
                    height: 12.0,
                },
                underline: true,
            },
        ];

        let commands = link_decoration_commands(&links, Some("/a"));
        assert_eq!(
            commands,
            vec![render::DisplayCommand::SolidRect(
                css::Color {
                    r: 180,
                    g: 60,
                    b: 140,
                    a: 255,
                },
                layout::Rect {
                    x: 12.0,
                    y: 31.0,
                    width: 28.0,
                    height: 1.0,
                },
            )]
        );
    }
}
