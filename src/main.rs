use std::{collections::HashMap, env};

use mini_browser::{css, dom, dom::NodeType, html, js, layout, net, render, resource, style, window};

// These constants define the browser chrome at the top of the window.
// Everything below `CHROME_HEIGHT` is treated as page content. The chrome stacks
// a tab strip on top of a toolbar; toolbar constants are expressed in screen
// coordinates so they already include `TAB_STRIP_HEIGHT` as their top offset.
const TAB_STRIP_HEIGHT: f32 = 42.0;
const TOOLBAR_HEIGHT: f32 = 60.0;
const CHROME_HEIGHT: f32 = TAB_STRIP_HEIGHT + TOOLBAR_HEIGHT;
const NAV_BUTTON_WIDTH: f32 = 32.0;
const NAV_BUTTON_HEIGHT: f32 = 32.0;
const NAV_BUTTON_Y: f32 = TAB_STRIP_HEIGHT + 12.0;
const BACK_BUTTON_X: f32 = 12.0;
const FORWARD_BUTTON_X: f32 = BACK_BUTTON_X + NAV_BUTTON_WIDTH + 4.0;
const REFRESH_BUTTON_X: f32 = FORWARD_BUTTON_X + NAV_BUTTON_WIDTH + 4.0;
const ADDRESS_BOX_X: f32 = REFRESH_BUTTON_X + NAV_BUTTON_WIDTH + 16.0;
const ADDRESS_BOX_Y: f32 = TAB_STRIP_HEIGHT + 12.0;
const ADDRESS_BOX_HEIGHT: f32 = 36.0;
const ADDRESS_TEXT_X: f32 = ADDRESS_BOX_X + 16.0;
const ADDRESS_TEXT_Y: f32 = ADDRESS_BOX_Y + 11.0;
const ADDRESS_FONT_SIZE: f32 = 14.0;
const STATUS_TEXT_Y: f32 = ADDRESS_BOX_Y + ADDRESS_BOX_HEIGHT + 4.0;
const STATUS_FONT_SIZE: f32 = 10.0;
const MENU_BUTTON_WIDTH: f32 = 32.0;
const MENU_BUTTON_RIGHT_PAD: f32 = 12.0;
const MENU_BUTTON_GAP: f32 = 8.0;
const TAB_X: f32 = 8.0;
const TAB_Y: f32 = 6.0;
const TAB_WIDTH: f32 = 272.0;
const TAB_HEIGHT: f32 = TAB_STRIP_HEIGHT - TAB_Y;
const TAB_RADIUS: f32 = 10.0;
const TAB_TITLE_X: f32 = TAB_X + 16.0;
const TAB_TITLE_Y: f32 = TAB_Y + 11.0;
const TAB_TITLE_FONT_SIZE: f32 = 13.0;

#[derive(Debug)]
struct BrowserState {
    // Address bar and focus state for the tiny browser chrome.
    address_input: String,
    address_bar_focused: bool,
    address_bar_selected: bool,
    frame_index: usize,

    // The currently displayed document snapshot.
    document_html: String,
    stylesheet: String,
    // Parsed forms of `document_html` and `stylesheet`, kept in sync via
    // `install_document`. Caching the parsed trees here keeps the per-frame
    // pipeline from re-parsing the same HTML/CSS at 60 fps — both parses are
    // O(input size) and dominate the frame budget on non-trivial pages.
    parsed_document: Vec<dom::Node>,
    parsed_stylesheet: css::Stylesheet,
    images: HashMap<String, resource::LoadedImage>,
    font_data: Vec<Vec<u8>>,
    current_url: Option<net::Url>,

    // UI state that is shown in the chrome.
    status_text: String,
    status_color: css::Color,
    scroll_offset: f32,

    // History stores whole snapshots so back/forward can restore instantly without refetching.
    back_stack: Vec<HistoryEntry>,
    forward_stack: Vec<HistoryEntry>,

    // DOM path of the element under the mouse, computed from the previous frame's layout
    // and fed into the next frame's style pass so :hover rules light up. Carries one frame
    // of latency, which is invisible at 60fps.
    hovered_dom_path: Option<Vec<usize>>,
    // DOM path of the most recently clicked page element. Persists across frames so
    // :focus rules keep matching after the click; cleared when the user clicks anywhere
    // outside the page (chrome buttons, the address bar, off-window).
    focused_dom_path: Option<Vec<usize>>,

    // JavaScript runtime. Globals (var bindings, declared functions) survive across
    // `<script>` tags within the same document but reset when the user navigates,
    // because `install_document` allocates a fresh runtime for the new page.
    js: js::JsRuntime,

    // Pre-fetched bodies for `<script src="…">` references in the current document,
    // keyed by the raw `src` attribute string (matches what the DOM walker sees).
    // Carried alongside `parsed_document` so that history restore can re-execute
    // every script without re-fetching from the network.
    external_scripts: HashMap<String, String>,
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
    // `layout_root` is kept around so post-render hit-testing (e.g. computing :hover paths
    // from the mouse position) can walk the same boxes the painter saw.
    commands: Vec<render::DisplayCommand>,
    links: Vec<LinkTarget>,
    layout_root: layout::LayoutBox,
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
    tab_title: &'a str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChromeAction {
    Back,
    Forward,
    Refresh,
    Menu,
}

#[derive(Debug, Clone)]
struct HistoryEntry {
    address_input: String,
    document_html: String,
    stylesheet: String,
    images: HashMap<String, resource::LoadedImage>,
    font_data: Vec<Vec<u8>>,
    external_scripts: HashMap<String, String>,
    current_url: Option<net::Url>,
    status_text: String,
    status_color: css::Color,
}

impl BrowserState {
    // The arg list is wide because every per-document resource is hoisted to
    // the call site (so test code can build a state without going through the
    // network loader). Bundling these into a struct is a Phase 1-style
    // refactor we explicitly defer per the Phase 2 plan — adding JS without
    // churning unrelated surfaces.
    #[allow(clippy::too_many_arguments)]
    fn new(
        address_input: String,
        document_html: String,
        stylesheet: String,
        images: HashMap<String, resource::LoadedImage>,
        font_data: Vec<Vec<u8>>,
        external_scripts: HashMap<String, String>,
        current_url: Option<net::Url>,
        status_text: impl Into<String>,
    ) -> Self {
        let parsed_document = html::parse(&document_html).unwrap_or_default();
        let parsed_stylesheet = css::parse(&stylesheet).unwrap_or_default();
        let mut state = Self {
            address_input,
            address_bar_focused: true,
            address_bar_selected: false,
            frame_index: 0,
            document_html,
            stylesheet,
            parsed_document,
            parsed_stylesheet,
            images,
            font_data,
            current_url,
            status_text: status_text.into(),
            status_color: css::Color::BLACK,
            scroll_offset: 0.0,
            back_stack: Vec::new(),
            forward_stack: Vec::new(),
            hovered_dom_path: None,
            focused_dom_path: None,
            js: js::JsRuntime::new(),
            external_scripts,
        };
        // The first page seen on construction also runs its scripts so the
        // initial document follows the same lifecycle as later navigations
        // (which all funnel through `install_document`).
        state.run_scripts();
        state
    }

