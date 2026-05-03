// BrowserState owns everything the per-frame loop has to keep coherent: the
// parsed document/CSS caches, the JS runtime that shares the document arena,
// the address bar / scroll / history state, and the `display_list` driver
// that calls into `crate::display_list` for view building and `crate::chrome`
// for chrome painting. Pure helpers (sample HTML/CSS, the env-arg loader, the
// font cache builder) live at the bottom so `main` can stay a one-liner.

use std::{cell::RefCell, collections::HashMap, env, rc::Rc};

use crate::{
    chrome::{
        CHROME_HEIGHT, ChromeAction, ChromeState, address_bar_rect, back_button_rect,
        chrome_commands, forward_button_rect, menu_button_rect, refresh_button_rect,
    },
    css,
    display_list::{
        DocumentView, LinkTarget, build_document_view, caret_commands_for_focused_input,
        compute_hovered_hit, document_height, link_decoration_commands, point_in_rect,
    },
    dom,
    dom::{NodeId, NodeType},
    html, js, layout,
    navigation::{error_document, load_remote_document},
    net, render, resource, style, window,
};

#[derive(Debug)]
pub struct BrowserState {
    // Address bar and focus state for the tiny browser chrome.
    pub address_input: String,
    pub address_bar_focused: bool,
    pub address_bar_selected: bool,
    pub frame_index: usize,

    // The currently displayed document snapshot.
    pub document_html: String,
    pub stylesheet: String,
    // Parsed forms of `document_html` and `stylesheet`, kept in sync via
    // `install_document`. Caching the parsed trees here keeps the per-frame
    // pipeline from re-parsing the same HTML/CSS at 60 fps — both parses are
    // O(input size) and dominate the frame budget on non-trivial pages.
    //
    // The Document lives behind `Rc<RefCell<…>>` because `JsRuntime` shares
    // the same arena: JS-side mutations (createElement, appendChild, …) flow
    // through the shared handle and the next frame's style/layout pass picks
    // up the new tree without a re-parse. BrowserState owns the canonical Rc
    // and the runtime gets a clone in `install_document`.
    pub parsed_document: Rc<RefCell<dom::Document>>,
    pub parsed_stylesheet: css::Stylesheet,
    pub images: HashMap<String, resource::LoadedImage>,
    pub font_data: Vec<Vec<u8>>,
    pub current_url: Option<net::Url>,

    // UI state that is shown in the chrome.
    pub status_text: String,
    pub status_color: css::Color,
    pub scroll_offset: f32,

    // History stores whole snapshots so back/forward can restore instantly without refetching.
    pub back_stack: Vec<HistoryEntry>,
    pub forward_stack: Vec<HistoryEntry>,

    // DOM path of the element under the mouse, computed from the previous frame's layout
    // and fed into the next frame's style pass so :hover rules light up. Carries one frame
    // of latency, which is invisible at 60fps.
    pub hovered_dom_path: Option<Vec<usize>>,
    // DOM path of the most recently clicked page element. Persists across frames so
    // :focus rules keep matching after the click; cleared when the user clicks anywhere
    // outside the page (chrome buttons, the address bar, off-window).
    pub focused_dom_path: Option<Vec<usize>>,

    // True once the focused <input> has had its value mutated by a user
    // keystroke since focus arrived. Read on focus-change to decide
    // whether to fire `change` before `blur`; reset whenever focus
    // moves. Pure JS-driven `.value =` never sets this — per the HTML
    // spec, programmatic value assignments don't fire input/change.
    pub focused_input_dirty: bool,

    // JavaScript runtime. Globals (var bindings, declared functions) survive across
    // `<script>` tags within the same document but reset when the user navigates,
    // because `install_document` allocates a fresh runtime for the new page.
    pub js: js::JsRuntime,

    // Pre-fetched bodies for `<script src="…">` references in the current document,
    // keyed by the raw `src` attribute string (matches what the DOM walker sees).
    // Carried alongside `parsed_document` so that history restore can re-execute
    // every script without re-fetching from the network.
    pub external_scripts: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub address_input: String,
    pub document_html: String,
    pub stylesheet: String,
    pub images: HashMap<String, resource::LoadedImage>,
    pub font_data: Vec<Vec<u8>>,
    pub external_scripts: HashMap<String, String>,
    pub current_url: Option<net::Url>,
    pub status_text: String,
    pub status_color: css::Color,
}

