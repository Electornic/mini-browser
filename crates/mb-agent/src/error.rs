// Agent-facing errors (docs/concept.md "에러 코드", "에러 응답 형식").
// Every variant knows its stable code, a browser-side message, and a
// one-line hint telling the LLM what to do next.

use std::fmt;

use mb_runtime::dom::NodeId;

use crate::Observation;

#[derive(Debug, Clone)]
pub enum AgentError {
    /// The element was removed from the page, or the id came from a page
    /// the browser has since navigated away from.
    StaleElement {
        id: NodeId,
        page_changed: bool,
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
            AgentError::InvalidUrl { .. } => "invalid_url",
        }
    }

    pub fn message(&self) -> String {
        match self {
            AgentError::StaleElement { id, page_changed } => {
                if *page_changed {
                    format!(
                        "Element e{} isn't on the current page: it wasn't listed in any observation since the last navigation (ids from a previous page don't carry over).",
                        id.raw()
                    )
                } else {
                    format!(
                        "Element e{} no longer exists (it was removed from the page).",
                        id.raw()
                    )
                }
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
            AgentError::InvalidUrl { url } => format!("{url:?} is not a valid http(s) URL."),
        }
    }

    pub fn hint(&self) -> &'static str {
        match self {
            AgentError::StaleElement { .. } => "Call `observe` to get the current element ids.",
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
