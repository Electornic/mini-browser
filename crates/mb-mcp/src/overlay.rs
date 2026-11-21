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
use mb_runtime::render::{CornerRadii, DisplayCommand, TextCommand, measure_text_width};

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
    /// The person paused the agent: the call waiting for them to resume
    /// (`Some(None)` = paused, nothing waiting).
    pub paused: Option<Option<String>>,
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
    /// `mb-mcp replay --window` controls (G5b).
    Replay(ReplayControl),
    /// Pause / resume / approval mode — the person running the agent.
    Control(Control),
}

/// What the person can do to the agent from the ⋮ menu or the panel.
/// Window input only: no MCP tool maps to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    Approve(ApproveMode),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayControl {
    Back,
    Next,
    RunAll,
    Restart,
}

/// How a step in the replay list is marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMark {
    Matched,
    Differs,
    NeedsPerson,
    Next,
    Pending,
}

/// What the replay panel shows.
pub struct ReplayPanelInfo {
    pub file: String,
    /// Steps run, steps in the log, steps that matched.
    pub position: usize,
    pub total: usize,
    pub matched: usize,
    pub steps: Vec<(StepMark, String)>,
    /// The next step, and whether its target is outlined on the page.
    pub next: Option<(String, bool)>,
    /// The last step run: its label and result lines (`- …` / `+ …` for
    /// a diff).
    pub last: Option<(String, Vec<String>)>,
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
pub fn wrap(s: &str, max: usize, limit: usize) -> Vec<String> {
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
        format!("approve: {approve}   (change it in the menu)"),
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
    if let Some(held) = &info.paused {
        let (card_commands, card_buttons, card_height) = paused_card(held.as_deref(), x, width, y);
        commands.extend(card_commands);
        buttons.extend(card_buttons);
        y += card_height + PANEL_LINE / 2.0;
    }
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

/// The replay panel (`mb-mcp replay --window`) at `[x, x + width)`.
pub fn replay_panel(
    info: &ReplayPanelInfo,
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
    let inner = width - PANEL_PAD * 2.0;
    let max_chars = (inner / 7.0) as usize;
    let mut y = PANEL_PAD;
    let line =
        |commands: &mut Vec<DisplayCommand>, y: &mut f32, s: String, color: Color, bold: bool| {
            commands.push(text(clip(&s, max_chars), left, *y, PANEL_FONT, color, bold));
            *y += PANEL_LINE;
        };

    line(
        &mut commands,
        &mut y,
        format!("Replay · {}", info.file),
        INK,
        true,
    );
    line(
        &mut commands,
        &mut y,
        format!(
            "step {} of {} · {} matched",
            info.position, info.total, info.matched
        ),
        MUTED,
        false,
    );
    y += 4.0;

    // Back / Next / Run all / Restart
    let controls = [
        ("Back", ReplayControl::Back, false),
        // Nothing to step when the replay is over or stopped.
        ("Next", ReplayControl::Next, info.next.is_some()),
        ("Run all", ReplayControl::RunAll, false),
        ("Restart", ReplayControl::Restart, false),
    ];
    let gap = 6.0;
    let button_width = (inner - gap * 3.0) / 4.0;
    let mut buttons = Vec::new();
    for (i, (label, control, primary)) in controls.into_iter().enumerate() {
        let rect = Rect {
            x: left + i as f32 * (button_width + gap),
            y,
            width: button_width,
            height: BUTTON_HEIGHT,
        };
        commands.extend(button(rect, label, primary));
        buttons.push(Button {
            rect,
            action: PanelAction::Replay(control),
        });
    }
    y += BUTTON_HEIGHT + PANEL_LINE / 2.0;

    // The result section is drawn last but needs its room reserved.
    let result_lines = info
        .last
        .as_ref()
        .map_or(0, |(_, lines)| lines.len().min(14) + 1);
    let next_lines = if info.next.is_some() { 2 } else { 0 };
    let reserved = (result_lines + next_lines + 2) as f32 * PANEL_LINE;
    let rows = (((height - y - PANEL_PAD - reserved) / PANEL_LINE).max(3.0)) as usize;

    line(&mut commands, &mut y, "Steps".into(), INK, true);
    // A window of rows around the next step.
    let focus = info.position.min(info.steps.len().saturating_sub(1));
    let start = focus
        .saturating_sub(rows / 2)
        .min(info.steps.len().saturating_sub(rows));
    for (mark, label) in info.steps.iter().skip(start).take(rows) {
        let (symbol, color) = match mark {
            StepMark::Matched => ("✓", OK),
            StepMark::Differs => ("✗", FAIL),
            StepMark::NeedsPerson => ("⏸", FAIL),
            StepMark::Next => ("▶", INK),
            StepMark::Pending => ("·", MUTED),
        };
        line(
            &mut commands,
            &mut y,
            format!("{symbol} {label}"),
            color,
            *mark == StepMark::Next,
        );
    }
    y += PANEL_LINE / 2.0;

    if let Some((label, outlined)) = &info.next {
        line(&mut commands, &mut y, format!("Next: {label}"), INK, true);
        if *outlined {
            line(
                &mut commands,
                &mut y,
                "  its target is outlined on the page".into(),
                MUTED,
                false,
            );
        }
    }
    if let Some((label, lines)) = &info.last {
        line(&mut commands, &mut y, format!("── {label} ──"), INK, true);
        let extra = lines.len().saturating_sub(14);
        for result in lines.iter().take(14) {
            let color = if result.trim_start().starts_with("- ") {
                FAIL
            } else if result.trim_start().starts_with("+ ") {
                OK
            } else {
                INK
            };
            line(&mut commands, &mut y, result.clone(), color, false);
        }
        if extra > 0 {
            line(
                &mut commands,
                &mut y,
                format!("  … {extra} more"),
                MUTED,
                false,
            );
        }
    }
    (commands, buttons)
}

// The design's two actors (docs/concept.md, design draft "Agent
// Browser"): blue = the agent, orange = the person.
const AGENT_BLUE: Color = Color {
    r: 45,
    g: 91,
    b: 227,
    a: 255,
};
const AGENT_TINT: Color = Color {
    r: 238,
    g: 242,
    b: 253,
    a: 255,
};
const PERSON_ORANGE: Color = Color {
    r: 217,
    g: 103,
    b: 11,
    a: 255,
};
const PERSON_TINT: Color = Color {
    r: 253,
    g: 241,
    b: 231,
    a: 255,
};
const PERSON_BORDER: Color = Color {
    r: 240,
    g: 199,
    b: 166,
    a: 255,
};
const MENU_INK: Color = Color {
    r: 21,
    g: 23,
    b: 28,
    a: 255,
};
const MENU_MUTED: Color = Color {
    r: 74,
    g: 80,
    b: 96,
    a: 255,
};
const MENU_DISABLED: Color = Color {
    r: 150,
    g: 155,
    b: 165,
    a: 255,
};
const MENU_BORDER: Color = Color {
    r: 213,
    g: 216,
    b: 222,
    a: 255,
};
const MENU_RULE: Color = Color {
    r: 226,
    g: 228,
    b: 232,
    a: 255,
};
const SEGMENT_BG: Color = Color {
    r: 244,
    g: 245,
    b: 247,
    a: 255,
};

pub const MENU_WIDTH: f32 = 300.0;
const MENU_PAD: f32 = 8.0;
const MENU_ROW: f32 = 36.0;
const MENU_FONT: f32 = 14.0;
const MENU_SMALL: f32 = 12.0;
const MENU_HEADER: f32 = 11.0;
const SEGMENT_HEIGHT: f32 = 32.0;

/// What the ⋮ menu shows.
pub struct MenuInfo<'a> {
    pub paused: bool,
    pub approve: ApproveMode,
    pub allow_origins: &'a [String],
    /// The mouse, for the hover wash.
    pub hover: Option<(f32, f32)>,
}

/// The open ⋮ menu: its commands, clickable items, and its whole rect
/// (input over it never reaches the page).
pub struct Menu {
    pub commands: Vec<DisplayCommand>,
    pub buttons: Vec<Button>,
    pub rect: Rect,
}

fn contains(rect: &Rect, (x, y): (f32, f32)) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

// A rounded box with a 1px border.
fn framed(rect: Rect, radius: f32, border: Color, fill: Color) -> [DisplayCommand; 2] {
    [
        DisplayCommand::RoundedRect(border, rect, CornerRadii::uniform(radius)),
        DisplayCommand::RoundedRect(
            fill,
            Rect {
                x: rect.x + 1.0,
                y: rect.y + 1.0,
                width: rect.width - 2.0,
                height: rect.height - 2.0,
            },
            CornerRadii::uniform((radius - 1.0).max(0.0)),
        ),
    ]
}

fn pause_icon(x: f32, y: f32, size: f32, color: Color) -> [DisplayCommand; 2] {
    let bar = |dx: f32| {
        DisplayCommand::RoundedRect(
            color,
            Rect {
                x: x + dx,
                y,
                width: size * 0.3,
                height: size,
            },
            CornerRadii::uniform(1.0),
        )
    };
    [bar(size * 0.05), bar(size * 0.6)]
}

// A right-pointing triangle, one 1px strip per row.
fn play_icon(x: f32, y: f32, size: f32, color: Color) -> Vec<DisplayCommand> {
    let rows = size.round() as usize;
    (0..rows)
        .map(|i| {
            let t = (i as f32 + 0.5) / size;
            let width = (1.0 - (2.0 * t - 1.0).abs()) * size * 0.85;
            DisplayCommand::SolidRect(
                color,
                Rect {
                    x: x + size * 0.1,
                    y: y + i as f32,
                    width,
                    height: 1.0,
                },
            )
        })
        .collect()
}

/// "Agent paused" pill at the right end of the address bar.
pub fn paused_pill(page_width: f32) -> Vec<DisplayCommand> {
    let label = "Agent paused";
    let address = mb_runtime::chrome::address_bar_rect(page_width);
    let width = 10.0 + 10.0 + 6.0 + measure_text_width(label, MENU_SMALL) + 12.0;
    let height = 24.0;
    let rect = Rect {
        x: address.x + address.width - width - 6.0,
        y: address.y + (address.height - height) / 2.0,
        width,
        height,
    };
    let mut commands = framed(rect, height / 2.0, PERSON_BORDER, PERSON_TINT).to_vec();
    commands.extend(pause_icon(rect.x + 10.0, rect.y + 7.0, 10.0, PERSON_ORANGE));
    commands.push(text(
        label,
        rect.x + 26.0,
        rect.y + 6.0,
        MENU_SMALL,
        MENU_INK,
        true,
    ));
    commands
}

/// The ⋮ menu, hanging under the menu button at the right of the page
/// area. Agent section first (pause, approval mode): the controls a
/// watcher needs. Items that aren't built yet are listed greyed out.
pub fn menu(info: &MenuInfo<'_>, page_width: f32) -> Menu {
    let button = mb_runtime::chrome::menu_button_rect(page_width);
    let right = button.x + button.width;
    let x = (right - MENU_WIDTH).max(4.0);
    let width = MENU_WIDTH.min(page_width - 8.0).max(0.0);
    let top = button.y + button.height + 6.0;
    let inner_x = x + MENU_PAD;
    let inner_width = width - 2.0 * MENU_PAD;
    let mut commands = Vec::new();
    let mut buttons = Vec::new();
    let mut y = top + MENU_PAD;

    let header = |commands: &mut Vec<DisplayCommand>, y: &mut f32, label: &str| {
        commands.push(text(
            label,
            inner_x + 10.0,
            *y + 8.0,
            MENU_HEADER,
            MENU_MUTED,
            true,
        ));
        *y += 26.0;
    };
    // One row: hover wash (or the given tint), optional icon slot, label,
    // a value at the right.
    let row = |commands: &mut Vec<DisplayCommand>,
               y: &mut f32,
               label: &str,
               value: &str,
               enabled: bool,
               tint: Option<Color>|
     -> Rect {
        let rect = Rect {
            x: inner_x,
            y: *y,
            width: inner_width,
            height: MENU_ROW,
        };
        let hovered = enabled && info.hover.is_some_and(|p| contains(&rect, p));
        if let Some(fill) = tint.or(hovered.then_some(SEGMENT_BG)) {
            commands.push(DisplayCommand::RoundedRect(
                fill,
                rect,
                CornerRadii::uniform(8.0),
            ));
        }
        let ink = if enabled { MENU_INK } else { MENU_DISABLED };
        commands.push(text(
            label,
            rect.x + 34.0,
            rect.y + (MENU_ROW - MENU_FONT) / 2.0 - 1.0,
            MENU_FONT,
            ink,
            false,
        ));
        if !value.is_empty() {
            let value_width = measure_text_width(value, MENU_SMALL);
            commands.push(text(
                value,
                rect.x + rect.width - 10.0 - value_width,
                rect.y + (MENU_ROW - MENU_SMALL) / 2.0 - 1.0,
                MENU_SMALL,
                if enabled { MENU_MUTED } else { MENU_DISABLED },
                false,
            ));
        }
        *y += MENU_ROW + 2.0;
        rect
    };
    let rule = |commands: &mut Vec<DisplayCommand>, y: &mut f32| {
        *y += 4.0;
        commands.push(DisplayCommand::SolidRect(
            MENU_RULE,
            Rect {
                x: inner_x + 4.0,
                y: *y,
                width: inner_width - 8.0,
                height: 1.0,
            },
        ));
        *y += 5.0;
    };

    header(&mut commands, &mut y, "AGENT");
    // Pause / resume: the watcher's stop button, tinted with the color of
    // whoever it hands the page to.
    let (label, control, tint) = if info.paused {
        ("Resume the agent", Control::Resume, PERSON_TINT)
    } else {
        ("Pause the agent", Control::Pause, AGENT_TINT)
    };
    let rect = row(&mut commands, &mut y, label, "", true, Some(tint));
    if info.paused {
        commands.extend(play_icon(rect.x + 11.0, rect.y + 11.0, 14.0, AGENT_BLUE));
    } else {
        commands.extend(pause_icon(rect.x + 11.0, rect.y + 11.0, 14.0, AGENT_BLUE));
    }
    buttons.push(Button {
        rect,
        action: PanelAction::Control(control),
    });

    // Approval mode: a segmented control, applied from the next action.
    commands.push(text(
        "Before POST or other origins",
        inner_x + 10.0,
        y + 4.0,
        MENU_SMALL,
        MENU_MUTED,
        false,
    ));
    y += 22.0;
    let track = Rect {
        x: inner_x + 10.0,
        y,
        width: inner_width - 20.0,
        height: SEGMENT_HEIGHT,
    };
    commands.push(DisplayCommand::RoundedRect(
        SEGMENT_BG,
        track,
        CornerRadii::uniform(8.0),
    ));
    let segments = [
        ("Ask me", ApproveMode::Ask),
        ("Refuse", ApproveMode::Deny),
        ("Allow", ApproveMode::Auto),
    ];
    let gap = 4.0;
    let segment_width = (track.width - 6.0 - gap * 2.0) / 3.0;
    for (i, (label, mode)) in segments.into_iter().enumerate() {
        let rect = Rect {
            x: track.x + 3.0 + i as f32 * (segment_width + gap),
            y: track.y + 3.0,
            width: segment_width,
            height: SEGMENT_HEIGHT - 6.0,
        };
        let selected = info.approve == mode;
        if selected {
            commands.extend(framed(rect, 6.0, MENU_BORDER, BADGE_FG));
        } else if info.hover.is_some_and(|p| contains(&rect, p)) {
            commands.push(DisplayCommand::RoundedRect(
                MENU_RULE,
                rect,
                CornerRadii::uniform(6.0),
            ));
        }
        let label_width = measure_text_width(label, 13.0);
        commands.push(text(
            label,
            rect.x + (rect.width - label_width).max(0.0) / 2.0,
            rect.y + 6.0,
            13.0,
            if selected { MENU_INK } else { MENU_MUTED },
            selected,
        ));
        buttons.push(Button {
            rect,
            action: PanelAction::Control(Control::Approve(mode)),
        });
    }
    y += SEGMENT_HEIGHT + 8.0;

    // Not built yet (design board "2b · ⋮ menu"): shown so the menu reads
    // as the place they will live, but inert.
    let origins = match info.allow_origins.len() {
        0 => "any".to_string(),
        1 => info.allow_origins[0].clone(),
        n => format!("{n} origins"),
    };
    row(
        &mut commands,
        &mut y,
        "Allowed sites",
        &origins,
        false,
        None,
    );
    rule(&mut commands, &mut y);
    row(
        &mut commands,
        &mut y,
        "Record this session",
        "soon",
        false,
        None,
    );
    row(
        &mut commands,
        &mut y,
        "What the agent sees",
        "soon",
        false,
        None,
    );
    y += MENU_PAD - 2.0;

    let rect = Rect {
        x,
        y: top,
        width,
        height: y - top,
    };
    let mut all = vec![DisplayCommand::BoxShadow(
        mb_runtime::render::ShadowCommand {
            rect: Rect {
                y: rect.y + 4.0,
                ..rect
            },
            blur_radius: 12.0,
            color: Color {
                r: 21,
                g: 23,
                b: 28,
                a: 40,
            },
        },
    )];
    all.extend(framed(rect, 12.0, MENU_BORDER, BADGE_FG));
    all.extend(commands);
    Menu {
        commands: all,
        buttons,
        rect,
    }
}

// The panel card while the agent is paused: the person has the page, and
// the one call waiting for them, with the Resume button.
fn paused_card(
    held: Option<&str>,
    x: f32,
    width: f32,
    top: f32,
) -> (Vec<DisplayCommand>, Vec<Button>, f32) {
    let inner_left = x + PANEL_PAD;
    let inner_width = width - 2.0 * PANEL_PAD;
    let max_chars = (inner_width / 6.5) as usize;
    let mut lines: Vec<(String, Color, bool)> = wrap(
        "The agent's calls wait until you resume — nothing it asks for happens meanwhile. The page is yours.",
        max_chars,
        3,
    )
    .into_iter()
    .map(|l| (l, INK, false))
    .collect();
    match held {
        Some(call) => {
            lines.push(("1 call is waiting for you:".into(), INK, true));
            lines.push((format!("  {}", clip(call, max_chars - 2)), INK, false));
            lines.push(("It runs on resume, on the page as it".into(), MUTED, false));
            lines.push(("is then (ids are checked first).".into(), MUTED, false));
        }
        None => lines.push(("No call is waiting.".into(), MUTED, false)),
    }
    let height = 12.0 + PANEL_LINE * (1 + lines.len()) as f32 + 8.0 + BUTTON_HEIGHT + 12.0;
    let card = Rect {
        x: inner_left - 4.0,
        y: top,
        width: inner_width + 8.0,
        height,
    };
    let mut commands = framed(card, 10.0, PERSON_BORDER, PERSON_TINT).to_vec();
    let mut y = top + 12.0;
    commands.extend(pause_icon(inner_left + 2.0, y + 1.0, 10.0, PERSON_ORANGE));
    commands.push(text(
        "Agent paused · you have the wheel",
        inner_left + 18.0,
        y,
        PANEL_FONT,
        INK,
        true,
    ));
    y += PANEL_LINE;
    for (line, color, bold) in lines {
        commands.push(text(line, inner_left, y, PANEL_FONT, color, bold));
        y += PANEL_LINE;
    }
    y += 8.0;
    let rect = Rect {
        x: inner_left,
        y,
        width: inner_width,
        height: BUTTON_HEIGHT,
    };
    commands.push(DisplayCommand::RoundedRect(
        MENU_INK,
        rect,
        CornerRadii::uniform(6.0),
    ));
    let label = "Resume agent";
    let label_width = measure_text_width(label, PANEL_FONT);
    commands.push(text(
        label,
        rect.x + (rect.width - label_width).max(0.0) / 2.0,
        rect.y + 6.0,
        PANEL_FONT,
        BADGE_FG,
        true,
    ));
    let buttons = vec![Button {
        rect,
        action: PanelAction::Control(Control::Resume),
    }];
    (commands, buttons, height)
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
                paused: None,
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
                paused: None,
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
                paused: None,
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
                paused: None,
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

    #[test]
    fn replay_panel_marks_steps_and_colors_the_diff() {
        let info = ReplayPanelInfo {
            file: "run.jsonl".into(),
            position: 2,
            total: 3,
            matched: 1,
            steps: vec![
                (StepMark::Matched, "1 navigate".into()),
                (StepMark::Differs, "2 click button \"Add\"".into()),
                (StepMark::Pending, "3 back".into()),
            ],
            next: None,
            last: Some((
                "step 2".into(),
                vec![
                    "✗ 2 click".into(),
                    "  - button \"Add\"".into(),
                    "  + button \"Add item\"".into(),
                ],
            )),
        };
        let (commands, buttons) = replay_panel(&info, 900.0, 300.0, 700.0);
        let actions: Vec<PanelAction> = buttons.iter().map(|b| b.action).collect();
        assert_eq!(
            actions,
            [
                ReplayControl::Back,
                ReplayControl::Next,
                ReplayControl::RunAll,
                ReplayControl::Restart
            ]
            .map(PanelAction::Replay)
        );
        let texts = badge_texts(&commands);
        assert!(
            texts.contains(&"step 2 of 3 · 1 matched".to_string()),
            "{texts:?}"
        );
        assert!(texts.contains(&"✓ 1 navigate".to_string()));
        assert!(texts.contains(&"✗ 2 click button \"Add\"".to_string()));
        let color_of = |needle: &str| {
            commands.iter().find_map(|c| match c {
                DisplayCommand::Text(t) if t.text.contains(needle) => Some(t.color),
                _ => None,
            })
        };
        assert_eq!(color_of("- button"), Some(FAIL));
        assert_eq!(color_of("+ button"), Some(OK));
    }

    fn centre(b: &Button) -> (f32, f32) {
        (
            b.rect.x + b.rect.width / 2.0,
            b.rect.y + b.rect.height / 2.0,
        )
    }

    fn controls(menu: &Menu) -> Vec<Control> {
        menu.buttons
            .iter()
            .filter_map(|b| match b.action {
                PanelAction::Control(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_menu_offers_pause_and_the_three_approval_modes() {
        let menu = menu(
            &MenuInfo {
                paused: false,
                approve: ApproveMode::Ask,
                allow_origins: &[],
                hover: None,
            },
            900.0,
        );
        assert_eq!(
            controls(&menu),
            [
                Control::Pause,
                Control::Approve(ApproveMode::Ask),
                Control::Approve(ApproveMode::Deny),
                Control::Approve(ApproveMode::Auto),
            ]
        );
        let texts = badge_texts(&menu.commands);
        for label in ["AGENT", "Pause the agent", "Ask me", "Refuse", "Allow"] {
            assert!(texts.contains(&label.to_string()), "{label}: {texts:?}");
        }
        // Not built yet: listed, but nothing to click.
        assert!(texts.contains(&"Record this session".to_string()));
        assert_eq!(menu.buttons.len(), 4);
        // Hangs under the ⋮ button, right-aligned to it, inside the page.
        let button = mb_runtime::chrome::menu_button_rect(900.0);
        assert!(menu.rect.y > button.y + button.height);
        assert_eq!(menu.rect.x + menu.rect.width, button.x + button.width);
        for b in &menu.buttons {
            let (x, y) = centre(b);
            assert!(contains(&menu.rect, (x, y)), "{b:?}");
        }
        // Each click lands on exactly one item.
        for b in &menu.buttons {
            let (x, y) = centre(b);
            assert_eq!(menu.buttons.iter().filter(|o| o.contains(x, y)).count(), 1);
        }
    }

    #[test]
    fn the_selected_mode_is_bold_and_a_paused_menu_offers_resume() {
        let bold_texts = |m: &Menu| -> Vec<String> {
            m.commands
                .iter()
                .filter_map(|c| match c {
                    DisplayCommand::Text(t) if t.font_weight == 700 => Some(t.text.clone()),
                    _ => None,
                })
                .collect()
        };
        let menu = menu(
            &MenuInfo {
                paused: true,
                approve: ApproveMode::Auto,
                allow_origins: &["http://127.0.0.1:8765".into()],
                hover: None,
            },
            900.0,
        );
        assert_eq!(controls(&menu)[0], Control::Resume);
        let bold = bold_texts(&menu);
        assert!(bold.contains(&"Allow".to_string()), "{bold:?}");
        assert!(!bold.contains(&"Ask me".to_string()), "{bold:?}");
        let texts = badge_texts(&menu.commands);
        assert!(texts.contains(&"Resume the agent".to_string()));
        assert!(texts.contains(&"http://127.0.0.1:8765".to_string()));
    }

    #[test]
    fn a_narrow_page_keeps_the_menu_on_screen() {
        let menu = menu(
            &MenuInfo {
                paused: false,
                approve: ApproveMode::Deny,
                allow_origins: &[],
                hover: None,
            },
            250.0,
        );
        assert!(menu.rect.x >= 0.0 && menu.rect.x + menu.rect.width <= 250.0);
    }

    #[test]
    fn paused_panel_shows_the_held_call_and_resume() {
        let info = |held: Option<Option<String>>| PanelInfo {
            approve: ApproveMode::Ask,
            paused: held,
            allow_origins: &[],
            timeline: &[],
            waiting: None,
            page_wait: None,
            attach: None,
            copied: false,
        };
        let (commands, buttons) = panel(&info(Some(Some("click e26".into()))), 900.0, 300.0, 700.0);
        let texts = badge_texts(&commands);
        assert!(
            texts.iter().any(|t| t.starts_with("Agent paused")),
            "{texts:?}"
        );
        assert!(texts.contains(&"  click e26".to_string()), "{texts:?}");
        assert_eq!(
            buttons.iter().map(|b| b.action).collect::<Vec<_>>(),
            [PanelAction::Control(Control::Resume)]
        );
        let (_, buttons) = panel(&info(None), 900.0, 300.0, 700.0);
        assert!(buttons.is_empty(), "running: no Resume button");
    }

    #[test]
    fn the_paused_pill_sits_inside_the_address_bar() {
        let commands = paused_pill(900.0);
        let address = mb_runtime::chrome::address_bar_rect(900.0);
        let texts = badge_texts(&commands);
        assert_eq!(texts, ["Agent paused"]);
        for command in &commands {
            if let DisplayCommand::RoundedRect(_, r, _) = command {
                assert!(r.x >= address.x && r.x + r.width <= address.x + address.width);
                assert!(r.y >= address.y && r.y + r.height <= address.y + address.height);
            }
        }
    }
}
