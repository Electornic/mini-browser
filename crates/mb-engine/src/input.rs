// `WindowInput` is the per-frame input snapshot the engine and runtime read.
// The actual winit driver lives in the root crate's `window` module (and will
// move to `mb-shell` in 4.9e); this module just owns the shape so engine and
// runtime can speak it without depending on winit.

#[derive(Debug, Clone, Default)]
pub struct WindowInput {
    pub typed: String,
    pub enter_pressed: bool,
    pub backspace_pressed: bool,
    pub focus_address_bar: bool,
    pub back_pressed: bool,
    pub forward_pressed: bool,
    pub scroll_y: f32,
    pub move_up: bool,
    pub move_down: bool,
    pub page_up_pressed: bool,
    pub page_down_pressed: bool,
    pub mouse_position: Option<(f32, f32)>,
    // `left_mouse_pressed` is the *edge* (true only on the frame the button
    // transitions from up to down). `left_mouse_held` is the *level*, which is
    // what `:active` needs.
    pub left_mouse_pressed: bool,
    pub left_mouse_held: bool,
    /// Tab: move page focus to the next focusable element.
    pub tab_pressed: bool,
    /// Escape: drop focus (address bar or page element).
    pub escape_pressed: bool,
    /// Provenance: this frame's input was synthesised by an agent, not
    /// typed or clicked by a person. The runtime's policy gate judges only
    /// agent-originated actions (docs/concept.md "정책 게이트").
    pub from_agent: bool,
}

/// One frame of single-line text editing, the way the address bar takes
/// typed text: printable characters append (replacing the text when it is
/// all selected), Backspace deletes the last character (or the selection).
/// Control characters winit lets through are dropped. Returns whether
/// Enter was pressed — what it means is the caller's.
pub fn edit_line(text: &mut String, selected: &mut bool, input: &WindowInput) -> bool {
    for ch in input.typed.chars() {
        if !ch.is_control() {
            if *selected {
                text.clear();
                *selected = false;
            }
            text.push(ch);
        }
    }
    if input.backspace_pressed {
        if *selected {
            text.clear();
            *selected = false;
        } else {
            text.pop();
        }
    }
    input.enter_pressed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_line_appends_replaces_a_selection_and_backspaces() {
        let mut text = "old".to_string();
        let mut selected = true;
        let typed = WindowInput {
            typed: "a\u{7}b".into(),
            ..WindowInput::default()
        };
        assert!(!edit_line(&mut text, &mut selected, &typed));
        assert_eq!((text.as_str(), selected), ("ab", false));
        let back = WindowInput {
            backspace_pressed: true,
            enter_pressed: true,
            ..WindowInput::default()
        };
        assert!(edit_line(&mut text, &mut selected, &back));
        assert_eq!(text, "a");
    }
}
