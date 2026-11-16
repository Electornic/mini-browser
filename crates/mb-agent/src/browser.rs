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
use mb_runtime::render::rasterize;
use mb_runtime::state::policy::{AgentPolicy, ApproveMode, GatedKind, PolicyDecision};
use mb_runtime::state::{BrowserState, install_fonts};

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
    /// Load system (and page) fonts like the window does. Real text metrics
    /// and readable screenshots, but layout then depends on the machine's
    /// fonts — so off by default, and off in the golden/determinism tests.
    pub system_fonts: bool,
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
            system_fonts: false,
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
    // Page font count last installed (system_fonts only); `None` = never.
    installed_fonts: Option<usize>,
    // Element the last click/type aimed at, with the document it was on —
    // for the observation window's highlight.
    last_target: Option<(u64, NodeId)>,
    // Ask mode: an action waiting for the person watching.
    pending_approval: Option<PendingApproval>,
    // (document generation, url) of the last observation handed out.
    last_reported: Option<(u64, Option<String>)>,
    // A password handoff in progress: the page before the person typed,
    // its document generation, and the field handed over.
    handoff_before: Option<(Observation, u64, NodeId)>,
}

/// An agent action held for approval, with what's needed to report its
/// outcome later.
struct PendingApproval {
    before: Observation,
    generation_before: u64,
    action: String,
    // The person had changed the page before this action started.
    moved: bool,
}

/// "POST http://…/signup" / "Navigation to http://…" — browser-computed.
fn describe_action(action: &mb_runtime::state::policy::GatedAction) -> String {
    match action.kind {
        GatedKind::Submit => format!("{} {}", action.method, action.target),
        GatedKind::Navigate => format!("Navigation to {}", action.target),
    }
}

/// A `wait_for` in progress in the observation window: `poll_wait`
/// checks it once per frame, so the window keeps drawing and taking the
/// person's input while the agent waits.
#[derive(Debug, Clone)]
pub struct PendingWait {
    condition: WaitCondition,
    // The person had changed the page before the wait began.
    moved: bool,
    // The page when the wait began, for the result's `changed:` line.
    before: Observation,
    generation_before: u64,
    started: Instant,
    timeout: Duration,
}

impl PendingWait {
    pub fn condition(&self) -> &WaitCondition {
        &self.condition
    }

    /// Time left before `wait_timeout`.
    pub fn remaining(&self) -> Duration {
        self.timeout.saturating_sub(self.started.elapsed())
    }
}

/// What `wait_for` waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitCondition {
    /// This text appears in the page text or an element name / value.
    Text(String),
    /// This element is rendered (and was handed out earlier, or appears now).
    Element(NodeId),
}

