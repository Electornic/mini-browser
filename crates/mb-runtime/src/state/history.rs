// History stack management for BrowserState. Each navigation pushes a
// `HistoryEntry` (a full snapshot of the visible document + its
// resources) onto the back stack so back/forward can restore instantly
// without refetching. The forward stack is cleared whenever the user
// commits a fresh navigation — the linear-history model says diverging
// from a back state drops every future state, matching real browsers.

use std::collections::HashMap;

use crate::{css, navigation::error_document};

use super::{BrowserState, HistoryEntry};

impl BrowserState {
    pub fn snapshot(&self) -> HistoryEntry {
        // History snapshots include the decoded image cache and pre-fetched
        // script bodies so back/forward feels immediate — no re-fetching.
        HistoryEntry {
            address_input: self.address_input.clone(),
            document_html: self.document_html.clone(),
            stylesheet: self.stylesheet.clone(),
            images: self.images.clone(),
            font_data: self.font_data.clone(),
            external_scripts: self.external_scripts.clone(),
            current_url: self.current_url.clone(),
            status_text: self.status_text.clone(),
            status_color: self.status_color,
        }
    }

    pub(super) fn restore_entry(&mut self, entry: HistoryEntry) {
        self.address_input = entry.address_input;
        // Set the URL before install_document so the new JsRuntime's
        // `location` accessors observe the restored page's URL on the
        // very first script execution rather than the stale previous
        // page (or empty buffer on the bootstrap path).
        self.current_url = entry.current_url;
        self.install_document(entry.document_html, entry.stylesheet, entry.external_scripts);
        self.images = entry.images;
        self.font_data = entry.font_data;
        self.status_text = entry.status_text;
        self.status_color = entry.status_color;
        self.scroll_offset = 0.0;
        self.address_bar_selected = false;
    }

    pub fn commit_navigation(&mut self, entry: HistoryEntry) {
        self.back_stack.push(self.snapshot());
        self.forward_stack.clear();
        self.restore_entry(entry);
    }

    pub fn go_back(&mut self) {
        if let Some(previous) = self.back_stack.pop() {
            self.forward_stack.push(self.snapshot());
            self.restore_entry(previous);
        }
    }

    pub fn go_forward(&mut self) {
        if let Some(next) = self.forward_stack.pop() {
            self.back_stack.push(self.snapshot());
            self.restore_entry(next);
        }
    }

    pub(super) fn can_go_back(&self) -> bool {
        !self.back_stack.is_empty()
    }

    pub(super) fn can_go_forward(&self) -> bool {
        !self.forward_stack.is_empty()
    }

    pub(super) fn error_entry(&self, title: &str, message: &str) -> HistoryEntry {
        let (document_html, stylesheet) = error_document(title, message, self.address_input.trim());
        HistoryEntry {
            address_input: self.address_input.clone(),
            document_html,
            stylesheet,
            images: HashMap::new(),
            font_data: Vec::new(),
            external_scripts: HashMap::new(),
            current_url: None,
            status_text: title.into(),
            status_color: css::Color {
                r: 180,
                g: 60,
                b: 60,
                a: 255,
            },
        }
    }
}
