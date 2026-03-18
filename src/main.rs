use std::{collections::HashMap, env};

use mini_browser::{css, dom::NodeType, html, layout, net, render, resource, style, window};

// These constants define the browser chrome at the top of the window.
// Everything below `CHROME_HEIGHT` is treated as page content.
const CHROME_HEIGHT: f32 = 56.0;
const ADDRESS_TEXT_Y: f32 = 12.0;
const STATUS_TEXT_Y: f32 = 34.0;
const ADDRESS_BOX_X: f32 = 68.0;
const ADDRESS_BOX_Y: f32 = 8.0;
const ADDRESS_BOX_HEIGHT: f32 = 18.0;
const ADDRESS_TEXT_X: f32 = 72.0;
const ADDRESS_CHAR_WIDTH: f32 = 6.0;
const NAV_BUTTON_Y: f32 = 8.0;
const NAV_BUTTON_WIDTH: f32 = 20.0;
const NAV_BUTTON_HEIGHT: f32 = 18.0;
const BACK_BUTTON_X: f32 = 12.0;
const FORWARD_BUTTON_X: f32 = 36.0;

#[derive(Debug, Clone)]
struct BrowserState {
    // Address bar and focus state for the tiny browser chrome.
    address_input: String,
    address_bar_focused: bool,
    address_bar_selected: bool,
    frame_index: usize,

    // The currently displayed document snapshot.
    document_html: String,
    stylesheet: String,
    images: HashMap<String, resource::LoadedImage>,
    current_url: Option<net::Url>,

    // UI state that is shown in the chrome.
    status_text: String,
    status_color: css::Color,
    scroll_offset: f32,

    // History stores whole snapshots so back/forward can restore instantly without refetching.
    back_stack: Vec<HistoryEntry>,
    forward_stack: Vec<HistoryEntry>,
}

#[derive(Debug, Clone)]
struct LinkTarget {
    href: String,
    rect: layout::Rect,
    underline: bool,
}

#[derive(Debug, Clone)]
struct DocumentView {
    // `commands` are what get painted, `links` are the separately tracked clickable regions.
    commands: Vec<render::DisplayCommand>,
    links: Vec<LinkTarget>,
}

