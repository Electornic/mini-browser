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

use crate::http::Attach;
use crate::worker::{Answer, TimelineEntry, Waiting};

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
    /// A request waiting for the person (approval / password handoff).
    pub waiting: Option<&'a Waiting>,
    /// The agent's pending `wait_for`: (what, seconds left).
    pub page_wait: Option<(String, u64)>,
    /// `--http`: where an agent attaches (G3b).
    pub attach: Option<&'a Attach>,
    /// The setup command was copied (the button says so).
    pub copied: bool,
}

/// What a panel button does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PanelAction {
    /// Answer the parked request.
    Answer(Answer),
    /// Copy the `claude mcp add ...` command for `--http`.
    CopySetup,
}

/// A clickable button in the panel, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Button {
    pub rect: Rect,
    pub action: PanelAction,
}

impl Button {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.rect.x
            && x < self.rect.x + self.rect.width
            && y >= self.rect.y
            && y < self.rect.y + self.rect.height
    }
}

const CARD_BG: Color = Color {
    r: 255,
    g: 247,
    b: 214,
    a: 255,
};
const CARD_BORDER: Color = Color {
    r: 217,
    g: 119,
    b: 6,
    a: 255,
};
const BUTTON_BG: Color = Color {
    r: 255,
    g: 255,
    b: 255,
    a: 255,
};
const BUTTON_PRIMARY: Color = Color {
    r: 37,
    g: 99,
    b: 235,
    a: 255,
};
const BUTTON_HEIGHT: f32 = 26.0;

