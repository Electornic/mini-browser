// Navigation lifecycle for BrowserState. Owns the per-document loading
// pipeline: URL/form/link/refresh dispatch, the async worker channel
// that drives the blocking fetch, and the commit funnel that lands a
// loaded (or failed) document on the visible page. History pushes live
// in `state/history.rs`; this module hands off to it via
// `commit_navigation` / `restore_entry`.

use std::sync::mpsc;

use crate::dom::NodeId;
use crate::{
    async_runtime, css,
    navigation::{LoadOutcome, LoadedDocument, NavigationRequest, load_remote_document_guarded},
    net,
};

use super::policy::{GatedKind, HopGuard, PolicyDecision};
use super::{
    BrowserState, HistoryEntry, PendingKind, PendingNavigation, build_form_submit_url,
    collect_form_data, encode_form_body,
};

impl BrowserState {
    pub(super) fn navigate(&mut self) {
        let target = self.address_input.trim().to_string();
        if target.is_empty() {
            self.show_error_page("enter url", "enter url then press enter");
            return;
        }

        if self.pending_navigation.is_some() {
            // Another load is already in flight — drop the new request
            // (first-click-owns), same rationale as `reload_current`.
            return;
        }

        // Unparseable input falls through to the loader, which turns it
        // into the usual error page; there's nothing to gate.
        let request = NavigationRequest::get(target.clone());
        if let Ok(url) = net::Url::parse(&target)
            && !self.gate(GatedKind::Navigate, &url, &request, "load failed")
        {
            return;
        }

        self.spawn_navigation(request, "load failed");
    }

    // Synthesise a `submit` event on `form_id` and, if no handler
    // calls `preventDefault()`, run the form's default action: a GET
    // navigation to `action` with the URL-encoded form data appended
    // as a query string. POST is intentionally not wired yet — once
    // the network goal lands (#15-17 in the Notion plan) it can route
    // through here too. Empty action falls back to the current URL so
    // self-submitting forms (`<form>` with no action attribute) work.
    pub(super) fn try_submit_form(&mut self, form_id: NodeId) {
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

        // HTML only knows get / post / dialog; anything else is GET.
        // `dialog` closes a <dialog>, which the toy doesn't model.
        let method = match method.trim() {
            "post" => "post",
            "dialog" => {
                eprintln!("[form] method=dialog not supported, skipping submit");
                return;
            }
            _ => "get",
        };

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

        if method == "post" {
            self.submit_post(&target_action, encode_form_body(&data));
            return;
        }
        let submit_url = build_form_submit_url(&target_action, &data);
        self.navigate_to_href(&submit_url);
    }

    fn submit_post(&mut self, action: &str, body: String) {
        if self.pending_navigation.is_some() {
            return;
        }
        let resolved = match self.resolve_href(action) {
            Ok(url) => url,
            Err(error) => {
                eprintln!("{error}");
                self.show_error_page("form failed", &error);
                return;
            }
        };
        let request = NavigationRequest::form_post(resolved.to_string(), body);
        if !self.gate(GatedKind::Submit, &resolved, &request, "form failed") {
            return;
        }
        self.address_input = resolved.to_string();
        self.address_bar_selected = false;
        self.address_bar_focused = false;
        self.spawn_navigation(request, "form failed");
    }

    pub(super) fn navigate_to_href(&mut self, href: &str) {
        if self.pending_navigation.is_some() {
            // First-click-owns: a click on a link while a previous
            // navigation is still loading is dropped silently. The
            // address bar stays where the in-flight load put it.
            return;
        }

        let resolved = match self.resolve_href(href) {
            Ok(url) => url,
            Err(error) => {
                eprintln!("{error}");
                self.show_error_page("link failed", &error);
                return;
            }
        };

        let request = NavigationRequest::get(resolved.to_string());
        if !self.gate(GatedKind::Navigate, &resolved, &request, "link failed") {
            return;
        }

        if self.is_same_document_fragment(&resolved) {
            self.navigate_to_fragment(resolved);
            return;
        }

        // Sync UI feedback (URL bar shows the click target, focus moves
        // off the bar) lands the same frame as the click. The actual
        // load runs on the worker thread and commits later via
        // `poll_pending_navigation`.
        self.address_input = resolved.to_string();
        self.address_bar_selected = false;
        self.address_bar_focused = false;
        self.spawn_navigation(request, "link failed");
    }

