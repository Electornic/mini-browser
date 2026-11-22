// Pure painting helpers and hit-region rects for the browser chrome (tab strip,
// toolbar, address bar, nav buttons). Kept module-local so `BrowserState` only
// needs `ChromeState` + `chrome_commands` + the per-button rects, with no
// awareness of pixel-level layout.

use crate::{css, layout, palette, render, resource::LoadedImage};

// These constants define the browser chrome at the top of the window
// (design board "2 · Browser chrome"). Everything below `CHROME_HEIGHT` is
// treated as page content. The chrome stacks a tab strip on top of a white
// toolbar; toolbar constants are expressed in screen coordinates so they
// already include `TAB_STRIP_HEIGHT` as their top offset.
//
// `CHROME_HEIGHT` is shared with the agent (click coordinates, screenshot
// cropping, window size = viewport + chrome), so the design's optional rows
// (the blue "agent is driving" bar, the bottom status bar) are not chrome
// rows here: the status text sits at the right end of the tab strip, and
// agent windows put their pill in the accessory slot (`ChromeExtras`).
pub const TAB_STRIP_HEIGHT: f32 = 44.0;
pub const TOOLBAR_HEIGHT: f32 = 52.0;
pub const CHROME_HEIGHT: f32 = TAB_STRIP_HEIGHT + TOOLBAR_HEIGHT;
pub const NAV_BUTTON_WIDTH: f32 = 36.0;
pub const NAV_BUTTON_HEIGHT: f32 = 36.0;
pub const NAV_BUTTON_Y: f32 = TAB_STRIP_HEIGHT + (TOOLBAR_HEIGHT - NAV_BUTTON_HEIGHT) / 2.0;
const TOOLBAR_GAP: f32 = 6.0;
const BUTTON_RADIUS: f32 = 8.0;
pub const BACK_BUTTON_X: f32 = 12.0;
pub const FORWARD_BUTTON_X: f32 = BACK_BUTTON_X + NAV_BUTTON_WIDTH + TOOLBAR_GAP;
pub const REFRESH_BUTTON_X: f32 = FORWARD_BUTTON_X + NAV_BUTTON_WIDTH + TOOLBAR_GAP;
pub const ADDRESS_BOX_X: f32 = REFRESH_BUTTON_X + NAV_BUTTON_WIDTH + TOOLBAR_GAP;
pub const ADDRESS_BOX_Y: f32 = NAV_BUTTON_Y;
pub const ADDRESS_BOX_HEIGHT: f32 = 36.0;
const ADDRESS_RADIUS: f32 = 10.0;
pub const ADDRESS_TEXT_X: f32 = ADDRESS_BOX_X + 12.0;
pub const ADDRESS_FONT_SIZE: f32 = 14.0;
pub const ADDRESS_TEXT_Y: f32 =
    ADDRESS_BOX_Y + (ADDRESS_BOX_HEIGHT - ADDRESS_FONT_SIZE * 1.2) / 2.0;
// When the current URL is https, the address bar paints a small padlock
// at its left edge and shifts the text right so glyphs do not overlap
// the icon. The HTTPS_ADDRESS_TEXT_X offset is calibrated to leave ~5px
// breathing room past the lock's rightmost stamp (`lock_icon_commands`).
pub const HTTPS_ADDRESS_TEXT_X: f32 = ADDRESS_BOX_X + 28.0;
// The scheme chip ("http") of a non-https page, shown while the bar is not
// being edited (design board 2): mono 11 px on white with a 1 px border.
const SCHEME_CHIP_FONT: f32 = 11.0;
const SCHEME_CHIP_HEIGHT: f32 = 20.0;
const SCHEME_CHIP_PAD: f32 = 6.0;
pub const STATUS_FONT_SIZE: f32 = 12.0;
pub const MENU_BUTTON_WIDTH: f32 = 36.0;
pub const MENU_BUTTON_RIGHT_PAD: f32 = 12.0;
pub const MENU_BUTTON_GAP: f32 = TOOLBAR_GAP;
pub const TAB_X: f32 = 12.0;
pub const TAB_Y: f32 = TAB_STRIP_HEIGHT - TAB_HEIGHT;
pub const TAB_WIDTH: f32 = 240.0;
pub const TAB_HEIGHT: f32 = 34.0;
pub const TAB_RADIUS: f32 = 10.0;
pub const TAB_TITLE_X: f32 = TAB_X + 12.0;
pub const TAB_TITLE_FONT_SIZE: f32 = 13.0;
pub const TAB_TITLE_Y: f32 = TAB_Y + (TAB_HEIGHT - TAB_TITLE_FONT_SIZE * 1.2) / 2.0;
// Favicon sits to the left of the tab title at 16×16, centred on the tab.
pub const TAB_FAVICON_X: f32 = TAB_X + 12.0;
pub const TAB_FAVICON_Y: f32 = TAB_Y + (TAB_HEIGHT - TAB_FAVICON_SIZE) / 2.0;
pub const TAB_FAVICON_SIZE: f32 = 16.0;
// When a favicon is present, the title shifts right by the icon
// width plus an 8px gap. Tabs without a favicon keep `TAB_TITLE_X`,
// so plain pages do not pay icon-sized left pad they cannot use.
pub const TAB_TITLE_X_WITH_FAVICON: f32 = TAB_FAVICON_X + TAB_FAVICON_SIZE + 8.0;
// The agent dot (8 px + 8 px gap) an agent window puts before the title.
const TAB_MARKER_SIZE: f32 = 8.0;

