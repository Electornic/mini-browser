use std::env;

use mini_browser::{css, dom::NodeType, html, layout, net, render, resource, style, window};

const CHROME_HEIGHT: f32 = 56.0;
const ADDRESS_TEXT_Y: f32 = 12.0;
const STATUS_TEXT_Y: f32 = 34.0;

#[derive(Debug, Clone)]
struct BrowserState {
    address_input: String,
    document_html: String,
    stylesheet: String,
    current_url: Option<net::Url>,
    status_text: String,
    status_color: css::Color,
    scroll_offset: f32,
}

#[derive(Debug, Clone)]
struct LinkTarget {
    href: String,
    rect: layout::Rect,
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
        current_url: Option<net::Url>,
        status_text: impl Into<String>,
    ) -> Self {
        Self {
            address_input,
            document_html,
            stylesheet,
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
        self.apply_input(input, viewport_height);

        let document_view =
            build_document_view(&self.document_html, &self.stylesheet, viewport_width)
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

        let mut commands = chrome_commands(
            viewport_width,
            &self.address_input,
            &self.status_text,
            self.status_color,
        );
        commands.extend(render::translate(
            document_view.commands,
            0.0,
            CHROME_HEIGHT - self.scroll_offset,
        ));
        commands
    }

    fn apply_input(&mut self, input: &window::WindowInput, viewport_height: usize) {
        for ch in input.typed.chars() {
            if !ch.is_control() {
                self.address_input.push(ch);
            }
        }

        if input.backspace_pressed {
            self.address_input.pop();
        }

        if input.enter_pressed {
            self.navigate();
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
            self.set_status(
                "enter url then press enter",
                css::Color {
                    r: 180,
                    g: 60,
                    b: 60,
                    a: 255,
                },
            );
            return;
        }

        match load_remote_document(&target) {
            Ok((document_html, stylesheet, resolved_url)) => {
                self.document_html = document_html;
                self.stylesheet = stylesheet;
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
                self.set_status(
                    "load failed",
                    css::Color {
                        r: 180,
                        g: 60,
                        b: 60,
                        a: 255,
                    },
                );
            }
        }
    }

    fn navigate_to_link(&mut self, link_target: &LinkTarget) {
        let resolved = match self.resolve_href(&link_target.href) {
            Ok(url) => url,
            Err(error) => {
                eprintln!("{error}");
                self.set_status(
                    "link failed",
                    css::Color {
                        r: 180,
                        g: 60,
                        b: 60,
                        a: 255,
                    },
                );
                return;
            }
        };

        self.address_input = resolved.to_string();
        match load_remote_document(&resolved.to_string()) {
            Ok((document_html, stylesheet, resolved_url)) => {
                self.document_html = document_html;
                self.stylesheet = stylesheet;
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
                self.set_status(
                    "link failed",
                    css::Color {
                        r: 180,
                        g: 60,
                        b: 60,
                        a: 255,
                    },
                );
            }
        }
    }

    fn set_status(&mut self, text: impl Into<String>, color: css::Color) {
        self.status_text = text.into();
        self.status_color = color;
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
    Ok(DocumentView {
        commands: render::build_display_list(&layout),
        links: collect_link_targets(&layout, None),
    })
}

fn chrome_commands(
    viewport_width: usize,
    address_input: &str,
    status_text: &str,
    status_color: css::Color,
) -> Vec<render::DisplayCommand> {
    let width = viewport_width as f32;
    let address_display = if address_input.is_empty() {
        "http://example.com".to_string()
    } else {
        address_input.to_string()
    };

    vec![
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
        render::DisplayCommand::SolidRect(
            css::Color {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            layout::Rect {
                x: 12.0,
                y: 8.0,
                width: (width - 24.0).max(0.0),
                height: 18.0,
            },
        ),
        render::DisplayCommand::Text(render::TextCommand {
            text: address_display,
            x: 16.0,
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
    ]
}

fn document_height(commands: &[render::DisplayCommand]) -> f32 {
    commands.iter().fold(0.0, |max_bottom, command| {
        let bottom = match command {
            render::DisplayCommand::SolidRect(_, rect) => rect.y + rect.height,
            render::DisplayCommand::Text(text) => text.y + text.font_size,
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
        });
    }

    for child in &layout_box.children {
        targets.extend(collect_link_targets(child, current_href));
    }

    targets
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

fn point_in_rect(x: f32, y: f32, rect: layout::Rect) -> bool {
    x >= rect.x && x <= rect.x + rect.width && y >= rect.y && y <= rect.y + rect.height
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

fn load_remote_document(raw_url: &str) -> Result<(String, String, net::Url), String> {
    let url = net::Url::parse(raw_url).map_err(|error| format!("url error: {error:?}"))?;
    let html = net::load_html(&url).map_err(|error| format!("network error: {error:?}"))?;
    let nodes = html::parse(&html)
        .map_err(|error| format!("html parse error at {}: {}", error.position, error.message))?;
    let stylesheets = resource::load_stylesheets(&nodes, &url)
        .map_err(|error| format!("resource error: {error:?}"))?;
    Ok((html, stylesheets.join("\n"), url))
}

fn load_initial_state() -> BrowserState {
    match env::args().nth(1) {
        Some(raw_url) => match load_remote_document(&raw_url) {
            Ok((document_html, stylesheet, current_url)) => BrowserState::new(
                raw_url,
                document_html,
                stylesheet,
                Some(current_url),
                "loaded",
            ),
            Err(error) => {
                eprintln!("{error}");
                let mut state = BrowserState::new(
                    raw_url,
                    sample_html().to_string(),
                    sample_css().to_string(),
                    None,
                    "load failed",
                );
                state.status_color = css::Color {
                    r: 180,
                    g: 60,
                    b: 60,
                    a: 255,
                };
                state
            }
        },
        None => BrowserState::new(
            "http://example.com".into(),
            sample_html().to_string(),
            sample_css().to_string(),
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
    use super::{CHROME_HEIGHT, collect_link_targets, document_height, page_step, point_in_rect};
    use mini_browser::{css, html, layout, render, style};

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
}
