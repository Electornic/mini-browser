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
    /// configured gets the safe behaviour.
    #[default]
    Deny,
    /// Hold the action and ask the person watching (observation window).
    /// The navigation waits in `BrowserState` until `resolve_held`.
    Ask,
}

#[derive(Debug, Clone, Default)]
pub struct AgentPolicy {
    pub approve: ApproveMode,
    /// When non-empty, agent navigation outside these origins
    /// (`scheme://host[:port]`) is blocked outright, approved or not.
    pub allow_origins: Vec<String>,
    /// Actions the person refused earlier this session (Deny / Do it
    /// myself on an approval card). Retrying the same one (from a
    /// different button, by typing the URL, ...) is refused again without
    /// asking — whatever the mode is now.
    denied: BTreeSet<String>,
    /// Actions refused by a rule rather than by the person: `Deny` mode,
    /// or a redirect hop nobody could be asked about. Remembered the same
    /// way, but only until the person changes the mode — a refusal the
    /// old mode made must not outlive it (Deny → Ask should ask).
    refused_by_rule: BTreeSet<String>,
}

impl AgentPolicy {
    pub fn new(approve: ApproveMode, allow_origins: Vec<String>) -> Self {
        Self {
            approve,
            allow_origins,
            denied: BTreeSet::new(),
            refused_by_rule: BTreeSet::new(),
        }
    }

    /// The person switched the approval mode from the observation window
    /// (never an agent: no MCP tool reaches this). Applies from the next
    /// gated action; refusals the previous mode made are forgotten,
    /// the person's own refusals stay.
    pub fn set_approve(&mut self, approve: ApproveMode) {
        if self.approve != approve {
            self.approve = approve;
            self.refused_by_rule.clear();
        }
    }

    /// Whether `allow_origins` lets the agent go to `target` at all (an
    /// empty list = any site). Checked before anything else.
    pub fn allows(&self, target: &Url) -> bool {
        self.allow_origins.is_empty() || self.allow_origins.contains(&origin_of(target))
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
    /// Risky, waiting for a person to approve or refuse (`Ask` mode).
    Held,
}

/// What the gate saw and decided, for the agent layer to turn into a
/// result or an error (`approval_denied`, `navigation_blocked`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyEvent {
    pub action: GatedAction,
    pub decision: PolicyDecision,
}

/// A site the person (or `--allow-origin`) names, as the `scheme://host[:port]`
/// the gate compares: any http(s) URL, path / query / fragment dropped,
/// default port left out. `None` for anything else — no scheme
/// (`localhost:3000` parses as the scheme "localhost"), another scheme,
/// no host, spaces.
pub fn parse_origin(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.chars().any(char::is_whitespace) {
        return None;
    }
    Url::parse(raw).ok().map(|url| origin_of(&url))
}

pub fn origin_of(url: &Url) -> String {
    let default_port = if url.scheme == "https" { 443 } else { 80 };
    if url.port == default_port {
        format!("{}://{}", url.scheme, url.host)
    } else {
        format!("{}://{}:{}", url.scheme, url.host, url.port)
    }
}

// Remember by what was attempted, not by which element tried it: submits
// by method + exact target, navigations by target origin.
fn denial_key(action: &GatedAction) -> String {
    match action.kind {
        GatedKind::Submit => format!("submit {} {}", action.method, action.target),
        GatedKind::Navigate => format!("navigate {}", origin_of(&action.target)),
    }
}

impl AgentPolicy {
    /// The person refused this action (approval card).
    pub fn record_denial(&mut self, action: &GatedAction) {
        self.denied.insert(denial_key(action));
    }

    /// The person pressed "Forget this refusal" on the receipt the card
    /// left (observation window only — no MCP tool reaches this): the
    /// agent asking for this action again gets a fresh question instead
    /// of `approval_denied`. Clears a rule's refusal of it too, so the
    /// gate is back to where it was before anyone refused it. Returns
    /// whether anything was remembered.
    pub fn forget_denial(&mut self, action: &GatedAction) -> bool {
        let key = denial_key(action);
        let person = self.denied.remove(&key);
        let rule = self.refused_by_rule.remove(&key);
        person || rule
    }

