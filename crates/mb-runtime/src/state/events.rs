// Per-frame input dispatch and the geometry helpers the frame loop
// uses to translate raw `WindowInput` into BrowserState transitions:
// chrome clicks, address-bar focus, page scroll, and (when focus is in
// the document) per-key JS dispatch via dispatch_typed_keys.
//
// Hit-testing helpers (clicked_link / hovered_link / hovered_chrome_action)
// live here because they all answer "what does this mouse position
// land on?" — the same question apply_input reaches for first.

use crate::{
    chrome::{
        CHROME_HEIGHT, ChromeAction, address_bar_rect, back_button_rect, forward_button_rect,
        menu_button_rect, refresh_button_rect,
    },
    css,
    view::{LinkTarget, point_in_rect},
    input,
};

use super::{
    BrowserState, find_enclosing_form, node_id_for_dom_path, page_step, pop_char_from_input_value,
    push_char_to_input_value,
};

impl BrowserState {
    pub fn apply_input(
        &mut self,
        input: &input::WindowInput,
        viewport_width: usize,
        viewport_height: usize,
    ) {
        // Chrome buttons get first chance at a click so they do not fall through to page links.
        if input.focus_address_bar {
            self.address_bar_focused = true;
            self.address_bar_selected = true;
        }

        if input.left_mouse_pressed {
            if let Some(action) = self.hovered_chrome_action(input, viewport_width) {
                match action {
                    ChromeAction::Back => self.go_back(),
                    ChromeAction::Forward => self.go_forward(),
                    ChromeAction::Refresh => self.reload_current(),
                    // The dropdown itself is not implemented yet, but acknowledging the click
                    // proves the hit region works and prevents the click from falling through
                    // to the page underneath.
                    ChromeAction::Menu => self.set_status(
                        "menu (todo)",
                        css::Color {
                            r: 60,
                            g: 64,
                            b: 67,
                            a: 255,
                        },
                    ),
                }
                self.address_bar_focused = false;
                self.address_bar_selected = false;
                return;
            }

            if let Some((mouse_x, mouse_y)) = input.mouse_position {
                if point_in_rect(mouse_x, mouse_y, address_bar_rect(viewport_width as f32)) {
                    self.address_bar_focused = true;
                    self.address_bar_selected = true;
                } else {
                    self.address_bar_focused = false;
                    self.address_bar_selected = false;
                }
            }
        }

        // Keyboard text entry only edits the address bar when it is focused.
        if self.address_bar_focused {
            for ch in input.typed.chars() {
                if !ch.is_control() {
                    if self.address_bar_selected {
                        self.address_input.clear();
                        self.address_bar_selected = false;
                    }
                    self.address_input.push(ch);
                }
            }

            if input.backspace_pressed {
                if self.address_bar_selected {
                    self.address_input.clear();
                    self.address_bar_selected = false;
                } else {
                    self.address_input.pop();
                }
            }

            if input.enter_pressed {
                self.address_bar_selected = false;
                self.navigate();
                self.address_bar_focused = false;
            }
        } else if let Some(focused_path) = self.focused_dom_path.clone() {
            // Address bar didn't claim the keystrokes — fire JS keyboard
            // events on the focused page element, then (if not prevented
            // and the target is an <input>) apply the default text-edit
            // action. Anything else — focused link, focused div, no focus
            // at all — still gets the keydown/keyup events but skips the
            // default action; Enter is reserved for #7 (form submit) and
            // currently has no default action of its own.
            self.dispatch_typed_keys(&focused_path, input);
        }

        if input.back_pressed {
            self.go_back();
        }

        if input.forward_pressed {
            self.go_forward();
        }

        // Scrolling is applied after navigation shortcuts so the restored page starts at offset 0.
        self.scroll_offset -= input.scroll_y * 24.0;
        if input.move_up {
            self.scroll_offset -= 24.0;
        }
        if input.move_down {
            self.scroll_offset += 24.0;
        }
        if input.page_up_pressed {
            self.scroll_offset -= page_step(viewport_height);
        }
        if input.page_down_pressed {
            self.scroll_offset += page_step(viewport_height);
        }
    }

