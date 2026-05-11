// BrowserState owns everything the per-frame loop has to keep coherent: the
// parsed document/CSS caches, the JS runtime that shares the document arena,
// the address bar / scroll / history state, and the `display_list` driver
// that calls into `crate::display_list` for view building and `crate::chrome`
// for chrome painting. Pure helpers (sample HTML/CSS, the env-arg loader, the
// font cache builder) live at the bottom so `main` can stay a one-liner.

mod events;
mod history;
mod lifecycle;

use std::{cell::RefCell, collections::HashMap, env, rc::Rc, sync::mpsc};

use crate::{
    chrome::{CHROME_HEIGHT, ChromeState, chrome_commands},
    css,
    display_list::{
        DocumentView, build_document_view, caret_commands_for_focused_input, compute_hovered_hit,
        document_height, link_decoration_commands,
    },
    dom,
    dom::{NodeId, NodeType},
    html, js, layout,
    navigation::{LoadedDocument, load_remote_document},
    input, net, render, resource, style,
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
    /// Favicon for the current document, if `<link rel="icon">` was
    /// present and the fetch + decode succeeded. The chrome paints it
    /// in the tab strip when present and shifts the title right to
    /// make room. Cleared on every fresh navigation; back/forward
    /// restores the document but not the favicon (that would need
    /// `HistoryEntry` plumbing — Phase 5.9c+ if requested).
    pub favicon: Option<resource::LoadedImage>,

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

    // Last `build_document_view` result plus the inputs it was built against.
    // Each frame compares the live inputs (document revision, viewport, hover/
    // focus paths, active state) against `key`; a match returns a clone of the
    // cached view and skips style/layout/paint entirely. A static page with a
    // still mouse therefore amortises the per-frame cost down to "clone a
    // command list", and any DOM mutation / hover transition / viewport
    // change naturally invalidates by mismatching the key.
    //
    // Caret blink and scroll already ride separate overlay layers above the
    // page commands (see `display_list()`), so they don't participate in the
    // key — that keeps the cache hit rate high during normal idle.
    cached_view: Option<CachedView>,

    // In-flight navigation kicked off via `async_runtime::handle().spawn_blocking`.
    // `display_list()` polls this at the top of every frame; when the worker
    // finishes the result lands on the receiver and the matching `PendingKind`
    // arm decides how to install the document. While the slot is `Some`, the
    // dispatch sites refuse to start a second load — last-wins coalescing is
    // not modelled because the user-visible "loading…" status already signals
    // that one fetch owns the page right now.
    //
    // Phase 5.8a only routes the refresh button through this state; future
    // sub-phase will fold `navigate` and `navigate_to_href` in once their
    // many existing sync tests pick up a wait-for-pending helper.
    pending_navigation: Option<PendingNavigation>,
}

#[derive(Debug)]
struct PendingNavigation {
    kind: PendingKind,
    receiver: mpsc::Receiver<Result<LoadedDocument, String>>,
}

