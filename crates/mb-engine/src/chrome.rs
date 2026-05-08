// Pure painting helpers and hit-region rects for the browser chrome (tab strip,
// toolbar, address bar, nav buttons). Kept module-local so `BrowserState` only
// needs `ChromeState` + `chrome_commands` + the per-button rects, with no
// awareness of pixel-level layout.

use crate::{css, layout, render, resource::LoadedImage};

// These constants define the browser chrome at the top of the window.
// Everything below `CHROME_HEIGHT` is treated as page content. The chrome stacks
// a tab strip on top of a toolbar; toolbar constants are expressed in screen
// coordinates so they already include `TAB_STRIP_HEIGHT` as their top offset.
pub const TAB_STRIP_HEIGHT: f32 = 42.0;
pub const TOOLBAR_HEIGHT: f32 = 60.0;
pub const CHROME_HEIGHT: f32 = TAB_STRIP_HEIGHT + TOOLBAR_HEIGHT;
pub const NAV_BUTTON_WIDTH: f32 = 32.0;
pub const NAV_BUTTON_HEIGHT: f32 = 32.0;
pub const NAV_BUTTON_Y: f32 = TAB_STRIP_HEIGHT + 12.0;
pub const BACK_BUTTON_X: f32 = 12.0;
pub const FORWARD_BUTTON_X: f32 = BACK_BUTTON_X + NAV_BUTTON_WIDTH + 4.0;
pub const REFRESH_BUTTON_X: f32 = FORWARD_BUTTON_X + NAV_BUTTON_WIDTH + 4.0;
pub const ADDRESS_BOX_X: f32 = REFRESH_BUTTON_X + NAV_BUTTON_WIDTH + 16.0;
pub const ADDRESS_BOX_Y: f32 = TAB_STRIP_HEIGHT + 12.0;
pub const ADDRESS_BOX_HEIGHT: f32 = 36.0;
pub const ADDRESS_TEXT_X: f32 = ADDRESS_BOX_X + 16.0;
pub const ADDRESS_TEXT_Y: f32 = ADDRESS_BOX_Y + 11.0;
pub const ADDRESS_FONT_SIZE: f32 = 14.0;
// When the current URL is https, the address bar paints a small padlock
// at its left edge and shifts the text right so glyphs do not overlap
// the icon. The HTTPS_ADDRESS_TEXT_X offset is calibrated to leave ~5px
// breathing room past the lock's rightmost stamp (`lock_icon_commands`).
pub const HTTPS_ADDRESS_TEXT_X: f32 = ADDRESS_BOX_X + 30.0;
pub const STATUS_TEXT_Y: f32 = ADDRESS_BOX_Y + ADDRESS_BOX_HEIGHT + 4.0;
pub const STATUS_FONT_SIZE: f32 = 10.0;
pub const MENU_BUTTON_WIDTH: f32 = 32.0;
pub const MENU_BUTTON_RIGHT_PAD: f32 = 12.0;
pub const MENU_BUTTON_GAP: f32 = 8.0;
pub const TAB_X: f32 = 8.0;
pub const TAB_Y: f32 = 6.0;
pub const TAB_WIDTH: f32 = 272.0;
pub const TAB_HEIGHT: f32 = TAB_STRIP_HEIGHT - TAB_Y;
pub const TAB_RADIUS: f32 = 10.0;
pub const TAB_TITLE_X: f32 = TAB_X + 16.0;
pub const TAB_TITLE_Y: f32 = TAB_Y + 11.0;
pub const TAB_TITLE_FONT_SIZE: f32 = 13.0;
// Favicon sits to the left of the tab title at 16×16. The vertical
// offset puts it on the title's text baseline (TAB_TITLE_Y - 4 lines
// up the icon's top with the cap line) so the two read as a single
// row instead of fighting for vertical center.
pub const TAB_FAVICON_X: f32 = TAB_X + 14.0;
pub const TAB_FAVICON_Y: f32 = TAB_Y + 7.0;
pub const TAB_FAVICON_SIZE: f32 = 16.0;
// When a favicon is present, the title shifts right by the icon
// width plus a 6px gap. Tabs without a favicon keep `TAB_TITLE_X`,
// so plain pages do not pay icon-sized left pad they cannot use.
pub const TAB_TITLE_X_WITH_FAVICON: f32 = TAB_FAVICON_X + TAB_FAVICON_SIZE + 6.0;

