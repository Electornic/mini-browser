// Agent-facing errors (docs/concept.md "에러 코드", "에러 응답 형식").
// Every variant knows its stable code, a browser-side message, and a
// one-line hint telling the LLM what to do next.

use std::fmt;

use mb_runtime::dom::NodeId;

use crate::Observation;
use crate::role::Role;
use crate::text::{quote, truncate_chars};

/// Why `NotInteractable` refused (docs/concept.md "에러 코드"; compat round
/// 4 step 1 reshaped it from a string so a refusal can say what covers the
/// element — N3).
#[derive(Debug, Clone, PartialEq)]
pub enum NotInteractableReason {
    /// No layout box (`display: none`, not laid out yet).
    NotRendered,
    /// A box of zero width or height.
    ZeroSize,
    /// Its overflow clip hides all of it (sr-only text, a slide outside its
    /// carousel window — step 3b); the hint depends on what clips it (1c).
    Clipped(ClipHint),
    /// Nothing of it is inside the viewport's width and the engine has no
    /// horizontal scrolling (step 1c, N3). `right`: it lies to the right.
    OutsideViewportX { right: bool },
    /// `visibility: hidden` (a closed menu, dialog or panel — step 1a).
    Hidden,
    /// Inside an `inert` subtree (step 1a, N4).
    Inert,
    /// `type` clicked the field but it didn't take focus.
    NoFocus,
    /// Something else is on top at the click point (step 1b, N3).
    Intercepted(Box<Interceptor>),
}

/// What clips a `Clipped` element away (docs/compat-round4.md 1c "거절
/// 사유와 힌트").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipHint {
    /// Its nearest clipping ancestor is `overflow: hidden`/`clip` and it
    /// lies sideways outside it — a carousel or slider slide.
    Carousel,
    /// Its nearest clipping ancestor scrolls (`overflow: auto`/`scroll`) —
    /// the engine can't scroll inside boxes.
    ScrollContainer,
    /// Anything else (sr-only text, clipped vertically — step 3b).
    Other,
}

/// The URL the refused target's link would load — resolved against the
/// page, http(s) only (the URLs `navigate` takes) — so the agent can go
/// there directly. Page-derived: untrusted part only (the user's 1b
/// threshold decision, 2026-11-28; 1c's outside-the-viewport and
/// scroll-container refusals too).
#[derive(Debug, Clone, PartialEq)]
pub struct PageLink {
    pub url: String,
    /// The page origin, for the untrusted label.
    pub origin: Option<String>,
}

/// What took an intercepted click. `role` and `id` are the browser's own
/// words; `name` and `tag` come from the page and only ever go out in the
/// untrusted part ([`AgentError::page_content`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Interceptor {
    /// The id the agent already has for it (ids come from observations
    /// only); `None` when it was never listed.
    pub id: Option<NodeId>,
    /// Its role, when it (or an ancestor) is something the agent can act on.
    pub role: Option<Role>,
    /// Its accessible name (page text).
    pub name: String,
    /// The element's tag when it has no role (page markup).
    pub tag: String,
    /// The page origin, for the untrusted label.
    pub origin: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AgentError {
    /// The element was removed from the page, or the id came from a page
    /// the browser has since navigated away from.
    StaleElement {
        id: NodeId,
        page_changed: bool,
        /// The person changed the page in the window since the agent's
        /// last result (that's why the id went stale).
        by_person: bool,
    },
    /// The element can't be pressed the way a person would: no box, hidden,
    /// clipped away, or covered by something else.
    NotInteractable {
        id: NodeId,
        reason: NotInteractableReason,
        /// For a checkbox / radio: an already-issued `<label>` of it, which
        /// toggles it (the checkbox hack hides the box and shows the label).
        label: Option<NodeId>,
        /// For the reasons that offer it (intercepted, outside the viewport,
        /// in a scroll container): the target's link.
        link: Option<Box<PageLink>>,
    },
    /// `type` on something that isn't a text field.
    NotATextField {
        id: NodeId,
    },
    /// A step only a human should do (password on a non-local site).
    HandoffRequired {
        id: NodeId,
        reason: &'static str,
    },
    /// The policy gate refused a risky action. Carries the observation of
    /// the page as it is after the refusal (nothing navigated).
    ApprovalDenied {
        action: String,
        remembered: bool,
        observation: Box<Observation>,
    },
    /// Outside `--allow-origin`.
    NavigationBlocked {
        target: String,
        observation: Box<Observation>,
    },
    UnsupportedKey {
        key: String,
    },
    /// Ask mode: the action is waiting for the person watching to approve
    /// it. (Transports that can wait keep the call open instead.)
    ApprovalPending {
        action: String,
    },
    /// `resolve_approval` with nothing waiting.
    NothingPending,
    /// `wait_for` gave up. Carries the page as it was at the deadline.
    WaitTimeout {
        waited_ms: u64,
        observation: Box<Observation>,
    },
    InvalidUrl {
        url: String,
    },
}

