// `AgentBrowser`: a `BrowserState` driven by an agent (docs/concept.md
// "행동"). Every action becomes synthetic `WindowInput` frames tagged
// `from_agent`, fed through the same `display_list` loop a person's input
// takes — no DOM shortcuts (AGENTS.md "Agent-track Rules"). After each
// action the page is settled and a fresh observation is returned.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use mb_runtime::chrome::CHROME_HEIGHT;
use mb_runtime::dom::NodeId;
use mb_runtime::input::WindowInput;
use mb_runtime::js::{FixedClock, JsOptions};
use mb_runtime::layout::Rect;
use mb_runtime::net::Url;
use mb_runtime::state::BrowserState;
use mb_runtime::state::policy::{AgentPolicy, ApproveMode, GatedKind, PolicyDecision};

use crate::error::AgentError;
use crate::index::find_rect;
use crate::observation::{Changes, Observation, ObserveInput, ObserveOptions, Scope, observe};

/// Virtual-clock step per frame (≈60 fps).
const FRAME_MS: u64 = 16;
/// Consecutive quiet frames that count as "settled".
const QUIET_FRAMES: usize = 2;
/// A timer due within this many ms still counts as pending work; a timer
/// further out (clock, polling interval) doesn't hold up settling.
const TIMER_HORIZON_MS: u64 = 50;
/// Upper bound on frames per settle, on top of the wall-clock timeout.
const MAX_SETTLE_FRAMES: usize = 600;
/// `WindowInput::scroll_y` is in wheel ticks; the runtime scrolls 24px each.
const PX_PER_SCROLL_TICK: f32 = 24.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockMode {
    /// Engine clock advanced by the agent loop: deterministic.
    Virtual,
    /// Wall clock: what a watching human expects.
    System,
}

#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Page area (the browser chrome is added on top).
    pub viewport_width: usize,
    pub viewport_height: usize,
    pub approve: ApproveMode,
    pub allow_origins: Vec<String>,
    pub clock: ClockMode,
    pub seed: u32,
    /// Wall-clock cap on one settle (network loads run on real time).
    pub settle_timeout: Duration,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            viewport_width: 1280,
            viewport_height: 800,
            approve: ApproveMode::Deny,
            allow_origins: Vec::new(),
            clock: ClockMode::Virtual,
            seed: 1,
            settle_timeout: Duration::from_secs(5),
        }
    }
}

pub struct AgentBrowser {
    state: BrowserState,
    config: AgentConfig,
    clock: Option<FixedClock>,
    last_settled: Option<bool>,
    // Ids handed out in observations of the current document, and which
    // document that was. NodeIds restart at zero in every new document, so
    // an id is only accepted if this page's observations listed it.
    issued_generation: u64,
    issued: BTreeSet<NodeId>,
}

/// Keys `press` accepts.
pub const SUPPORTED_KEYS: &[&str] = &[
    "Enter",
    "Backspace",
    "Tab",
    "Escape",
    "ArrowUp",
    "ArrowDown",
    "PageUp",
    "PageDown",
];

impl AgentBrowser {
    pub fn new(config: AgentConfig) -> Self {
        let mut state = BrowserState::new(
            "about:blank".into(),
            "<html><body></body></html>".into(),
            String::new(),
            HashMap::new(),
            Vec::new(),
            HashMap::new(),
            None,
            "",
        );
        state.agent_policy = AgentPolicy::new(config.approve, config.allow_origins.clone());
        let clock = (config.clock == ClockMode::Virtual).then(|| FixedClock::from_millis(0));
        state.set_js_options(JsOptions {
            fixed_clock: clock.clone(),
            random_seed: Some(config.seed),
        });
        let mut browser = Self {
            state,
            config,
            clock,
            last_settled: None,
            issued_generation: 0,
            issued: BTreeSet::new(),
        };
        browser.settle();
        browser
    }

