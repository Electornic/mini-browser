// What the observation window paints on top of the page (docs/concept.md
// G3, principle 1 "에이전트가 보는 것을 사람도 본다"):
//
//   * an id badge on every interactive element — the same `e21` the agent
//     sees in its observation;
//   * an outline around the element the agent's last click / type aimed at;
//   * a side panel with the policy settings and the action timeline.
//
// Pure functions from data to display commands, so they're unit-tested
// without a window.

use mb_agent::{ApproveMode, ElementObservation};
use mb_runtime::chrome::CHROME_HEIGHT;
use mb_runtime::css::Color;
use mb_runtime::dom::NodeId;
use mb_runtime::layout::Rect;
use mb_runtime::render::{DisplayCommand, TextCommand, measure_text_width};

use crate::worker::TimelineEntry;

/// Width of the agent panel on the right. Below `MIN_WINDOW_FOR_PANEL` the
/// panel is dropped so the page keeps a usable width.
pub const PANEL_WIDTH: usize = 300;
const MIN_WINDOW_FOR_PANEL: usize = PANEL_WIDTH + 400;

const BADGE_FONT: f32 = 10.0;
const BADGE_HEIGHT: f32 = 14.0;
const BADGE_PAD: f32 = 2.0;
const PANEL_FONT: f32 = 12.0;
const PANEL_LINE: f32 = 18.0;
const PANEL_PAD: f32 = 12.0;

const BADGE_BG: Color = Color {
    r: 37,
    g: 99,
    b: 235,
    a: 225,
};
const BADGE_FG: Color = Color {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};
const TARGET: Color = Color {
    r: 234,
    g: 88,
    b: 12,
    a: 255,
};
const PANEL_BG: Color = Color {
    r: 246,
    g: 246,
    b: 248,
    a: 255,
};
const PANEL_RULE: Color = Color {
    r: 210,
    g: 210,
    b: 216,
    a: 255,
};
const INK: Color = Color {
    r: 32,
    g: 33,
    b: 36,
    a: 255,
};
const MUTED: Color = Color {
    r: 110,
    g: 112,
    b: 118,
    a: 255,
};
const OK: Color = Color {
    r: 22,
    g: 128,
    b: 61,
    a: 255,
};
const FAIL: Color = Color {
    r: 185,
    g: 28,
    b: 28,
    a: 255,
};

/// Split a window width into (page width, panel width).
pub fn layout(window_width: usize) -> (usize, usize) {
    if window_width >= MIN_WINDOW_FOR_PANEL {
        (window_width - PANEL_WIDTH, PANEL_WIDTH)
    } else {
        (window_width, 0)
    }
}

fn text(
    text: impl Into<String>,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
    bold: bool,
) -> DisplayCommand {
    DisplayCommand::Text(TextCommand {
        text: text.into(),
        x,
        y,
        color,
        font_size: size,
        wrap_width: None,
        font_family: None,
        font_weight: if bold { 700 } else { 400 },
    })
}

/// Badges and the target outline, in window coordinates. `scroll_y` is the
/// page's current scroll; `page_bottom` the window height. Nothing is drawn
/// over the browser chrome.
pub fn page_overlay(
    elements: &[ElementObservation],
    target: Option<NodeId>,
    scroll_y: f32,
    page_bottom: f32,
) -> Vec<DisplayCommand> {
    let to_window = |rect: Rect| Rect {
        y: rect.y - scroll_y + CHROME_HEIGHT,
        ..rect
    };
    let visible = |r: &Rect| r.y + r.height > CHROME_HEIGHT && r.y < page_bottom;

    let mut commands = Vec::new();
    if let Some(target) = target
        && let Some(element) = elements.iter().find(|e| e.id == target)
    {
        let r = to_window(element.rect);
        if visible(&r) {
            commands.extend(outline(r, 2.0, TARGET));
        }
    }
    for element in elements {
        let r = to_window(element.rect);
        if !visible(&r) {
            continue;
        }
        let label = format!("e{}", element.id.raw());
        let width = measure_text_width(&label, BADGE_FONT) + BADGE_PAD * 2.0;
        // Sit just left of the element when the margin has room, so small
        // controls (checkboxes, short links) stay visible; otherwise pin to
        // its top-left corner. Never under the chrome.
        let x = if r.x - width >= 0.0 { r.x - width } else { r.x };
        let y = r.y.max(CHROME_HEIGHT);
        commands.push(DisplayCommand::SolidRect(
            BADGE_BG,
            Rect {
                x,
                y,
                width,
                height: BADGE_HEIGHT,
            },
        ));
        commands.push(text(
            label,
            x + BADGE_PAD,
            y + 1.5,
            BADGE_FONT,
            BADGE_FG,
            true,
        ));
    }
    commands
}