    // Per-key dispatch path. For each typed character (and Backspace /
    // Enter) we fire a `keydown`, run the default action only if no
    // handler called `preventDefault()` AND the target is an <input>,
    // and finish with a `keyup`. Events bubble per spec. The default
    // action for a typed character is "append the char to the input's
    // `value` attribute"; for Backspace it's "pop the last char". Enter
    // dispatches but has no default action yet — #7 (form submit) is
    // where that gets wired in. Non-input focus (a div picked up by a
    // future tab path, etc.) still receives the events; only the value
    // mutation is gated by the tag check.
    fn dispatch_typed_keys(&mut self, focused_path: &[usize], input: &input::WindowInput) {
        if input.typed.is_empty() && !input.backspace_pressed && !input.enter_pressed {
            return;
        }
        let Some(focused_id) =
            node_id_for_dom_path(&self.parsed_document.borrow(), focused_path)
        else {
            return;
        };

        for ch in input.typed.chars() {
            // `event.key` for printable characters is the character itself
            // ("a", " ", "2"); we surface even control chars (rare in the
            // typed buffer) but the default action drops them.
            let key = ch.to_string();
            let prevented = self.js.dispatch_keyboard_event(focused_id, "keydown", &key);
            if !prevented
                && !ch.is_control()
                && push_char_to_input_value(&self.parsed_document, focused_id, ch)
            {
                // Per spec, `input` fires after the value is updated and
                // before `keyup`. Bubbles, so handlers on ancestors see
                // it too. Only after a real mutation — control chars and
                // a tag mismatch leave the helper as a no-op and we skip
                // the event so observers don't see phantom changes.
                self.focused_input_dirty = true;
                self.js.dispatch_event(focused_id, "input");
            }
            self.js.dispatch_keyboard_event(focused_id, "keyup", &key);
        }

        if input.backspace_pressed {
            let prevented = self
                .js
                .dispatch_keyboard_event(focused_id, "keydown", "Backspace");
            if !prevented && pop_char_from_input_value(&self.parsed_document, focused_id) {
                // Backspace on an empty value reports no mutation, so we
                // skip the `input` event — real browsers do the same.
                self.focused_input_dirty = true;
                self.js.dispatch_event(focused_id, "input");
            }
            self.js.dispatch_keyboard_event(focused_id, "keyup", "Backspace");
        }

        if input.enter_pressed {
            // Enter splits two ways depending on the focused field:
            //   * <textarea>: insert a literal newline into the value
            //     buffer (the value-text paint splits on `\n` and the
            //     caret rides the trailing line).
            //   * Anything else: submit the enclosing <form>, if any.
            // preventDefault on keydown blocks both paths — same gate
            // as the typed-char path. Resolving the form is a separate
            // borrow from `try_submit_form` so the dispatch inside it
            // doesn't see an outstanding read borrow on the document.
            let prevented = self
                .js
                .dispatch_keyboard_event(focused_id, "keydown", "Enter");
            if !prevented {
                let is_textarea = matches!(
                    self.parsed_document
                        .borrow()
                        .element_data(focused_id)
                        .map(|elem| elem.tag_name.as_str()),
                    Some("textarea")
                );
                if is_textarea {
                    if push_char_to_input_value(&self.parsed_document, focused_id, '\n') {
                        self.focused_input_dirty = true;
                        self.js.dispatch_event(focused_id, "input");
                    }
                } else {
                    let form_id =
                        find_enclosing_form(&self.parsed_document.borrow(), focused_id);
                    if let Some(form_id) = form_id {
                        self.try_submit_form(form_id);
                    }
                }
            }
            self.js.dispatch_keyboard_event(focused_id, "keyup", "Enter");
        }
    }

    pub(super) fn show_caret(&self) -> bool {
        self.address_bar_focused
            && !self.address_bar_selected
            && (self.frame_index / 30).is_multiple_of(2)
    }

    pub(super) fn clamp_scroll(&mut self, viewport_height: usize, document_height: f32) {
        let visible_height = (viewport_height as f32 - CHROME_HEIGHT).max(0.0);
        let max_scroll = (document_height - visible_height).max(0.0);
        self.scroll_offset = self.scroll_offset.clamp(0.0, max_scroll);
    }

    pub(super) fn clicked_link<'a>(
        &self,
        input: &input::WindowInput,
        links: &'a [LinkTarget],
    ) -> Option<&'a LinkTarget> {
        if !input.left_mouse_pressed {
            return None;
        }

        self.hovered_link(input, links)
    }

    pub(super) fn hovered_link<'a>(
        &self,
        input: &input::WindowInput,
        links: &'a [LinkTarget],
    ) -> Option<&'a LinkTarget> {
        let (mouse_x, mouse_y) = input.mouse_position?;
        if mouse_y < CHROME_HEIGHT {
            return None;
        }

        let document_y = mouse_y - CHROME_HEIGHT + self.scroll_offset;
        links
            .iter()
            .rev()
            .find(|link| point_in_rect(mouse_x, document_y, link.rect))
    }

    pub fn hovered_chrome_action(
        &self,
        input: &input::WindowInput,
        viewport_width: usize,
    ) -> Option<ChromeAction> {
        let (mouse_x, mouse_y) = input.mouse_position?;

        if point_in_rect(mouse_x, mouse_y, back_button_rect()) && self.can_go_back() {
            return Some(ChromeAction::Back);
        }
        if point_in_rect(mouse_x, mouse_y, forward_button_rect()) && self.can_go_forward() {
            return Some(ChromeAction::Forward);
        }
        // Refresh stays hover-able on the NTP too; the click handler decides whether
        // there is anything to reload, mirroring how Chrome shows an enabled button
        // but with a no-op effect when there is no current document.
        if point_in_rect(mouse_x, mouse_y, refresh_button_rect()) {
            return Some(ChromeAction::Refresh);
        }
        // The menu button always reports as hover-able even though its action is still a stub,
        // so the user gets visual feedback that the hit region is wired up.
        if point_in_rect(mouse_x, mouse_y, menu_button_rect(viewport_width as f32)) {
            return Some(ChromeAction::Menu);
        }

        None
    }
}