impl AgentError {
    pub fn code(&self) -> &'static str {
        match self {
            AgentError::StaleElement { .. } => "stale_element",
            AgentError::NotInteractable { .. } => "not_interactable",
            AgentError::NotATextField { .. } => "not_a_text_field",
            AgentError::HandoffRequired { .. } => "handoff_required",
            AgentError::ApprovalDenied { .. } => "approval_denied",
            AgentError::NavigationBlocked { .. } => "navigation_blocked",
            AgentError::UnsupportedKey { .. } => "unsupported_key",
            AgentError::WaitTimeout { .. } => "wait_timeout",
            AgentError::ApprovalPending { .. } => "approval_pending",
            AgentError::NothingPending => "nothing_pending",
            AgentError::InvalidUrl { .. } => "invalid_url",
        }
    }

    pub fn message(&self) -> String {
        match self {
            AgentError::StaleElement {
                id,
                page_changed,
                by_person,
            } => {
                let mut message = if *page_changed {
                    format!(
                        "Element e{} isn't on the current page: it wasn't listed in any observation since the last navigation (ids from a previous page don't carry over).",
                        id.raw()
                    )
                } else {
                    format!(
                        "Element e{} no longer exists (it was removed from the page).",
                        id.raw()
                    )
                };
                if *by_person {
                    message.push_str(
                        " The person changed the page in the window since your last result.",
                    );
                }
                message
            }
            AgentError::NotInteractable { id, reason, .. } => {
                format!(
                    "Element e{} can't be acted on: {}.",
                    id.raw(),
                    reason.describe()
                )
            }
            AgentError::NotATextField { id } => {
                format!("Element e{} is not a text field.", id.raw())
            }
            AgentError::HandoffRequired { id, reason } => {
                format!("Element e{} needs a human: {reason}.", id.raw())
            }
            AgentError::ApprovalDenied {
                action, remembered, ..
            } => {
                if *remembered {
                    format!("{action} was already refused earlier in this session.")
                } else {
                    format!("{action} was refused by the approval policy.")
                }
            }
            AgentError::NavigationBlocked { target, .. } => {
                format!("Navigation to {target} is outside the allowed origins.")
            }
            AgentError::UnsupportedKey { key } => format!("Key {key:?} is not supported."),
            AgentError::WaitTimeout { waited_ms, .. } => {
                format!("The condition did not become true within {waited_ms} ms.")
            }
            AgentError::ApprovalPending { action } => {
                format!("{action} is waiting for the person watching to approve it.")
            }
            AgentError::NothingPending => "No action is waiting for approval.".into(),
            AgentError::InvalidUrl { url } => format!("{url:?} is not a valid http(s) URL."),
        }
    }

    pub fn hint(&self) -> String {
        if let AgentError::NotInteractable {
            reason,
            label,
            link,
            ..
        } = self
        {
            let mut hint = reason.hint(link.is_some());
            if let Some(label) = label {
                hint.push_str(&format!(
                    " Its label e{} toggles it the way a person would: click that instead.",
                    label.raw()
                ));
            }
            return hint;
        }
        self.static_hint().to_string()
    }

    fn static_hint(&self) -> &'static str {
        match self {
            AgentError::StaleElement { .. } => "Call `observe` to get the current element ids.",
            // `hint()` builds this one from the reason.
            AgentError::NotInteractable { .. } => "",
            AgentError::NotATextField { .. } => {
                "Use `type` only on elements listed as textbox; use `click` for others."
            }
            AgentError::HandoffRequired { .. } => {
                "Tell the user what to enter in the browser window and wait; do not ask them to paste it to you."
            }
            AgentError::ApprovalDenied { .. } => {
                "Do not retry or look for a workaround; report this to the user."
            }
            AgentError::NavigationBlocked { .. } => {
                "Stay on the allowed origins, or ask the user to allow this one."
            }
            AgentError::UnsupportedKey { .. } => {
                "Supported keys: Enter, Backspace, Tab, Escape, ArrowUp, ArrowDown, PageUp, PageDown."
            }
            AgentError::InvalidUrl { .. } => "Use an absolute http:// or https:// URL.",
            AgentError::WaitTimeout { .. } => {
                "Check the observation: the page may show an error, or the text may differ."
            }
            AgentError::ApprovalPending { .. } => {
                "Wait for the person's answer; don't start other actions meanwhile."
            }
            AgentError::NothingPending => "There is nothing to resolve.",
        }
    }

    /// Page-written details of the error, already labelled as untrusted
    /// page content — kept out of `message`/`hint`, which only the browser
    /// writes (the trusted block rule, AGENTS.md): what took an intercepted
    /// click, and the refused target's link.
    pub fn page_content(&self) -> Option<String> {
        let AgentError::NotInteractable { reason, link, .. } = self else {
            return None;
        };
        let by = match reason {
            NotInteractableReason::Intercepted(by) => Some(by.as_ref()),
            _ => None,
        };
        if by.is_none() && link.is_none() {
            return None;
        }
        let origin = by
            .and_then(|b| b.origin.as_deref())
            .or_else(|| link.as_ref().and_then(|l| l.origin.as_deref()));
        let mut out = format!(
            "# page content (untrusted, origin={})",
            origin.unwrap_or("unknown")
        );
        if let Some(by) = by {
            let what = match (by.role, by.id) {
                (Some(role), Some(id)) => {
                    format!("[e{}] {role} name={}", id.raw(), quote(&by.name))
                }
                (Some(role), None) => format!("{role} name={}", quote(&by.name)),
                (None, _) => format!("<{}> (not an element you can act on)", tag_text(&by.tag)),
            };
            out.push_str(&format!("\ncovered by: {what}"));
        }
        if let Some(link) = link {
            out.push_str(&format!("\nlink: {}", quote(&link.url)));
        }
        Some(out)
    }

    /// The page state after a policy refusal, when there is one.
    pub fn observation(&self) -> Option<&Observation> {
        match self {
            AgentError::ApprovalDenied { observation, .. }
            | AgentError::NavigationBlocked { observation, .. }
            | AgentError::WaitTimeout { observation, .. } => Some(observation),
            _ => None,
        }
    }
}