    // Single funnel for "the displayed document changed". Updates the raw
    // strings and the parsed caches together so the per-frame pipeline can
    // assume `parsed_document` / `parsed_stylesheet` mirror `document_html` /
    // `stylesheet`. Parse failures degrade to empty trees so the rest of the
    // browser keeps running (build_document_view already has its own fallback
    // path for empty inputs).
    fn install_document(
        &mut self,
        document_html: String,
        stylesheet: String,
        external_scripts: HashMap<String, String>,
    ) {
        self.parsed_document = html::parse(&document_html).unwrap_or_default();
        self.parsed_stylesheet = css::parse(&stylesheet).unwrap_or_default();
        self.document_html = document_html;
        self.stylesheet = stylesheet;
        self.external_scripts = external_scripts;
        // Each navigated document starts with a fresh JS runtime — globals from
        // the previous page should not leak into the new one. Back/forward also
        // route through here, so the same reset rule applies on history moves.
        self.js = js::JsRuntime::new();
        self.run_scripts();
    }

    // Walks the parsed document in tree order and runs every `<script>` tag
    // through the JS runtime. Inline scripts use their text-child content;
    // external scripts (with a `src` attribute) look up their pre-fetched body
    // in `external_scripts`, keyed by the raw `src` value. Lookups that miss
    // (network failure, missing entry) are silently dropped — same degradation
    // pattern as broken stylesheets / images.
    fn run_scripts(&mut self) {
        // Snapshot the parsed DOM into the runtime first so that scripts
        // observing `document.getElementById` etc. see the document they were
        // shipped with, not whatever ran the previous load.
        self.js.bind_document(&self.parsed_document);
        let mut sources = Vec::new();
        for node in &self.parsed_document {
            collect_script_sources(node, &self.external_scripts, &mut sources);
        }
        for source in sources {
            if let Err(err) = self.js.execute(&source) {
                eprintln!("script error: {err}");
            }
        }
    }