    /// A link to `#fragment` of the document already shown (same URL up to
    /// the fragment) is a same-document navigation: browsers don't reload,
    /// they add a history entry and fire `hashchange` — what hash routers
    /// are built on.
    fn is_same_document_fragment(&self, target: &net::Url) -> bool {
        let Some(current) = &self.current_url else {
            return false;
        };
        let (target, current) = (target.to_string(), current.to_string());
        let without_fragment = |href: &str| href.split('#').next().unwrap_or("").to_string();
        target.contains('#') && without_fragment(&target) == without_fragment(&current)
    }

    fn navigate_to_fragment(&mut self, target: net::Url) {
        let href = target.to_string();
        self.address_input = href.clone();
        self.address_bar_selected = false;
        self.address_bar_focused = false;
        self.current_url = Some(target);
        self.js.navigate_to_fragment(&href);
    }

    /// Mirror the page's `location` into `current_url` (the address bar and
    /// the agent's trusted `url:` line) after `pushState` / `replaceState`.
    /// The JS side only accepts same-origin URLs; the origin check here is
    /// a second line so the trusted URL can never move to another site.
    pub(super) fn sync_url_from_js(&mut self) {
        let href = self.js.location_href();
        let Some(current) = &self.current_url else {
            return;
        };
        if href.is_empty() || href == current.to_string() {
            return;
        }
        let Ok(next) = net::Url::parse(&href) else {
            return;
        };
        if (&next.scheme, &next.host, next.port) != (&current.scheme, &current.host, current.port) {
            return;
        }
        if !self.address_bar_focused {
            self.address_input = next.to_string();
        }
        self.current_url = Some(next);
    }

    pub(super) fn spawn_navigation(
        &mut self,
        request: NavigationRequest,
        error_title: &'static str,
    ) {
        // Caller already established `pending_navigation.is_none()` and
        // resolved the target string. The worker owns the blocking
        // `load_remote_document` call; the receiver lands on
        // `pending_navigation` and `poll_pending_navigation` drains it
        // on a later frame.
        let (sender, receiver) = mpsc::channel();
        let wake = self.navigation_wake.clone();
        let guard = self.hop_guard();
        async_runtime::handle().spawn_blocking(move || {
            let _ = sender.send(run_guarded_load(&request, guard));
            // Poke the shell's `EventLoopProxy` so the next frame fires
            // immediately. Without this the main thread would have to
            // poll `pending_navigation.is_some()` via
            // `wants_continuous_redraw` and stay at 60 fps until the
            // worker landed — which it used to do.
            if let Some(hook) = wake {
                hook.call();
            }
        });
        self.pending_navigation = Some(PendingNavigation {
            kind: PendingKind::Navigate { error_title },
            receiver,
        });
        self.set_status(
            "loading…",
            css::Color {
                r: 80,
                g: 100,
                b: 140,
                a: 255,
            },
        );
    }

    pub(super) fn set_status(&mut self, text: impl Into<String>, color: css::Color) {
        self.status_text = text.into();
        self.status_color = color;
    }

    pub(super) fn show_error_page(&mut self, title: &str, message: &str) {
        self.restore_entry(self.error_entry(title, message));
    }

    pub(super) fn resolve_href(&self, href: &str) -> Result<net::Url, String> {
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

    pub(super) fn reload_current(&mut self) {
        // Refresh refetches the current document in place. Unlike navigate(), it does not
        // touch the back/forward stacks — the user expects "reload" to land on the same
        // page they were already viewing.
        if self.pending_navigation.is_some() {
            // A previous refresh is still in flight — let it finish.
            // Last-wins coalescing would force-cancel the worker, but
            // ureq has no cancellation token, so the simpler rule is
            // "first click owns the slot until the worker reports back".
            return;
        }

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

        let (sender, receiver) = mpsc::channel();
        let target = url.to_string();
        let wake = self.navigation_wake.clone();
        let guard = self.hop_guard();
        async_runtime::handle().spawn_blocking(move || {
            // The worker thread owns the blocking `load_remote_document` call.
            // Send may fail if the BrowserState was dropped while the load
            // was in flight (e.g. window close mid-fetch); ignore that case.
            let _ = sender.send(run_guarded_load(&NavigationRequest::get(target), guard));
            // Wake the main thread so the next frame fires immediately;
            // same rationale as `spawn_navigation`.
            if let Some(hook) = wake {
                hook.call();
            }
        });
        self.pending_navigation = Some(PendingNavigation {
            kind: PendingKind::Refresh,
            receiver,
        });
        self.set_status(
            "loading…",
            css::Color {
                r: 80,
                g: 100,
                b: 140,
                a: 255,
            },
        );
    }

    // Drain at most one completed result from the worker channel. Called at
    // the top of every `display_list()` so that a finished load lands before
    // the frame's input dispatch sees stale `current_url` / status. The
    // `Disconnected` arm protects against a worker thread that panicked
    // inside the spawn_blocking closure: the slot clears and the user gets
    // an error page rather than a permanently stuck "loading…" indicator.
    pub(super) fn poll_pending_navigation(&mut self) {
        let Some(pending) = self.pending_navigation.as_ref() else {
            return;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending_navigation = None;
                self.show_error_page("refresh failed", "worker disconnected");
                return;
            }
        };
        let kind = pending.kind;
        self.pending_navigation = None;
        // Redirect-hop decisions made on the worker join the session's
        // record; refusals also join its memory.
        for event in result.hop_events {
            if matches!(event.decision, PolicyDecision::Denied { .. }) {
                self.agent_policy.record_rule_refusal(&event.action);
            }
            self.policy_events.push(event);
        }
        let result = match result.outcome {
            LoadOutcome::Loaded(result) => *result,
            // A refused hop leaves the page exactly as it was: no error
            // page, no history entry.
            LoadOutcome::Refused => {
                self.set_status(
                    "blocked by policy",
                    css::Color {
                        r: 170,
                        g: 60,
                        b: 40,
                        a: 255,
                    },
                );
                return;
            }
        };
        match kind {
            PendingKind::Refresh => self.commit_refresh(result),
            PendingKind::Navigate { error_title } => self.commit_navigate(result, error_title),
        }
    }