#[derive(Debug, Clone, Copy)]
pub struct ChromeState<'a> {
    pub viewport_width: usize,
    pub address_input: &'a str,
    pub status_text: &'a str,
    pub status_color: css::Color,
    pub address_bar_focused: bool,
    pub address_bar_selected: bool,
    pub show_caret: bool,
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub hovered_action: Option<ChromeAction>,
    pub tab_title: &'a str,
    /// True when the current page was loaded over `https`. Drives the
    /// padlock icon at the left of the address bar; `false` (http /
    /// about:blank / file://) leaves that area empty and falls back to
    /// the unindented text origin so non-secure pages do not look like
    /// they paid a half-icon worth of pad on the left.
    pub is_https: bool,
    /// Favicon to render to the left of the tab title, if the document
    /// exposed `<link rel="icon">` and the fetch + decode succeeded.
    /// `None` keeps the title at its unshifted origin so plain pages
    /// do not paint a 16-px gap of nothing.
    pub tab_favicon: Option<&'a LoadedImage>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeAction {
    Back,
    Forward,
    Refresh,
    Menu,
}

pub fn chrome_commands(chrome: ChromeState<'_>) -> Vec<render::DisplayCommand> {
    // Chrome rendering is intentionally separate from page rendering so scrolling never moves it.
    let width = chrome.viewport_width as f32;
    let input_empty = chrome.address_input.is_empty();
    // The placeholder only renders when the bar is empty AND not in select-all-on-focus mode,
    // so a user who clicks the bar to type sees an empty field instead of greyed text under
    // their cursor.
    let address_display = if input_empty {
        if chrome.address_bar_selected {
            String::new()
        } else {
            "http://example.com".to_string()
        }
    } else {
        chrome.address_input.to_string()
    };
    let address_color = if input_empty {
        css::Color {
            r: 154,
            g: 160,
            b: 166,
            a: 255,
        }
    } else {
        css::Color::BLACK
    };
    let address_box = address_bar_rect(width);
    let border_color = if chrome.address_bar_focused {
        css::Color {
            r: 54,
            g: 116,
            b: 217,
            a: 255,
        }
    } else {
        css::Color {
            r: 218,
            g: 220,
            b: 224,
            a: 255,
        }
    };
    let pill_radius = address_box.height / 2.0;
    let pill_outer = render::CornerRadii::uniform(pill_radius);
    let pill_inner = render::CornerRadii::uniform((pill_radius - 1.0).max(0.0));
    // The padlock and the text live in the same pill, so the text origin
    // and the caret/selection highlight have to honour the same indent —
    // otherwise the caret would jump under the icon when the lock paints.
    let address_text_x = if chrome.is_https {
        HTTPS_ADDRESS_TEXT_X
    } else {
        ADDRESS_TEXT_X
    };

    let toolbar_bg = css::Color {
        r: 236,
        g: 239,
        b: 244,
        a: 255,
    };
    let tab_strip_bg = css::Color {
        r: 222,
        g: 225,
        b: 230,
        a: 255,
    };
    let mut commands = vec![
        // Tab strip sits behind everything else and is the darker band of chrome.
        render::DisplayCommand::SolidRect(
            tab_strip_bg,
            layout::Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: TAB_STRIP_HEIGHT,
            },
        ),
        // Toolbar fills the rest of the chrome with the lighter foreground color.
        render::DisplayCommand::SolidRect(
            toolbar_bg,
            layout::Rect {
                x: 0.0,
                y: TAB_STRIP_HEIGHT,
                width,
                height: TOOLBAR_HEIGHT,
            },
        ),
        // Active tab paints in the same color as the toolbar so the two surfaces merge
        // seamlessly along the bottom edge while the rounded top corners show on the
        // darker strip.
        render::DisplayCommand::RoundedRect(
            toolbar_bg,
            layout::Rect {
                x: TAB_X,
                y: TAB_Y,
                width: TAB_WIDTH,
                height: TAB_HEIGHT,
            },
            render::CornerRadii {
                tl: TAB_RADIUS,
                tr: TAB_RADIUS,
                br: 0.0,
                bl: 0.0,
            },
        ),
        render::DisplayCommand::Text(render::TextCommand {
            text: chrome.tab_title.to_string(),
            x: if chrome.tab_favicon.is_some() {
                TAB_TITLE_X_WITH_FAVICON
            } else {
                TAB_TITLE_X
            },
            y: TAB_TITLE_Y,
            color: css::Color {
                r: 60,
                g: 64,
                b: 67,
                a: 255,
            },
            font_size: TAB_TITLE_FONT_SIZE,
            wrap_width: None,
            font_family: None,
            font_weight: 400,
        }),
        render::DisplayCommand::RoundedRect(border_color, address_box, pill_outer),
        render::DisplayCommand::RoundedRect(
            css::Color {
                r: 255,
                g: 255,
                b: 255,
                a: 255,
            },
            layout::Rect {
                x: address_box.x + 1.0,
                y: address_box.y + 1.0,
                width: (address_box.width - 2.0).max(0.0),
                height: (address_box.height - 2.0).max(0.0),
            },
            pill_inner,
        ),
        render::DisplayCommand::Text(render::TextCommand {
            text: address_display.clone(),
            x: address_text_x,
            y: ADDRESS_TEXT_Y,
            color: address_color,
            font_size: ADDRESS_FONT_SIZE,
            wrap_width: None,
            font_family: None,
            font_weight: 400,
        }),
        render::DisplayCommand::Text(render::TextCommand {
            text: chrome.status_text.to_string(),
            x: 16.0,
            y: STATUS_TEXT_Y,
            color: chrome.status_color,
            font_size: STATUS_FONT_SIZE,
            wrap_width: None,
            font_family: None,
            font_weight: 400,
        }),
    ];
    commands.extend(nav_button_commands(
        back_button_rect(),
        true,
        chrome.can_go_back,
        chrome.hovered_action == Some(ChromeAction::Back),
    ));
    commands.extend(nav_button_commands(
        forward_button_rect(),
        false,
        chrome.can_go_forward,
        chrome.hovered_action == Some(ChromeAction::Forward),
    ));
    commands.extend(refresh_button_commands(
        refresh_button_rect(),
        chrome.hovered_action == Some(ChromeAction::Refresh),
    ));
    commands.extend(menu_button_commands(
        menu_button_rect(width),
        chrome.hovered_action == Some(ChromeAction::Menu),
    ));

    if chrome.address_bar_selected {
        let measured = render::measure_text_width(&address_display, ADDRESS_FONT_SIZE);
        commands.push(render::DisplayCommand::SolidRect(
            css::Color {
                r: 214,
                g: 229,
                b: 255,
                a: 255,
            },
            layout::Rect {
                x: address_text_x - 2.0,
                y: ADDRESS_TEXT_Y - 2.0,
                width: (measured + 4.0).min((address_box.width - 8.0).max(0.0)),
                height: ADDRESS_FONT_SIZE + 4.0,
            },
        ));
        commands.push(render::DisplayCommand::Text(render::TextCommand {
            text: address_display,
            x: address_text_x,
            y: ADDRESS_TEXT_Y,
            color: css::Color::BLACK,
            font_size: ADDRESS_FONT_SIZE,
            wrap_width: None,
            font_family: None,
            font_weight: 400,
        }));
    } else if chrome.show_caret {
        // Caret position is measured from the *actual input* (empty when only the
        // placeholder is showing), and uses cosmic-text's shaped advance so it
        // lines up with where `draw_text` actually ends — a fixed average glyph
        // width would always drift on proportional fonts.
        let caret_offset = if input_empty {
            0.0
        } else {
            render::measure_text_width(&address_display, ADDRESS_FONT_SIZE)
        };
        commands.push(render::DisplayCommand::SolidRect(
            css::Color::BLACK,
            layout::Rect {
                x: address_text_x + caret_offset,
                y: ADDRESS_TEXT_Y - 1.0,
                width: 1.0,
                height: ADDRESS_FONT_SIZE + 2.0,
            },
        ));
    }

    if chrome.is_https {
        commands.extend(lock_icon_commands(
            address_box,
            css::Color {
                r: 95,
                g: 99,
                b: 104,
                a: 255,
            },
        ));
    }

    if let Some(favicon) = chrome.tab_favicon {
        commands.push(render::DisplayCommand::Image(render::ImageCommand {
            x: TAB_FAVICON_X,
            y: TAB_FAVICON_Y,
            width: TAB_FAVICON_SIZE,
            height: TAB_FAVICON_SIZE,
            source_width: favicon.width,
            source_height: favicon.height,
            // Cloning the pixel buffer matches `<img>` rendering: the
            // raster pass owns its commands by value, so the chrome
            // path can't borrow into the LoadedImage. Tab favicons are
            // small (typically 16×16 = 1KB) so the per-frame copy is
            // negligible compared to the layout / paint work.
            pixels: favicon.pixels.clone(),
            source_x: 0.0,
            source_y: 0.0,
        }));
    }

    commands
}