/// (label, answer, primary?)
type CardButton = (&'static str, Answer, bool);

// The card asking the person something, at `top`. Returns its commands,
// its buttons and its height.
fn card(
    waiting: &Waiting,
    x: f32,
    width: f32,
    top: f32,
) -> (Vec<DisplayCommand>, Vec<Button>, f32) {
    let (title, lines, buttons): (&str, Vec<String>, Vec<CardButton>) = match waiting {
        Waiting::Approval { action } => (
            "The agent wants to:",
            wrap(action, ((width - 2.0 * PANEL_PAD) / 7.0) as usize, 3),
            vec![
                ("Approve", Answer::Approve, true),
                ("Deny", Answer::Deny, false),
                ("Do it myself", Answer::TakeOver, false),
            ],
        ),
        Waiting::Handoff { id } => (
            "Type the password yourself",
            vec![
                format!("Field {id} is outlined on the page."),
                "Click it, type, then press Done.".into(),
            ],
            vec![
                ("Done", Answer::HandoffDone, true),
                ("Cancel", Answer::HandoffCancel, false),
            ],
        ),
    };
    let inner_left = x + PANEL_PAD;
    let inner_width = width - 2.0 * PANEL_PAD;
    let height = 12.0 + PANEL_LINE * (1 + lines.len()) as f32 + 8.0 + BUTTON_HEIGHT + 12.0;
    let mut commands = vec![
        DisplayCommand::SolidRect(
            CARD_BORDER,
            Rect {
                x: inner_left - 6.0,
                y: top,
                width: inner_width + 12.0,
                height,
            },
        ),
        DisplayCommand::SolidRect(
            CARD_BG,
            Rect {
                x: inner_left - 4.0,
                y: top + 2.0,
                width: inner_width + 8.0,
                height: height - 4.0,
            },
        ),
    ];
    let mut y = top + 12.0;
    commands.push(text(title, inner_left, y, PANEL_FONT, INK, true));
    y += PANEL_LINE;
    for line in &lines {
        commands.push(text(line.clone(), inner_left, y, PANEL_FONT, INK, false));
        y += PANEL_LINE;
    }
    y += 8.0;
    let gap = 6.0;
    let count = buttons.len() as f32;
    let button_width = (inner_width - gap * (count - 1.0)) / count;
    let mut hit = Vec::new();
    for (i, (label, answer, primary)) in buttons.into_iter().enumerate() {
        let rect = Rect {
            x: inner_left + i as f32 * (button_width + gap),
            y,
            width: button_width,
            height: BUTTON_HEIGHT,
        };
        commands.extend(button(rect, label, primary));
        hit.push(Button {
            rect,
            action: PanelAction::Answer(answer),
        });
    }
    (commands, hit, height)
}

fn button(rect: Rect, label: &str, primary: bool) -> Vec<DisplayCommand> {
    let (bg, fg) = if primary {
        (BUTTON_PRIMARY, BADGE_FG)
    } else {
        (BUTTON_BG, INK)
    };
    let mut commands = outline(rect, 1.0, PANEL_RULE).to_vec();
    commands.push(DisplayCommand::SolidRect(bg, rect));
    let label_width = measure_text_width(label, PANEL_FONT);
    commands.push(text(
        label.to_string(),
        rect.x + (rect.width - label_width).max(0.0) / 2.0,
        rect.y + 6.0,
        PANEL_FONT,
        fg,
        primary,
    ));
    commands
}

// Split on spaces into lines of at most `max` chars, `limit` lines.
fn wrap(s: &str, max: usize, limit: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in s.split(' ') {
        let mut word = word.to_string();
        while word.chars().count() > max {
            let head: String = word.chars().take(max).collect();
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.push(head);
            word = word.chars().skip(max).collect();
        }
        if current.is_empty() {
            current = word;
        } else if current.chars().count() + 1 + word.chars().count() <= max {
            current.push(' ');
            current.push_str(&word);
        } else {
            lines.push(std::mem::replace(&mut current, word));
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.len() > limit {
        lines.truncate(limit);
        if let Some(last) = lines.last_mut() {
            *last = clip(&format!("{last}…"), max);
        }
    }
    lines
}

/// The agent panel occupying `[x, x + width) × [0, height)`, plus the
/// buttons of any question card in it.
pub fn panel(
    info: &PanelInfo<'_>,
    x: f32,
    width: f32,
    height: f32,
) -> (Vec<DisplayCommand>, Vec<Button>) {
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
        ApproveMode::Ask => ("ask", "you approve POST / other-origin moves"),
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
    let mut buttons = Vec::new();
    if let Some(attach) = info.attach {
        line(
            &mut commands,
            &mut y,
            "Agents attach over HTTP".into(),
            INK,
            true,
        );
        line(&mut commands, &mut y, attach.url.clone(), MUTED, false);
        line(
            &mut commands,
            &mut y,
            format!("token: {}", crate::http::masked(&attach.token)),
            MUTED,
            false,
        );
        let rect = Rect {
            x: left,
            y,
            width: width - PANEL_PAD * 2.0,
            height: BUTTON_HEIGHT,
        };
        let label = if info.copied {
            "Copied — paste it in a terminal"
        } else {
            "Copy setup command"
        };
        commands.extend(button(rect, label, false));
        buttons.push(Button {
            rect,
            action: PanelAction::CopySetup,
        });
        y += BUTTON_HEIGHT + PANEL_LINE;
    }
    if let Some(waiting) = info.waiting {
        let (card_commands, card_buttons, card_height) = card(waiting, x, width, y);
        commands.extend(card_commands);
        buttons.extend(card_buttons);
        y += card_height + PANEL_LINE / 2.0;
    }
    if let Some((what, left)) = &info.page_wait {
        line(
            &mut commands,
            &mut y,
            format!("Agent waits for {what}"),
            INK,
            true,
        );
        line(
            &mut commands,
            &mut y,
            format!("  {left}s left · you can keep using the page"),
            MUTED,
            false,
        );
        y += PANEL_LINE / 2.0;
    }
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
    (commands, buttons)
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
        let (commands, _) = panel(
            &PanelInfo {
                approve: ApproveMode::Deny,
                allow_origins: &[],
                timeline: &timeline,
                waiting: None,
                page_wait: None,
                attach: None,
                copied: false,
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
    fn approval_card_offers_three_answers_with_hit_boxes() {
        let waiting = Waiting::Approval {
            action: "POST http://127.0.0.1:8765/signup".into(),
        };
        let (commands, buttons) = panel(
            &PanelInfo {
                approve: ApproveMode::Ask,
                allow_origins: &[],
                timeline: &[],
                waiting: Some(&waiting),
                page_wait: None,
                attach: None,
                copied: false,
            },
            900.0,
            300.0,
            700.0,
        );
        let texts = badge_texts(&commands);
        assert!(
            texts.contains(&"The agent wants to:".to_string()),
            "{texts:?}"
        );
        assert!(texts.iter().any(|t| t.contains("/signup")), "{texts:?}");
        let answers: Vec<PanelAction> = buttons.iter().map(|b| b.action).collect();
        assert_eq!(
            answers,
            [Answer::Approve, Answer::Deny, Answer::TakeOver].map(PanelAction::Answer)
        );
        // Buttons sit inside the panel and don't overlap.
        for b in &buttons {
            assert!(
                b.rect.x >= 900.0 && b.rect.x + b.rect.width <= 1200.0,
                "{b:?}"
            );
        }
        let centre = |b: &Button| {
            (
                b.rect.x + b.rect.width / 2.0,
                b.rect.y + b.rect.height / 2.0,
            )
        };
        let (x, y) = centre(&buttons[1]);
        assert!(buttons[1].contains(x, y));
        assert!(!buttons[0].contains(x, y));
    }

    #[test]
    fn handoff_card_offers_done_and_cancel() {
        let waiting = Waiting::Handoff { id: "e15".into() };
        let (_, buttons) = panel(
            &PanelInfo {
                approve: ApproveMode::Ask,
                allow_origins: &[],
                timeline: &[],
                waiting: Some(&waiting),
                page_wait: None,
                attach: None,
                copied: false,
            },
            900.0,
            300.0,
            700.0,
        );
        let answers: Vec<PanelAction> = buttons.iter().map(|b| b.action).collect();
        assert_eq!(
            answers,
            [Answer::HandoffDone, Answer::HandoffCancel].map(PanelAction::Answer)
        );
    }

    #[test]
    fn wrap_breaks_long_actions_into_lines() {
        let lines = wrap(
            "POST http://127.0.0.1:8765/a-very-long-path/that/keeps/going",
            20,
            3,
        );
        assert!(lines.len() <= 3);
        assert!(lines.iter().all(|l| l.chars().count() <= 20), "{lines:?}");
    }

    #[test]
    fn narrow_windows_drop_the_panel() {
        assert_eq!(layout(1280), (980, 300));
        assert_eq!(layout(600), (600, 0));
    }

    #[test]
    fn attach_section_shows_the_url_masked_token_and_copy_button() {
        let attach = Attach {
            url: "http://127.0.0.1:7333/mcp".into(),
            token: "0123456789abcdef0123456789abcdef".into(),
        };
        let waiting = Waiting::Handoff { id: "e7".into() };
        let (commands, buttons) = panel(
            &PanelInfo {
                approve: ApproveMode::Ask,
                allow_origins: &[],
                timeline: &[],
                waiting: Some(&waiting),
                page_wait: Some(("\"This page\"".into(), 42)),
                attach: Some(&attach),
                copied: false,
            },
            900.0,
            300.0,
            700.0,
        );
        let texts = badge_texts(&commands);
        assert!(texts.iter().any(|t| t.contains("127.0.0.1:7333/mcp")));
        assert!(texts.iter().any(|t| t == "token: 0123…cdef"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains(&attach.token)));
        assert!(texts.contains(&"Agent waits for \"This page\"".to_string()));
        // Copy button first, then the card's answers below it.
        assert_eq!(buttons[0].action, PanelAction::CopySetup);
        assert_eq!(buttons.len(), 3);
        assert!(buttons[1].rect.y > buttons[0].rect.y + buttons[0].rect.height);
    }
}
