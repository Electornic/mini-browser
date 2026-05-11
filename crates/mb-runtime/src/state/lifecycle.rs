// Navigation lifecycle for BrowserState. Owns the per-document loading
// pipeline: URL/form/link/refresh dispatch, the async worker channel
// that drives the blocking fetch, and the commit funnel that lands a
// loaded (or failed) document on the visible page. History pushes live
// in `state/history.rs`; this module hands off to it via
// `commit_navigation` / `restore_entry`.

use std::sync::mpsc;

use crate::{
    async_runtime, css, net,
    navigation::{LoadedDocument, load_remote_document},
};
use crate::dom::NodeId;

use super::{
    BrowserState, HistoryEntry, PendingKind, PendingNavigation, build_form_submit_url,
    collect_form_data,
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

        self.spawn_navigation(target, "load failed");
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

        // Sync UI feedback (URL bar shows the click target, focus moves
        // off the bar) lands the same frame as the click. The actual
        // load runs on the worker thread and commits later via
        // `poll_pending_navigation`.
        self.address_input = resolved.to_string();
        self.address_bar_selected = false;
        self.address_bar_focused = false;
        self.spawn_navigation(resolved.to_string(), "link failed");
    }

    fn spawn_navigation(&mut self, target: String, error_title: &'static str) {
        // Caller already established `pending_navigation.is_none()` and
        // resolved the target string. The worker owns the blocking
        // `load_remote_document` call; the receiver lands on
        // `pending_navigation` and `poll_pending_navigation` drains it
        // on a later frame.
        let (sender, receiver) = mpsc::channel();
        async_runtime::handle().spawn_blocking(move || {
            let _ = sender.send(load_remote_document(&target));
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
        async_runtime::handle().spawn_blocking(move || {
            // The worker thread owns the blocking `load_remote_document` call.
            // Send may fail if the BrowserState was dropped while the load
            // was in flight (e.g. window close mid-fetch); ignore that case.
            let _ = sender.send(load_remote_document(&target));
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
        match kind {
            PendingKind::Refresh => self.commit_refresh(result),
            PendingKind::Navigate { error_title } => self.commit_navigate(result, error_title),
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
                self.install_document(loaded.document_html, loaded.stylesheet, loaded.external_scripts);
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