fn outline(r: Rect, w: f32, color: Color) -> [DisplayCommand; 4] {
    [
        DisplayCommand::SolidRect(
            color,
            Rect {
                x: r.x - w,
                y: r.y - w,
                width: r.width + 2.0 * w,
                height: w,
            },
        ),
        DisplayCommand::SolidRect(
            color,
            Rect {
                x: r.x - w,
                y: r.y + r.height,
                width: r.width + 2.0 * w,
                height: w,
            },
        ),
        DisplayCommand::SolidRect(
            color,
            Rect {
                x: r.x - w,
                y: r.y,
                width: w,
                height: r.height,
            },
        ),
        DisplayCommand::SolidRect(
            color,
            Rect {
                x: r.x + r.width,
                y: r.y,
                width: w,
                height: r.height,
            },
        ),
    ]
}

pub struct PanelInfo<'a> {
    pub approve: ApproveMode,
    pub allow_origins: &'a [String],
    pub timeline: &'a [TimelineEntry],
}

/// The agent panel occupying `[x, x + width) × [0, height)`.
pub fn panel(info: &PanelInfo<'_>, x: f32, width: f32, height: f32) -> Vec<DisplayCommand> {
    let mut commands = vec![
        DisplayCommand::SolidRect(
            PANEL_BG,
            Rect {
                x,
                y: 0.0,
                width,
                height,
            },
        ),
        DisplayCommand::SolidRect(
            PANEL_RULE,
            Rect {
                x,
                y: 0.0,
                width: 1.0,
                height,
            },
        ),
    ];
    let left = x + PANEL_PAD;
    let max_chars = ((width - PANEL_PAD * 2.0) / 7.0) as usize;
    let mut y = PANEL_PAD;
    let line =
        |commands: &mut Vec<DisplayCommand>, y: &mut f32, s: String, color: Color, bold: bool| {
            commands.push(text(clip(&s, max_chars), left, *y, PANEL_FONT, color, bold));
            *y += PANEL_LINE;
        };

    line(
        &mut commands,
        &mut y,
        "mini-browser · agent view".into(),
        INK,
        true,
    );
    let (approve, meaning) = match info.approve {
        ApproveMode::Auto => ("auto", "risky actions are allowed"),
        ApproveMode::Deny => ("deny", "POST / other-origin moves refused"),
    };
    line(
        &mut commands,
        &mut y,
        format!("approve: {approve}"),
        MUTED,
        false,
    );
    line(&mut commands, &mut y, format!("  {meaning}"), MUTED, false);
    let origins = if info.allow_origins.is_empty() {
        "any".to_string()
    } else {
        info.allow_origins.join(", ")
    };
    line(
        &mut commands,
        &mut y,
        format!("origins: {origins}"),
        MUTED,
        false,
    );
    line(
        &mut commands,
        &mut y,
        "You can use this window too; only the".into(),
        MUTED,
        false,
    );
    line(
        &mut commands,
        &mut y,
        "agent's actions are gated.".into(),
        MUTED,
        false,
    );
    y += PANEL_LINE / 2.0;
    line(&mut commands, &mut y, "Agent actions".into(), INK, true);

    // Newest at the bottom, as many as fit.
    let rows = ((height - y - PANEL_PAD) / PANEL_LINE).max(0.0) as usize;
    if info.timeline.is_empty() {
        line(
            &mut commands,
            &mut y,
            "(waiting for the agent)".into(),
            MUTED,
            false,
        );
    }
    let start = info.timeline.len().saturating_sub(rows);
    for entry in &info.timeline[start..] {
        match &entry.error {
            None => line(
                &mut commands,
                &mut y,
                format!("✓ {}", entry.label),
                OK,
                false,
            ),
            Some(code) => line(
                &mut commands,
                &mut y,
                format!("✗ {} — {code}", entry.label),
                FAIL,
                false,
            ),
        }
    }
    commands
}