impl BrowserState {
    // The arg list is wide because every per-document resource is hoisted to
    // the call site (so test code can build a state without going through the
    // network loader). Bundling these into a struct is a Phase 1-style
    // refactor we explicitly defer per the Phase 2 plan — adding JS without
    // churning unrelated surfaces.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        address_input: String,
        document_html: String,
        stylesheet: String,
        images: HashMap<String, resource::LoadedImage>,
        font_data: Vec<Vec<u8>>,
        external_scripts: HashMap<String, String>,
        current_url: Option<net::Url>,
        status_text: impl Into<String>,
    ) -> Self {
        let parsed_document = Rc::new(RefCell::new(
            html::parse(&document_html).unwrap_or_default(),
        ));
        let parsed_stylesheet = css::parse(&stylesheet).unwrap_or_default();
        // The runtime shares the document handle so JS-side mutations land
        // in the same arena BrowserState reads for layout.
        let js = js::JsRuntime::new(parsed_document.clone());
        // Bind the URL the script-bound globals (`location.href` etc.)
        // observe. Pages with no resolved URL yet (the about:blank
        // bootstrap and most tests) leave the buffer empty — every
        // location accessor collapses to "" in that state.
        if let Some(url) = current_url.as_ref() {
            js.set_location_url(url.to_string());
        }
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
            focused_input_dirty: false,
            js,
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
    pub fn install_document(
        &mut self,
        document_html: String,
        stylesheet: String,
        external_scripts: HashMap<String, String>,
    ) {
        // Replace the Document in place rather than swapping the Rc itself —
        // any external clones (currently just the now-stale JsRuntime's) get
        // dropped right after, but keeping the Rc identity stable means tests
        // and any future caller that holds on to the handle observe the new
        // tree without re-fetching the Rc.
        *self.parsed_document.borrow_mut() = html::parse(&document_html).unwrap_or_default();
        self.parsed_stylesheet = css::parse(&stylesheet).unwrap_or_default();
        self.document_html = document_html;
        self.stylesheet = stylesheet;
        self.external_scripts = external_scripts;
        // Each navigated document starts with a fresh JS runtime — globals from
        // the previous page should not leak into the new one. Back/forward also
        // route through here, so the same reset rule applies on history moves.
        // The new runtime takes a clone of the same Rc so JS mutations during
        // run_scripts land in the document we're about to render.
        self.js = js::JsRuntime::new(self.parsed_document.clone());
        // Mirror the live current_url into the new runtime so the
        // `location` global reflects the page the script is running
        // against. Callers that swap `current_url` must do so *before*
        // calling `install_document`; restore_entry and reload_current
        // both honour that contract.
        self.js.set_location_url(
            self.current_url
                .as_ref()
                .map(|u| u.to_string())
                .unwrap_or_default(),
        );
        // Each navigated document starts with a clean dirty flag —
        // a value edited on the previous page must not be allowed to
        // ride into the new page and trigger a spurious `change` on
        // the next focus move.
        self.focused_input_dirty = false;
        self.run_scripts();
    }

    // Walks the parsed document in tree order and runs every `<script>` tag
    // through the JS runtime. Inline scripts use their text-child content;
    // external scripts (with a `src` attribute) look up their pre-fetched body
    // in `external_scripts`, keyed by the raw `src` value. Lookups that miss
    // (network failure, missing entry) are silently dropped — same degradation
    // pattern as broken stylesheets / images.
    fn run_scripts(&mut self) {
        // Collect script bodies under a short-lived borrow so JS execution
        // (which may take a borrow_mut via the shared Document handle to
        // mutate the DOM) doesn't overlap with our read.
        let mut sources: Vec<(String, String)> = Vec::new();
        let page_url = self
            .current_url
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "about:blank".to_string());
        {
            let document = self.parsed_document.borrow();
            let mut inline_counter = 0usize;
            for &root in document.roots() {
                collect_script_sources(
                    &document,
                    root,
                    &self.external_scripts,
                    &page_url,
                    &mut inline_counter,
                    &mut sources,
                );
            }
        }
        for (source, url) in sources {
            if let Err(err) = self.js.execute_with_url(&source, &url) {
                eprintln!("script error: {err}");
            }
        }
    }

    pub fn display_list(
        &mut self,
        viewport_width: usize,
        viewport_height: usize,
        input: &window::WindowInput,
        fonts: &[fontdue::Font],
    ) -> Vec<render::DisplayCommand> {
        // The browser re-builds its visible scene every frame from current state + fresh input.
        self.frame_index = self.frame_index.wrapping_add(1);
        self.apply_input(input, viewport_width, viewport_height);

        // Step 7 async: pump the JS event loop once per frame *before* the
        // layout pass. Timers/microtasks that came due since the previous
        // frame run first, then queued requestAnimationFrame callbacks fire
        // (Boa's wall clock makes "due" align with `setTimeout` deadlines).
        // Running both before the borrow on `parsed_document` below means
        // any DOM mutations the handlers perform feed into this very
        // frame's style/layout — no one-frame lag like :hover has.
        self.js.drain_pending_jobs();
        self.js.run_animation_frame_callbacks();

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
        // Borrow the shared Document just long enough to build the layout —
        // dropping the Ref before unwrap_or_else lets the error path call
        // back into &mut self (set_status). No JS executes during display_list
        // so we don't have to worry about an interleaved borrow_mut here.
        let layout_result = {
            let document = self.parsed_document.borrow();
            build_document_view(
                &document,
                &self.parsed_stylesheet,
                viewport_width,
                self.current_url.as_ref(),
                &self.images,
                interaction,
            )
        };
        let document_view = layout_result.unwrap_or_else(|build_error| {
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

        // Compute the deepest layout-box hit once: `path` feeds the next
        // frame's :hover/:focus styling, `node_id` feeds the click dispatch
        // below. Doing it before `clicked_link` matters because link
        // navigation rebuilds the JS runtime — handlers must run against
        // the page they were registered on, not the next one.
        let hover_hit =
            compute_hovered_hit(input, &document_view.layout_root, self.scroll_offset);

        // Page-area clicks fire JS click handlers on the live page first,
        // then fall through to link navigation unless a handler called
        // `event.preventDefault()`. Dispatch returns true in that case;
        // the toy's first "JS suppresses a default browser action" path.
        let click_in_page = input.left_mouse_pressed
            && input.mouse_position.is_some_and(|(_, y)| y >= CHROME_HEIGHT);
        let click_node_id = hover_hit.as_ref().and_then(|hit| hit.node_id);
        let click_default_prevented = if click_in_page
            && let Some(node_id) = click_node_id
        {
            self.js.dispatch_event(node_id, "click")
        } else {
            false
        };

        // A click that lands on a default-submit `<button>` inside a
        // `<form>` resolves the form node now, so the post-dispatch
        // default action below can submit it. We do the resolution
        // here (before the borrow-mut path of navigation) but defer
        // the actual submit until we know preventDefault wasn't called.
        let submit_form_id = if click_in_page
            && let Some(id) = click_node_id
        {
            let document = self.parsed_document.borrow();
            find_default_submit_button(&document, id)
                .and_then(|btn_id| find_enclosing_form(&document, btn_id))
        } else {
            None
        };

        // Page clicks are handled after layout exists so hit testing
        // can use real rectangles. preventDefault on the click bubble
        // suppresses both default actions (form submit and link nav).
        // Form submit takes priority over link navigation since you
        // don't typically nest a default-submit button inside a link.
        if !click_default_prevented {
            if let Some(form_id) = submit_form_id {
                self.try_submit_form(form_id);
            } else if let Some(link_target) =
                self.clicked_link(input, &document_view.links)
            {
                let href = link_target.href.clone();
                self.navigate_to_href(&href);
            }
        }

        self.clamp_scroll(viewport_height, document_height(&document_view.commands));
        // The next frame's style pass picks up `hovered_dom_path` — a
        // deliberate one-frame lag that keeps style and layout strictly
        // forward, no double-pass per frame required.
        self.hovered_dom_path = hover_hit.map(|hit| hit.path);

        // A page-area click moves :focus to the just-hovered element; clicks
        // anywhere outside the page (chrome buttons, the address bar,
        // off-window) clear it. When the path actually changes we also fire
        // blur on the previously-focused element and focus on the new one
        // (non-bubbling per spec — handlers register directly, ancestors
        // shouldn't see the event).
        if input.left_mouse_pressed {
            let new_focus = match input.mouse_position {
                Some((_, mouse_y)) if mouse_y >= CHROME_HEIGHT => self.hovered_dom_path.clone(),
                _ => None,
            };
            if new_focus != self.focused_dom_path {
                // Resolve both old and new paths in a single short-lived
                // borrow so the dispatch calls below (which re-borrow the
                // shared Document via the JsRuntime) don't conflict.
                let (old_id, new_id) = {
                    let document = self.parsed_document.borrow();
                    (
                        self.focused_dom_path
                            .as_deref()
                            .and_then(|path| node_id_for_dom_path(&document, path)),
                        new_focus
                            .as_deref()
                            .and_then(|path| node_id_for_dom_path(&document, path)),
                    )
                };
                if let Some(id) = old_id {
                    // `change` fires when focus leaves an input whose
                    // value was edited during this focus session. Spec
                    // order is change-then-blur, and it bubbles (modern
                    // spec), so use `dispatch_event` not `dispatch_event_at`.
                    // The dirty flag is set only by user keystrokes —
                    // pure JS-driven `.value =` never trips it, matching
                    // the HTML spec's "user committed change" semantics.
                    if self.focused_input_dirty {
                        self.js.dispatch_event(id, "change");
                    }
                    self.js.dispatch_event_at(id, "blur");
                }
                // Reset for the next focus session whether or not we
                // fired change — the new input starts with a clean slate.
                self.focused_input_dirty = false;
                if let Some(id) = new_id {
                    self.js.dispatch_event_at(id, "focus");
                }
                self.focused_dom_path = new_focus;
            }
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
        // Page input caret rides on top of the page's own painted commands
        // and any link decorations, but underneath the chrome strip — same
        // z-order story as link underlines. Translation matches the page
        // commands so the caret scrolls with the input box it belongs to.
        let focused_node_id = self
            .focused_dom_path
            .as_deref()
            .and_then(|path| node_id_for_dom_path(&self.parsed_document.borrow(), path));
        commands.extend(render::translate(
            caret_commands_for_focused_input(
                &document_view.layout_root,
                focused_node_id,
                self.frame_index,
                fonts,
            ),
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

    pub fn apply_input(
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
        } else if let Some(focused_path) = self.focused_dom_path.clone() {
            // Address bar didn't claim the keystrokes — fire JS keyboard
            // events on the focused page element, then (if not prevented
            // and the target is an <input>) apply the default text-edit
            // action. Anything else — focused link, focused div, no focus
            // at all — still gets the keydown/keyup events but skips the
            // default action; Enter is reserved for #7 (form submit) and
            // currently has no default action of its own.
            self.dispatch_typed_keys(&focused_path, input);
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

    // Per-key dispatch path. For each typed character (and Backspace /
    // Enter) we fire a `keydown`, run the default action only if no
    // handler called `preventDefault()` AND the target is an <input>,
    // and finish with a `keyup`. Events bubble per spec. The default
    // action for a typed character is "append the char to the input's
    // `value` attribute"; for Backspace it's "pop the last char". Enter
    // dispatches but has no default action yet — #7 (form submit) is
    // where that gets wired in. Non-input focus (a div picked up by a
    // future tab path, etc.) still receives the events; only the value
    // mutation is gated by the tag check.
    fn dispatch_typed_keys(&mut self, focused_path: &[usize], input: &window::WindowInput) {
        if input.typed.is_empty() && !input.backspace_pressed && !input.enter_pressed {
            return;
        }
        let Some(focused_id) =
            node_id_for_dom_path(&self.parsed_document.borrow(), focused_path)
        else {
            return;
        };

        for ch in input.typed.chars() {
            // `event.key` for printable characters is the character itself
            // ("a", " ", "2"); we surface even control chars (rare in the
            // typed buffer) but the default action drops them.
            let key = ch.to_string();
            let prevented = self.js.dispatch_keyboard_event(focused_id, "keydown", &key);
            if !prevented
                && !ch.is_control()
                && push_char_to_input_value(&self.parsed_document, focused_id, ch)
            {
                // Per spec, `input` fires after the value is updated and
                // before `keyup`. Bubbles, so handlers on ancestors see
                // it too. Only after a real mutation — control chars and
                // a tag mismatch leave the helper as a no-op and we skip
                // the event so observers don't see phantom changes.
                self.focused_input_dirty = true;
                self.js.dispatch_event(focused_id, "input");
            }
            self.js.dispatch_keyboard_event(focused_id, "keyup", &key);
        }

        if input.backspace_pressed {
            let prevented = self
                .js
                .dispatch_keyboard_event(focused_id, "keydown", "Backspace");
            if !prevented && pop_char_from_input_value(&self.parsed_document, focused_id) {
                // Backspace on an empty value reports no mutation, so we
                // skip the `input` event — real browsers do the same.
                self.focused_input_dirty = true;
                self.js.dispatch_event(focused_id, "input");
            }
            self.js.dispatch_keyboard_event(focused_id, "keyup", "Backspace");
        }

        if input.enter_pressed {
            // Enter splits two ways depending on the focused field:
            //   * <textarea>: insert a literal newline into the value
            //     buffer (the value-text paint splits on `\n` and the
            //     caret rides the trailing line).
            //   * Anything else: submit the enclosing <form>, if any.
            // preventDefault on keydown blocks both paths — same gate
            // as the typed-char path. Resolving the form is a separate
            // borrow from `try_submit_form` so the dispatch inside it
            // doesn't see an outstanding read borrow on the document.
            let prevented = self
                .js
                .dispatch_keyboard_event(focused_id, "keydown", "Enter");
            if !prevented {
                let is_textarea = matches!(
                    self.parsed_document
                        .borrow()
                        .element_data(focused_id)
                        .map(|elem| elem.tag_name.as_str()),
                    Some("textarea")
                );
                if is_textarea {
                    if push_char_to_input_value(&self.parsed_document, focused_id, '\n') {
                        self.focused_input_dirty = true;
                        self.js.dispatch_event(focused_id, "input");
                    }
                } else {
                    let form_id =
                        find_enclosing_form(&self.parsed_document.borrow(), focused_id);
                    if let Some(form_id) = form_id {
                        self.try_submit_form(form_id);
                    }
                }
            }
            self.js.dispatch_keyboard_event(focused_id, "keyup", "Enter");
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

    // Synthesise a `submit` event on `form_id` and, if no handler
    // calls `preventDefault()`, run the form's default action: a GET
    // navigation to `action` with the URL-encoded form data appended
    // as a query string. POST is intentionally not wired yet — once
    // the network goal lands (#15-17 in the Notion plan) it can route
    // through here too. Empty action falls back to the current URL so
    // self-submitting forms (`<form>` with no action attribute) work.
    fn try_submit_form(&mut self, form_id: NodeId) {
        let prevented = self.js.dispatch_event(form_id, "submit");
        if prevented {
            return;
        }

        // Read form attributes + data under a short borrow so the
        // navigation path below (which may rebuild the JS runtime)
        // doesn't overlap with our read.
        let (action, method, data) = {
            let document = self.parsed_document.borrow();
            let Some(elem) = document.element_data(form_id) else {
                return;
            };
            let action = elem.attributes.get("action").cloned().unwrap_or_default();
            let method = elem
                .attributes
                .get("method")
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_else(|| "get".to_string());
            let data = collect_form_data(&document, form_id);
            (action, method, data)
        };

        if method != "get" {
            // POST/PUT/DELETE land in the network goal later; for now
            // log and skip so a misconfigured form doesn't silently
            // navigate as if it were GET.
            eprintln!("[form] method={method} not supported yet, skipping submit");
            return;
        }

        // Empty action means "self-post"; fall back to the current
        // URL so a query-string-only update still navigates somewhere.
        let target_action = if action.is_empty() {
            match self.current_url.as_ref() {
                Some(url) => url.to_string(),
                None => {
                    eprintln!("[form] empty action and no current url, skipping submit");
                    return;
                }
            }
        } else {
            action
        };

        let submit_url = build_form_submit_url(&target_action, &data);
        self.navigate_to_href(&submit_url);
    }

    fn navigate_to_href(&mut self, href: &str) {
        let resolved = match self.resolve_href(href) {
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

    pub fn snapshot(&self) -> HistoryEntry {
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
        // Set the URL before install_document so the new JsRuntime's
        // `location` accessors observe the restored page's URL on the
        // very first script execution rather than the stale previous
        // page (or empty buffer on the bootstrap path).
        self.current_url = entry.current_url;
        self.install_document(entry.document_html, entry.stylesheet, entry.external_scripts);
        self.images = entry.images;
        self.font_data = entry.font_data;
        self.status_text = entry.status_text;
        self.status_color = entry.status_color;
        self.scroll_offset = 0.0;
        self.address_bar_selected = false;
    }

    pub fn commit_navigation(&mut self, entry: HistoryEntry) {
        self.back_stack.push(self.snapshot());
        self.forward_stack.clear();
        self.restore_entry(entry);
    }

    pub fn go_back(&mut self) {
        if let Some(previous) = self.back_stack.pop() {
            self.forward_stack.push(self.snapshot());
            self.restore_entry(previous);
        }
    }

    pub fn go_forward(&mut self) {
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
                // Same install_document precondition as restore_entry:
                // current_url has to land first so the runtime's
                // `location` global picks up the reloaded URL instead
                // of the previous page's.
                self.current_url = Some(resolved_url);
                self.install_document(document_html, stylesheet, external_scripts);
                self.images = images;
                self.font_data = font_data;
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

    pub fn hovered_chrome_action(
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
// `out`, in document (tree) order. Each entry is paired with a label used
// for error reporting: external scripts (with a `src` attribute) keep the
// raw `src` value; inline scripts get a synthetic `{page_url}#inline-script-N`
// where `N` is a 1-based index incremented per inline script in document order.
// `inline_counter` is threaded through the recursion so siblings and nested
// trees share a single sequence — that keeps the labels stable across the
// whole document. A `src` whose body is missing from the map silently
// produces no entry (fetch failure already logged upstream). Recursion stops
// at the script tag itself so a `<script>` is captured exactly once.
fn collect_script_sources(
    document: &dom::Document,
    node_id: NodeId,
    externals: &HashMap<String, String>,
    page_url: &str,
    inline_counter: &mut usize,
    out: &mut Vec<(String, String)>,
) {
    let Some(node) = document.get(node_id) else {
        return;
    };
    if let NodeType::Element(elem) = &node.node_type
        && elem.tag_name.eq_ignore_ascii_case("script")
    {
        // HTML spec: only "classic" script types execute as JS. Real
        // pages routinely embed `<script type="application/ld+json">`
        // (SEO metadata) and `<script type="application/json">`
        // (config blocks); evaluating those as JS produces spurious
        // SyntaxErrors. ES modules (`type="module"`) need a module
        // loader the toy doesn't have — skipping them avoids running
        // module code in a script context where `import` / `export`
        // would also fail to parse.
        if !is_classic_script_type(elem.attributes.get("type").map(String::as_str)) {
            return;
        }
        if let Some(src) = elem.attributes.get("src") {
            if let Some(body) = externals.get(src) {
                out.push((body.clone(), src.clone()));
            }
            return;
        }
        let mut source = String::new();
        for child_id in &node.children {
            if let Some(NodeType::Text(text)) = document.get(*child_id).map(|n| &n.node_type) {
                source.push_str(text);
            }
        }
        if !source.trim().is_empty() {
            *inline_counter += 1;
            out.push((source, format!("{page_url}#inline-script-{inline_counter}")));
        }
        return;
    }
    for child in &node.children {
        collect_script_sources(document, *child, externals, page_url, inline_counter, out);
    }
}

// Whether a `<script type="...">` value identifies a *classic* script
// (i.e. the kind we should hand to the JS engine). Per HTML spec the
// type attribute's value, case-insensitively trimmed of leading and
// trailing ASCII whitespace, runs through these rules:
//
//   - missing or empty → classic
//   - "module" → ES module (we don't run it; future module-loader work)
//   - "importmap" → import map data (we don't run it)
//   - any of the JavaScript MIME-type aliases → classic
//   - anything else (json, ld+json, application/x-handlebars-template, …)
//     is a *data block* and must NOT execute
//
// The MIME alias list mirrors the spec's "JavaScript MIME type" table
// (text/javascript and friends). MIME parameters like ";version=1.7"
// are stripped before matching since real pages occasionally include
// them. Any leading/trailing whitespace on the attribute value is
// trimmed too — `type=" text/javascript "` is technically malformed
// but appears in older hand-written HTML.
fn is_classic_script_type(type_attr: Option<&str>) -> bool {
    let Some(value) = type_attr else {
        return true;
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Strip MIME parameters (everything after the first `;`) before
    // matching. The spec's "JavaScript MIME type" check is essentially
    // a lookup against the bare type/subtype.
    let bare = trimmed
        .split_once(';')
        .map(|(prefix, _)| prefix.trim())
        .unwrap_or(trimmed);
    matches!(
        bare.to_ascii_lowercase().as_str(),
        "application/ecmascript"
            | "application/javascript"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

pub fn page_step(viewport_height: usize) -> f32 {
    (viewport_height as f32 - CHROME_HEIGHT - 24.0).max(24.0)
}

// Append `ch` to the focused input's `value` attribute. Returns true when
// the value actually changed — that's the signal the caller uses to fire
// the `input` event and mark the input dirty. Silent no-op (returns false)
// when the slot has been removed by a previous handler or the focused
// element is not an `<input>` or `<textarea>`.
fn push_char_to_input_value(
    document: &Rc<RefCell<dom::Document>>,
    node_id: NodeId,
    ch: char,
) -> bool {
    let mut document = document.borrow_mut();
    let Some(elem) = document.element_data_mut(node_id) else {
        return false;
    };
    if !is_text_field_tag(&elem.tag_name) {
        return false;
    }
    let mut value = elem.attributes.get("value").cloned().unwrap_or_default();
    value.push(ch);
    elem.attributes.insert("value".into(), value);
    true
}

// Pop the last character off the focused field's `value` attribute.
// Returns true only when there was a character to pop — pop on an empty
// value reports false so the caller can skip the `input` event (real
// browsers don't fire `input` when there's no actual change).
fn pop_char_from_input_value(document: &Rc<RefCell<dom::Document>>, node_id: NodeId) -> bool {
    let mut document = document.borrow_mut();
    let Some(elem) = document.element_data_mut(node_id) else {
        return false;
    };
    if !is_text_field_tag(&elem.tag_name) {
        return false;
    }
    let mut value = elem.attributes.get("value").cloned().unwrap_or_default();
    if value.is_empty() {
        return false;
    }
    value.pop();
    elem.attributes.insert("value".into(), value);
    true
}

// Single source of truth for "is this element one of the typeable form
// controls the toy understands?". The typing path, the form-data
// collector, and the layout/render code all gate on the same set so
// that <textarea> picks up the same affordances <input> already had.
fn is_text_field_tag(tag: &str) -> bool {
    matches!(tag, "input" | "textarea")
}

// Walks the parent chain from `start` looking for the nearest
// `<button>` ancestor whose default action is "submit the form" —
// i.e. it has no explicit `type="button"` or `type="reset"`. The HTML
// spec defaults a `<button>` to `type="submit"` when the attribute is
// missing or unknown, and that's the case the click path turns into a
// form submission. Returns None when the click landed outside any
// button (or inside a non-submit one), in which case the caller falls
// through to link navigation / nothing.
fn find_default_submit_button(document: &dom::Document, start: NodeId) -> Option<NodeId> {
    let mut cur = Some(start);
    while let Some(id) = cur {
        let node = document.get(id)?;
        if let NodeType::Element(elem) = &node.node_type
            && elem.tag_name == "button"
        {
            let button_type = elem
                .attributes
                .get("type")
                .map(|s| s.to_ascii_lowercase());
            return match button_type.as_deref() {
                Some("button") | Some("reset") => None,
                _ => Some(id),
            };
        }
        cur = node.parent;
    }
    None
}

// Walks the parent chain from `start` looking for the nearest `<form>`
// ancestor. Returns the form's NodeId, or None if `start` isn't inside
// a form. Used by both the Enter-in-input path and the button-click
// path to figure out which form a submission targets.
fn find_enclosing_form(document: &dom::Document, start: NodeId) -> Option<NodeId> {
    let mut cur = Some(start);
    while let Some(id) = cur {
        let node = document.get(id)?;
        if let NodeType::Element(elem) = &node.node_type
            && elem.tag_name == "form"
        {
            return Some(id);
        }
        cur = node.parent;
    }
    None
}

// Walks the form subtree and collects (name, value) pairs from every
// text field (`<input>` / `<textarea>`) with a `name` attribute. Fields
// without `name` are not submittable per the HTML spec — same rule
// real browsers apply. Recursive so fields nested inside
// `<div>`/`<fieldset>` still surface.
fn collect_form_data(document: &dom::Document, form_id: NodeId) -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk_form_subtree(document, form_id, &mut out);
    out
}

fn walk_form_subtree(
    document: &dom::Document,
    node_id: NodeId,
    out: &mut Vec<(String, String)>,
) {
    let Some(node) = document.get(node_id) else {
        return;
    };
    if let NodeType::Element(elem) = &node.node_type
        && is_text_field_tag(&elem.tag_name)
        && let Some(name) = elem.attributes.get("name")
    {
        let value = elem.attributes.get("value").cloned().unwrap_or_default();
        out.push((name.clone(), value));
    }
    for child in &node.children {
        walk_form_subtree(document, *child, out);
    }
}

// Percent-encode a single form field per `application/x-www-form-urlencoded`
// — unreserved chars pass through, spaces become `+`, everything else
// becomes `%HH` per UTF-8 byte. Good enough for a toy GET form; the
// spec's full algorithm has more nuance around CR/LF normalisation that
// real apps rarely depend on.
fn url_encode(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => out.push(ch),
            ' ' => out.push('+'),
            _ => {
                let mut buf = [0u8; 4];
                let bytes = ch.encode_utf8(&mut buf);
                for &b in bytes.as_bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

// Build the GET-form submission URL: `action` with the encoded query
// appended. If `action` already has a `?`, fields append with `&`;
// otherwise we introduce `?`. An empty data list returns `action`
// unchanged so a buttonless form with no fields still routes through
// the navigator (rare, but exercises the same path).
fn build_form_submit_url(action: &str, data: &[(String, String)]) -> String {
    let query = data
        .iter()
        .map(|(name, value)| format!("{}={}", url_encode(name), url_encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        return action.to_string();
    }
    let separator = if action.contains('?') { '&' } else { '?' };
    format!("{action}{separator}{query}")
}

// Walks a stored hover/focus path back to its NodeId. The path is a
// sequence of child indices starting at the document's last root —
// matching the convention in `display_list::build_document_view`, which
// picks `roots().last()` as the visible page root, and the hit-test in
// `compute_hovered_hit`, which produces paths against the same tree.
// Layout child positions still mirror DOM child positions today (no
// anonymous boxes are produced), so a path computed against the layout
// tree resolves to the same NodeId here.
fn node_id_for_dom_path(document: &dom::Document, path: &[usize]) -> Option<NodeId> {
    let mut current = *document.roots().last()?;
    for &idx in path {
        let node = document.get(current)?;
        current = *node.children.get(idx)?;
    }
    Some(current)
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

pub fn load_initial_state() -> BrowserState {
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

pub fn build_font_cache(font_data: &[Vec<u8>]) -> Vec<fontdue::Font> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_encode_passes_unreserved_chars_unchanged() {
        // The four "unreserved" punctuation chars (- _ . ~) plus
        // alphanumerics ride through as-is; this matches RFC 3986's
        // unreserved set, which the form-urlencoded layer inherits.
        assert_eq!(url_encode("abc123-_.~"), "abc123-_.~");
    }

    #[test]
    fn url_encode_replaces_space_with_plus() {
        // application/x-www-form-urlencoded specifically maps space
        // to `+` (not `%20`) — the legacy form-encoding contract real
        // backends still parse against.
        assert_eq!(url_encode("hello world"), "hello+world");
    }

    #[test]
    fn url_encode_percent_encodes_special_ascii() {
        // `&` and `=` are the field separators in a query string;
        // they must escape so a value of "a&b=c" doesn't smuggle
        // extra fields into the URL.
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn url_encode_emits_one_percent_pair_per_utf8_byte() {
        // Korean "한" is three bytes in UTF-8; each byte gets its own
        // %HH pair so the server-side decoder can reassemble it.
        assert_eq!(url_encode("한"), "%ED%95%9C");
    }

    #[test]
    fn build_form_submit_url_appends_query_with_question_mark() {
        let data = vec![("q".to_string(), "hi".to_string())];
        assert_eq!(build_form_submit_url("/search", &data), "/search?q=hi");
    }

    #[test]
    fn build_form_submit_url_appends_with_amp_when_action_has_query() {
        // The action already carries a `?lang=en`, so the form data
        // joins with `&` instead of introducing a second `?`.
        let data = vec![("q".to_string(), "hi".to_string())];
        assert_eq!(
            build_form_submit_url("/search?lang=en", &data),
            "/search?lang=en&q=hi"
        );
    }

    #[test]
    fn build_form_submit_url_returns_action_unchanged_when_no_data() {
        // No name'd fields → no query string → action passes through.
        let data: Vec<(String, String)> = Vec::new();
        assert_eq!(build_form_submit_url("/search", &data), "/search");
    }

    #[test]
    fn build_form_submit_url_encodes_field_names_and_values() {
        // Both halves of each pair go through the encoder, so a name
        // with a space and a value with `&` both round-trip cleanly.
        let data = vec![
            ("first name".to_string(), "Alice".to_string()),
            ("note".to_string(), "a&b".to_string()),
        ];
        assert_eq!(
            build_form_submit_url("/save", &data),
            "/save?first+name=Alice&note=a%26b"
        );
    }

    #[test]
    fn is_classic_script_type_accepts_missing_empty_and_javascript_aliases() {
        // No type attribute → classic. Same for an empty / whitespace
        // attribute. The full JavaScript MIME alias table is treated
        // case-insensitively, and a `;version=...` parameter on the
        // type doesn't disqualify the script.
        assert!(is_classic_script_type(None));
        assert!(is_classic_script_type(Some("")));
        assert!(is_classic_script_type(Some("   ")));
        assert!(is_classic_script_type(Some("text/javascript")));
        assert!(is_classic_script_type(Some("TEXT/JAVASCRIPT")));
        assert!(is_classic_script_type(Some("application/javascript")));
        assert!(is_classic_script_type(Some(
            "text/javascript; charset=utf-8"
        )));
        assert!(is_classic_script_type(Some("text/jscript")));
    }

    #[test]
    fn is_classic_script_type_rejects_data_blocks_and_modules() {
        // The non-classic types real pages use most often: JSON-LD
        // SEO metadata, JSON config blocks, ES modules, import maps,
        // and arbitrary template MIMEs (Handlebars, etc.). Each must
        // be skipped — running them as classic JS would surface
        // SyntaxErrors that confuse the user.
        assert!(!is_classic_script_type(Some("application/ld+json")));
        assert!(!is_classic_script_type(Some("application/json")));
        assert!(!is_classic_script_type(Some("module")));
        assert!(!is_classic_script_type(Some("importmap")));
        assert!(!is_classic_script_type(Some(
            "application/x-handlebars-template"
        )));
        assert!(!is_classic_script_type(Some("text/template")));
    }

    #[test]
    fn collect_script_sources_skips_non_classic_script_types() {
        // The HTML-spec script type filter is enforced by the
        // collector — a JSON-LD block beside a real classic script
        // must produce only the classic source. Without this filter,
        // the JSON-LD body would be handed to the JS engine and
        // crash with a SyntaxError mid-page-load.
        let html = "<script type=\"application/ld+json\">{\"@context\":\"x\"}</script>\
                    <script>var ok = 1;</script>\
                    <script type=\"module\">import x from 'a';</script>";
        let document = crate::html::parse(html).unwrap();
        let externals: HashMap<String, String> = HashMap::new();
        let mut out = Vec::new();
        let mut counter = 0;
        for &root in document.roots() {
            collect_script_sources(
                &document,
                root,
                &externals,
                "https://example.com/page",
                &mut counter,
                &mut out,
            );
        }
        assert_eq!(
            out,
            vec![(
                "var ok = 1;".to_string(),
                "https://example.com/page#inline-script-1".to_string(),
            )]
        );
    }

    #[test]
    fn collect_script_sources_labels_inline_scripts_with_running_index() {
        // Multiple inline scripts in the same document each get a unique
        // synthetic URL so the script-error log can name the offending
        // block. The counter is global (document order), not per-parent —
        // a script nested under a <body> still continues the sequence
        // started by an earlier <head> script.
        let html = "<head><script>var a = 1;</script></head>\
                    <body><script>var b = 2;</script>\
                    <div><script>var c = 3;</script></div></body>";
        let document = crate::html::parse(html).unwrap();
        let externals: HashMap<String, String> = HashMap::new();
        let mut out = Vec::new();
        let mut counter = 0;
        for &root in document.roots() {
            collect_script_sources(
                &document,
                root,
                &externals,
                "https://example.com/p",
                &mut counter,
                &mut out,
            );
        }
        assert_eq!(
            out,
            vec![
                (
                    "var a = 1;".to_string(),
                    "https://example.com/p#inline-script-1".to_string(),
                ),
                (
                    "var b = 2;".to_string(),
                    "https://example.com/p#inline-script-2".to_string(),
                ),
                (
                    "var c = 3;".to_string(),
                    "https://example.com/p#inline-script-3".to_string(),
                ),
            ]
        );
    }

    #[test]
    fn collect_script_sources_labels_external_scripts_with_src_attr() {
        // External scripts (with a `src`) keep the raw attribute value as
        // the label. The page URL is unused here because the `src` is
        // already a more useful identifier — a developer scanning the
        // log can grep their HTML for the matching tag directly.
        let html = "<script src=\"/static/app.js\"></script>\
                    <script src=\"https://cdn.example.com/lib.js\"></script>";
        let document = crate::html::parse(html).unwrap();
        let mut externals: HashMap<String, String> = HashMap::new();
        externals.insert("/static/app.js".to_string(), "var app = 1;".to_string());
        externals.insert(
            "https://cdn.example.com/lib.js".to_string(),
            "var lib = 2;".to_string(),
        );
        let mut out = Vec::new();
        let mut counter = 0;
        for &root in document.roots() {
            collect_script_sources(
                &document,
                root,
                &externals,
                "https://example.com/page",
                &mut counter,
                &mut out,
            );
        }
        assert_eq!(
            out,
            vec![
                ("var app = 1;".to_string(), "/static/app.js".to_string()),
                (
                    "var lib = 2;".to_string(),
                    "https://cdn.example.com/lib.js".to_string(),
                ),
            ]
        );
    }
}