#[derive(Debug, Clone, Copy)]
struct ChromeState<'a> {
    viewport_width: usize,
    address_input: &'a str,
    status_text: &'a str,
    status_color: css::Color,
    address_bar_focused: bool,
    address_bar_selected: bool,
    show_caret: bool,
    can_go_back: bool,
    can_go_forward: bool,
    hovered_action: Option<ChromeAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChromeAction {
    Back,
    Forward,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    address_input: String,
    document_html: String,
    stylesheet: String,
    images: HashMap<String, resource::LoadedImage>,
    current_url: Option<net::Url>,
    status_text: String,
    status_color: css::Color,
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
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
        }
    }

    fn display_list(
        &mut self,
        viewport_width: usize,
        viewport_height: usize,
        input: &window::WindowInput,
    ) -> Vec<render::DisplayCommand> {
        // The browser re-builds its visible scene every frame from current state + fresh input.
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

        // Page clicks are handled after layout exists so hit testing can use real rectangles.
        if let Some(link_target) = self.clicked_link(input, &document_view.links) {
            self.navigate_to_link(link_target);
        }

        self.clamp_scroll(viewport_height, document_height(&document_view.commands));
        let hovered_href = self
            .hovered_link(input, &document_view.links)
            .map(|link| link.href.as_str());
        let hovered_action = self.hovered_chrome_action(input);

        let mut commands = chrome_commands(ChromeState {
            viewport_width,
            address_input: &self.address_input,
            status_text: &self.status_text,
            status_color: self.status_color,
            address_bar_focused: self.address_bar_focused,
            address_bar_selected: self.address_bar_selected,
            show_caret: self.show_caret(),
            can_go_back: self.can_go_back(),
            can_go_forward: self.can_go_forward(),
            hovered_action,
        });
        // Page commands are translated below the fixed chrome and then decorated with link underlines.
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
        // Chrome buttons get first chance at a click so they do not fall through to page links.
        if input.focus_address_bar {
            self.address_bar_focused = true;
            self.address_bar_selected = true;
        }

        if input.left_mouse_pressed {
            if let Some(action) = self.hovered_chrome_action(input) {
                match action {
                    ChromeAction::Back => self.go_back(),
                    ChromeAction::Forward => self.go_forward(),
                }
                self.address_bar_focused = false;
                self.address_bar_selected = false;
                return;
            }

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

        // Keyboard text entry only edits the address bar when it is focused.
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

        if input.back_pressed {
            self.go_back();
        }

        if input.forward_pressed {
            self.go_forward();
        }

        // Scrolling is applied after navigation shortcuts so the restored page starts at offset 0.
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

        // Successful navigation replaces the visible page and pushes the old snapshot to history.
        match load_remote_document(&target) {
            Ok((document_html, stylesheet, images, resolved_url)) => {
                let next_entry = HistoryEntry {
                    address_input: resolved_url.to_string(),
                    document_html,
                    stylesheet,
                    images,
                    current_url: Some(resolved_url),
                    status_text: "loaded".into(),
                    status_color: css::Color {
                        r: 40,
                        g: 120,
                        b: 40,
                        a: 255,
                    },
                };
                self.commit_navigation(next_entry);
            }
            Err(error) => {
                eprintln!("{error}");
                self.commit_navigation(self.error_entry("load failed", &error));
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
        // Link navigation reuses the same loader path as manual URL entry.
        match load_remote_document(&resolved.to_string()) {
            Ok((document_html, stylesheet, images, resolved_url)) => {
                let next_entry = HistoryEntry {
                    address_input: resolved_url.to_string(),
                    document_html,
                    stylesheet,
                    images,
                    current_url: Some(resolved_url),
                    status_text: "loaded".into(),
                    status_color: css::Color {
                        r: 40,
                        g: 120,
                        b: 40,
                        a: 255,
                    },
                };
                self.commit_navigation(next_entry);
            }
            Err(error) => {
                eprintln!("{error}");
                self.commit_navigation(self.error_entry("link failed", &error));
            }
        }
    }

    fn set_status(&mut self, text: impl Into<String>, color: css::Color) {
        self.status_text = text.into();
        self.status_color = color;
    }

    fn show_error_page(&mut self, title: &str, message: &str) {
        self.restore_entry(self.error_entry(title, message));
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

    fn snapshot(&self) -> HistoryEntry {
        // History snapshots include the decoded image cache so back/forward feels immediate.
        HistoryEntry {
            address_input: self.address_input.clone(),
            document_html: self.document_html.clone(),
            stylesheet: self.stylesheet.clone(),
            images: self.images.clone(),
            current_url: self.current_url.clone(),
            status_text: self.status_text.clone(),
            status_color: self.status_color,
        }
    }

    fn restore_entry(&mut self, entry: HistoryEntry) {
        self.address_input = entry.address_input;
        self.document_html = entry.document_html;
        self.stylesheet = entry.stylesheet;
        self.images = entry.images;
        self.current_url = entry.current_url;
        self.status_text = entry.status_text;
        self.status_color = entry.status_color;
        self.scroll_offset = 0.0;
        self.address_bar_selected = false;
    }

    fn commit_navigation(&mut self, entry: HistoryEntry) {
        self.back_stack.push(self.snapshot());
        self.forward_stack.clear();
        self.restore_entry(entry);
    }

    fn go_back(&mut self) {
        if let Some(previous) = self.back_stack.pop() {
            self.forward_stack.push(self.snapshot());
            self.restore_entry(previous);
        }
    }

    fn go_forward(&mut self) {
        if let Some(next) = self.forward_stack.pop() {
            self.back_stack.push(self.snapshot());
            self.restore_entry(next);
        }
    }

    fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    fn hovered_chrome_action(&self, input: &window::WindowInput) -> Option<ChromeAction> {
        let (mouse_x, mouse_y) = input.mouse_position?;

        if point_in_rect(mouse_x, mouse_y, back_button_rect()) && self.can_go_back() {
            return Some(ChromeAction::Back);
        }
        if point_in_rect(mouse_x, mouse_y, forward_button_rect()) && self.can_go_forward() {
            return Some(ChromeAction::Forward);
        }

        None
    }

    fn error_entry(&self, title: &str, message: &str) -> HistoryEntry {
        let (document_html, stylesheet) = error_document(title, message, self.address_input.trim());
        HistoryEntry {
            address_input: self.address_input.clone(),
            document_html,
            stylesheet,
            images: HashMap::new(),
            current_url: None,
            status_text: title.into(),
            status_color: css::Color {
                r: 180,
                g: 60,
                b: 60,
                a: 255,
            },
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
    // This is the full browser pipeline in one place:
    // HTML/CSS text -> styled tree -> layout tree -> display commands + clickable metadata.
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

fn chrome_commands(chrome: ChromeState<'_>) -> Vec<render::DisplayCommand> {
    // Chrome rendering is intentionally separate from page rendering so scrolling never moves it.
    let width = chrome.viewport_width as f32;
    let address_display = if chrome.address_input.is_empty() {
        "http://example.com".to_string()
    } else {
        chrome.address_input.to_string()
    };
    let address_box = address_bar_rect(width);
    let border_color = if chrome.address_bar_focused {
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
            text: chrome.status_text.to_string(),
            x: 16.0,
            y: STATUS_TEXT_Y,
            color: chrome.status_color,
            font_size: 8.0,
        }),
    ];
    commands.extend(nav_button_commands(
        back_button_rect(),
        "<",
        chrome.can_go_back,
        chrome.hovered_action == Some(ChromeAction::Back),
    ));
    commands.extend(nav_button_commands(
        forward_button_rect(),
        ">",
        chrome.can_go_forward,
        chrome.hovered_action == Some(ChromeAction::Forward),
    ));

    if chrome.address_bar_selected {
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
    } else if chrome.show_caret {
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

fn nav_button_commands(
    rect: layout::Rect,
    label: &str,
    enabled: bool,
    hovered: bool,
) -> [render::DisplayCommand; 2] {
    // Buttons are just a background rect plus a text glyph. There is no separate widget system.
    [
        render::DisplayCommand::SolidRect(
            if !enabled {
                css::Color {
                    r: 235,
                    g: 235,
                    b: 235,
                    a: 255,
                }
            } else if hovered {
                css::Color {
                    r: 210,
                    g: 223,
                    b: 246,
                    a: 255,
                }
            } else {
                css::Color {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 255,
                }
            },
            rect,
        ),
        render::DisplayCommand::Text(render::TextCommand {
            text: label.to_string(),
            x: rect.x + 7.0,
            y: rect.y + 5.0,
            color: if enabled {
                css::Color::BLACK
            } else {
                css::Color {
                    r: 150,
                    g: 150,
                    b: 150,
                    a: 255,
                }
            },
            font_size: 8.0,
        }),
    ]
}

fn address_bar_rect(viewport_width: f32) -> layout::Rect {
    layout::Rect {
        x: ADDRESS_BOX_X,
        y: ADDRESS_BOX_Y,
        width: (viewport_width - ADDRESS_BOX_X - 12.0).max(0.0),
        height: ADDRESS_BOX_HEIGHT,
    }
}

fn back_button_rect() -> layout::Rect {
    layout::Rect {
        x: BACK_BUTTON_X,
        y: NAV_BUTTON_Y,
        width: NAV_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
    }
}

fn forward_button_rect() -> layout::Rect {
    layout::Rect {
        x: FORWARD_BUTTON_X,
        y: NAV_BUTTON_Y,
        width: NAV_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
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

    // Link targets are collected separately from display commands because clicking needs rectangles,
    // not just painted pixels.
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
    // Layout decides *where* an image box goes; the image cache supplies *what* pixels fill it.
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
    // Link underlines are drawn as separate commands so hover state can change them cheaply.
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
    // Error pages are rendered with the same browser pipeline as normal documents.
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

fn text_document(body: &str, target: &str) -> (String, String) {
    // `text/plain` is wrapped in a simple HTML shell so the browser can display it without a
    // separate rendering path.
    let escaped_body = escape_html(body);
    let escaped_target = escape_html(target);
    let detail = if escaped_target.is_empty() {
        String::new()
    } else {
        format!("<p>{escaped_target}</p>")
    };

    let html = format!(
        r#"
        <div id="app" class="plain-text">
            <h1>text document</h1>
            {detail}
            <pre>{escaped_body}</pre>
        </div>
    "#
    );

    let css = r#"
        #app {
            width: 680px;
            padding-top: 16px;
            padding-left: 12px;
            background-color: #f7f4ee;
        }
        h1 { font-size: 22px; margin-bottom: 8px; color: #433526; }
        p { font-size: 14px; margin-top: 6px; color: #6b5947; }
        pre {
            margin-top: 10px;
            color: #2f2a24;
        }
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
    // This function is the app-facing loader. It translates low-level fetch/content-type details
    // into "what document should the browser show?".
    let url = net::Url::parse(raw_url).map_err(|error| format!("url error: {error:?}"))?;
    let fetch_result = net::fetch(&url).map_err(|error| describe_network_error(&error))?;
    let response = fetch_result.response;
    let final_url = fetch_result.final_url;

    if response.status_code != 200 {
        return Err(describe_network_error(&net::NetworkError::HttpStatus(
            response.status_code,
            response.reason_phrase,
        )));
    }

    let content_type = response.header("content-type").unwrap_or("text/html");
    if content_type.starts_with("text/plain") {
        let body = String::from_utf8(response.body)
            .map_err(|_| describe_network_error(&net::NetworkError::InvalidBodyEncoding))?;
        let (document_html, stylesheet) = text_document(&body, &final_url.to_string());
        return Ok((document_html, stylesheet, HashMap::new(), final_url));
    }

    if !content_type.starts_with("text/html") {
        return Err(format!("unsupported content type {content_type}"));
    }

    let html = String::from_utf8(response.body)
        .map_err(|_| describe_network_error(&net::NetworkError::InvalidBodyEncoding))?;
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
        ADDRESS_BOX_HEIGHT, ADDRESS_BOX_X, ADDRESS_BOX_Y, BACK_BUTTON_X, BrowserState,
        CHROME_HEIGHT, HistoryEntry, LinkTarget, NAV_BUTTON_Y, address_bar_rect, back_button_rect,
        collect_image_commands, collect_link_targets, describe_network_error, document_height,
        error_document, link_decoration_commands, page_step, point_in_rect, text_document,
    };
    use mini_browser::{css, html, layout, render, resource, style, window};

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
        assert_eq!(rect.width, 720.0);
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
    fn text_document_escapes_plain_text() {
        let (html, _) = text_document("a < b", "http://example.com/file.txt");
        assert!(html.contains("a &lt; b"));
        assert!(html.contains("text document"));
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

    #[test]
    fn history_navigation_restores_previous_entries() {
        let mut browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        browser.commit_navigation(HistoryEntry {
            address_input: "http://second.test".into(),
            document_html: "<div>second</div>".into(),
            stylesheet: String::new(),
            images: HashMap::new(),
            current_url: None,
            status_text: "loaded".into(),
            status_color: css::Color::BLACK,
        });

        browser.go_back();
        assert_eq!(browser.address_input, "http://first.test");
        assert_eq!(browser.document_html, "<div>first</div>");

        browser.go_forward();
        assert_eq!(browser.address_input, "http://second.test");
        assert_eq!(browser.document_html, "<div>second</div>");
    }

    #[test]
    fn back_button_hover_requires_history() {
        let mut browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        let hover = browser.hovered_chrome_action(&window::WindowInput {
            mouse_position: Some((BACK_BUTTON_X + 2.0, NAV_BUTTON_Y + 2.0)),
            ..window::WindowInput::default()
        });
        assert_eq!(hover, None);

        browser.back_stack.push(browser.snapshot());
        let hover = browser.hovered_chrome_action(&window::WindowInput {
            mouse_position: Some((back_button_rect().x + 2.0, back_button_rect().y + 2.0)),
            ..window::WindowInput::default()
        });
        assert_eq!(hover, Some(super::ChromeAction::Back));
    }
}
