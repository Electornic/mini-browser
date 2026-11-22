// Design tokens from the "Agent Browser" design draft (board 1 "Identity").
// Colors answer one question — who is acting, and does a person need to?
//
//   agent  (blue)   id badges, agent actions, the attached-agent pill
//   person (orange) needs you: target outline, approval, handoff, paused.
//                   A fill or outline only: text on it is always INK.
//   done   (green)  approved, matched, succeeded
//   stop   (red)    refused, differs, failed
//
// Neutrals carry everything else. The chrome (shared with `mb-shell`) and
// the observation window's overlay (`mb-mcp`) both paint from here, so a
// color decision lives in exactly one place.
//
// Fonts: the design names Inter (interface) and JetBrains Mono (ids, URLs,
// origins). The font system only installs the macOS fallbacks
// (AppleSDGothicNeo / Menlo), so `MONO` routes through the generic
// `monospace` family and everything else is the default sans.

use crate::css::Color;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

// Roles.
pub const AGENT: Color = rgb(0x2D, 0x5B, 0xE3);
/// Text on an agent tint (pill label, links).
pub const AGENT_INK: Color = rgb(0x1F, 0x44, 0xB8);
pub const AGENT_TINT: Color = rgb(0xEE, 0xF2, 0xFD);
pub const AGENT_BORDER: Color = rgb(0xC9, 0xD5, 0xFA);

pub const PERSON: Color = rgb(0xD9, 0x67, 0x0B);
pub const PERSON_TINT: Color = rgb(0xFD, 0xF1, 0xE7);
pub const PERSON_BORDER: Color = rgb(0xF0, 0xC7, 0xA6);
/// Icon circles and labels that point at something the person must do.
pub const PERSON_SOFT: Color = rgb(0xFB, 0xE7, 0xD6);

pub const DONE: Color = rgb(0x1E, 0x7A, 0x46);
pub const DONE_TINT: Color = rgb(0xE2, 0xF2, 0xE8);
pub const DONE_INK: Color = rgb(0x14, 0x57, 0x34);

pub const STOP: Color = rgb(0xC2, 0x36, 0x2B);
pub const STOP_TINT: Color = rgb(0xFB, 0xE3, 0xE0);
pub const STOP_INK: Color = rgb(0x8E, 0x22, 0x19);
/// A red label on white (Deny).
pub const STOP_TEXT: Color = rgb(0xA7, 0x2D, 0x23);
pub const STOP_BORDER: Color = rgb(0xE2, 0xB7, 0xB2);

// Neutrals.
pub const INK: Color = rgb(0x15, 0x17, 0x1C);
pub const MUTED: Color = rgb(0x4A, 0x50, 0x60);
/// Placeholder, pending steps, a ring with nobody in it.
pub const FAINT: Color = rgb(0x8A, 0x90, 0x9C);
/// Disabled icons (Back with no history).
pub const DISABLED: Color = rgb(0xA3, 0xA8, 0xB3);
pub const DISABLED_BG: Color = rgb(0xED, 0xEE, 0xF1);
pub const LINE: Color = rgb(0xE2, 0xE4, 0xE8);
/// Window frame, menu border, pending progress segments.
pub const FRAME: Color = rgb(0xD5, 0xD8, 0xDE);
pub const PANEL: Color = rgb(0xF4, 0xF5, 0xF7);
/// The tab strip behind the active tab.
pub const STRIP: Color = rgb(0xE9, 0xEB, 0xEF);
pub const PAGE: Color = rgb(0xFF, 0xFF, 0xFF);

/// `TextCommand::font_family` for what the agent sees: ids, URLs, origins.
pub const MONO: &str = "monospace";

/// Titles and buttons (the font system picks a bold face at ≥ 600).
pub const WEIGHT_STRONG: u16 = 700;
pub const WEIGHT_REGULAR: u16 = 400;

/// Shaped width of a chrome / overlay label in the face it will paint with.
pub fn text_width(text: &str, size: f32, mono: bool, weight: u16) -> f32 {
    crate::render::measure_text_wrap_with_family(text, size, None, mono.then_some(MONO), weight).0
}

/// `TextCommand::y` that centres one line of `size` text in a row of
/// `height` starting at `top` (the rasteriser's line box is `1.2 × size`).
pub fn centered_text_y(top: f32, height: f32, size: f32) -> f32 {
    top + ((height - size * 1.2) / 2.0).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_text_sits_in_the_middle_of_its_row() {
        // 36 px row, 14 px text → 16.8 px line box, 9.6 px above it.
        assert!((centered_text_y(52.0, 36.0, 14.0) - 61.6).abs() < 1e-4);
        // Text taller than the row starts at the row's top.
        assert_eq!(centered_text_y(10.0, 8.0, 14.0), 10.0);
    }
}
