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
