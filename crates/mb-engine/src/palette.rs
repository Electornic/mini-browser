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

//
// Two themes (board 10 "Dark"): the same roles, lifted for a dark ground.
// Only the browser's own surfaces follow the theme — the chrome and the
// observation window's overlay. A page keeps its own colors (the canvas
// under it is white whatever the theme). The theme is per thread: the
// window thread sets it each frame (`set_theme`), and an agent screenshot
// paints in `Light` (`with_theme`) so its pixels never depend on the
// person's appearance setting.

use std::cell::Cell;

use crate::css::Color;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Light,
    Dark,
}

impl Theme {
    /// The OS appearance as the window system reports it (`None` = it
    /// doesn't say): light unless it says dark.
    pub fn from_system(dark: Option<bool>) -> Theme {
        if dark == Some(true) {
            Theme::Dark
        } else {
            Theme::Light
        }
    }

    pub fn palette(self) -> &'static Palette {
        match self {
            Theme::Light => &LIGHT,
            Theme::Dark => &DARK,
        }
    }
}

/// Every color token, for one theme. Read through the accessor functions
/// below (`palette::ink()`), which pick the current thread's theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    // Roles.
    pub agent: Color,
    /// Text on an agent tint (pill label, links).
    pub agent_ink: Color,
    pub agent_tint: Color,
    pub agent_border: Color,
    pub person: Color,
    pub person_tint: Color,
    pub person_border: Color,
    /// Icon circles and labels that point at something the person must do.
    pub person_soft: Color,
    pub done: Color,
    pub done_tint: Color,
    pub done_ink: Color,
    pub stop: Color,
    pub stop_tint: Color,
    pub stop_ink: Color,
    /// A red label on the surface (Deny).
    pub stop_text: Color,
    pub stop_border: Color,
    // Neutrals.
    pub ink: Color,
    pub muted: Color,
    /// Placeholder, pending steps, a ring with nobody in it.
    pub faint: Color,
    /// Disabled icons (Back with no history).
    pub disabled: Color,
    pub disabled_bg: Color,
    pub line: Color,
    /// Window frame, menu border, pending progress segments.
    pub frame: Color,
    pub panel: Color,
    /// The tab strip behind the active tab.
    pub strip: Color,
    /// The browser's surface: toolbar, cards, menus, dialogs. Not the
    /// page's canvas (always white — the page's own).
    pub page: Color,
    /// Text and marks on a filled role color (id badges, Approve, Add):
    /// white in both themes.
    pub on_fill: Color,
}

/// Board 1 "Identity".
pub const LIGHT: Palette = Palette {
    agent: rgb(0x2D, 0x5B, 0xE3),
    agent_ink: rgb(0x1F, 0x44, 0xB8),
    agent_tint: rgb(0xEE, 0xF2, 0xFD),
    agent_border: rgb(0xC9, 0xD5, 0xFA),
    person: rgb(0xD9, 0x67, 0x0B),
    person_tint: rgb(0xFD, 0xF1, 0xE7),
    person_border: rgb(0xF0, 0xC7, 0xA6),
    person_soft: rgb(0xFB, 0xE7, 0xD6),
    done: rgb(0x1E, 0x7A, 0x46),
    done_tint: rgb(0xE2, 0xF2, 0xE8),
    done_ink: rgb(0x14, 0x57, 0x34),
    stop: rgb(0xC2, 0x36, 0x2B),
    stop_tint: rgb(0xFB, 0xE3, 0xE0),
    stop_ink: rgb(0x8E, 0x22, 0x19),
    stop_text: rgb(0xA7, 0x2D, 0x23),
    stop_border: rgb(0xE2, 0xB7, 0xB2),
    ink: rgb(0x15, 0x17, 0x1C),
    muted: rgb(0x4A, 0x50, 0x60),
    faint: rgb(0x8A, 0x90, 0x9C),
    disabled: rgb(0xA3, 0xA8, 0xB3),
    disabled_bg: rgb(0xED, 0xEE, 0xF1),
    line: rgb(0xE2, 0xE4, 0xE8),
    frame: rgb(0xD5, 0xD8, 0xDE),
    panel: rgb(0xF4, 0xF5, 0xF7),
    strip: rgb(0xE9, 0xEB, 0xEF),
    page: rgb(0xFF, 0xFF, 0xFF),
    on_fill: rgb(0xFF, 0xFF, 0xFF),
};