fn lock_icon_commands(address_box: layout::Rect, color: css::Color) -> Vec<render::DisplayCommand> {
    // Compact padlock anchored to the left of the address pill: a rounded
    // body sits a hair below center, three thin strokes form the U-shaped
    // shackle above it. Sized to fit comfortably in the 36px-tall bar
    // (~14px of total icon height) with the body's right edge clearing
    // `HTTPS_ADDRESS_TEXT_X` by ~5px so glyphs do not graze the metal.
    let cx = address_box.x + 14.0;
    let cy = address_box.y + address_box.height / 2.0;
    let body_width = 10.0;
    let body_height = 8.0;
    let body_x = cx - body_width / 2.0;
    let body_y = cy - 1.0;
    let shackle_width = 6.0;
    let shackle_height = 5.0;
    let shackle_x = cx - shackle_width / 2.0;
    let shackle_y = body_y - shackle_height;
    let stroke = 1.5;
    vec![
        render::DisplayCommand::RoundedRect(
            color,
            layout::Rect {
                x: body_x,
                y: body_y,
                width: body_width,
                height: body_height,
            },
            render::CornerRadii::uniform(1.5),
        ),
        // Left arm of the shackle.
        render::DisplayCommand::SolidRect(
            color,
            layout::Rect {
                x: shackle_x,
                y: shackle_y,
                width: stroke,
                height: shackle_height,
            },
        ),
        // Top of the shackle.
        render::DisplayCommand::SolidRect(
            color,
            layout::Rect {
                x: shackle_x,
                y: shackle_y,
                width: shackle_width,
                height: stroke,
            },
        ),
        // Right arm of the shackle.
        render::DisplayCommand::SolidRect(
            color,
            layout::Rect {
                x: shackle_x + shackle_width - stroke,
                y: shackle_y,
                width: stroke,
                height: shackle_height,
            },
        ),
    ]
}