#[derive(Debug, Clone, Copy)]
enum PendingKind {
    Refresh,
    /// URL bar Enter or link click / form submit. Successful loads
    /// push to the back/forward stack via `commit_navigation`; failed
    /// loads commit the canned error page through the same funnel,
    /// so the user can `back` out of a broken navigation. The string
    /// is the title shown on the error page when the load fails —
    /// "load failed" for URL-bar entries, "link failed" for clicks.
    Navigate { error_title: &'static str },
}

// Snapshot of the inputs `build_document_view` was last called with, paired
// with its output. The struct lives next to `BrowserState` because it's an
// implementation detail of `display_list()` — no other module needs to know
// about it. Cloning a `DocumentView` to satisfy the per-frame consume-by-
// value pattern in `render::translate` is still much cheaper than rerunning
// style + layout + paint, so the indirection pays for itself.
#[derive(Debug)]
struct CachedView {
    revision: u64,
    viewport_width: usize,
    hover_path: Option<Vec<usize>>,
    focus_path: Option<Vec<usize>>,
    active: bool,
    view: DocumentView,
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
            cached_view: None,
            pending_navigation: None,
            // Favicon arrives via `commit_navigate` / `commit_refresh` /
            // `load_initial_state`; the constructor only receives the
            // document strings + resources, not the favicon. Tests
            // without a favicon naturally see `None`.
            favicon: None,
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
        // The previous document's view cache references its old DOM/CSS,
        // images, and base URL. The freshly-installed Document also starts
        // with `revision = 0`, which would collide with the previous cached
        // view's revision and silently serve stale paint commands — so blow
        // the cache away on every install rather than relying on revision
        // alone.
        self.cached_view = None;
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
        input: &input::WindowInput,
    ) -> Vec<render::DisplayCommand> {
        // The browser re-builds its visible scene every frame from current state + fresh input.
        self.frame_index = self.frame_index.wrapping_add(1);
        // Drain any completed async load before input dispatch. Refresh today,
        // navigate / navigate_to_href once 5.8b lands. Doing it before
        // `apply_input` matters: a click that lands the same frame the load
        // resolves should see the freshly installed document, not the stale
        // one whose paint we're about to discard.
        self.poll_pending_navigation();
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

        // `:active` matches only while the left button is held over an
        // already-hovered element — capturing it as a bool here means a
        // mouse-still page hits the view cache every frame, while a
        // press/release cycle naturally produces two cache misses.
        let active = input.left_mouse_held && self.hovered_dom_path.is_some();
        let document_view = self.build_or_reuse_view(viewport_width, active);

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
            ),
            0.0,
            CHROME_HEIGHT - self.scroll_offset,
        ));
        let is_https = self
            .current_url
            .as_ref()
            .is_some_and(|url| url.scheme.eq_ignore_ascii_case("https"));
        commands.extend(chrome_commands(ChromeState {
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
            is_https,
            tab_favicon: self.favicon.as_ref(),
        }));
        commands
    }

    // Cache-aware wrapper around `display_list::build_document_view`. The
    // function returns a `DocumentView` either by cloning a previously
    // computed one (if every input that style/layout/paint depends on is
    // unchanged) or by rerunning the full pipeline and refreshing the
    // cache. The caller treats the returned value the same way regardless;
    // the savings come purely from skipping redundant work on idle frames.
    //
    // Cache key components:
    //   - `Document::revision()` covers every DOM mutation that flows
    //     through the arena's mutating methods or through the JS bridge's
    //     attribute setters (which now call `document.touch()` themselves).
    //   - `viewport_width` is the only viewport input that build_document_view
    //     pays attention to; height affects scroll clamping but not the
    //     painted commands.
    //   - `hover_path` / `focus_path` / `active` snapshot the interaction
    //     state that style_tree_with_state cascades into the styled tree;
    //     a hover transition or a press flips one of them and forces a
    //     rebuild on that frame only.
    //   - `parsed_stylesheet`, `current_url`, `images`, and `font_data` all
    //     change exclusively at install time, so `cached_view = None` in
    //     `install_document` covers them implicitly without a per-field
    //     comparison.
    //
    // Render failures don't get cached — we want the next frame to retry
    // a fresh build (the failure may have been transient).
    fn build_or_reuse_view(&mut self, viewport_width: usize, active: bool) -> DocumentView {
        let revision = self.parsed_document.borrow().revision();
        if let Some(cached) = self.cached_view.as_ref()
            && cached.revision == revision
            && cached.viewport_width == viewport_width
            && cached.active == active
            && cached.hover_path == self.hovered_dom_path
            && cached.focus_path == self.focused_dom_path
        {
            return cached.view.clone();
        }
        let interaction = style::InteractionState {
            hover: self.hovered_dom_path.as_deref(),
            focus: self.focused_dom_path.as_deref(),
            // The :active path lights up only while the mouse is held over
            // the same node :hover already named, so we can reuse the hover
            // path here — saves a second Vec.
            active: if active {
                self.hovered_dom_path.as_deref()
            } else {
                None
            },
        };
        // Borrow scope keeps the document Ref out of the way of the
        // `set_status` mutation below in the error branch, and out of the
        // way of `self.cached_view = ...` on the success branch.
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
        match layout_result {
            Ok(view) => {
                self.cached_view = Some(CachedView {
                    revision,
                    viewport_width,
                    hover_path: self.hovered_dom_path.clone(),
                    focus_path: self.focused_dom_path.clone(),
                    active,
                    view: view.clone(),
                });
                view
            }
            Err(build_error) => {
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
                // Tear down any stale cached view from before the failure
                // so a subsequent recovery rebuilds against current inputs.
                self.cached_view = None;
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
            }
        }
    }

    /// Reports whether the browser has *time-driven* visual changes
    /// that need a follow-up frame even with no input. The shell uses
    /// this to decide whether to schedule another `request_redraw`
    /// after the current paint — `false` lets winit block on the next
    /// real event and drops idle CPU to ~0%.
    ///
    /// Hover, click, scroll, and keyboard changes are *event-driven*:
    /// the shell already calls `request_redraw` from the matching
    /// winit event handler, so they don't need to live here.
    ///
    /// Today this covers the caret blink (animating only while the
    /// address bar owns focus and isn't in select-all mode) and an
    /// in-flight async navigation (the worker channel must be polled
    /// every frame until it resolves). JS timers / requestAnimationFrame
    /// are a follow-up — they don't currently advertise pendingness
    /// through `js`, so a page that uses `setInterval` will redraw
    /// only when other triggers happen.
    pub fn wants_continuous_redraw(&self) -> bool {
        let caret_blinking = self.address_bar_focused && !self.address_bar_selected;
        caret_blinking || self.pending_navigation.is_some()
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
    // The keystroke went through `element_data_mut`, which doesn't bump the
    // Document's revision on its own — touch explicitly so the next frame's
    // view cache rebuilds and the user actually sees the new caret position
    // and value text rather than a stale paint.
    document.touch();
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
    document.touch();
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
            Ok((document_html, stylesheet, images, font_data, external_scripts, current_url, favicon)) => {
                let mut state = BrowserState::new(
                    raw_url,
                    document_html,
                    stylesheet,
                    images,
                    font_data,
                    external_scripts,
                    Some(current_url),
                    "loaded",
                );
                state.favicon = favicon;
                state
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

// Font system now lives in `mb-engine::font_system`; re-export the public
// helpers so `state::install_fonts` etc. remain valid call sites.
pub use mb_engine::font_system::{install_fonts, shared_font_system, shared_swash_cache};

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

    fn make_state(html_source: &str) -> BrowserState {
        // Minimal BrowserState fixture for the view-cache tests below. The
        // empty fonts/images/url defaults match what `display_list` sees on
        // the NTP — the cache's behaviour is independent of those, but we
        // still need a parseable document so `build_document_view` produces
        // a real `Ok` result rather than the error-fallback path.
        BrowserState::new(
            String::new(),
            html_source.to_string(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        )
    }

    #[test]
    fn cached_view_populates_after_first_display_list_frame() {
        // The cache is empty until the first frame builds something —
        // `BrowserState::new` deliberately leaves it `None` so a build
        // failure on frame 1 doesn't get masked by a stale entry.
        let mut state = make_state("<div>hi</div>");
        assert!(state.cached_view.is_none());
        let input = input::WindowInput::default();
        let _ = state.display_list(800, 600, &input);
        assert!(state.cached_view.is_some());
    }

    #[test]
    fn cached_view_revision_unchanged_across_idle_frames() {
        // No DOM mutation, no hover/focus/viewport change → the second
        // frame must hit the cache, observable as the cached revision
        // staying put. (A miss would rebuild and rewrite cached_view
        // from scratch, but with the same inputs the revision would
        // also be equal — so we additionally verify the cache slot
        // wasn't dropped between frames by checking is_some both times.)
        let mut state = make_state("<div>hi</div>");
        let input = input::WindowInput::default();
        let _ = state.display_list(800, 600, &input);
        let rev_after_first = state
            .cached_view
            .as_ref()
            .map(|c| c.revision)
            .expect("first frame must populate the cache");
        let _ = state.display_list(800, 600, &input);
        let rev_after_second = state
            .cached_view
            .as_ref()
            .map(|c| c.revision)
            .expect("second frame must keep the cache populated");
        assert_eq!(rev_after_first, rev_after_second);
    }

    #[test]
    fn cached_view_picks_up_dom_mutations_via_revision_bump() {
        // A `Document::touch()` between frames stands in for any of the
        // real mutation paths (set_attribute, value setter, classList,
        // appendChild, …). The cache key check on the next frame must
        // notice the bumped revision and rebuild — otherwise a script
        // that mutates the DOM would leave the user staring at a stale
        // paint.
        let mut state = make_state("<div>hi</div>");
        let input = input::WindowInput::default();
        let _ = state.display_list(800, 600, &input);
        let rev_first = state.cached_view.as_ref().unwrap().revision;
        state.parsed_document.borrow_mut().touch();
        let _ = state.display_list(800, 600, &input);
        let rev_second = state.cached_view.as_ref().unwrap().revision;
        assert!(rev_second > rev_first);
    }

    #[test]
    fn install_document_drops_cached_view() {
        // Navigation replaces the parsed Document in place; the new
        // arena starts at revision 0 and would silently match a
        // previous cache entry that also happened to be at revision 0.
        // `install_document` therefore clears the cache explicitly so
        // the very next frame rebuilds against the new tree's images,
        // base URL, and stylesheet rather than serving the prior page.
        let mut state = make_state("<div>old</div>");
        let input = input::WindowInput::default();
        let _ = state.display_list(800, 600, &input);
        assert!(state.cached_view.is_some());
        state.install_document(
            "<p>new</p>".to_string(),
            String::new(),
            HashMap::new(),
        );
        assert!(state.cached_view.is_none());
    }

    #[test]
    fn clamp_scroll_pins_to_zero_when_document_fits_viewport() {
        // Document shorter than the visible page area → no scrolling
        // possible; whatever the user accumulated must collapse to 0.
        // Otherwise the page would slide above the chrome and reveal a
        // blank band underneath.
        let mut state = make_state("<div>hi</div>");
        state.scroll_offset = 500.0;
        state.clamp_scroll(800, 100.0);
        assert_eq!(state.scroll_offset, 0.0);
    }

    #[test]
    fn clamp_scroll_caps_at_document_height_minus_visible() {
        // viewport_height=800 → visible = 800 - CHROME_HEIGHT (102) = 698.
        // doc_height=1000 → max scroll = 1000 - 698 = 302. A request
        // beyond that pins to the cap so the page bottom anchors to the
        // viewport bottom rather than scrolling into empty space.
        let mut state = make_state("<div>hi</div>");
        state.scroll_offset = 5_000.0;
        state.clamp_scroll(800, 1_000.0);
        assert_eq!(state.scroll_offset, 1_000.0 - (800.0 - CHROME_HEIGHT));
    }

    #[test]
    fn clamp_scroll_rejects_negative_offsets() {
        // Trackpad inertia + the `-=` accumulation in apply_input can
        // drift the raw scroll below zero; clamp must snap to 0 so the
        // user can't pull the page below the chrome.
        let mut state = make_state("<div>hi</div>");
        state.scroll_offset = -200.0;
        state.clamp_scroll(800, 2_000.0);
        assert_eq!(state.scroll_offset, 0.0);
    }

    #[test]
    fn show_caret_alternates_with_frame_index_when_address_bar_focused() {
        // The blink uses 30-frame buckets (`frame_index / 30`) with an
        // is-even gate, so frames 0..29 paint the caret and 30..59 hide
        // it. Verifying both halves of the same cycle locks the rhythm.
        let mut state = make_state("");
        state.address_bar_focused = true;
        state.address_bar_selected = false;
        state.frame_index = 0;
        assert!(state.show_caret());
        state.frame_index = 30;
        assert!(!state.show_caret());
        state.frame_index = 60;
        assert!(state.show_caret());
    }

    #[test]
    fn show_caret_off_when_address_bar_unfocused() {
        // No focus → no caret regardless of frame phase. Without this
        // guard, the caret would keep blinking after a page click moved
        // focus into the document.
        let mut state = make_state("");
        state.address_bar_focused = false;
        state.frame_index = 0;
        assert!(!state.show_caret());
    }

    #[test]
    fn show_caret_off_while_selection_active() {
        // Cmd-L / focus-after-navigate select-all the URL; in that state
        // the chrome paints a highlight band instead of a caret. Showing
        // both at once would look like two cursors stacked on the URL.
        let mut state = make_state("");
        state.address_bar_focused = true;
        state.address_bar_selected = true;
        state.frame_index = 0;
        assert!(!state.show_caret());
    }

    #[test]
    fn resolve_href_accepts_absolute_url_without_base() {
        // Anything containing `://` is treated as already-absolute and
        // parses directly, even when the browser has no current page —
        // matches Chrome's "Open Link in New Tab" semantics where the
        // target works fine on a fresh tab with no referrer.
        let state = make_state("");
        let url = state.resolve_href("https://example.com/path").unwrap();
        assert_eq!(url.scheme, "https");
        assert_eq!(url.host, "example.com");
    }

    #[test]
    fn resolve_href_rejects_relative_when_no_base() {
        // The NTP has no `current_url`, so a relative link has nothing
        // to resolve against. The error message goes straight into the
        // error page so the user sees *why* the click failed rather
        // than a generic "couldn't load".
        let state = make_state("");
        let err = state.resolve_href("/about").unwrap_err();
        assert!(err.contains("relative link"), "got: {err}");
    }

    #[test]
    fn resolve_href_joins_relative_against_current_url() {
        // Standard same-origin relative resolution. `/about` against
        // `https://example.com/blog/post` should land on the document
        // root, not append. This is the path most in-page <a href>
        // clicks take, so a regression here would break navigation
        // on essentially every real page.
        let mut state = make_state("");
        state.current_url = Some(net::Url::parse("https://example.com/blog/post").unwrap());
        let url = state.resolve_href("/about").unwrap();
        assert_eq!(url.scheme, "https");
        assert_eq!(url.host, "example.com");
        assert_eq!(url.path, "/about");
    }

    #[test]
    fn wants_continuous_redraw_true_when_address_bar_blinks() {
        // Caret blink is the canonical idle-but-animating case; the
        // shell needs to keep scheduling redraws so the cursor stays
        // alive. Without this the caret would freeze the moment input
        // stopped flowing.
        let mut state = make_state("");
        state.address_bar_focused = true;
        state.address_bar_selected = false;
        assert!(state.wants_continuous_redraw());
    }

    #[test]
    fn wants_continuous_redraw_false_when_idle_and_address_bar_blurred() {
        // No focus, no select-all band, no pending nav → fully idle.
        // The shell drops to `ControlFlow::Wait` and burns ~0% CPU.
        // This is the case that closes the regression Phase 5 chased.
        let mut state = make_state("");
        state.address_bar_focused = false;
        state.address_bar_selected = false;
        assert!(state.pending_navigation.is_none());
        assert!(!state.wants_continuous_redraw());
    }

    #[test]
    fn wants_continuous_redraw_false_when_address_bar_in_select_all() {
        // Select-all paints a static highlight band, not a blinking
        // caret. The shell can park itself until the next real event
        // (keystroke, click) without dropping frames the user would
        // perceive.
        let mut state = make_state("");
        state.address_bar_focused = true;
        state.address_bar_selected = true;
        assert!(!state.wants_continuous_redraw());
    }

    #[test]
    fn snapshot_captures_visible_browser_state() {
        // The snapshot is the unit history pushes onto the back/forward
        // stacks; anything user-visible at the moment of capture must
        // make it across. Today that is: address bar text, the raw
        // HTML/CSS pair, the resolved current URL, and the status line.
        // Decoded images / font bytes / external scripts also travel
        // so back/forward can repaint without re-fetching.
        let mut state = make_state("<p>hello</p>");
        state.address_input = "https://example.com/".to_string();
        state.current_url = Some(net::Url::parse("https://example.com/").unwrap());
        state.status_text = "ready".to_string();

        let snap = state.snapshot();
        assert_eq!(snap.address_input, "https://example.com/");
        assert_eq!(snap.document_html, "<p>hello</p>");
        assert_eq!(
            snap.current_url.as_ref().map(|u| u.host.as_str()),
            Some("example.com")
        );
        assert_eq!(snap.status_text, "ready");
    }

    #[test]
    fn commit_navigation_pushes_previous_snapshot_onto_back_stack() {
        // A successful navigation pushes the page the user is leaving
        // onto the back stack, then swaps in the new one. Without this
        // half of the contract `go_back` would have nothing to pop.
        let mut state = make_state("<p>first</p>");
        state.address_input = "first".to_string();
        let next = HistoryEntry {
            address_input: "second".to_string(),
            document_html: "<p>second</p>".to_string(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: String::new(),
            status_color: css::Color::BLACK,
        };

        state.commit_navigation(next);

        assert_eq!(state.back_stack.len(), 1);
        assert_eq!(state.back_stack[0].address_input, "first");
        assert_eq!(state.address_input, "second");
    }

    #[test]
    fn commit_navigation_clears_forward_stack() {
        // Following a `back` with a brand new navigation must drop the
        // forward stack — the linear-history model says the future
        // changes the moment the user diverges. Otherwise stale entries
        // would resurface on a later forward press.
        let mut state = make_state("<p>a</p>");
        state.forward_stack.push(HistoryEntry {
            address_input: "stale".to_string(),
            document_html: String::new(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: String::new(),
            status_color: css::Color::BLACK,
        });

        state.commit_navigation(HistoryEntry {
            address_input: "fresh".to_string(),
            document_html: String::new(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: String::new(),
            status_color: css::Color::BLACK,
        });

        assert!(state.forward_stack.is_empty());
    }

    #[test]
    fn go_back_restores_previous_and_routes_current_to_forward_stack() {
        // back/forward is the user's most-used navigation surface. The
        // invariant: pressing back pops the back stack, pushes the page
        // we were just on onto the forward stack, and restores the
        // popped entry. A second back press would then dig deeper into
        // history. Without the forward-push half, `forward` couldn't
        // undo a back press.
        let mut state = make_state("<p>start</p>");
        state.address_input = "start".to_string();
        state.commit_navigation(HistoryEntry {
            address_input: "next".to_string(),
            document_html: "<p>next</p>".to_string(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: String::new(),
            status_color: css::Color::BLACK,
        });
        assert_eq!(state.address_input, "next");

        state.go_back();

        assert_eq!(state.address_input, "start");
        assert_eq!(state.forward_stack.len(), 1);
        assert_eq!(state.forward_stack[0].address_input, "next");
        assert!(state.back_stack.is_empty());
    }

    #[test]
    fn go_forward_undoes_go_back() {
        // After a back/forward round-trip the user should land back on
        // the exact entry they began with, with the back stack rebuilt
        // and the forward stack drained — a contract real users notice
        // when the URL bar text changes between presses.
        let mut state = make_state("<p>start</p>");
        state.address_input = "start".to_string();
        state.commit_navigation(HistoryEntry {
            address_input: "next".to_string(),
            document_html: "<p>next</p>".to_string(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: String::new(),
            status_color: css::Color::BLACK,
        });
        state.go_back();

        state.go_forward();

        assert_eq!(state.address_input, "next");
        assert!(state.forward_stack.is_empty());
        assert_eq!(state.back_stack.len(), 1);
    }

    #[test]
    fn go_back_is_noop_on_empty_back_stack() {
        // The NTP has no prior page; pressing back must not corrupt
        // current state. Real browsers grey the button out, but the
        // toy keeps the chrome button hover-enabled and relies on this
        // guard for the actual no-op.
        let mut state = make_state("<p>only</p>");
        state.address_input = "only".to_string();
        assert!(state.back_stack.is_empty());

        state.go_back();

        assert_eq!(state.address_input, "only");
        assert!(state.back_stack.is_empty());
        assert!(state.forward_stack.is_empty());
    }

    #[test]
    fn restore_entry_resets_scroll_and_selection() {
        // Restoring a snapshot lands the user at the top of the
        // restored page with the address-bar select-all band cleared.
        // Without the resets, jumping back to a previously deep-
        // scrolled page would start mid-document and the URL bar would
        // appear highlighted as if the user had just pressed Cmd-L.
        let mut state = make_state("<p>start</p>");
        state.scroll_offset = 250.0;
        state.address_bar_selected = true;

        state.restore_entry(HistoryEntry {
            address_input: "restored".to_string(),
            document_html: "<p>restored</p>".to_string(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: String::new(),
            status_color: css::Color::BLACK,
        });

        assert_eq!(state.scroll_offset, 0.0);
        assert!(!state.address_bar_selected);
        assert_eq!(state.address_input, "restored");
    }

    #[test]
    fn apply_input_focus_address_bar_flag_selects_existing_text() {
        // Cmd-L (the only producer of `focus_address_bar`) is the
        // "edit the URL" shortcut; both real browsers and this toy
        // select the current URL on focus so the next keystroke
        // replaces it instead of appending to the end.
        let mut state = make_state("");
        state.address_bar_focused = false;
        state.address_bar_selected = false;
        let input = input::WindowInput {
            focus_address_bar: true,
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert!(state.address_bar_focused);
        assert!(state.address_bar_selected);
    }

    #[test]
    fn apply_input_typed_char_replaces_selection_then_appends() {
        // First keystroke after Cmd-L: the select-all band must clear
        // first, then the char appends to an empty buffer. Without the
        // clear, the user would see their typing concatenated onto the
        // previous URL, which would surprise anyone retyping a fresh
        // address.
        let mut state = make_state("");
        state.address_input = "https://old.example/".to_string();
        state.address_bar_focused = true;
        state.address_bar_selected = true;
        let input = input::WindowInput {
            typed: "a".to_string(),
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert_eq!(state.address_input, "a");
        assert!(!state.address_bar_selected);
    }

    #[test]
    fn apply_input_subsequent_chars_append_after_selection_cleared() {
        // After the first keystroke cleared select-all, every later
        // char is a plain append. This is the steady-state typing
        // path and exercises the loop boundary inside `apply_input`.
        let mut state = make_state("");
        state.address_input = "ab".to_string();
        state.address_bar_focused = true;
        state.address_bar_selected = false;
        let input = input::WindowInput {
            typed: "cd".to_string(),
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert_eq!(state.address_input, "abcd");
    }

    #[test]
    fn apply_input_backspace_pops_last_char_when_no_selection() {
        // Backspace with no selection deletes one trailing char; the
        // typed buffer stays empty so a paired typed event doesn't
        // confuse the two paths. Matches the legacy minifb behaviour
        // where Backspace was a level-press with `KeyRepeat::Yes`.
        let mut state = make_state("");
        state.address_input = "abc".to_string();
        state.address_bar_focused = true;
        state.address_bar_selected = false;
        let input = input::WindowInput {
            backspace_pressed: true,
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert_eq!(state.address_input, "ab");
    }

    #[test]
    fn apply_input_backspace_during_selection_clears_entire_buffer() {
        // With select-all active, Backspace must empty the buffer
        // (not just pop one char). Mirrors the typing-replaces-
        // selection branch — both forms of "first edit after Cmd-L"
        // converge on the same blank-slate state.
        let mut state = make_state("");
        state.address_input = "https://old.example/".to_string();
        state.address_bar_focused = true;
        state.address_bar_selected = true;
        let input = input::WindowInput {
            backspace_pressed: true,
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert!(state.address_input.is_empty());
        assert!(!state.address_bar_selected);
    }

    #[test]
    fn apply_input_typing_when_address_bar_blurred_does_not_mutate_url() {
        // Keystrokes outside the address bar route to page JS handlers,
        // never to the URL. Without this guard, typing in a search box
        // on the page would also append to the URL bar.
        let mut state = make_state("");
        state.address_input = "untouched".to_string();
        state.address_bar_focused = false;
        let input = input::WindowInput {
            typed: "x".to_string(),
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert_eq!(state.address_input, "untouched");
    }

    #[test]
    fn apply_input_arrow_down_advances_scroll_by_one_step() {
        // Arrow keys move the page in 24-px increments — the same step
        // size the scroll wheel emits. Matches the legacy `minifb` feel
        // and keeps keyboard navigation at parity with mouse scrolling.
        let mut state = make_state("<div style='height: 5000px'></div>");
        state.scroll_offset = 100.0;
        let input = input::WindowInput {
            move_down: true,
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert_eq!(state.scroll_offset, 124.0);
    }

    #[test]
    fn apply_input_arrow_up_retreats_scroll_by_one_step() {
        // Symmetric to arrow-down; the offset decreases by the same
        // 24-px constant. Together they pin the page to a uniform
        // keyboard step regardless of mouse vs trackpad scroll deltas.
        let mut state = make_state("<div style='height: 5000px'></div>");
        state.scroll_offset = 100.0;
        let input = input::WindowInput {
            move_up: true,
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert_eq!(state.scroll_offset, 76.0);
    }

    #[test]
    fn apply_input_scroll_y_subtracts_from_offset_with_24_multiplier() {
        // Mouse-wheel / trackpad scroll arrives as "lines per second"
        // and gets multiplied by 24 px/line to match keyboard arrow
        // distance. The sign flips because positive scroll_y means
        // "wheel pushed up / page goes down" — a one-line spin should
        // *advance* the document.
        let mut state = make_state("<div style='height: 5000px'></div>");
        state.scroll_offset = 200.0;
        let input = input::WindowInput {
            scroll_y: -2.0,
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        // -=  (-2.0 * 24.0) → +48 movement
        assert_eq!(state.scroll_offset, 248.0);
    }

    #[test]
    fn apply_input_back_pressed_pops_back_stack() {
        // Cmd-[ / Alt-Left both surface as `back_pressed`. They must
        // walk the history independently of mouse clicks on the
        // chrome back button — keyboard users never touch the
        // chrome strip. Both paths share `go_back`, so this just
        // verifies the wiring from the input bit to the stack pop.
        let mut state = make_state("<p>start</p>");
        state.address_input = "start".to_string();
        state.commit_navigation(HistoryEntry {
            address_input: "next".to_string(),
            document_html: "<p>next</p>".to_string(),
            stylesheet: String::new(),
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: String::new(),
            status_color: css::Color::BLACK,
        });
        let input = input::WindowInput {
            back_pressed: true,
            ..Default::default()
        };

        state.apply_input(&input, 800, 600);

        assert_eq!(state.address_input, "start");
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