    fn display_list(
        &mut self,
        viewport_width: usize,
        viewport_height: usize,
        input: &window::WindowInput,
        fonts: &[fontdue::Font],
    ) -> Vec<render::DisplayCommand> {
        // The browser re-builds its visible scene every frame from current state + fresh input.
        self.frame_index = self.frame_index.wrapping_add(1);
        self.apply_input(input, viewport_width, viewport_height);

        // Interaction state piped into the style pass. Hover and focus are tracked
        // across frames (one-frame lag); active is purely transient — true only while
        // the left mouse is currently held over the previously-hovered element.
        let interaction = style::InteractionState {
            hover: self.hovered_dom_path.as_deref(),
            focus: self.focused_dom_path.as_deref(),
            active: if input.left_mouse_held {
                self.hovered_dom_path.as_deref()
            } else {
                None
            },
        };
        let document_view = build_document_view(
            &self.parsed_document,
            &self.parsed_stylesheet,
            viewport_width,
            self.current_url.as_ref(),
            &self.images,
            interaction,
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
                // Empty fallback root so downstream hit-testing can run safely.
                layout_root: layout::LayoutBox {
                    box_type: layout::BoxType::AnonymousBlock,
                    dimensions: layout::Dimensions::default(),
                    children: Vec::new(),
                },
            }
        });

        // Page clicks are handled after layout exists so hit testing can use real rectangles.
        if let Some(link_target) = self.clicked_link(input, &document_view.links) {
            self.navigate_to_link(link_target);
        }

        self.clamp_scroll(viewport_height, document_height(&document_view.commands));
        // Recompute the hovered DOM path from this frame's layout. The next frame's style
        // pass will pick it up — a deliberate one-frame lag that keeps style and layout
        // strictly forward, no double-pass per frame required.
        self.hovered_dom_path =
            compute_hovered_dom_path(input, &document_view.layout_root, self.scroll_offset);

        // A page-area click moves :focus to the just-hovered element; clicks anywhere
        // outside the page (chrome buttons, the address bar, off-window) clear it.
        if input.left_mouse_pressed {
            self.focused_dom_path = match input.mouse_position {
                Some((_, mouse_y)) if mouse_y >= CHROME_HEIGHT => self.hovered_dom_path.clone(),
                _ => None,
            };
        }
        let hovered_href = self
            .hovered_link(input, &document_view.links)
            .map(|link| link.href.as_str());
        let hovered_action = self.hovered_chrome_action(input, viewport_width);

        let tab_title = self
            .current_url
            .as_ref()
            .map(|url| url.host.as_str())
            .filter(|host| !host.is_empty())
            .unwrap_or("New Tab");
        // Painter's-algorithm order: page first, then chrome on top. Painting
        // chrome last means any page content that would otherwise scroll up
        // into the chrome band (y < CHROME_HEIGHT) gets covered, so the chrome
        // visually pins to the top instead of "scrolling away" with the page.
        let mut commands = render::translate(
            document_view.commands,
            0.0,
            CHROME_HEIGHT - self.scroll_offset,
        );
        commands.extend(render::translate(
            link_decoration_commands(&document_view.links, hovered_href),
            0.0,
            CHROME_HEIGHT - self.scroll_offset,
        ));
        commands.extend(chrome_commands(
            ChromeState {
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
                tab_title,
            },
            fonts,
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
            if let Some(action) = self.hovered_chrome_action(input, viewport_width) {
                match action {
                    ChromeAction::Back => self.go_back(),
                    ChromeAction::Forward => self.go_forward(),
                    ChromeAction::Refresh => self.reload_current(),
                    // The dropdown itself is not implemented yet, but acknowledging the click
                    // proves the hit region works and prevents the click from falling through
                    // to the page underneath.
                    ChromeAction::Menu => self.set_status(
                        "menu (todo)",
                        css::Color {
                            r: 60,
                            g: 64,
                            b: 67,
                            a: 255,
                        },
                    ),
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
            Ok((document_html, stylesheet, images, font_data, external_scripts, resolved_url)) => {
                let next_entry = HistoryEntry {
                    address_input: resolved_url.to_string(),
                    document_html,
                    stylesheet,
                    images,
                    font_data,
                    external_scripts,
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
            Ok((document_html, stylesheet, images, font_data, external_scripts, resolved_url)) => {
                let next_entry = HistoryEntry {
                    address_input: resolved_url.to_string(),
                    document_html,
                    stylesheet,
                    images,
                    font_data,
                    external_scripts,
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
        // History snapshots include the decoded image cache and pre-fetched
        // script bodies so back/forward feels immediate — no re-fetching.
        HistoryEntry {
            address_input: self.address_input.clone(),
            document_html: self.document_html.clone(),
            stylesheet: self.stylesheet.clone(),
            images: self.images.clone(),
            font_data: self.font_data.clone(),
            external_scripts: self.external_scripts.clone(),
            current_url: self.current_url.clone(),
            status_text: self.status_text.clone(),
            status_color: self.status_color,
        }
    }

    fn restore_entry(&mut self, entry: HistoryEntry) {
        self.address_input = entry.address_input;
        self.install_document(entry.document_html, entry.stylesheet, entry.external_scripts);
        self.images = entry.images;
        self.font_data = entry.font_data;
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

    fn reload_current(&mut self) {
        // Refresh refetches the current document in place. Unlike navigate(), it does not
        // touch the back/forward stacks — the user expects "reload" to land on the same
        // page they were already viewing.
        let Some(url) = self.current_url.clone() else {
            self.set_status(
                "nothing to refresh",
                css::Color {
                    r: 154,
                    g: 160,
                    b: 166,
                    a: 255,
                },
            );
            return;
        };

        match load_remote_document(&url.to_string()) {
            Ok((document_html, stylesheet, images, font_data, external_scripts, resolved_url)) => {
                self.install_document(document_html, stylesheet, external_scripts);
                self.images = images;
                self.font_data = font_data;
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
                self.show_error_page("refresh failed", &error);
            }
        }
    }

    fn hovered_chrome_action(
        &self,
        input: &window::WindowInput,
        viewport_width: usize,
    ) -> Option<ChromeAction> {
        let (mouse_x, mouse_y) = input.mouse_position?;

        if point_in_rect(mouse_x, mouse_y, back_button_rect()) && self.can_go_back() {
            return Some(ChromeAction::Back);
        }
        if point_in_rect(mouse_x, mouse_y, forward_button_rect()) && self.can_go_forward() {
            return Some(ChromeAction::Forward);
        }
        // Refresh stays hover-able on the NTP too; the click handler decides whether
        // there is anything to reload, mirroring how Chrome shows an enabled button
        // but with a no-op effect when there is no current document.
        if point_in_rect(mouse_x, mouse_y, refresh_button_rect()) {
            return Some(ChromeAction::Refresh);
        }
        // The menu button always reports as hover-able even though its action is still a stub,
        // so the user gets visual feedback that the hit region is wired up.
        if point_in_rect(mouse_x, mouse_y, menu_button_rect(viewport_width as f32)) {
            return Some(ChromeAction::Menu);
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
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
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

// Recursive helper that appends every `<script>` body found under `node` to
// `out`, in document (tree) order. Inline scripts use their text-child
// content; external scripts (with a `src` attribute) read from the pre-fetched
// `externals` map keyed by the raw `src` value. A `src` whose body is missing
// from the map silently produces no entry — it indicates a fetch failure that
// was already logged upstream. Recursion stops at the script tag itself so a
// `<script>` is captured exactly once.
fn collect_script_sources(
    node: &dom::Node,
    externals: &HashMap<String, String>,
    out: &mut Vec<String>,
) {
    if let dom::NodeType::Element(elem) = &node.node_type
        && elem.tag_name.eq_ignore_ascii_case("script")
    {
        if let Some(src) = elem.attributes.get("src") {
            if let Some(body) = externals.get(src) {
                out.push(body.clone());
            }
            return;
        }
        let mut source = String::new();
        for child in &node.children {
            if let dom::NodeType::Text(text) = &child.node_type {
                source.push_str(text);
            }
        }
        if !source.trim().is_empty() {
            out.push(source);
        }
        return;
    }
    for child in &node.children {
        collect_script_sources(child, externals, out);
    }
}

fn build_document_view(
    parsed_document: &[dom::Node],
    parsed_stylesheet: &css::Stylesheet,
    viewport_width: usize,
    current_url: Option<&net::Url>,
    images: &HashMap<String, resource::LoadedImage>,
    interaction: style::InteractionState<'_>,
) -> Result<DocumentView, String> {
    // The HTML/CSS parse steps used to live here and run every frame; they now
    // happen once at navigate time (see `BrowserState::install_document`) and
    // this function takes the cached trees, so the per-frame pipeline is just:
    // styled tree -> layout tree -> display commands + clickable metadata.
    let root = parsed_document
        .last()
        .ok_or_else(|| "document did not produce a root node".to_string())?;
    let styled = style::style_tree_with_state(root, std::slice::from_ref(parsed_stylesheet), interaction);
    let layout = layout::layout_tree(&styled, viewport_width as f32);
    let mut commands = render::build_display_list(&layout);
    commands.extend(collect_image_commands(&layout, current_url, images));
    let links = collect_link_targets(&layout, None, false, render::Affine::IDENTITY);
    Ok(DocumentView {
        commands,
        links,
        layout_root: layout,
    })
}

fn chrome_commands(
    chrome: ChromeState<'_>,
    fonts: &[fontdue::Font],
) -> Vec<render::DisplayCommand> {
    // Chrome rendering is intentionally separate from page rendering so scrolling never moves it.
    let width = chrome.viewport_width as f32;
    let input_empty = chrome.address_input.is_empty();
    // The placeholder only renders when the bar is empty AND not in select-all-on-focus mode,
    // so a user who clicks the bar to type sees an empty field instead of greyed text under
    // their cursor.
    let address_display = if input_empty {
        if chrome.address_bar_selected {
            String::new()
        } else {
            "http://example.com".to_string()
        }
    } else {
        chrome.address_input.to_string()
    };
    let address_color = if input_empty {
        css::Color {
            r: 154,
            g: 160,
            b: 166,
            a: 255,
        }
    } else {
        css::Color::BLACK
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
            r: 218,
            g: 220,
            b: 224,
            a: 255,
        }
    };
    let pill_radius = address_box.height / 2.0;
    let pill_outer = render::CornerRadii::uniform(pill_radius);
    let pill_inner = render::CornerRadii::uniform((pill_radius - 1.0).max(0.0));

    let toolbar_bg = css::Color {
        r: 236,
        g: 239,
        b: 244,
        a: 255,
    };
    let tab_strip_bg = css::Color {
        r: 222,
        g: 225,
        b: 230,
        a: 255,
    };
    let mut commands = vec![
        // Tab strip sits behind everything else and is the darker band of chrome.
        render::DisplayCommand::SolidRect(
            tab_strip_bg,
            layout::Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: TAB_STRIP_HEIGHT,
            },
        ),
        // Toolbar fills the rest of the chrome with the lighter foreground color.
        render::DisplayCommand::SolidRect(
            toolbar_bg,
            layout::Rect {
                x: 0.0,
                y: TAB_STRIP_HEIGHT,
                width,
                height: TOOLBAR_HEIGHT,
            },
        ),
        // Active tab paints in the same color as the toolbar so the two surfaces merge
        // seamlessly along the bottom edge while the rounded top corners show on the
        // darker strip.
        render::DisplayCommand::RoundedRect(
            toolbar_bg,
            layout::Rect {
                x: TAB_X,
                y: TAB_Y,
                width: TAB_WIDTH,
                height: TAB_HEIGHT,
            },
            render::CornerRadii {
                tl: TAB_RADIUS,
                tr: TAB_RADIUS,
                br: 0.0,
                bl: 0.0,
            },
        ),
        render::DisplayCommand::Text(render::TextCommand {
            text: chrome.tab_title.to_string(),
            x: TAB_TITLE_X,
            y: TAB_TITLE_Y,
            color: css::Color {
                r: 60,
                g: 64,
                b: 67,
                a: 255,
            },
            font_size: TAB_TITLE_FONT_SIZE,
        }),
        render::DisplayCommand::RoundedRect(border_color, address_box, pill_outer),
        render::DisplayCommand::RoundedRect(
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
            pill_inner,
        ),
        render::DisplayCommand::Text(render::TextCommand {
            text: address_display.clone(),
            x: ADDRESS_TEXT_X,
            y: ADDRESS_TEXT_Y,
            color: address_color,
            font_size: ADDRESS_FONT_SIZE,
        }),
        render::DisplayCommand::Text(render::TextCommand {
            text: chrome.status_text.to_string(),
            x: 16.0,
            y: STATUS_TEXT_Y,
            color: chrome.status_color,
            font_size: STATUS_FONT_SIZE,
        }),
    ];
    commands.extend(nav_button_commands(
        back_button_rect(),
        true,
        chrome.can_go_back,
        chrome.hovered_action == Some(ChromeAction::Back),
    ));
    commands.extend(nav_button_commands(
        forward_button_rect(),
        false,
        chrome.can_go_forward,
        chrome.hovered_action == Some(ChromeAction::Forward),
    ));
    commands.extend(refresh_button_commands(
        refresh_button_rect(),
        chrome.hovered_action == Some(ChromeAction::Refresh),
    ));
    commands.extend(menu_button_commands(
        menu_button_rect(width),
        chrome.hovered_action == Some(ChromeAction::Menu),
    ));

    if chrome.address_bar_selected {
        let measured = render::measure_text_width(&address_display, ADDRESS_FONT_SIZE, fonts);
        commands.push(render::DisplayCommand::SolidRect(
            css::Color {
                r: 214,
                g: 229,
                b: 255,
                a: 255,
            },
            layout::Rect {
                x: ADDRESS_TEXT_X - 2.0,
                y: ADDRESS_TEXT_Y - 2.0,
                width: (measured + 4.0).min((address_box.width - 8.0).max(0.0)),
                height: ADDRESS_FONT_SIZE + 4.0,
            },
        ));
        commands.push(render::DisplayCommand::Text(render::TextCommand {
            text: address_display,
            x: ADDRESS_TEXT_X,
            y: ADDRESS_TEXT_Y,
            color: css::Color::BLACK,
            font_size: ADDRESS_FONT_SIZE,
        }));
    } else if chrome.show_caret {
        // Caret position is measured from the *actual input* (empty when only the
        // placeholder is showing), and uses fontdue's advance widths so it lines
        // up with where draw_text actually ends — a fixed average glyph width
        // would always drift on proportional fonts.
        let caret_offset = if input_empty {
            0.0
        } else {
            render::measure_text_width(&address_display, ADDRESS_FONT_SIZE, fonts)
        };
        commands.push(render::DisplayCommand::SolidRect(
            css::Color::BLACK,
            layout::Rect {
                x: ADDRESS_TEXT_X + caret_offset,
                y: ADDRESS_TEXT_Y - 1.0,
                width: 1.0,
                height: ADDRESS_FONT_SIZE + 2.0,
            },
        ));
    }

    commands
}

fn nav_button_commands(
    rect: layout::Rect,
    pointing_left: bool,
    enabled: bool,
    hovered: bool,
) -> Vec<render::DisplayCommand> {
    // Toolbar buttons are flat by default and only paint a circular hover wash on rollover.
    let mut commands = Vec::new();
    if hovered && enabled {
        commands.push(render::DisplayCommand::RoundedRect(
            css::Color {
                r: 232,
                g: 234,
                b: 237,
                a: 255,
            },
            rect,
            render::CornerRadii::uniform(rect.height.min(rect.width) / 2.0),
        ));
    }

    let icon_color = if enabled {
        css::Color {
            r: 60,
            g: 64,
            b: 67,
            a: 255,
        }
    } else {
        css::Color {
            r: 154,
            g: 160,
            b: 166,
            a: 255,
        }
    };
    commands.extend(chevron_commands(rect, icon_color, pointing_left));
    commands
}

fn chevron_commands(
    rect: layout::Rect,
    color: css::Color,
    pointing_left: bool,
) -> Vec<render::DisplayCommand> {
    // Chevrons are seven 1px rows offset from the center line, forming a 2px-thick caret.
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    (0i32..7)
        .map(|row| {
            let dy = row - 3;
            let offset = dy.unsigned_abs() as f32;
            let x = if pointing_left {
                cx - 1.0 + offset
            } else {
                cx - 1.0 - offset
            };
            render::DisplayCommand::SolidRect(
                color,
                layout::Rect {
                    x,
                    y: cy - 3.0 + row as f32,
                    width: 2.0,
                    height: 1.0,
                },
            )
        })
        .collect()
}

fn refresh_button_commands(rect: layout::Rect, hovered: bool) -> Vec<render::DisplayCommand> {
    // Refresh glyph: ~330° arc opening at the top, plus a filled triangular
    // arrow head at the start of the arc pointing radially outward. The arc is
    // stamped by ~80 small squares (density scales with radius so the ring
    // never reads as dotted) — without a dedicated arc primitive in the
    // renderer this is the cleanest way to fake a curve.
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_6, TAU};

    let mut commands = Vec::new();
    if hovered {
        commands.push(render::DisplayCommand::RoundedRect(
            css::Color {
                r: 232,
                g: 234,
                b: 237,
                a: 255,
            },
            rect,
            render::CornerRadii::uniform(rect.height.min(rect.width) / 2.0),
        ));
    }

    let icon_color = css::Color {
        r: 60,
        g: 64,
        b: 67,
        a: 255,
    };
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let radius = (rect.width.min(rect.height) / 2.0 - 5.0).max(4.0);

    // Arc starts ~30° past 12 o'clock (top-right), sweeps clockwise around back
    // to ~30° before 12 o'clock — leaves a clean 60° gap at the top for the
    // arrow head to sit in.
    let arc_start = -FRAC_PI_2 + FRAC_PI_6;
    let arc_total = TAU - 2.0 * FRAC_PI_6;

    // Density chosen so adjacent stamps overlap by ~half their width — gives a
    // visually continuous stroke instead of dotted-line.
    let stops = (radius * 8.0).ceil() as i32;
    for i in 0..=stops {
        let t = i as f32 / stops as f32;
        let theta = arc_start + t * arc_total;
        let x = cx + theta.cos() * radius;
        let y = cy + theta.sin() * radius;
        commands.push(render::DisplayCommand::SolidRect(
            icon_color,
            layout::Rect {
                x: x - 1.0,
                y: y - 1.0,
                width: 2.0,
                height: 2.0,
            },
        ));
    }

    // Triangular arrow head at the arc's start. Base sits on the ring; tip
    // extends `arrow_len` pixels radially outward. Filled by stepping along
    // the radial axis and stamping a 1px-tall band whose width tapers with
    // distance — a poor man's flat-shaded triangle.
    let nx = arc_start.cos();
    let ny = arc_start.sin();
    let tx = -ny;
    let ty = nx;
    let base_cx = cx + radius * nx;
    let base_cy = cy + radius * ny;
    let arrow_len = 5.0_f32;
    let arrow_half = 3.0_f32;
    let steps = arrow_len.ceil() as i32 + 1;
    for i in 0..=steps {
        let progress = i as f32 / steps as f32;
        let half_width = arrow_half * (1.0 - progress);
        let dist = arrow_len * progress;
        let row_cx = base_cx + dist * nx;
        let row_cy = base_cy + dist * ny;
        let span = half_width.ceil() as i32;
        for j in -span..=span {
            if (j as f32).abs() > half_width + 0.5 {
                continue;
            }
            let px = row_cx + (j as f32) * tx;
            let py = row_cy + (j as f32) * ty;
            commands.push(render::DisplayCommand::SolidRect(
                icon_color,
                layout::Rect {
                    x: px - 0.5,
                    y: py - 0.5,
                    width: 1.0,
                    height: 1.0,
                },
            ));
        }
    }

    commands
}

fn menu_button_commands(rect: layout::Rect, hovered: bool) -> Vec<render::DisplayCommand> {
    // Three vertical dots stand in for the overflow menu. The dropdown is still a stub,
    // but the hover wash and click hit-test are wired so the button feels real.
    let mut commands = Vec::new();
    if hovered {
        commands.push(render::DisplayCommand::RoundedRect(
            css::Color {
                r: 232,
                g: 234,
                b: 237,
                a: 255,
            },
            rect,
            render::CornerRadii::uniform(rect.height.min(rect.width) / 2.0),
        ));
    }

    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let dot_size = 3.0;
    let spacing = 5.0;
    let icon_color = css::Color {
        r: 60,
        g: 64,
        b: 67,
        a: 255,
    };

    commands.extend((-1..=1i32).map(|i| {
        render::DisplayCommand::RoundedRect(
            icon_color,
            layout::Rect {
                x: cx - dot_size / 2.0,
                y: cy + (i as f32 * spacing) - dot_size / 2.0,
                width: dot_size,
                height: dot_size,
            },
            render::CornerRadii::uniform(dot_size / 2.0),
        )
    }));
    commands
}

fn address_bar_rect(viewport_width: f32) -> layout::Rect {
    let menu_reserved = MENU_BUTTON_RIGHT_PAD + MENU_BUTTON_WIDTH + MENU_BUTTON_GAP;
    layout::Rect {
        x: ADDRESS_BOX_X,
        y: ADDRESS_BOX_Y,
        width: (viewport_width - ADDRESS_BOX_X - menu_reserved).max(0.0),
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

fn refresh_button_rect() -> layout::Rect {
    layout::Rect {
        x: REFRESH_BUTTON_X,
        y: NAV_BUTTON_Y,
        width: NAV_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
    }
}

fn menu_button_rect(viewport_width: f32) -> layout::Rect {
    layout::Rect {
        x: viewport_width - MENU_BUTTON_RIGHT_PAD - MENU_BUTTON_WIDTH,
        y: NAV_BUTTON_Y,
        width: MENU_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
    }
}

fn document_height(commands: &[render::DisplayCommand]) -> f32 {
    commands.iter().fold(0.0, |max_bottom, command| {
        let bottom = command_bottom(command);
        max_bottom.max(bottom)
    })
}

fn command_bottom(command: &render::DisplayCommand) -> f32 {
    match command {
        render::DisplayCommand::SolidRect(_, rect) => rect.y + rect.height,
        render::DisplayCommand::RoundedRect(_, rect, _) => rect.y + rect.height,
        render::DisplayCommand::Text(text) => text.y + text.font_size,
        render::DisplayCommand::Image(image) => image.y + image.height,
        render::DisplayCommand::Gradient(gradient) => gradient.rect.y + gradient.rect.height,
        render::DisplayCommand::BoxShadow(shadow) => shadow.rect.y + shadow.rect.height,
        render::DisplayCommand::TransformGroup(transform, inner) => {
            // Logical bottom is the max-y of inner commands; map every
            // inner command's logical bbox through the matrix and take the
            // worst y of the four projected corners. Anything bigger is a
            // false positive here, but better that than under-reporting and
            // clipping a rotated element off the bottom of the document.
            inner
                .iter()
                .map(|cmd| projected_command_bottom(cmd, *transform))
                .fold(0.0_f32, f32::max)
        }
    }
}

fn projected_command_bottom(command: &render::DisplayCommand, transform: render::Affine) -> f32 {
    let bounds = match command {
        render::DisplayCommand::SolidRect(_, rect) => *rect,
        render::DisplayCommand::RoundedRect(_, rect, _) => *rect,
        render::DisplayCommand::Text(text) => layout::Rect {
            x: text.x,
            y: text.y,
            // Bitmap-rasterised text doesn't know its own width here; for
            // overflow purposes the font_size box is a safe upper bound.
            width: text.font_size,
            height: text.font_size,
        },
        render::DisplayCommand::Image(image) => layout::Rect {
            x: image.x,
            y: image.y,
            width: image.width,
            height: image.height,
        },
        render::DisplayCommand::Gradient(gradient) => gradient.rect,
        render::DisplayCommand::BoxShadow(shadow) => shadow.rect,
        // Inner TransformGroups should never appear in practice; treat as 0.
        render::DisplayCommand::TransformGroup(_, _) => return 0.0,
    };
    let corners = [
        transform.apply_point(bounds.x, bounds.y),
        transform.apply_point(bounds.x + bounds.width, bounds.y),
        transform.apply_point(bounds.x + bounds.width, bounds.y + bounds.height),
        transform.apply_point(bounds.x, bounds.y + bounds.height),
    ];
    corners
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::NEG_INFINITY, f32::max)
}

fn collect_link_targets(
    layout_box: &layout::LayoutBox,
    inherited_href: Option<&str>,
    inherited_no_underline: bool,
    inherited_transform: render::Affine,
) -> Vec<LinkTarget> {
    let own_href = href_for_layout_box(layout_box);
    let current_href = own_href.or(inherited_href);
    // text-decoration: none on any ancestor (typically the <a> itself) suppresses
    // underlines for everything below it.
    let no_underline = inherited_no_underline || has_text_decoration_none(layout_box);
    // Compose this box's own `transform` onto the inherited matrix the same
    // way the paint pass does. The link rect is stored in screen space so
    // click hit-testing and underline drawing can stay axis-aligned for the
    // translate-only support shipping in this commit.
    let effective_transform = inherited_transform.compose(render::transform_for(layout_box));
    let mut targets = Vec::new();

    // Link targets are collected separately from display commands because clicking needs rectangles,
    // not just painted pixels.
    if let Some(href) = current_href.filter(|_| should_collect_link_target(layout_box, own_href)) {
        let content = layout_box.dimensions.content;
        let (x, y) = effective_transform.apply_point(content.x, content.y);
        targets.push(LinkTarget {
            href: href.to_string(),
            rect: layout::Rect {
                x,
                y,
                width: content.width,
                height: content.height,
            },
            underline: own_href.is_none() && !no_underline,
        });
    }

    for child in &layout_box.children {
        targets.extend(collect_link_targets(
            child,
            current_href,
            no_underline,
            effective_transform,
        ));
    }

    targets
}

fn has_text_decoration_none(layout_box: &layout::LayoutBox) -> bool {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(node)
        | layout::BoxType::FlexNode(node)
        | layout::BoxType::GridNode(node) => matches!(
            node.value("text-decoration"),
            Some(css::Value::Keyword(keyword)) if keyword == "none"
        ),
        layout::BoxType::AnonymousBlock => false,
    }
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
        layout::BoxType::BlockNode(styled_node)
            | layout::BoxType::FlexNode(styled_node)
            | layout::BoxType::GridNode(styled_node)
            if matches!(styled_node.node.node_type, NodeType::Text(_))
    )
}

fn href_for_layout_box(layout_box: &layout::LayoutBox) -> Option<&str> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(styled_node)
        | layout::BoxType::FlexNode(styled_node)
        | layout::BoxType::GridNode(styled_node) => match &styled_node.node.node_type {
            NodeType::Element(element) => element.attributes.get("href").map(String::as_str),
            NodeType::Text(_) => None,
        },
        layout::BoxType::AnonymousBlock => None,
    }
}

fn src_for_layout_box(layout_box: &layout::LayoutBox) -> Option<&str> {
    match &layout_box.box_type {
        layout::BoxType::BlockNode(styled_node)
        | layout::BoxType::FlexNode(styled_node)
        | layout::BoxType::GridNode(styled_node) => match &styled_node.node.node_type {
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

fn compute_hovered_dom_path(
    input: &window::WindowInput,
    layout_root: &layout::LayoutBox,
    scroll_offset: f32,
) -> Option<Vec<usize>> {
    // Hover is only meaningful when the pointer is over the page area (i.e. below the
    // chrome). Anywhere else — chrome, off-window — leaves the styled tree in its
    // "nothing hovered" state.
    let (mouse_x, mouse_y) = input.mouse_position?;
    if mouse_y < CHROME_HEIGHT {
        return None;
    }
    let doc_y = mouse_y - CHROME_HEIGHT + scroll_offset;

    // Walk the layout tree depth-first, tracking the path of child indices. Layout child
    // positions mirror DOM child positions (no anonymous boxes are created today), so the
    // path doubles as a DOM path. The deepest containing box wins by virtue of being
    // visited last.
    let mut best: Option<Vec<usize>> = None;
    let mut path: Vec<usize> = Vec::new();
    walk_for_hover(
        layout_root,
        mouse_x,
        doc_y,
        render::Affine::IDENTITY,
        &mut path,
        &mut best,
    );
    best
}

fn walk_for_hover(
    layout_box: &layout::LayoutBox,
    mouse_x: f32,
    doc_y: f32,
    inherited_transform: render::Affine,
    path: &mut Vec<usize>,
    best: &mut Option<Vec<usize>>,
) {
    // Compose this box's own `transform` onto the inherited matrix, then map
    // the screen-space cursor back into the box's logical coordinates so the
    // padding-box compare can stay axis-aligned. Pages without `transform`
    // keep the matrix at identity, so the inverse + apply collapse to a no-op.
    let effective_transform = inherited_transform.compose(render::transform_for(layout_box));
    let (logical_x, logical_y) = effective_transform.inverse().apply_point(mouse_x, doc_y);
    let outer = padding_box(layout_box);
    if point_in_rect(logical_x, logical_y, outer) {
        *best = Some(path.clone());
    }
    for (idx, child) in layout_box.children.iter().enumerate() {
        path.push(idx);
        walk_for_hover(child, mouse_x, doc_y, effective_transform, path, best);
        path.pop();
    }
}

fn padding_box(layout_box: &layout::LayoutBox) -> layout::Rect {
    let dims = &layout_box.dimensions;
    let content = dims.content;
    let pad = dims.padding;
    layout::Rect {
        x: content.x - pad.left,
        y: content.y - pad.top,
        width: content.width + pad.left + pad.right,
        height: content.height + pad.top + pad.bottom,
    }
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
    // The default landing page mimics Chrome's new tab page so the browser has
    // something visually meaningful to show before any URL is entered.
    r#"
        <div id="ntp">
            <div class="logo">mini browser</div>
            <div class="search-pill">Search the web or type a URL</div>
            <div class="shortcuts">
                <a href="https://example.com" class="tile">example</a>
                <a href="https://www.rust-lang.org" class="tile">rust</a>
                <a href="https://news.ycombinator.com" class="tile">hn</a>
                <a href="https://github.com" class="tile">github</a>
            </div>
        </div>
    "#
}

fn sample_css() -> &'static str {
    // Centering relies on the layout engine's new margin: auto + text-align: center
    // support, and the rounded surfaces rely on border-radius being wired through
    // the renderer. Together they sketch a Chrome-NTP silhouette without leaving
    // the block layout regime.
    r#"
        #ntp {
            width: 720px;
            margin-left: auto;
            margin-right: auto;
            padding-top: 48px;
            padding-bottom: 60px;
            text-align: center;
            background-color: #ffffff;
        }
        .logo {
            font-size: 48px;
            color: #5f6368;
            margin-bottom: 28px;
        }
        .search-pill {
            width: 472px;
            height: 22px;
            padding-top: 14px;
            margin-left: auto;
            margin-right: auto;
            margin-bottom: 40px;
            background-color: #f1f3f4;
            border-radius: 22px;
            color: #80868b;
            font-size: 14px;
        }
        .shortcuts {
            width: 600px;
            margin-left: auto;
            margin-right: auto;
        }
        .tile {
            width: 96px;
            height: 16px;
            padding-top: 36px;
            padding-bottom: 12px;
            padding-left: 8px;
            padding-right: 8px;
            background-color: #f1f3f4;
            border-radius: 12px;
            margin-left: 12px;
            margin-right: 12px;
            color: #3c4043;
            font-size: 12px;
            text-decoration: none;
        }
        .tile:hover {
            background-color: #e8eaed;
        }
        .tile:active {
            background-color: #dadce0;
        }
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

// Bundle of everything `load_remote_document` produces. The `HashMap<String, String>`
// holds external `<script src>` bodies keyed by the raw `src` attribute string;
// `install_document` looks them up by attribute when walking the DOM, so no extra
// URL resolution is needed at execution time.
type LoadedDocument = (
    String,
    String,
    HashMap<String, resource::LoadedImage>,
    Vec<Vec<u8>>,
    HashMap<String, String>,
    net::Url,
);

fn load_remote_document(raw_url: &str) -> Result<LoadedDocument, String> {
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
        return Ok((
            document_html,
            stylesheet,
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            final_url,
        ));
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
    let font_data = resource::load_fonts(&stylesheets, &final_url);
    let images = resource::load_images(&nodes, &final_url)
        .map_err(|error| describe_resource_error(&error))?
        .into_iter()
        .map(|image| (image.url.to_string(), image))
        .collect();
    let external_scripts = resource::load_scripts(&nodes, &final_url)
        .map_err(|error| describe_resource_error(&error))?;
    Ok((
        html,
        stylesheets.join("\n"),
        images,
        font_data,
        external_scripts,
        final_url,
    ))
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
            Ok((document_html, stylesheet, images, font_data, external_scripts, current_url)) => {
                BrowserState::new(
                    raw_url,
                    document_html,
                    stylesheet,
                    images,
                    font_data,
                    external_scripts,
                    Some(current_url),
                    "loaded",
                )
            }
            Err(error) => {
                eprintln!("{error}");
                let mut state = BrowserState::new(
                    raw_url,
                    String::new(),
                    String::new(),
                    HashMap::new(),
                    Vec::new(),
                    HashMap::new(),
                    None,
                    "load failed",
                );
                state.show_error_page("load failed", &error);
                state
            }
        },
        None => BrowserState::new(
            // NTP starts with an empty address bar so the placeholder text shows in
            // muted gray rather than as a real URL the user appears to have typed.
            String::new(),
            sample_html().to_string(),
            sample_css().to_string(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        ),
    }
}

fn build_font_cache(font_data: &[Vec<u8>]) -> Vec<fontdue::Font> {
    let mut fonts: Vec<fontdue::Font> = font_data
        .iter()
        .filter_map(|data| {
            fontdue::Font::from_bytes(data.as_slice(), fontdue::FontSettings::default()).ok()
        })
        .collect();

    // Fall back to a macOS system font so pages without web fonts can still render Korean/CJK.
    if let Ok(system_font_bytes) = std::fs::read("/System/Library/Fonts/AppleSDGothicNeo.ttc")
        && let Ok(font) = fontdue::Font::from_bytes(
            system_font_bytes.as_slice(),
            fontdue::FontSettings {
                collection_index: 0,
                ..fontdue::FontSettings::default()
            },
        )
    {
        fonts.push(font);
    }

    fonts
}

fn main() {
    let mut browser = load_initial_state();
    let mut fonts = build_font_cache(&browser.font_data);
    let mut last_font_count = browser.font_data.len();

    if let Err(error) = window::run("mini-browser", 800, 600, |width, height, input| {
        // Rebuild font cache when navigation loads new fonts. Done before
        // display_list so chrome's caret-width measurement sees the fresh fonts.
        if browser.font_data.len() != last_font_count {
            fonts = build_font_cache(&browser.font_data);
            last_font_count = browser.font_data.len();
        }

        let commands = browser.display_list(width, height, input, &fonts);

        render::rasterize(&commands, width, height, &fonts)
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
        let links = collect_link_targets(&layout_tree, None, false, render::Affine::IDENTITY);

        assert_eq!(links.len(), 2);
        assert_eq!(links[0].href, "/next");
        assert_eq!(links[1].href, "/next");
        assert!(!links[0].underline);
        assert!(links[1].underline);
    }

    #[test]
    fn text_decoration_none_suppresses_link_underline() {
        let node = html::parse(r#"<a href="/next" class="tile">Hello</a>"#)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = css::parse(".tile { text-decoration: none; }").unwrap();
        let styled = style::style_tree(&node, &[stylesheet]);
        let layout_tree = layout::layout_tree(&styled, 300.0);
        let links = collect_link_targets(&layout_tree, None, false, render::Affine::IDENTITY);

        // Both the <a> target and the inherited text target keep their click rects, but the
        // text-decoration declaration on the <a> suppresses the underline that would normally
        // appear on the descendant text node.
        assert!(!links.is_empty());
        assert!(
            links.iter().all(|link| !link.underline),
            "no underline should be emitted when text-decoration is none"
        );
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
        // Address bar reserves space for the menu button on the right edge.
        let expected_width = 800.0
            - ADDRESS_BOX_X
            - (super::MENU_BUTTON_RIGHT_PAD + super::MENU_BUTTON_WIDTH + super::MENU_BUTTON_GAP);
        assert_eq!(rect.width, expected_width);
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
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        browser.commit_navigation(HistoryEntry {
            address_input: "http://second.test".into(),
            document_html: "<div>second</div>".into(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
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
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        let hover = browser.hovered_chrome_action(
            &window::WindowInput {
                mouse_position: Some((BACK_BUTTON_X + 2.0, NAV_BUTTON_Y + 2.0)),
                ..window::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, None);

        browser.back_stack.push(browser.snapshot());
        let hover = browser.hovered_chrome_action(
            &window::WindowInput {
                mouse_position: Some((back_button_rect().x + 2.0, back_button_rect().y + 2.0)),
                ..window::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, Some(super::ChromeAction::Back));
    }

    #[test]
    fn hovered_dom_path_picks_deepest_layout_box_under_mouse() {
        // Build a tiny tree where only one nested element exists; the hit-test should walk
        // down to it. <div id="root"><span class="leaf">x</span></div>
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let css_source = r#"
            #root { width: 100px; height: 80px; }
            .leaf { width: 40px; height: 20px; }
        "#;
        let node = mini_browser::html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = mini_browser::css::parse(css_source).unwrap();
        let styled = mini_browser::style::style_tree(&node, &[stylesheet]);
        let layout = mini_browser::layout::layout_tree(&styled, 800.0);

        // Mouse coordinates: window-space pointer over the leaf, accounting for the
        // chrome strip we subtract inside compute_hovered_dom_path.
        let leaf_window_y = super::CHROME_HEIGHT + 5.0;
        let path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((10.0, leaf_window_y)),
                ..window::WindowInput::default()
            },
            &layout,
            0.0,
        );

        // Layout root is #root, its first child is the .leaf span ([0]), and the span's
        // text "hi" is laid out as the next inline child ([0, 0]). The hit-test descends
        // to the deepest containing box, so the text node wins.
        assert_eq!(path, Some(vec![0, 0]));
    }

    #[test]
    fn hovered_dom_path_accounts_for_transform_translate() {
        // The leaf is shifted right by 50px via `transform: translate`. A pointer
        // at the leaf's *original* logical x should now MISS, while a pointer at
        // the post-translate screen x should HIT the leaf.
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let css_source = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 40px; height: 20px; transform: translate(50px, 0); }
        "#;
        let node = mini_browser::html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = mini_browser::css::parse(css_source).unwrap();
        let styled = mini_browser::style::style_tree(&node, &[stylesheet]);
        let layout = mini_browser::layout::layout_tree(&styled, 800.0);

        // Logical x = 10 (inside leaf's untransformed box) but cursor is in
        // *screen* space — after the leaf is translated by 50, screen x=10
        // no longer overlaps the leaf, only the root.
        let leaf_window_y = super::CHROME_HEIGHT + 5.0;
        let logical_path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((10.0, leaf_window_y)),
                ..window::WindowInput::default()
            },
            &layout,
            0.0,
        );
        // Root still covers the area around (10, 5); leaf does not. The
        // hit-test should pick the root, not the now-shifted leaf.
        assert_eq!(logical_path, Some(vec![]));

        // Cursor at screen x=60 lands on the post-translate leaf box.
        let translated_path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((60.0, leaf_window_y)),
                ..window::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(translated_path, Some(vec![0, 0]));
    }

    #[test]
    fn hovered_dom_path_accounts_for_transform_scale() {
        // The leaf is scaled 2x around its centre. Its logical box is
        // 40x20 at (0, 0) inside the root's content area; after the scale
        // its visible box becomes 80x40 centered on (20, 10), so screen
        // x ∈ [-20, 60] and y ∈ [-10, 30] all hit it. (Only the part
        // overlapping the root will get hovered, since the deepest hit
        // wins.)
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let css_source = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 40px; height: 20px; transform: scale(2); }
        "#;
        let node = mini_browser::html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = mini_browser::css::parse(css_source).unwrap();
        let styled = mini_browser::style::style_tree(&node, &[stylesheet]);
        let layout = mini_browser::layout::layout_tree(&styled, 800.0);

        // Cursor at screen x=55: outside the leaf's *logical* 40-wide box,
        // but well inside the post-scale 80-wide visible box. Hit-test
        // should walk into the leaf (path [0]). The inner text glyph "hi"
        // does not extend to logical x=37.5, so we stop at the leaf and
        // not its text child.
        let leaf_window_y = super::CHROME_HEIGHT + 5.0;
        let path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((55.0, leaf_window_y)),
                ..window::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(path, Some(vec![0]));

        // Sanity: with no transform, screen x=55 lies *outside* the
        // unscaled 40-wide leaf, so hit-test returns the root path. This
        // confirms the leaf hit above is genuinely caused by the scale.
        let no_transform_html = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let no_transform_css = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 40px; height: 20px; }
        "#;
        let plain_node = mini_browser::html::parse(no_transform_html)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let plain_sheet = mini_browser::css::parse(no_transform_css).unwrap();
        let plain_styled = mini_browser::style::style_tree(&plain_node, &[plain_sheet]);
        let plain_layout = mini_browser::layout::layout_tree(&plain_styled, 800.0);
        let plain_path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((55.0, leaf_window_y)),
                ..window::WindowInput::default()
            },
            &plain_layout,
            0.0,
        );
        assert_eq!(plain_path, Some(vec![]));
    }

    #[test]
    fn hovered_dom_path_accounts_for_transform_rotate() {
        // Rotate a 20×20 square leaf 45° around its centre (10, 10). The
        // rotated diamond extends beyond the leaf's logical x range (out
        // to ~24) along the screen axis, so a cursor parked at screen
        // (23, 10) must hit the leaf even though the same cursor would
        // miss the unrotated 20×20 box.
        // Use a div with no text child so the deepest hit is unambiguously
        // the leaf — adding text would introduce an inline-flow child whose
        // own box also picks up the inherited rotation transform, and it is
        // separately interesting to track its post-transform extent.
        let html_source = r#"<div id="root"><div class="leaf"></div></div>"#;
        let css_source = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 20px; height: 20px; transform: rotate(45deg); }
        "#;
        let node = mini_browser::html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let stylesheet = mini_browser::css::parse(css_source).unwrap();
        let styled = mini_browser::style::style_tree(&node, &[stylesheet]);
        let layout = mini_browser::layout::layout_tree(&styled, 800.0);

        let leaf_window_y = super::CHROME_HEIGHT + 10.0;
        let path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((23.0, leaf_window_y)),
                ..window::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(path, Some(vec![0]));

        // Sanity: with no rotation, the same cursor lands outside the leaf
        // and the deepest hit is the root.
        let plain_html = r#"<div id="root"><div class="leaf"></div></div>"#;
        let plain_css = r#"
            #root { width: 200px; height: 80px; }
            .leaf { width: 20px; height: 20px; }
        "#;
        let plain_node = mini_browser::html::parse(plain_html)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let plain_sheet = mini_browser::css::parse(plain_css).unwrap();
        let plain_styled = mini_browser::style::style_tree(&plain_node, &[plain_sheet]);
        let plain_layout = mini_browser::layout::layout_tree(&plain_styled, 800.0);
        let plain_path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((23.0, leaf_window_y)),
                ..window::WindowInput::default()
            },
            &plain_layout,
            0.0,
        );
        assert_eq!(plain_path, Some(vec![]));
    }

    #[test]
    fn hovered_dom_path_returns_none_when_pointer_is_in_chrome() {
        let html_source = r#"<div id="root"><span class="leaf">hi</span></div>"#;
        let node = mini_browser::html::parse(html_source)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let styled = mini_browser::style::style_tree(&node, &[]);
        let layout = mini_browser::layout::layout_tree(&styled, 800.0);

        // Pointer parked above the chrome cutoff — there is no page element to hover.
        let path = super::compute_hovered_dom_path(
            &window::WindowInput {
                mouse_position: Some((10.0, super::CHROME_HEIGHT - 1.0)),
                ..window::WindowInput::default()
            },
            &layout,
            0.0,
        );
        assert_eq!(path, None);
    }

    #[test]
    fn refresh_button_is_hover_able_without_current_url() {
        let browser = BrowserState::new(
            String::new(),
            String::new(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        );
        let refresh_rect = super::refresh_button_rect();
        let hover = browser.hovered_chrome_action(
            &window::WindowInput {
                mouse_position: Some((refresh_rect.x + 2.0, refresh_rect.y + 2.0)),
                ..window::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, Some(super::ChromeAction::Refresh));
    }

    #[test]
    fn refresh_without_current_url_sets_status_and_does_not_fetch() {
        // On the NTP there is no document to reload — the click should land cleanly with
        // a status hint rather than triggering an empty-URL network fetch.
        let mut browser = BrowserState::new(
            String::new(),
            "<div>ntp</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        );
        let original_html = browser.document_html.clone();
        let refresh_rect = super::refresh_button_rect();

        browser.apply_input(
            &window::WindowInput {
                mouse_position: Some((refresh_rect.x + 2.0, refresh_rect.y + 2.0)),
                left_mouse_pressed: true,
                ..window::WindowInput::default()
            },
            800,
            600,
        );

        assert_eq!(browser.status_text, "nothing to refresh");
        assert_eq!(browser.document_html, original_html);
    }

    #[test]
    fn menu_button_hover_is_independent_of_history() {
        let browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );

        let menu_rect = super::menu_button_rect(800.0);
        let hover = browser.hovered_chrome_action(
            &window::WindowInput {
                mouse_position: Some((menu_rect.x + 2.0, menu_rect.y + 2.0)),
                ..window::WindowInput::default()
            },
            800,
        );
        assert_eq!(hover, Some(super::ChromeAction::Menu));
    }

    #[test]
    fn clicking_menu_button_sets_status_and_does_not_navigate() {
        let mut browser = BrowserState::new(
            "http://first.test".into(),
            "<div>first</div>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "loaded",
        );
        let original_html = browser.document_html.clone();
        let menu_rect = super::menu_button_rect(800.0);

        browser.apply_input(
            &window::WindowInput {
                mouse_position: Some((menu_rect.x + 2.0, menu_rect.y + 2.0)),
                left_mouse_pressed: true,
                ..window::WindowInput::default()
            },
            800,
            600,
        );

        // Menu click registers as a chrome action: status flips to the stub label and the
        // current document is left untouched (no fall-through to page link handling).
        assert_eq!(browser.status_text, "menu (todo)");
        assert_eq!(browser.document_html, original_html);
    }

    fn browser_with_html(html: &str) -> BrowserState {
        BrowserState::new(
            "about:blank".into(),
            html.into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        )
    }

    #[test]
    fn inline_script_runs_during_construction() {
        let mut browser = browser_with_html("<script>var phase2 = 42;</script>");
        assert_eq!(browser.js.execute("phase2").unwrap(), "42");
    }

    #[test]
    fn inline_scripts_execute_in_document_order() {
        let mut browser = browser_with_html(
            "<script>var n = 1;</script><div><script>n = n + 5;</script></div>",
        );
        assert_eq!(browser.js.execute("n").unwrap(), "6");
    }

    #[test]
    fn navigation_resets_js_runtime() {
        let mut browser = browser_with_html("<script>var leaked = 'first';</script>");
        assert_eq!(browser.js.execute("leaked").unwrap(), "\"first\"");
        // install_document funnels every navigation/back-forward; it must clear
        // page-defined globals so the next document starts clean.
        browser.install_document("<p>second</p>".into(), String::new(), HashMap::new());
        assert!(browser.js.execute("leaked").is_err());
    }

    fn browser_with_externals(html: &str, externals: HashMap<String, String>) -> BrowserState {
        BrowserState::new(
            "about:blank".into(),
            html.into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            externals,
            None,
            "",
        )
    }

    #[test]
    fn external_script_body_runs_when_present_in_externals_map() {
        let externals = HashMap::from([("lib.js".to_string(), "var lib = 7;".to_string())]);
        let mut browser = browser_with_externals(r#"<script src="lib.js"></script>"#, externals);
        assert_eq!(browser.js.execute("lib").unwrap(), "7");
    }

    #[test]
    fn external_script_with_missing_body_is_silently_skipped() {
        // Empty externals map simulates a fetch failure — `missing.js` simply has
        // no entry. The browser must not error; later inline scripts must still run.
        let mut browser = browser_with_externals(
            r#"<script src="missing.js"></script><script>var still_ran = 1;</script>"#,
            HashMap::new(),
        );
        assert_eq!(browser.js.execute("still_ran").unwrap(), "1");
    }

    #[test]
    fn inline_script_can_read_dom_via_document_get_element_by_id() {
        // Run a <script> that depends on `document.getElementById` resolving
        // against the page's parsed DOM. Confirms the browser-level wiring
        // (BrowserState::run_scripts → js.bind_document → js.execute) hands
        // the engine the document it just installed, not an empty tree.
        let mut browser = browser_with_html(
            r#"<div id="hero">welcome</div><script>var greeting = document.getElementById('hero').textContent;</script>"#,
        );
        assert_eq!(browser.js.execute("greeting").unwrap(), "\"welcome\"");
    }

    #[test]
    fn navigation_rebinds_dom_for_next_document() {
        // After install_document a new page, the same JS APIs must resolve
        // against the new DOM. Catches a regression where bind_document is
        // forgotten on the second-and-later install path.
        let mut browser = browser_with_html(r#"<p id="x">first</p>"#);
        assert_eq!(
            browser
                .js
                .execute("document.getElementById('x').textContent")
                .unwrap(),
            "\"first\""
        );
        browser.install_document(
            r#"<p id="y">second</p>"#.into(),
            String::new(),
            HashMap::new(),
        );
        assert_eq!(
            browser
                .js
                .execute("document.getElementById('x')")
                .unwrap(),
            "null"
        );
        assert_eq!(
            browser
                .js
                .execute("document.getElementById('y').textContent")
                .unwrap(),
            "\"second\""
        );
    }

    #[test]
    fn inline_and_external_scripts_execute_in_document_order() {
        // The order must be: inline 'a' → external 'b' → inline 'c'. If externals
        // were appended after all inlines (or vice versa), `seq` would not be "abc".
        let externals = HashMap::from([("b.js".to_string(), r#"seq += "b";"#.to_string())]);
        let mut browser = browser_with_externals(
            r#"<script>var seq = "a";</script><script src="b.js"></script><script>seq += "c";</script>"#,
            externals,
        );
        assert_eq!(browser.js.execute("seq").unwrap(), "\"abc\"");
    }
}