    /// Fold a refusal made elsewhere (a redirect hop judged on the loader
    /// thread against a copy of this policy) back into the session memory.
    pub fn record_rule_refusal(&mut self, action: &GatedAction) {
        self.refused_by_rule.insert(denial_key(action));
    }
}

/// Gate state a navigation worker carries so it can judge redirect hops
/// with the same rules, without touching `BrowserState` across threads.
#[derive(Debug, Clone)]
pub struct HopGuard {
    pub policy: AgentPolicy,
    pub from_agent: bool,
    /// The page the navigation started from; "cross-origin" is measured
    /// against it, not against the previous hop.
    pub origin_page: Option<Url>,
    pub events: Vec<PolicyEvent>,
}

impl HopGuard {
    pub fn allow(&mut self, method: &str, target: Url) -> bool {
        let action = GatedAction {
            kind: if method == "GET" {
                GatedKind::Navigate
            } else {
                GatedKind::Submit
            },
            method: method.to_string(),
            target,
        };
        let mut decision = decide(
            &mut self.policy,
            self.from_agent,
            self.origin_page.as_ref(),
            &action,
        );
        // A redirect hop is judged on the loader thread mid-fetch, where
        // nobody can be asked. Refuse it; the agent sees approval_denied
        // and the person can follow the link themselves.
        if decision == PolicyDecision::Held {
            decision = PolicyDecision::Denied { remembered: false };
        }
        let allowed = matches!(decision, PolicyDecision::Allowed | PolicyDecision::Approved);
        if decision != PolicyDecision::Allowed {
            self.events.push(PolicyEvent { action, decision });
        }
        allowed
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
    if !policy.allows(&action.target) {
        return PolicyDecision::Blocked;
    }
    let target_origin = origin_of(&action.target);
    // Leaving a page with no origin (about:blank, the start page) isn't
    // "leaving an origin".
    let crosses_origin = current.is_some_and(|url| origin_of(url) != target_origin);
    let risky = action.kind == GatedKind::Submit || action.method != "GET" || crosses_origin;
    if !risky {
        return PolicyDecision::Allowed;
    }
    let key = denial_key(action);
    if policy.denied.contains(&key) || policy.refused_by_rule.contains(&key) {
        return PolicyDecision::Denied { remembered: true };
    }
    match policy.approve {
        ApproveMode::Auto => PolicyDecision::Approved,
        ApproveMode::Deny => {
            policy.refused_by_rule.insert(key);
            PolicyDecision::Denied { remembered: false }
        }
        ApproveMode::Ask => PolicyDecision::Held,
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
    fn parse_origin_normalizes_like_allow_origin_and_rejects_garbage() {
        assert_eq!(
            parse_origin(" http://localhost:3000/app?x=1#y ").as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            parse_origin("https://Staging.Example.test:443").as_deref(),
            Some("https://staging.example.test")
        );
        for garbage in [
            "",
            "hello",
            "localhost:3000",
            "ftp://example.test",
            "http://",
            "http://exa mple.test",
            "file:///etc/passwd",
            "javascript:alert(1)",
        ] {
            assert_eq!(parse_origin(garbage), None, "{garbage:?}");
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
    fn hop_guard_judges_redirects_against_the_starting_page() {
        let mut guard = HopGuard {
            policy: AgentPolicy::new(ApproveMode::Deny, vec![]),
            from_agent: true,
            origin_page: Some(url("http://a.test/start")),
            events: Vec::new(),
        };
        // Same origin hop: fine. Hop to another origin: refused and recorded.
        assert!(guard.allow("GET", url("http://a.test/next")));
        assert!(!guard.allow("GET", url("http://evil.test/grab")));
        // A 307 keeps the POST: judged as a submit.
        assert!(!guard.allow("POST", url("http://a.test/other")));
        assert_eq!(guard.events.len(), 2);
        assert_eq!(guard.events[1].action.kind, GatedKind::Submit);
    }

    #[test]
    fn ask_mode_holds_risky_actions_but_not_safe_ones() {
        let mut policy = AgentPolicy::new(ApproveMode::Ask, vec![]);
        let here = url("http://a.test/");
        assert_eq!(
            decide(
                &mut policy,
                true,
                Some(&here),
                &post("http://a.test/signup")
            ),
            PolicyDecision::Held
        );
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://a.test/next")),
            PolicyDecision::Allowed
        );
        // Once the person refused it, the same action isn't asked again.
        policy.record_denial(&post("http://a.test/signup"));
        assert_eq!(
            decide(
                &mut policy,
                true,
                Some(&here),
                &post("http://a.test/signup")
            ),
            PolicyDecision::Denied { remembered: true }
        );
    }

    #[test]
    fn a_forgotten_refusal_is_asked_again() {
        let mut policy = AgentPolicy::new(ApproveMode::Ask, vec![]);
        let here = url("http://a.test/");
        let signup = post("http://a.test/signup");
        let other = post("http://a.test/login");
        policy.record_denial(&signup);
        policy.record_denial(&other);
        assert!(policy.forget_denial(&signup));
        assert_eq!(
            decide(&mut policy, true, Some(&here), &signup),
            PolicyDecision::Held,
            "forgotten: the person is asked again"
        );
        assert_eq!(
            decide(&mut policy, true, Some(&here), &other),
            PolicyDecision::Denied { remembered: true },
            "only that refusal is forgotten"
        );
        assert!(!policy.forget_denial(&signup), "nothing left to forget");
        // A navigation refusal is keyed by origin: forgetting one target
        // forgets that origin.
        policy.record_denial(&nav("http://b.test/x"));
        assert!(policy.forget_denial(&nav("http://b.test/y")));
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://b.test/x")),
            PolicyDecision::Held
        );
    }

    #[test]
    fn redirect_hops_cannot_be_held() {
        let mut guard = HopGuard {
            policy: AgentPolicy::new(ApproveMode::Ask, vec![]),
            from_agent: true,
            origin_page: Some(url("http://a.test/")),
            events: Vec::new(),
        };
        assert!(!guard.allow("GET", url("http://b.test/")));
        assert_eq!(
            guard.events[0].decision,
            PolicyDecision::Denied { remembered: false }
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

    #[test]
    fn changing_the_mode_forgets_rule_refusals_but_not_the_persons() {
        let mut policy = AgentPolicy::new(ApproveMode::Deny, vec![]);
        let here = url("http://a.test/");
        let signup = post("http://a.test/signup");
        assert_eq!(
            decide(&mut policy, true, Some(&here), &signup),
            PolicyDecision::Denied { remembered: false }
        );
        // Same mode again: nothing changes, the refusal is still remembered.
        policy.set_approve(ApproveMode::Deny);
        assert_eq!(
            decide(&mut policy, true, Some(&here), &signup),
            PolicyDecision::Denied { remembered: true }
        );
        // Deny → Ask: what Deny mode refused is asked now.
        policy.set_approve(ApproveMode::Ask);
        assert_eq!(
            decide(&mut policy, true, Some(&here), &signup),
            PolicyDecision::Held
        );
        // The person refuses it; switching to Allow doesn't undo that.
        policy.record_denial(&signup);
        policy.set_approve(ApproveMode::Auto);
        assert_eq!(
            decide(&mut policy, true, Some(&here), &signup),
            PolicyDecision::Denied { remembered: true }
        );
        assert_eq!(
            decide(&mut policy, true, Some(&here), &nav("http://b.test/")),
            PolicyDecision::Approved
        );
    }
}