fn clip(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use mb_agent::Role;

    fn element(id: u32, y: f32) -> ElementObservation {
        ElementObservation {
            id: NodeId::from_raw(id),
            role: Role::Button,
            name: "x".into(),
            field: None,
            value: None,
            checked: None,
            href: None,
            flags: Vec::new(),
            rect: Rect {
                x: 10.0,
                y,
                width: 80.0,
                height: 20.0,
            },
        }
    }

    fn badge_texts(commands: &[DisplayCommand]) -> Vec<String> {
        commands
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn badges_follow_scroll_and_skip_offscreen_elements() {
        let elements = [element(5, 20.0), element(9, 900.0)];
        let top = page_overlay(&elements, None, 0.0, 700.0);
        assert_eq!(badge_texts(&top), vec!["e5"]);
        let scrolled = page_overlay(&elements, None, 600.0, 700.0);
        assert_eq!(badge_texts(&scrolled), vec!["e9"]);
        // e9 at document y 900, scrolled 600 → window y 300 + chrome; the
        // element starts at x 10, too close to the edge for a left badge.
        let DisplayCommand::SolidRect(_, badge) = &scrolled[0] else {
            panic!("badge background first");
        };
        assert_eq!(badge.y, 300.0 + CHROME_HEIGHT);
        assert_eq!(badge.x, 10.0);
    }

    #[test]
    fn badges_move_left_of_the_element_when_there_is_room() {
        let mut far = element(5, 20.0);
        far.rect.x = 200.0;
        let commands = page_overlay(&[far], None, 0.0, 700.0);
        let DisplayCommand::SolidRect(_, badge) = &commands[0] else {
            panic!("badge background first");
        };
        assert!(
            badge.x + badge.width <= 200.0,
            "badge clear of the element: {badge:?}"
        );
    }

    #[test]
    fn target_gets_an_outline() {
        let elements = [element(5, 20.0)];
        let plain = page_overlay(&elements, None, 0.0, 700.0);
        let targeted = page_overlay(&elements, Some(NodeId::from_raw(5)), 0.0, 700.0);
        assert_eq!(targeted.len(), plain.len() + 4);
    }

    #[test]
    fn panel_lists_recent_actions_with_outcomes() {
        let timeline = vec![
            TimelineEntry {
                label: "click e24".into(),
                error: None,
            },
            TimelineEntry {
                label: "click e30".into(),
                error: Some("approval_denied".into()),
            },
        ];
        let commands = panel(
            &PanelInfo {
                approve: ApproveMode::Deny,
                allow_origins: &[],
                timeline: &timeline,
            },
            900.0,
            300.0,
            700.0,
        );
        let texts = badge_texts(&commands);
        assert!(texts.contains(&"✓ click e24".to_string()), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("✗ click e30 — approval_denied")),
            "{texts:?}"
        );
    }

    #[test]
    fn narrow_windows_drop_the_panel() {
        assert_eq!(layout(1280), (980, 300));
        assert_eq!(layout(600), (600, 0));
    }
}