/// Board 10 "Dark — same roles, lifted for a dark ground": ground
/// #111317, panel #1A1D23, line #2A2E36, ink #ECEEF2, muted #A3A9B5;
/// agent #3D68F0 (pill #1B2445 / #34488A / #B4C6FA), person #F0913F,
/// done #2E9A5C, stop text #F2A39B on #6B3A36. Tokens the board doesn't
/// show are derived the same way: tints are the role mixed far into the
/// ground, inks are the role lifted toward white.
pub const DARK: Palette = Palette {
    agent: rgb(0x3D, 0x68, 0xF0),
    agent_ink: rgb(0xB4, 0xC6, 0xFA),
    agent_tint: rgb(0x1B, 0x24, 0x45),
    agent_border: rgb(0x34, 0x48, 0x8A),
    person: rgb(0xF0, 0x91, 0x3F),
    person_tint: rgb(0x36, 0x24, 0x15),
    person_border: rgb(0x7A, 0x4A, 0x22),
    person_soft: rgb(0x4A, 0x2E, 0x17),
    done: rgb(0x2E, 0x9A, 0x5C),
    done_tint: rgb(0x16, 0x30, 0x1F),
    done_ink: rgb(0x8F, 0xD9, 0xAE),
    stop: rgb(0xE5, 0x56, 0x4A),
    stop_tint: rgb(0x3A, 0x1C, 0x1A),
    stop_ink: rgb(0xF2, 0xA3, 0x9B),
    stop_text: rgb(0xF2, 0xA3, 0x9B),
    stop_border: rgb(0x6B, 0x3A, 0x36),
    ink: rgb(0xEC, 0xEE, 0xF2),
    muted: rgb(0xA3, 0xA9, 0xB5),
    faint: rgb(0x7A, 0x80, 0x8C),
    disabled: rgb(0x55, 0x5B, 0x66),
    disabled_bg: rgb(0x1F, 0x23, 0x2A),
    line: rgb(0x2A, 0x2E, 0x36),
    frame: rgb(0x3A, 0x3F, 0x49),
    panel: rgb(0x1A, 0x1D, 0x23),
    strip: rgb(0x0B, 0x0C, 0x0F),
    page: rgb(0x11, 0x13, 0x17),
    on_fill: rgb(0xFF, 0xFF, 0xFF),
};

thread_local! {
    static THEME: Cell<Theme> = const { Cell::new(Theme::Light) };
}

/// This thread's theme (Light until someone sets it).
pub fn theme() -> Theme {
    THEME.with(Cell::get)
}

/// Paint this thread's chrome and overlay in `theme` from now on.
pub fn set_theme(theme: Theme) {
    THEME.with(|t| t.set(theme));
}

/// Run `f` with `theme`, then put the previous one back (an agent
/// screenshot paints in Light whatever the window shows).
pub fn with_theme<T>(theme: Theme, f: impl FnOnce() -> T) -> T {
    let previous = self::theme();
    set_theme(theme);
    let result = f();
    set_theme(previous);
    result
}

/// The current thread's palette.
pub fn current() -> &'static Palette {
    theme().palette()
}

macro_rules! tokens {
    ($($name:ident),* $(,)?) => {
        $(
            #[inline]
            pub fn $name() -> Color {
                current().$name
            }
        )*
    };
}

tokens!(
    agent,
    agent_ink,
    agent_tint,
    agent_border,
    person,
    person_tint,
    person_border,
    person_soft,
    done,
    done_tint,
    done_ink,
    stop,
    stop_tint,
    stop_ink,
    stop_text,
    stop_border,
    ink,
    muted,
    faint,
    disabled,
    disabled_bg,
    line,
    frame,
    panel,
    strip,
    page,
    on_fill,
);

/// A status-line color by role, resolved when painted — so a status set
/// before the theme changed still reads in the new one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    #[default]
    Ink,
    Muted,
    Faint,
    Stop,
}

impl Tone {
    pub fn color(self) -> Color {
        match self {
            Tone::Ink => ink(),
            Tone::Muted => muted(),
            Tone::Faint => faint(),
            Tone::Stop => stop(),
        }
    }
}

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

    // WCAG relative luminance / contrast ratio.
    fn contrast(a: Color, b: Color) -> f32 {
        let lum = |c: Color| {
            let ch = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b)
        };
        let (x, y) = (lum(a) + 0.05, lum(b) + 0.05);
        x.max(y) / x.min(y)
    }

    #[test]
    fn both_themes_keep_text_readable_on_their_surfaces() {
        for palette in [&LIGHT, &DARK] {
            for ground in [palette.page, palette.panel] {
                assert!(contrast(palette.ink, ground) >= 7.0, "{palette:?}");
                assert!(contrast(palette.muted, ground) >= 4.5, "{palette:?}");
                assert!(contrast(palette.stop_text, ground) >= 4.5, "{palette:?}");
            }
            assert!(contrast(palette.agent_ink, palette.agent_tint) >= 4.5);
            assert!(contrast(palette.on_fill, palette.agent) >= 4.0);
            assert!(contrast(palette.on_fill, palette.done) >= 3.0);
            // The four roles stay four colors.
            let roles = [palette.agent, palette.person, palette.done, palette.stop];
            for (i, a) in roles.iter().enumerate() {
                for b in &roles[i + 1..] {
                    assert_ne!(a, b);
                }
            }
        }
        // Dark is dark: the surfaces are below the ink, not above it.
        const { assert!(DARK.page.r < 0x30 && DARK.panel.r < 0x30) };
        assert_eq!(DARK.on_fill, LIGHT.on_fill);
    }

    #[test]
    fn the_theme_is_per_thread_and_with_theme_restores_it() {
        assert_eq!(theme(), Theme::Light);
        assert_eq!(ink(), LIGHT.ink);
        set_theme(Theme::Dark);
        assert_eq!(page(), DARK.page);
        let inside = with_theme(Theme::Light, || (theme(), page()));
        assert_eq!(inside, (Theme::Light, LIGHT.page));
        assert_eq!(theme(), Theme::Dark);
        assert_eq!(Tone::Muted.color(), DARK.muted);
        // Another thread starts light.
        assert_eq!(std::thread::spawn(theme).join().unwrap(), Theme::Light);
        set_theme(Theme::Light);
        assert_eq!(Theme::from_system(None), Theme::Light);
        assert_eq!(Theme::from_system(Some(true)), Theme::Dark);
    }
}