/// A rendered frame of the page area (no browser chrome), row-major
/// `0x00RRGGBB` pixels.
pub struct Screenshot {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u32>,
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
            installed_fonts: None,
            last_target: None,
            pending_approval: None,
            last_reported: None,
            handoff_before: None,
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
        let moved = self.changed_by_person();
        self.report(options, moved)
    }

    // Did the document change since the agent's last result, without the
    // agent? (Only the person at the window can do that: page scripts have
    // no navigation API, and agent navigations update `last_reported`.)
    pub fn changed_by_person(&self) -> bool {
        self.last_reported.as_ref().is_some_and(|last| {
            *last
                != (
                    self.state.document_generation(),
                    self.state.current_url.as_ref().map(|u| u.to_string()),
                )
        })
    }

    // An observation handed to the agent: issues its ids, drains console /
    // dialog records, and becomes the baseline for `changed_by_person`.
    fn report(&mut self, options: &ObserveOptions, changed_by_person: bool) -> Observation {
        let mut observation = self.observe_raw(options);
        observation.changed_by_person = changed_by_person;
        self.last_reported = Some((
            self.state.document_generation(),
            self.state.current_url.as_ref().map(|u| u.to_string()),
        ));
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

    /// The whole document as an observation, without issuing ids or
    /// draining console / dialog records (for diffs and session logs).
    pub fn full_snapshot(&mut self) -> Observation {
        self.observe_raw(&ObserveOptions {
            scope: Scope::Full,
            max_elements: usize::MAX,
            // Text is needed for the `changes:` text diff.
            max_text_chars: 100_000,
            cursor: 0,
        })
    }

    // -----------------------------------------------------------------
    // Observation window support (G3)
    // -----------------------------------------------------------------

    /// Follow the window: the page area the agent lays out against is the
    /// one the human sees.
    pub fn set_viewport(&mut self, width: usize, height: usize) {
        self.config.viewport_width = width.max(1);
        self.config.viewport_height = height.max(1);
    }

    /// A frame driven by the person at the window. Never tagged as agent
    /// input, so the policy gate lets it through like any human action.
    pub fn human_frame(&mut self, input: &WindowInput) -> Vec<mb_runtime::render::DisplayCommand> {
        self.sync_fonts();
        let mut input = input.clone();
        input.from_agent = false;
        let width = self.config.viewport_width;
        let height = self.config.viewport_height + CHROME_HEIGHT as usize;
        self.state.display_list(width, height, &input)
    }

    /// Every interactive element on the page with its rect, for drawing
    /// id badges. Read-only: unlike `observe` it issues no ids and drains
    /// no console/dialog records.
    pub fn element_boxes(&mut self) -> Vec<crate::ElementObservation> {
        self.observe_raw(&ObserveOptions {
            scope: Scope::Full,
            max_elements: usize::MAX,
            max_text_chars: 0,
            cursor: 0,
        })
        .elements
    }

    /// Point the window's target outline at `id` (e.g. the password field a
    /// person is asked to fill in).
    pub fn highlight(&mut self, id: NodeId) {
        self.last_target = Some((self.state.document_generation(), id));
    }

    /// The element the last click / type aimed at, if it's on the current
    /// document.
    pub fn last_target(&self) -> Option<NodeId> {
        self.last_target
            .filter(|(generation, _)| *generation == self.state.document_generation())
            .map(|(_, id)| id)
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

    /// Keep the page running until `condition` holds or `timeout` passes
    /// (virtual time under `ClockMode::Virtual`, wall time otherwise).
    /// Unlike settle, this lets slow timers fire: it's how an agent waits
    /// for "the toast that appears after 2 seconds".
    pub fn wait_for(
        &mut self,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<Observation, AgentError> {
        // Waiting on an element is only meaningful for an id this page's
        // observations handed out. Never grant ids here: a guessed or
        // previous-page id must stay unusable (same rule as `target_rect`).
        if let WaitCondition::Element(id) = condition
            && !self.is_issued(id)
        {
            return Err(AgentError::StaleElement {
                id,
                page_changed: true,
                by_person: self.changed_by_person(),
            });
        }
        let moved = self.changed_by_person();
        let before = self.full_snapshot();
        let generation_before = self.state.document_generation();
        let started = Instant::now();
        let start_clock = self.state.js.clock_now_ms();
        let timeout_ms = timeout.as_millis() as u64;
        loop {
            self.settle();
            if self.condition_holds(&condition) {
                // The returned observation issues whatever ids are on the
                // page now, through the normal path.
                let moved = moved || self.changed_by_person();
                return Ok(self.finish_wait(&before, generation_before, moved));
            }
            let elapsed = match self.clock {
                Some(_) => self.state.js.clock_now_ms().saturating_sub(start_clock),
                None => started.elapsed().as_millis() as u64,
            };
            if elapsed >= timeout_ms || started.elapsed() > timeout + self.config.settle_timeout {
                let moved = moved || self.changed_by_person();
                return Err(AgentError::WaitTimeout {
                    waited_ms: elapsed,
                    observation: Box::new(self.finish_wait(&before, generation_before, moved)),
                });
            }
            // Let time pass even when the page is "quiet": the thing we're
            // waiting for may sit behind a timer beyond settle's horizon.
            for _ in 0..8 {
                if let Some(clock) = &self.clock {
                    clock.advance(FRAME_MS);
                } else {
                    std::thread::sleep(Duration::from_millis(FRAME_MS));
                }
                self.frame(WindowInput::default());
            }
        }
    }

    // A `wait_for` result: the observation plus what changed while
    // waiting (the agent did nothing meanwhile, so it's the page or the
    // person).
    fn finish_wait(
        &mut self,
        before: &Observation,
        generation_before: u64,
        moved: bool,
    ) -> Observation {
        let after = self.full_snapshot();
        let new_document = self.state.document_generation() != generation_before;
        let mut observation = self.report(&ObserveOptions::default(), moved);
        observation.changed = Some(Changes::between(before, &after, new_document));
        observation
    }

    /// Window mode: start a wait without blocking. The window's frames
    /// move the page along; `poll_wait` checks after each one.
    pub fn begin_wait(
        &mut self,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<PendingWait, AgentError> {
        if let WaitCondition::Element(id) = condition
            && !self.is_issued(id)
        {
            return Err(AgentError::StaleElement {
                id,
                page_changed: true,
                by_person: self.changed_by_person(),
            });
        }
        Ok(PendingWait {
            condition,
            moved: self.changed_by_person(),
            before: self.full_snapshot(),
            generation_before: self.state.document_generation(),
            started: Instant::now(),
            timeout,
        })
    }

    /// `None` while still waiting; the result once the condition holds or
    /// the time is up. Any page change during the wait was the person's
    /// (the agent is waiting), so the result notes it.
    pub fn poll_wait(&mut self, wait: &PendingWait) -> Option<Result<Observation, AgentError>> {
        let moved = wait.moved || self.changed_by_person();
        let done = self.condition_holds(&wait.condition);
        if !done && wait.started.elapsed() < wait.timeout {
            return None;
        }
        let observation = self.finish_wait(&wait.before, wait.generation_before, moved);
        Some(if done {
            Ok(observation)
        } else {
            Err(AgentError::WaitTimeout {
                waited_ms: wait.started.elapsed().as_millis() as u64,
                observation: Box::new(observation),
            })
        })
    }

    fn condition_holds(&mut self, condition: &WaitCondition) -> bool {
        let snapshot = self.observe_raw(&ObserveOptions {
            scope: Scope::Full,
            max_elements: usize::MAX,
            max_text_chars: usize::MAX,
            cursor: 0,
        });
        match condition {
            WaitCondition::Text(needle) => {
                snapshot.text.iter().any(|t| t.contains(needle.as_str()))
                    || snapshot.title.as_deref().is_some_and(|t| t.contains(needle.as_str()))
                    || snapshot.elements.iter().any(|e| {
                        e.name.contains(needle.as_str())
                            || matches!(&e.value, Some(crate::ElementValue::Text(v)) if v.contains(needle.as_str()))
                    })
            }
            WaitCondition::Element(id) => {
                // A navigation during the wait makes the id meaningless.
                if !self.is_issued(*id) {
                    return false;
                }
                let view = self.state.current_view();
                self.state.parsed_document.borrow().get(*id).is_some()
                    && view
                        .and_then(|v| find_rect(&v.layout_root, *id))
                        .is_some_and(|r| r.width > 0.0 && r.height > 0.0)
            }
        }
    }

    /// Render the page area as it looks now.
    pub fn screenshot(&mut self) -> Screenshot {
        self.sync_fonts();
        let width = self.config.viewport_width;
        let chrome = CHROME_HEIGHT as usize;
        let window_height = self.config.viewport_height + chrome;
        let commands = self.state.display_list(
            width,
            window_height,
            &WindowInput {
                from_agent: true,
                ..WindowInput::default()
            },
        );
        let full = rasterize(&commands, width, window_height);
        Screenshot {
            width,
            height: self.config.viewport_height,
            pixels: full[chrome * width..].to_vec(),
        }
    }

    // Shared action envelope: snapshot → act → settle → policy verdict →
    // observation with a `changed:` summary.
    fn act(
        &mut self,
        action: impl FnOnce(&mut Self) -> Result<(), AgentError>,
    ) -> Result<Observation, AgentError> {
        if let Some(pending) = &self.pending_approval {
            return Err(AgentError::ApprovalPending {
                action: pending.action.clone(),
            });
        }
        let moved = self.changed_by_person();
        let before = self.full_snapshot();
        let generation_before = self.state.document_generation();
        self.state.take_policy_events();
        action(self)?;
        self.settle();
        let events = self.state.take_policy_events();
        // Ask mode: the gate parked the navigation. Remember where we were
        // so the answer can be reported as this action's result.
        if let Some(held) = events.iter().find(|e| e.decision == PolicyDecision::Held) {
            let action = describe_action(&held.action);
            self.pending_approval = Some(PendingApproval {
                before,
                generation_before,
                action: action.clone(),
                moved,
            });
            return Err(AgentError::ApprovalPending { action });
        }
        self.finish_action(&before, generation_before, events, moved)
    }

    // Observation with the `changed:` summary, or the policy verdict as an
    // error. Shared by `act` and `resolve_approval`.
    fn finish_action(
        &mut self,
        before: &Observation,
        generation_before: u64,
        events: Vec<mb_runtime::state::policy::PolicyEvent>,
        moved: bool,
    ) -> Result<Observation, AgentError> {
        let after = self.full_snapshot();
        let mut observation = self.report(&ObserveOptions::default(), moved);
        let new_document = self.state.document_generation() != generation_before;
        observation.changed = Some(Changes::between(before, &after, new_document));

        for event in events {
            let target = event.action.target.to_string();
            match event.decision {
                PolicyDecision::Denied { remembered } => {
                    return Err(AgentError::ApprovalDenied {
                        action: describe_action(&event.action),
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
                PolicyDecision::Allowed | PolicyDecision::Approved | PolicyDecision::Held => {}
            }
        }
        Ok(observation)
    }

    /// The action waiting for a person's answer (`ApproveMode::Ask`), as
    /// shown to them, e.g. "POST http://localhost:3000/signup".
    pub fn pending_approval(&self) -> Option<&str> {
        self.pending_approval.as_ref().map(|p| p.action.as_str())
    }

    /// The person answered. Approved: the held navigation runs now and the
    /// result is the observation the original action would have returned.
    /// Refused: `approval_denied` (and the refusal is remembered).
    pub fn resolve_approval(&mut self, approve: bool) -> Result<Observation, AgentError> {
        let Some(pending) = self.pending_approval.take() else {
            return Err(AgentError::NothingPending);
        };
        self.state.take_policy_events();
        self.state.resolve_held(approve);
        self.settle();
        let events = self.state.take_policy_events();
        self.finish_action(
            &pending.before,
            pending.generation_before,
            events,
            pending.moved,
        )
    }

    /// Forget what the agent was last shown, so the next result doesn't
    /// count earlier changes as the person's (e.g. after `--start-url`,
    /// which no agent saw).
    pub fn forget_reported(&mut self) {
        self.last_reported = None;
    }

    /// A password handoff begins (window mode): remember the page so the
    /// result can say what the person changed.
    pub fn begin_handoff(&mut self, field: NodeId) {
        let before = self.full_snapshot();
        self.handoff_before = Some((before, self.state.document_generation(), field));
    }

    /// The person says they're done: the observation now, with a
    /// `changed:` summary since `begin_handoff` (values stay hidden).
    pub fn finish_handoff(&mut self) -> Observation {
        let moved = self.changed_by_person();
        let Some((before, generation_before, field)) = self.handoff_before.take() else {
            return self.observe(&ObserveOptions::default());
        };
        self.settle();
        let after = self.full_snapshot();
        let new_document = self.state.document_generation() != generation_before;
        let mut observation = self.report(&ObserveOptions::default(), moved && new_document);
        let changes = Changes::between(&before, &after, new_document);
        // Done doesn't prove anything was typed: say so when the field the
        // person was handed didn't change (and they didn't submit).
        let filled = changes.values_changed.iter().any(|c| c.id == field);
        observation.notes.push(if new_document {
            "the person filled in the field and submitted the form themselves".into()
        } else if filled {
            format!("the person typed into field e{} and pressed Done", field.raw())
        } else {
            format!(
                "the person pressed Done but field e{} did not change — the password was probably not entered",
                field.raw()
            )
        });
        observation.changed = Some(changes);
        observation
    }

    /// Replay only (`mb-mcp replay`): do what the person did during a
    /// recorded password handoff — click field `id` and type `text` as
    /// *human* input (`from_agent = false`), so the policy and the
    /// password rule see exactly what they saw when it was recorded.
    pub fn fill_as_person(&mut self, id: NodeId, text: &str) -> Result<(), AgentError> {
        let rect = self.target_rect(id)?;
        self.scroll_into_view(rect);
        let rect = self.target_rect(id)?;
        let point = self.click_point(rect);
        let press = WindowInput {
            mouse_position: Some(point),
            left_mouse_pressed: true,
            left_mouse_held: true,
            ..WindowInput::default()
        };
        self.human_frame(&press);
        self.human_frame(&WindowInput {
            mouse_position: Some(point),
            ..WindowInput::default()
        });
        self.human_frame(&WindowInput {
            typed: text.to_string(),
            mouse_position: Some(point),
            ..WindowInput::default()
        });
        self.settle();
        Ok(())
    }

    fn click_raw(&mut self, id: NodeId) -> Result<(), AgentError> {
        let rect = self.target_rect(id)?;
        self.last_target = Some((self.state.document_generation(), id));
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

    // Was `id` listed in an observation of the document currently shown?
    fn is_issued(&self, id: NodeId) -> bool {
        self.state.document_generation() == self.issued_generation && self.issued.contains(&id)
    }

    fn target_rect(&mut self, id: NodeId) -> Result<Rect, AgentError> {
        if !self.is_issued(id) {
            return Err(AgentError::StaleElement {
                id,
                page_changed: true,
                by_person: self.changed_by_person(),
            });
        }
        if self.state.parsed_document.borrow().get(id).is_none() {
            return Err(AgentError::StaleElement {
                id,
                page_changed: false,
                by_person: self.changed_by_person(),
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

    // Same rule as the shell: (re)install when the page's font set changes.
    fn sync_fonts(&mut self) {
        if !self.config.system_fonts {
            return;
        }
        let count = self.state.font_data.len();
        if self.installed_fonts != Some(count) {
            install_fonts(&self.state.font_data);
            self.installed_fonts = Some(count);
        }
    }

    fn frame(&mut self, mut input: WindowInput) {
        self.sync_fonts();
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
