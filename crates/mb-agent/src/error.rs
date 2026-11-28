// Agent-facing errors (docs/concept.md "에러 코드", "에러 응답 형식").
// Every variant knows its stable code, a browser-side message, and a
// one-line hint telling the LLM what to do next.

use std::fmt;

use mb_runtime::dom::NodeId;

use crate::Observation;

/// `NotInteractable` reason for an element whose overflow clip hides all
/// of it (sr-only text, a slide outside its carousel window — step 3b).
pub const CLIPPED_OUT_OF_VIEW: &str = "it is clipped out of view";

/// `NotInteractable` reason for a `visibility: hidden` element (compat
/// round 4 step 1a — a closed menu, dialog or panel).
pub const HIDDEN: &str = "it is hidden";

/// `NotInteractable` reason for an element in an `inert` subtree.
pub const INERT: &str = "it is inert";

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
    /// The element exists but has no box to click (display:none, 0×0).
    NotInteractable {
        id: NodeId,
        reason: &'static str,
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
            AgentError::NotInteractable { id, reason } => {
                format!("Element e{} can't be acted on: {reason}.", id.raw())
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

    pub fn hint(&self) -> &'static str {
        match self {
            AgentError::StaleElement { .. } => "Call `observe` to get the current element ids.",
            AgentError::NotInteractable { reason, .. } if *reason == CLIPPED_OUT_OF_VIEW => {
                "It is visually hidden (a screen-reader-only label or skip link, or content scrolled out of a box): its text is still in the observation as information. Click the visible element that does the same thing instead."
            }
            AgentError::NotInteractable { reason, .. } if *reason == HIDDEN || *reason == INERT => {
                "It isn't shown (or is disabled) on the page right now, so a person couldn't use it either: open or enable it first with the control that does (a menu button, tab or dialog), then `observe` again."
            }
            AgentError::NotInteractable { .. } => {
                "Call `observe`; the element may be hidden or not rendered yet."
            }
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

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

impl std::error::Error for AgentError {}