/// What an embedder adds to the plain chrome. `mb-shell` uses the default
/// (a plain browser, design board 6); `mb-mcp --window` reserves a slot for
/// its agent pill and marks the tab (boards 2 / 3).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ChromeExtras {
    /// Width reserved between the address bar and ⋮ for a pill the
    /// embedder paints into `accessory_rect` (0 = no slot).
    pub accessory_width: f32,
    /// A dot before the tab title (agent windows: agent blue).
    pub tab_marker: Option<css::Color>,
    /// ⋮ drawn pressed: its menu is open.
    pub menu_open: bool,
}

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
    /// about:blank / file://) leaves that area to the scheme chip.
    pub is_https: bool,
    /// Favicon to render to the left of the tab title, if the document
    /// exposed `<link rel="icon">` and the fetch + decode succeeded.
    /// `None` keeps the title at its unshifted origin so plain pages
    /// do not paint a 16-px gap of nothing.
    pub tab_favicon: Option<&'a LoadedImage>,
    pub extras: ChromeExtras,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromeAction {
    Back,
    Forward,
    Refresh,
    Menu,
}

fn label(
    text: impl Into<String>,
    x: f32,
    y: f32,
    size: f32,
    color: css::Color,
    mono: bool,
    weight: u16,
) -> render::DisplayCommand {
    render::DisplayCommand::Text(render::TextCommand {
        text: text.into(),
        x,
        y,
        color,
        font_size: size,
        wrap_width: None,
        font_family: mono.then(|| palette::MONO.to_string()),
        font_weight: weight,
    })
}

// A rounded box with a 1 px border (the renderer only fills, so the border
// is the outer fill and the inside is painted over it).
pub fn framed_rect(
    rect: layout::Rect,
    radius: f32,
    border: css::Color,
    fill: css::Color,
) -> [render::DisplayCommand; 2] {
    [
        render::DisplayCommand::RoundedRect(border, rect, render::CornerRadii::uniform(radius)),
        render::DisplayCommand::RoundedRect(
            fill,
            layout::Rect {
                x: rect.x + 1.0,
                y: rect.y + 1.0,
                width: (rect.width - 2.0).max(0.0),
                height: (rect.height - 2.0).max(0.0),
            },
            render::CornerRadii::uniform((radius - 1.0).max(0.0)),
        ),
    ]
}

