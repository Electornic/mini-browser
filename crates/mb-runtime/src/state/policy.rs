// Agent policy gate (docs/concept.md "정책 게이트").
//
// Agent input travels the exact same path as human input (synthetic
// `WindowInput`), so the only place a risky action can be stopped is where
// that path ends: the navigation entry points (`navigate`,
// `navigate_to_href`, `submit_post`). Each of them asks `BrowserState::gate`
// before starting a load. Human-originated input always passes; the gate
// only judges actions the agent's input caused.
//
// Every new way of navigating (location.href =, window.open, meta refresh,
// ...) must go through `gate` too — see AGENTS.md "Agent-track Rules".

use std::collections::BTreeSet;

use crate::net::Url;

/// What the gate does with a risky agent action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApproveMode {
    /// Approve everything. For tests and evals against local fixtures.
    Auto,
    /// Refuse every risky action. Also the default: an agent nobody
    /// configured gets the safe behaviour. (G4 adds an `Ask` mode that
    /// holds the action until a human answers.)
    #[default]
    Deny,
}

#[derive(Debug, Clone, Default)]
pub struct AgentPolicy {
    pub approve: ApproveMode,
    /// When non-empty, agent navigation outside these origins
    /// (`scheme://host[:port]`) is blocked outright, approved or not.
    pub allow_origins: Vec<String>,
    /// Actions refused earlier this session. Retrying the same one (from a
    /// different button, by typing the URL, ...) is refused again without
    /// asking.
    denied: BTreeSet<String>,
}

impl AgentPolicy {
    pub fn new(approve: ApproveMode, allow_origins: Vec<String>) -> Self {
        Self {
            approve,
            allow_origins,
            denied: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatedKind {
    /// URL bar, link click, or GET form.
    Navigate,
    /// A form submission that isn't a plain GET.
    Submit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatedAction {
    pub kind: GatedKind,
    pub method: String,
    pub target: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyDecision {
    /// Not risky, or human input. Nothing to report.
    Allowed,
    /// Risky, approved by the current mode.
    Approved,
    /// Risky, refused (by the mode, or because it was refused before).
    Denied { remembered: bool },
    /// Outside `allow_origins`.
    Blocked,
}

/// What the gate saw and decided, for the agent layer to turn into a
/// result or an error (`approval_denied`, `navigation_blocked`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEvent {
    pub action: GatedAction,
    pub decision: PolicyDecision,
}

pub fn origin_of(url: &Url) -> String {
    let default_port = if url.scheme == "https" { 443 } else { 80 };
    if url.port == default_port {
        format!("{}://{}", url.scheme, url.host)
    } else {
        format!("{}://{}:{}", url.scheme, url.host, url.port)
    }
}

/// The decision table. Pure, so it's unit-tested on its own.
pub fn decide(
    policy: &mut AgentPolicy,
    from_agent: bool,
    current: Option<&Url>,
    action: &GatedAction,
) -> PolicyDecision {
    if !from_agent {
        return PolicyDecision::Allowed;
    }
    let target_origin = origin_of(&action.target);
    if !policy.allow_origins.is_empty() && !policy.allow_origins.contains(&target_origin) {
        return PolicyDecision::Blocked;
    }
    // Leaving a page with no origin (about:blank, the start page) isn't
    // "leaving an origin".
    let crosses_origin = current.is_some_and(|url| origin_of(url) != target_origin);
    let risky = action.kind == GatedKind::Submit || action.method != "GET" || crosses_origin;
    if !risky {
        return PolicyDecision::Allowed;
    }
    // Remember by what was attempted, not by which element tried it:
    // submits by method + exact target, navigations by target origin.
    let key = match action.kind {
        GatedKind::Submit => format!("submit {} {}", action.method, action.target),
        GatedKind::Navigate => format!("navigate {target_origin}"),
    };
    if policy.denied.contains(&key) {
        return PolicyDecision::Denied { remembered: true };
    }
    match policy.approve {
        ApproveMode::Auto => PolicyDecision::Approved,
        ApproveMode::Deny => {
            policy.denied.insert(key);
            PolicyDecision::Denied { remembered: false }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn nav(target: &str) -> GatedAction {
        GatedAction {
            kind: GatedKind::Navigate,
            method: "GET".into(),
            target: url(target),
        }
    }

    fn post(target: &str) -> GatedAction {
        GatedAction {
            kind: GatedKind::Submit,
            method: "POST".into(),
            target: url(target),
        }
    }

    #[test]
    fn human_input_always_passes() {
        let mut policy = AgentPolicy::new(ApproveMode::Deny, vec!["http://a.test".into()]);
        let here = url("http://a.test/");
        assert_eq!(
            decide(&mut policy, false, Some(&here), &post("http://evil.test/x")),
            PolicyDecision::Allowed
        );
    }

    #[test]
    fn same_origin_get_is_not_risky() {
        let mut policy = AgentPolicy::default();
        let here = url("http://a.test/");
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://a.test/next")),
            PolicyDecision::Allowed
        );
    }

    #[test]
    fn first_navigation_from_blank_is_not_a_cross_origin_move() {
        let mut policy = AgentPolicy::default();
        assert_eq!(
            decide(&mut policy, true, None, &nav("http://a.test/")),
            PolicyDecision::Allowed
        );
    }

    #[test]
    fn deny_mode_refuses_post_and_cross_origin_then_remembers() {
        let mut policy = AgentPolicy::new(ApproveMode::Deny, vec![]);
        let here = url("http://a.test/");
        assert_eq!(
            decide(
                &mut policy,
                true,
                Some(&here),
                &post("http://a.test/signup")
            ),
            PolicyDecision::Denied { remembered: false }
        );
        assert_eq!(
            decide(
                &mut policy,
                true,
                Some(&here),
                &post("http://a.test/signup")
            ),
            PolicyDecision::Denied { remembered: true }
        );
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://b.test/")),
            PolicyDecision::Denied { remembered: false }
        );
        // A different path on the same refused origin is the same refusal.
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://b.test/other")),
            PolicyDecision::Denied { remembered: true }
        );
    }

    #[test]
    fn auto_mode_approves_risky_actions() {
        let mut policy = AgentPolicy::new(ApproveMode::Auto, vec![]);
        let here = url("http://a.test/");
        assert_eq!(
            decide(
                &mut policy,
                true,
                Some(&here),
                &post("http://a.test/signup")
            ),
            PolicyDecision::Approved
        );
    }

    #[test]
    fn allow_list_blocks_even_in_auto_mode() {
        let mut policy = AgentPolicy::new(ApproveMode::Auto, vec!["http://a.test".into()]);
        let here = url("http://a.test/");
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://b.test/")),
            PolicyDecision::Blocked
        );
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://a.test:8080/")),
            PolicyDecision::Blocked,
            "a different port is a different origin"
        );
    }
}