    // Snapshot of the gate for a navigation worker (see `HopGuard`).
    fn hop_guard(&self) -> HopGuard {
        HopGuard {
            policy: self.agent_policy.clone(),
            from_agent: self.input_from_agent,
            origin_page: self.current_url.clone(),
            events: Vec::new(),
        }
    }

    fn commit_navigate(
        &mut self,
        result: Result<LoadedDocument, String>,
        error_title: &'static str,
    ) {
        // Both URL-bar Enter and link/form clicks land here. Successful
        // loads push to the back/forward stack via `commit_navigation`;
        // failed loads commit the canned error page through the same
        // funnel so the user can `back` out of a broken navigation —
        // matching the pre-async behaviour exactly, just delayed by
        // the worker round-trip.
        match result {
            Ok(loaded) => {
                let next_entry = HistoryEntry {
                    address_input: loaded.final_url.to_string(),
                    document_html: loaded.document_html,
                    stylesheet: loaded.stylesheet,
                    images: loaded.images,
                    font_data: loaded.font_data,
                    external_scripts: loaded.external_scripts,
                    current_url: Some(loaded.final_url),
                    status_text: "loaded".into(),
                    status_color: css::Color {
                        r: 40,
                        g: 120,
                        b: 40,
                        a: 255,
                    },
                };
                self.commit_navigation(next_entry);
                // Favicon is owned by `BrowserState` rather than
                // `HistoryEntry` for now — back/forward will not
                // restore the icon in 5.9c. Set after `commit_navigation`
                // so a back-forward push doesn't snapshot the new icon
                // onto the *previous* page's history record.
                self.favicon = loaded.favicon;
            }
            Err(error) => {
                eprintln!("{error}");
                let entry = self.error_entry(error_title, &error);
                self.commit_navigation(entry);
                self.favicon = None;
            }
        }
    }

    fn commit_refresh(&mut self, result: Result<LoadedDocument, String>) {
        match result {
            Ok(loaded) => {
                // Same install_document precondition as restore_entry:
                // current_url has to land first so the runtime's
                // `location` global picks up the reloaded URL instead
                // of the previous page's.
                self.current_url = Some(loaded.final_url);
                self.install_document(
                    loaded.document_html,
                    loaded.stylesheet,
                    loaded.external_scripts,
                );
                self.images = loaded.images;
                self.font_data = loaded.font_data;
                self.favicon = loaded.favicon;
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
                self.favicon = None;
                self.show_error_page("refresh failed", &error);
            }
        }
    }

    /// Returns true while a navigation is awaiting its worker thread.
    /// Tests use this to drive the per-frame poll loop without poking
    /// the private channel directly.
    pub fn has_pending_navigation(&self) -> bool {
        self.pending_navigation.is_some()
    }
}

// Worker-side load: follow redirects hop by hop, asking the guard first.
fn run_guarded_load(request: &NavigationRequest, mut guard: HopGuard) -> super::WorkerResult {
    let outcome =
        load_remote_document_guarded(request, &mut |hop| match net::Url::parse(&hop.url) {
            Ok(url) => guard.allow(&hop.method, url),
            // Unparseable hop: let the loader report it as a load error.
            Err(_) => true,
        });
    super::WorkerResult {
        outcome,
        hop_events: guard.events,
    }
}