/// `scheme://rest` → (`scheme`, `rest`) for the scheme chip; `None` for
/// addresses without one (`about:blank`, a half-typed host).
pub fn split_scheme(address: &str) -> Option<(&str, &str)> {
    let (scheme, rest) = address.split_once("://")?;
    (!scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+'))
    .then_some((scheme, rest))
}

pub fn chrome_commands(chrome: ChromeState<'_>) -> Vec<render::DisplayCommand> {
    // Chrome rendering is intentionally separate from page rendering so scrolling never moves it.
    let width = chrome.viewport_width as f32;
    let input_empty = chrome.address_input.is_empty();
    let editing = chrome.address_bar_focused || chrome.address_bar_selected;
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
        palette::FAINT
    } else {
        palette::INK
    };
    let address_box = address_bar_rect_with(width, chrome.extras.accessory_width);
    // Editing turns the field white with a darker edge; at rest it is a
    // panel-grey well (board 2). Blue is the agent's color, not focus.
    let (border_color, field_color) = if chrome.address_bar_focused {
        (palette::MUTED, palette::PAGE)
    } else {
        (palette::LINE, palette::PANEL)
    };
    // The padlock and the text live in the same field, so the text origin
    // and the caret/selection highlight have to honour the same indent —
    // otherwise the caret would jump under the icon when the lock paints.
    let address_text_x = if chrome.is_https {
        HTTPS_ADDRESS_TEXT_X
    } else {
        ADDRESS_TEXT_X
    };

    let mut commands = vec![
        // Tab strip sits behind everything else and is the darker band of chrome.
        render::DisplayCommand::SolidRect(
            palette::STRIP,
            layout::Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: TAB_STRIP_HEIGHT,
            },
        ),
        // The toolbar is white like the page, set off by a 1px rule.
        render::DisplayCommand::SolidRect(
            palette::PAGE,
            layout::Rect {
                x: 0.0,
                y: TAB_STRIP_HEIGHT,
                width,
                height: TOOLBAR_HEIGHT,
            },
        ),
        render::DisplayCommand::SolidRect(
            palette::LINE,
            layout::Rect {
                x: 0.0,
                y: CHROME_HEIGHT - 1.0,
                width,
                height: 1.0,
            },
        ),
        // Active tab paints in the toolbar's white so the two surfaces merge
        // along the bottom edge while the rounded top corners show on the
        // darker strip.
        render::DisplayCommand::RoundedRect(
            palette::PAGE,
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
    ];
    let mut title_x = if chrome.tab_favicon.is_some() {
        TAB_TITLE_X_WITH_FAVICON
    } else {
        TAB_TITLE_X
    };
    if let Some(marker) = chrome.extras.tab_marker {
        commands.push(render::DisplayCommand::RoundedRect(
            marker,
            layout::Rect {
                x: title_x,
                y: TAB_Y + (TAB_HEIGHT - TAB_MARKER_SIZE) / 2.0,
                width: TAB_MARKER_SIZE,
                height: TAB_MARKER_SIZE,
            },
            render::CornerRadii::uniform(TAB_MARKER_SIZE / 2.0),
        ));
        title_x += TAB_MARKER_SIZE + 8.0;
    }
    commands.push(label(
        clip_to_width(
            chrome.tab_title,
            TAB_X + TAB_WIDTH - 12.0 - title_x,
            TAB_TITLE_FONT_SIZE,
        ),
        title_x,
        TAB_TITLE_Y,
        TAB_TITLE_FONT_SIZE,
        palette::INK,
        false,
        palette::WEIGHT_REGULAR,
    ));
    // Status ("loaded", "loading…", errors) at the right end of the strip,
    // clear of the tab.
    if !chrome.status_text.is_empty() {
        let status_width = render::measure_text_width(chrome.status_text, STATUS_FONT_SIZE);
        let status_x = width - 16.0 - status_width;
        if status_x > TAB_X + TAB_WIDTH + 12.0 {
            commands.push(label(
                chrome.status_text,
                status_x,
                palette::centered_text_y(TAB_Y, TAB_HEIGHT, STATUS_FONT_SIZE),
                STATUS_FONT_SIZE,
                chrome.status_color,
                false,
                palette::WEIGHT_REGULAR,
            ));
        }
    }
    commands.extend(framed_rect(
        address_box,
        ADDRESS_RADIUS,
        border_color,
        field_color,
    ));
    // At rest a non-https page shows its scheme as a chip and the rest of
    // the address after it; editing shows the raw text the person types.
    let chip = (!editing && !chrome.is_https && !input_empty)
        .then(|| split_scheme(chrome.address_input))
        .flatten();
    if let Some((scheme, rest)) = chip {
        let chip_width =
            palette::text_width(scheme, SCHEME_CHIP_FONT, true, 600) + SCHEME_CHIP_PAD * 2.0;
        let chip_rect = layout::Rect {
            x: address_box.x + 10.0,
            y: address_box.y + (address_box.height - SCHEME_CHIP_HEIGHT) / 2.0,
            width: chip_width,
            height: SCHEME_CHIP_HEIGHT,
        };
        commands.extend(framed_rect(chip_rect, 6.0, palette::LINE, palette::PAGE));
        commands.push(label(
            scheme,
            chip_rect.x + SCHEME_CHIP_PAD,
            palette::centered_text_y(chip_rect.y, chip_rect.height, SCHEME_CHIP_FONT),
            SCHEME_CHIP_FONT,
            palette::MUTED,
            true,
            600,
        ));
        commands.push(label(
            rest,
            chip_rect.x + chip_rect.width + 10.0,
            ADDRESS_TEXT_Y,
            ADDRESS_FONT_SIZE,
            address_color,
            true,
            palette::WEIGHT_REGULAR,
        ));
    } else {
        let shown = if !editing && chrome.is_https {
            split_scheme(&address_display).map_or(address_display.as_str(), |(_, rest)| rest)
        } else {
            address_display.as_str()
        };
        commands.push(label(
            shown,
            address_text_x,
            ADDRESS_TEXT_Y,
            ADDRESS_FONT_SIZE,
            address_color,
            true,
            palette::WEIGHT_REGULAR,
        ));
    }
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
        chrome.extras.menu_open,
    ));

    if chrome.address_bar_selected {
        let measured = palette::text_width(&address_display, ADDRESS_FONT_SIZE, true, 400);
        commands.push(render::DisplayCommand::SolidRect(
            palette::AGENT_BORDER,
            layout::Rect {
                x: address_text_x - 2.0,
                y: ADDRESS_TEXT_Y - 1.0,
                width: (measured + 4.0).min((address_box.width - 8.0).max(0.0)),
                height: ADDRESS_FONT_SIZE * 1.2 + 2.0,
            },
        ));
        commands.push(label(
            address_display,
            address_text_x,
            ADDRESS_TEXT_Y,
            ADDRESS_FONT_SIZE,
            palette::INK,
            true,
            palette::WEIGHT_REGULAR,
        ));
    } else if chrome.show_caret {
        // Caret position is measured from the *actual input* (empty when only the
        // placeholder is showing), and uses cosmic-text's shaped advance in the
        // same monospace face so it lines up with where the text actually ends.
        let caret_offset = if input_empty {
            0.0
        } else {
            palette::text_width(&address_display, ADDRESS_FONT_SIZE, true, 400)
        };
        commands.push(render::DisplayCommand::SolidRect(
            palette::INK,
            layout::Rect {
                x: address_text_x + caret_offset,
                y: ADDRESS_TEXT_Y,
                width: 1.0,
                height: ADDRESS_FONT_SIZE * 1.2,
            },
        ));
    }

    if chrome.is_https {
        commands.extend(lock_icon_commands(address_box, palette::MUTED));
    }

    if let Some(favicon) = chrome.tab_favicon {
        commands.push(render::DisplayCommand::Image(render::ImageCommand {
            x: TAB_FAVICON_X,
            y: TAB_FAVICON_Y,
            width: TAB_FAVICON_SIZE,
            height: TAB_FAVICON_SIZE,
            source_width: favicon.width,
            source_height: favicon.height,
            // One Vec copy at chrome construction so the raster pass
            // owns its data; subsequent display-list clones (cache
            // hits) only bump the Rc refcount.
            pixels: std::rc::Rc::new(favicon.pixels.clone()),
            source_x: 0.0,
            source_y: 0.0,
        }));
    }

    commands
}