    pub fn state(&self) -> &BrowserState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut BrowserState {
        &mut self.state
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    // -----------------------------------------------------------------
    // Observation
    // -----------------------------------------------------------------

    /// The current page, as the agent reads it. Drains console / dialog
    /// records accumulated since the previous observation.
    pub fn observe(&mut self, options: &ObserveOptions) -> Observation {
        let mut observation = self.observe_raw(options);
        let generation = self.state.document_generation();
        if generation != self.issued_generation {
            self.issued.clear();
            self.issued_generation = generation;
        }
        self.issued
            .extend(observation.elements.iter().map(|e| e.id));
        observation.console = self.state.js.take_console_messages();
        observation.dialogs = self.state.js.take_dialogs();
        observation
    }

    fn observe_raw(&mut self, options: &ObserveOptions) -> Observation {
        if self.state.current_view().is_none() {
            self.frame(WindowInput::default());
        }
        let view = self
            .state
            .current_view()
            .expect("display_list always leaves a view behind");
        let listeners = self.state.js.nodes_with_listener("click");
        let document = self.state.parsed_document.borrow();
        observe(
            &ObserveInput {
                document: &document,
                layout_root: &view.layout_root,
                url: self.state.current_url.as_ref(),
                scroll_y: self.state.scroll_offset,
                viewport_width: self.config.viewport_width as f32,
                viewport_height: self.config.viewport_height as f32,
                settled: self.last_settled,
                click_listeners: &listeners,
            },
            options,
        )
    }

    fn full_snapshot(&mut self) -> Observation {
        self.observe_raw(&ObserveOptions {
            scope: Scope::Full,
            max_elements: usize::MAX,
            max_text_chars: 0,
            cursor: 0,
        })
    }

    // -----------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------

    pub fn navigate(&mut self, url: &str) -> Result<Observation, AgentError> {
        if Url::parse(url).is_err() {
            return Err(AgentError::InvalidUrl { url: url.into() });
        }
        self.act(|browser| {
            // The URL bar path: focus selects its text, typing replaces it,
            // Enter navigates — all in one frame, like a quick typist.
            browser.frame(WindowInput {
                focus_address_bar: true,
                typed: url.to_string(),
                enter_pressed: true,
                ..WindowInput::default()
            });
            Ok(())
        })
    }

    pub fn click(&mut self, id: NodeId) -> Result<Observation, AgentError> {
        self.target_rect(id)?;
        self.act(|browser| browser.click_raw(id))
    }

    /// Type into a text field. `clear` empties it first; `submit` presses
    /// Enter afterwards.
    pub fn type_text(
        &mut self,
        id: NodeId,
        text: &str,
        clear: bool,
        submit: bool,
    ) -> Result<Observation, AgentError> {
        self.target_rect(id)?;
        let (is_text_field, is_password) = {
            let document = self.state.parsed_document.borrow();
            let element = document.element_data(id);
            let input_type = element
                .and_then(|e| e.attributes.get("type"))
                .map(|t| t.trim().to_ascii_lowercase())
                .unwrap_or_default();
            let tag = element.map(|e| e.tag_name.as_str()).unwrap_or("");
            let text_field = tag == "textarea"
                || (tag == "input"
                    && matches!(
                        input_type.as_str(),
                        "" | "text" | "email" | "search" | "url" | "tel" | "number" | "password"
                    ));
            (text_field, input_type == "password")
        };
        if !is_text_field {
            return Err(AgentError::NotATextField { id });
        }
        if is_password && !self.may_type_password() {
            return Err(AgentError::HandoffRequired {
                id,
                reason: "password fields are filled by the human unless this is a local test site in auto-approve mode",
            });
        }
        self.act(|browser| {
            browser.click_raw(id)?;
            browser.settle();
            if browser.state.focused_element() != Some(id) {
                return Err(AgentError::NotInteractable {
                    id,
                    reason: "clicking it did not give it focus",
                });
            }
            if clear {
                let length = browser
                    .state
                    .parsed_document
                    .borrow()
                    .element_data(id)
                    .and_then(|e| e.attributes.get("value"))
                    .map(|v| v.chars().count())
                    .unwrap_or(0);
                // The runtime applies one Backspace per frame.
                for _ in 0..length {
                    browser.frame(WindowInput {
                        backspace_pressed: true,
                        ..WindowInput::default()
                    });
                }
            }
            if !text.is_empty() {
                browser.frame(WindowInput {
                    typed: text.to_string(),
                    ..WindowInput::default()
                });
            }
            if submit {
                browser.frame(WindowInput {
                    enter_pressed: true,
                    ..WindowInput::default()
                });
            }
            Ok(())
        })
    }

    pub fn press(&mut self, key: &str) -> Result<Observation, AgentError> {
        let mut input = WindowInput::default();
        match key {
            "Enter" => input.enter_pressed = true,
            "Backspace" => input.backspace_pressed = true,
            "Tab" => input.tab_pressed = true,
            "Escape" => input.escape_pressed = true,
            "ArrowUp" => input.move_up = true,
            "ArrowDown" => input.move_down = true,
            "PageUp" => input.page_up_pressed = true,
            "PageDown" => input.page_down_pressed = true,
            _ => return Err(AgentError::UnsupportedKey { key: key.into() }),
        }
        self.act(|browser| {
            browser.frame(input);
            Ok(())
        })
    }

    /// Scroll the page by `dy` CSS px (negative = up).
    pub fn scroll(&mut self, dy: f32) -> Result<Observation, AgentError> {
        self.act(|browser| {
            browser.frame(WindowInput {
                scroll_y: -dy / PX_PER_SCROLL_TICK,
                ..WindowInput::default()
            });
            Ok(())
        })
    }

    pub fn back(&mut self) -> Result<Observation, AgentError> {
        self.act(|browser| {
            browser.frame(WindowInput {
                back_pressed: true,
                ..WindowInput::default()
            });
            Ok(())
        })
    }

    pub fn forward(&mut self) -> Result<Observation, AgentError> {
        self.act(|browser| {
            browser.frame(WindowInput {
                forward_pressed: true,
                ..WindowInput::default()
            });
            Ok(())
        })
    }

    // Shared action envelope: snapshot → act → settle → policy verdict →
    // observation with a `changed:` summary.
    fn act(
        &mut self,
        action: impl FnOnce(&mut Self) -> Result<(), AgentError>,
    ) -> Result<Observation, AgentError> {
        let before = self.full_snapshot();
        self.state.take_policy_events();
        action(self)?;
        self.settle();
        let events = self.state.take_policy_events();
        let after = self.full_snapshot();
        let mut observation = self.observe(&ObserveOptions::default());
        observation.changed = Some(Changes::between(&before, &after));

        for event in events {
            let target = event.action.target.to_string();
            match event.decision {
                PolicyDecision::Denied { remembered } => {
                    let action = match event.action.kind {
                        GatedKind::Submit => format!("{} {target}", event.action.method),
                        GatedKind::Navigate => format!("Navigation to {target}"),
                    };
                    return Err(AgentError::ApprovalDenied {
                        action,
                        remembered,
                        observation: Box::new(observation),
                    });
                }
                PolicyDecision::Blocked => {
                    return Err(AgentError::NavigationBlocked {
                        target,
                        observation: Box::new(observation),
                    });
                }
                PolicyDecision::Allowed | PolicyDecision::Approved => {}
            }
        }
        Ok(observation)
    }

    fn click_raw(&mut self, id: NodeId) -> Result<(), AgentError> {
        let rect = self.target_rect(id)?;
        self.scroll_into_view(rect);
        let rect = self.target_rect(id)?;
        let point = self.click_point(rect);
        // Move first so :hover lands, then press and release on their own
        // frames — the same edge/level sequence the window produces.
        self.frame(WindowInput {
            mouse_position: Some(point),
            ..WindowInput::default()
        });
        self.frame(WindowInput {
            mouse_position: Some(point),
            left_mouse_pressed: true,
            left_mouse_held: true,
            ..WindowInput::default()
        });
        self.frame(WindowInput {
            mouse_position: Some(point),
            ..WindowInput::default()
        });
        Ok(())
    }

    // -----------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------

    fn target_rect(&mut self, id: NodeId) -> Result<Rect, AgentError> {
        if self.state.document_generation() != self.issued_generation || !self.issued.contains(&id)
        {
            return Err(AgentError::StaleElement {
                id,
                page_changed: true,
            });
        }
        if self.state.parsed_document.borrow().get(id).is_none() {
            return Err(AgentError::StaleElement {
                id,
                page_changed: false,
            });
        }
        if self.state.current_view().is_none() {
            self.frame(WindowInput::default());
        }
        let view = self.state.current_view().expect("view after a frame");
        match find_rect(&view.layout_root, id) {
            Some(rect) if rect.width > 0.0 && rect.height > 0.0 => Ok(rect),
            Some(_) => Err(AgentError::NotInteractable {
                id,
                reason: "it has zero size",
            }),
            None => Err(AgentError::NotInteractable {
                id,
                reason: "it is not rendered",
            }),
        }
    }

    // Bring the element's box into the viewport, centred, via the wheel.
    fn scroll_into_view(&mut self, rect: Rect) {
        let viewport = self.config.viewport_height as f32;
        let scroll = self.state.scroll_offset;
        if rect.y >= scroll && rect.y + rect.height <= scroll + viewport {
            return;
        }
        let target = (rect.y + rect.height / 2.0 - viewport / 2.0).max(0.0);
        self.frame(WindowInput {
            scroll_y: -(target - scroll) / PX_PER_SCROLL_TICK,
            ..WindowInput::default()
        });
    }

    // Window coordinates of the element's centre, clamped into the visible
    // part of the box for elements taller than the viewport.
    fn click_point(&self, rect: Rect) -> (f32, f32) {
        let scroll = self.state.scroll_offset;
        let viewport = self.config.viewport_height as f32;
        let top = rect.y.max(scroll);
        let bottom = (rect.y + rect.height).min(scroll + viewport);
        let doc_y = if bottom > top {
            (top + bottom) / 2.0
        } else {
            rect.y + rect.height / 2.0
        };
        (rect.x + rect.width / 2.0, doc_y - scroll + CHROME_HEIGHT)
    }

    fn may_type_password(&self) -> bool {
        self.config.approve == ApproveMode::Auto
            && self
                .state
                .current_url
                .as_ref()
                .is_some_and(|url| is_local_dev_host(&url.host))
    }

    fn frame(&mut self, mut input: WindowInput) {
        input.from_agent = true;
        let width = self.config.viewport_width;
        let height = self.config.viewport_height + CHROME_HEIGHT as usize;
        let _ = self.state.display_list(width, height, &input);
    }

    fn revision(&self) -> u64 {
        self.state.parsed_document.borrow().revision()
    }

    /// Run frames until the page is quiet (docs/concept.md "settle"): no
    /// navigation in flight, no timer due within `TIMER_HORIZON_MS`, and
    /// the DOM revision unchanged for `QUIET_FRAMES` frames in a row.
    /// Returns false when the frame or wall-clock budget ran out first.
    pub fn settle(&mut self) -> bool {
        let started = Instant::now();
        let mut quiet = 0;
        let mut last_revision = self.revision();
        let mut last_url = self.state.current_url.clone();
        for _ in 0..MAX_SETTLE_FRAMES {
            if let Some(clock) = &self.clock {
                clock.advance(FRAME_MS);
            }
            self.frame(WindowInput::default());

            let navigating = self.state.has_pending_navigation();
            let timer_soon = self.state.js.next_timer_deadline().is_some_and(|deadline| {
                deadline.saturating_sub(self.state.js.clock_now_ms()) <= TIMER_HORIZON_MS
            });
            let revision = self.revision();
            let url = self.state.current_url.clone();
            let unchanged = revision == last_revision && url == last_url;
            last_revision = revision;
            last_url = url;

            if !navigating && !timer_soon && unchanged {
                quiet += 1;
                if quiet >= QUIET_FRAMES {
                    self.last_settled = Some(true);
                    return true;
                }
            } else {
                quiet = 0;
            }
            if started.elapsed() > self.config.settle_timeout {
                break;
            }
            if navigating {
                // The load runs on a worker thread in real time.
                std::thread::sleep(Duration::from_millis(2));
            } else if self.clock.is_none() {
                std::thread::sleep(Duration::from_millis(FRAME_MS));
            }
        }
        self.last_settled = Some(false);
        false
    }
}

/// Hosts where an agent may type test credentials (docs/concept.md
/// "자격증명 규칙"): loopback and the reserved `.localhost` / `.test` names.
pub fn is_local_dev_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host == "localhost"
        || host == "127.0.0.1"
        || host == "::1"
        || host.ends_with(".localhost")
        || host.ends_with(".test")
}

#[cfg(test)]
mod tests {
    use super::is_local_dev_host;

    #[test]
    fn local_dev_hosts() {
        for host in [
            "localhost",
            "127.0.0.1",
            "[::1]",
            "app.localhost",
            "shop.test",
        ] {
            assert!(is_local_dev_host(host), "{host}");
        }
        for host in [
            "example.com",
            "localhost.evil.com",
            "test.example",
            "10.0.0.1",
        ] {
            assert!(!is_local_dev_host(host), "{host}");
        }
    }
}