impl NotInteractableReason {
    /// The browser's sentence for it (after "can't be acted on: ").
    fn describe(&self) -> String {
        match self {
            Self::NotRendered => "it is not rendered".into(),
            Self::ZeroSize => "it has zero size".into(),
            Self::Clipped(_) => "it is clipped out of view".into(),
            Self::OutsideViewportX { right } => format!(
                "it lies entirely to the {} of the viewport",
                if *right { "right" } else { "left" }
            ),
            Self::Hidden => "it is hidden".into(),
            Self::Inert => "it is inert".into(),
            Self::NoFocus => "clicking it did not give it focus".into(),
            Self::Intercepted(by) => {
                let what = match (by.role, by.id) {
                    (Some(role), Some(id)) => format!("a {role} e{}", id.raw()),
                    (Some(role), None) => format!("a {role} not in your observations"),
                    (None, _) => "an element you can't act on".into(),
                };
                format!(
                    "it is covered at its click point by {what} (shown below as page content), which would take the click"
                )
            }
        }
    }

    /// `link`: the error carries the target's link URL (page content).
    fn hint(&self, link: bool) -> String {
        // The intended detour past what the engine can't do (no scrolling
        // sideways or inside boxes): the keyboard path doesn't hit-test, and
        // Chrome too activates a Tab-focused element with Enter.
        let detour = if link {
            " It is a link: `navigate` to its URL (the `link:` line below)."
        } else {
            " Focus it with Tab (press Tab until it has focus), then press Enter."
        };
        match self {
            Self::Clipped(ClipHint::Carousel) => "It is outside the visible window of a box that hides its overflow — likely a slide of a carousel or slider: use that box's next/previous controls to bring it into view, then `observe` again.".into(),
            Self::Clipped(ClipHint::ScrollContainer) => {
                format!("It is outside the visible part of a scrollable box, and the engine can't scroll inside boxes.{detour}")
            }
            Self::OutsideViewportX { .. } => {
                format!("The page is wider than the viewport and the engine can't scroll sideways.{detour}")
            }
            Self::Clipped(ClipHint::Other) => "It is visually hidden (a screen-reader-only label or skip link, or content scrolled out of a box): its text is still in the observation as information. Click the visible element that does the same thing instead.".into(),
            Self::Hidden | Self::Inert => "It isn't shown (or is disabled) on the page right now, so a person couldn't use it either: open or enable it first with the control that does (a menu button, tab or dialog), then `observe` again.".into(),
            // No Tab + Enter detour here: a cover is usually a dialog or a
            // consent banner the page wants answered first (docs/compat-
            // round4.md 1b).
            Self::Intercepted(by) => {
                let mut hint = match by.id {
                    Some(id) => format!(
                        "Something is on top of it, as for a person in the window. If e{} is a dialog, banner or menu, deal with it first (close, accept or dismiss it), then `observe` again; don't click e{} itself unless that is what you want.",
                        id.raw(),
                        id.raw()
                    ),
                    None => "Something is on top of it, as for a person in the window: close or dismiss what covers it first (a dialog, banner or menu), then `observe` again.".into(),
                };
                if link {
                    hint.push_str(" It is a link: if nothing needs dismissing, `navigate` to its URL (the `link:` line below) instead.");
                }
                hint
            }
            Self::NotRendered | Self::ZeroSize | Self::NoFocus => {
                "Call `observe`; the element may be hidden or not rendered yet.".into()
            }
        }
    }
}

/// A tag name as page content: element names are page-chosen too (custom
/// elements), so they're cut and reduced to name characters.
fn tag_text(tag: &str) -> String {
    truncate_chars(tag, 40)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for AgentError {}