fn nav_button_commands(
    rect: layout::Rect,
    pointing_left: bool,
    enabled: bool,
    hovered: bool,
) -> Vec<render::DisplayCommand> {
    // Toolbar buttons are flat by default and only paint a circular hover wash on rollover.
    let mut commands = Vec::new();
    if hovered && enabled {
        commands.push(render::DisplayCommand::RoundedRect(
            css::Color {
                r: 232,
                g: 234,
                b: 237,
                a: 255,
            },
            rect,
            render::CornerRadii::uniform(rect.height.min(rect.width) / 2.0),
        ));
    }

    let icon_color = if enabled {
        css::Color {
            r: 60,
            g: 64,
            b: 67,
            a: 255,
        }
    } else {
        css::Color {
            r: 154,
            g: 160,
            b: 166,
            a: 255,
        }
    };
    commands.extend(chevron_commands(rect, icon_color, pointing_left));
    commands
}

fn chevron_commands(
    rect: layout::Rect,
    color: css::Color,
    pointing_left: bool,
) -> Vec<render::DisplayCommand> {
    // Chevrons are seven 1px rows offset from the center line, forming a 2px-thick caret.
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    (0i32..7)
        .map(|row| {
            let dy = row - 3;
            let offset = dy.unsigned_abs() as f32;
            let x = if pointing_left {
                cx - 1.0 + offset
            } else {
                cx - 1.0 - offset
            };
            render::DisplayCommand::SolidRect(
                color,
                layout::Rect {
                    x,
                    y: cy - 3.0 + row as f32,
                    width: 2.0,
                    height: 1.0,
                },
            )
        })
        .collect()
}