// Cut `text` with an ellipsis so it fits `max_width` at `size`.
fn clip_to_width(text: &str, max_width: f32, size: f32) -> String {
    if render::measure_text_width(text, size) <= max_width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate: String = chars.iter().collect::<String>() + "…";
        if render::measure_text_width(&candidate, size) <= max_width {
            return candidate;
        }
    }
    String::new()
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

fn hover_wash(rect: layout::Rect) -> render::DisplayCommand {
    render::DisplayCommand::RoundedRect(
        palette::PANEL,
        rect,
        render::CornerRadii::uniform(BUTTON_RADIUS),
    )
}

fn nav_button_commands(
    rect: layout::Rect,
    pointing_left: bool,
    enabled: bool,
    hovered: bool,
) -> Vec<render::DisplayCommand> {
    // Toolbar buttons are flat by default and only paint a rounded wash on rollover.
    let mut commands = Vec::new();
    if hovered && enabled {
        commands.push(hover_wash(rect));
    }
    let icon_color = if enabled {
        palette::INK
    } else {
        palette::DISABLED
    };
    commands.extend(chevron_commands(rect, icon_color, pointing_left));
    commands
}

fn chevron_commands(
    rect: layout::Rect,
    color: css::Color,
    pointing_left: bool,
) -> Vec<render::DisplayCommand> {
    // A 16 px icon's chevron (board 2: "M10 3L5 8l5 5"): eleven 1px rows
    // offset from the center line, each 2px wide, forming a 2px-thick caret.
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    (0i32..11)
        .map(|row| {
            let dy = row - 5;
            let offset = dy.unsigned_abs() as f32;
            let x = if pointing_left {
                cx - 3.0 + offset
            } else {
                cx + 1.0 - offset
            };
            render::DisplayCommand::SolidRect(
                color,
                layout::Rect {
                    x,
                    y: cy - 5.5 + row as f32,
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
        commands.push(hover_wash(rect));
    }

    let icon_color = palette::INK;
    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let radius = (rect.width.min(rect.height) / 2.0 - 12.0).max(4.0);

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

fn menu_button_commands(
    rect: layout::Rect,
    hovered: bool,
    open: bool,
) -> Vec<render::DisplayCommand> {
    // Three vertical dots. Pressed (its menu open) it sits on a line-grey
    // wash, as on board 2b; otherwise only a hover wash.
    let mut commands = Vec::new();
    if open {
        commands.push(render::DisplayCommand::RoundedRect(
            palette::LINE,
            rect,
            render::CornerRadii::uniform(BUTTON_RADIUS),
        ));
    } else if hovered {
        commands.push(hover_wash(rect));
    }

    let cx = rect.x + rect.width / 2.0;
    let cy = rect.y + rect.height / 2.0;
    let dot_size = 3.0;
    let spacing = 5.0;
    commands.extend((-1..=1i32).map(|i| {
        render::DisplayCommand::RoundedRect(
            palette::INK,
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

/// The address field with no accessory slot (`mb-shell`).
pub fn address_bar_rect(viewport_width: f32) -> layout::Rect {
    address_bar_rect_with(viewport_width, 0.0)
}

/// The address field when `accessory_width` is reserved before ⋮.
pub fn address_bar_rect_with(viewport_width: f32, accessory_width: f32) -> layout::Rect {
    let menu_reserved = MENU_BUTTON_RIGHT_PAD + MENU_BUTTON_WIDTH + MENU_BUTTON_GAP;
    let accessory = if accessory_width > 0.0 {
        accessory_width + TOOLBAR_GAP
    } else {
        0.0
    };
    layout::Rect {
        x: ADDRESS_BOX_X,
        y: ADDRESS_BOX_Y,
        width: (viewport_width - ADDRESS_BOX_X - menu_reserved - accessory).max(0.0),
        height: ADDRESS_BOX_HEIGHT,
    }
}

/// Where the embedder paints its pill: right of the address field, left
/// of ⋮, toolbar-button height.
pub fn accessory_rect(viewport_width: f32, accessory_width: f32) -> layout::Rect {
    let menu = menu_button_rect(viewport_width);
    layout::Rect {
        x: menu.x - MENU_BUTTON_GAP - accessory_width,
        y: NAV_BUTTON_Y,
        width: accessory_width,
        height: NAV_BUTTON_HEIGHT,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_rows_line_up() {
        assert_eq!(CHROME_HEIGHT, 96.0);
        // Buttons and the address field share one 36 px row, centred in the toolbar.
        assert_eq!(NAV_BUTTON_Y, TAB_STRIP_HEIGHT + 8.0);
        assert_eq!(ADDRESS_BOX_Y, NAV_BUTTON_Y);
        let refresh = refresh_button_rect();
        assert_eq!(ADDRESS_BOX_X, refresh.x + refresh.width + 6.0);
        // The active tab sits on the strip's bottom edge.
        assert_eq!(TAB_Y + TAB_HEIGHT, TAB_STRIP_HEIGHT);
    }

    #[test]
    fn the_accessory_slot_sits_between_the_address_field_and_the_menu() {
        let plain = address_bar_rect(900.0);
        let field = address_bar_rect_with(900.0, 180.0);
        let pill = accessory_rect(900.0, 180.0);
        let menu = menu_button_rect(900.0);
        assert_eq!(plain.width - field.width, 180.0 + 6.0);
        assert_eq!(field.x + field.width + 6.0, pill.x);
        assert_eq!(pill.x + pill.width + 6.0, menu.x);
        assert_eq!(address_bar_rect_with(900.0, 0.0), plain);
    }

    #[test]
    fn scheme_chip_splits_only_real_schemes() {
        assert_eq!(
            split_scheme("http://127.0.0.1:8765/"),
            Some(("http", "127.0.0.1:8765/"))
        );
        assert_eq!(split_scheme("about:blank"), None);
        assert_eq!(split_scheme("://x"), None);
        assert_eq!(split_scheme("a b://x"), None);
    }
}