fn refresh_button_commands(rect: layout::Rect, hovered: bool) -> Vec<render::DisplayCommand> {
    // Refresh glyph: ~330° arc opening at the top, plus a filled triangular
    // arrow head at the start of the arc pointing radially outward. The arc is
    // stamped by ~80 small squares (density scales with radius so the ring
    // never reads as dotted) — without a dedicated arc primitive in the
    // renderer this is the cleanest way to fake a curve.
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_6, TAU};

    let mut commands = Vec::new();
    if hovered {
        commands.push(render::DisplayCommand::RoundedRect(
            css::Color {
                r: 232,
                g: 234,
                b: 237,
                a: 255,
            },
            rect,
            render::CornerRadii::uniform(rect.height.min(rect.width) / 2.0),
        ));
    }

    let icon_color = css::Color {
        r: 60,
        g: 64,
        b: 67,
        a: 255,
    };
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let radius = (rect.width.min(rect.height) / 2.0 - 5.0).max(4.0);

    // Arc starts ~30° past 12 o'clock (top-right), sweeps clockwise around back
    // to ~30° before 12 o'clock — leaves a clean 60° gap at the top for the
    // arrow head to sit in.
    let arc_start = -FRAC_PI_2 + FRAC_PI_6;
    let arc_total = TAU - 2.0 * FRAC_PI_6;

    // Density chosen so adjacent stamps overlap by ~half their width — gives a
    // visually continuous stroke instead of dotted-line.
    let stops = (radius * 8.0).ceil() as i32;
    for i in 0..=stops {
        let t = i as f32 / stops as f32;
        let theta = arc_start + t * arc_total;
        let x = cx + theta.cos() * radius;
        let y = cy + theta.sin() * radius;
        commands.push(render::DisplayCommand::SolidRect(
            icon_color,
            layout::Rect {
                x: x - 1.0,
                y: y - 1.0,
                width: 2.0,
                height: 2.0,
            },
        ));
    }

    // Triangular arrow head at the arc's start. Base sits on the ring; tip
    // extends `arrow_len` pixels radially outward. Filled by stepping along
    // the radial axis and stamping a 1px-tall band whose width tapers with
    // distance — a poor man's flat-shaded triangle.
    let nx = arc_start.cos();
    let ny = arc_start.sin();
    let tx = -ny;
    let ty = nx;
    let base_cx = cx + radius * nx;
    let base_cy = cy + radius * ny;
    let arrow_len = 5.0_f32;
    let arrow_half = 3.0_f32;
    let steps = arrow_len.ceil() as i32 + 1;
    for i in 0..=steps {
        let progress = i as f32 / steps as f32;
        let half_width = arrow_half * (1.0 - progress);
        let dist = arrow_len * progress;
        let row_cx = base_cx + dist * nx;
        let row_cy = base_cy + dist * ny;
        let span = half_width.ceil() as i32;
        for j in -span..=span {
            if (j as f32).abs() > half_width + 0.5 {
                continue;
            }
            let px = row_cx + (j as f32) * tx;
            let py = row_cy + (j as f32) * ty;
            commands.push(render::DisplayCommand::SolidRect(
                icon_color,
                layout::Rect {
                    x: px - 0.5,
                    y: py - 0.5,
                    width: 1.0,
                    height: 1.0,
                },
            ));
        }
    }

    commands
}

fn menu_button_commands(rect: layout::Rect, hovered: bool) -> Vec<render::DisplayCommand> {
    // Three vertical dots stand in for the overflow menu. The dropdown is still a stub,
    // but the hover wash and click hit-test are wired so the button feels real.
    let mut commands = Vec::new();
    if hovered {
        commands.push(render::DisplayCommand::RoundedRect(
            css::Color {
                r: 232,
                g: 234,
                b: 237,
                a: 255,
            },
            rect,
            render::CornerRadii::uniform(rect.height.min(rect.width) / 2.0),
        ));
    }

    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let dot_size = 3.0;
    let spacing = 5.0;
    let icon_color = css::Color {
        r: 60,
        g: 64,
        b: 67,
        a: 255,
    };

    commands.extend((-1..=1i32).map(|i| {
        render::DisplayCommand::RoundedRect(
            icon_color,
            layout::Rect {
                x: cx - dot_size / 2.0,
                y: cy + (i as f32 * spacing) - dot_size / 2.0,
                width: dot_size,
                height: dot_size,
            },
            render::CornerRadii::uniform(dot_size / 2.0),
        )
    }));
    commands
}

pub fn address_bar_rect(viewport_width: f32) -> layout::Rect {
    let menu_reserved = MENU_BUTTON_RIGHT_PAD + MENU_BUTTON_WIDTH + MENU_BUTTON_GAP;
    layout::Rect {
        x: ADDRESS_BOX_X,
        y: ADDRESS_BOX_Y,
        width: (viewport_width - ADDRESS_BOX_X - menu_reserved).max(0.0),
        height: ADDRESS_BOX_HEIGHT,
    }
}

pub fn back_button_rect() -> layout::Rect {
    layout::Rect {
        x: BACK_BUTTON_X,
        y: NAV_BUTTON_Y,
        width: NAV_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
    }
}

pub fn forward_button_rect() -> layout::Rect {
    layout::Rect {
        x: FORWARD_BUTTON_X,
        y: NAV_BUTTON_Y,
        width: NAV_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
    }
}

pub fn refresh_button_rect() -> layout::Rect {
    layout::Rect {
        x: REFRESH_BUTTON_X,
        y: NAV_BUTTON_Y,
        width: NAV_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
    }
}

pub fn menu_button_rect(viewport_width: f32) -> layout::Rect {
    layout::Rect {
        x: viewport_width - MENU_BUTTON_RIGHT_PAD - MENU_BUTTON_WIDTH,
        y: NAV_BUTTON_Y,
        width: MENU_BUTTON_WIDTH,
        height: NAV_BUTTON_HEIGHT,
    }
}
