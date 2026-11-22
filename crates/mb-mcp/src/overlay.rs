// What the observation window paints on top of the page (docs/concept.md
// G3, principle 1 "에이전트가 보는 것을 사람도 본다"):
//
//   * an id badge on every interactive element — the same `e21` the agent
//     sees in its observation;
//   * an outline around the element the agent's last click / type aimed at;
//   * the agent pill in the chrome, the side panel (policy, question cards,
//     the action timeline), the ⋮ menu and the replay panel.
//
// Styled after the design draft "Agent Browser" (docs/concept.md "디자인
// 적용"): colors come from `mb_runtime::palette` — blue = the agent,
// orange = needs the person, green = done, red = stop.
//
// Pure functions from data to display commands, so they're unit-tested
// without a window.

use std::time::Duration;

use mb_agent::{ApproveMode, ElementObservation};
use mb_runtime::chrome::{CHROME_HEIGHT, accessory_rect, framed_rect};
use mb_runtime::css::Color;
use mb_runtime::dom::NodeId;
use mb_runtime::layout::Rect;
use mb_runtime::palette::{self as p, centered_text_y, text_width};
use mb_runtime::render::{CornerRadii, DisplayCommand, TextCommand};

use crate::http::Attach;
use crate::worker::{Answer, TimelineEntry, Waiting};

/// Width of the agent panel on the right. Below `MIN_WINDOW_FOR_PANEL` the
/// panel is dropped so the page keeps a usable width.
pub const PANEL_WIDTH: usize = 340;
const MIN_WINDOW_FOR_PANEL: usize = PANEL_WIDTH + 400;

const BADGE_FONT: f32 = 11.0;
const BADGE_HEIGHT: f32 = 16.0;
const BADGE_PAD: f32 = 4.0;
const PANEL_PAD: f32 = 20.0;
const CARD_PAD: f32 = 16.0;
const TITLE: f32 = 15.0;
const BODY: f32 = 13.0;
const SMALL: f32 = 12.0;
const BUTTON_HEIGHT: f32 = 44.0;
const ROW: f32 = 24.0;

/// Split a window width into (page width, panel width).
pub fn layout(window_width: usize) -> (usize, usize) {
    if window_width >= MIN_WINDOW_FOR_PANEL {
        (window_width - PANEL_WIDTH, PANEL_WIDTH)
    } else {
        (window_width, 0)
    }
}

fn text_cmd(
    text: impl Into<String>,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
    weight: u16,
    mono: bool,
) -> DisplayCommand {
    DisplayCommand::Text(TextCommand {
        text: text.into(),
        x,
        y,
        color,
        font_size: size,
        wrap_width: None,
        font_family: mono.then(|| p::MONO.to_string()),
        font_weight: weight,
    })
}

fn text(
    text: impl Into<String>,
    x: f32,
    y: f32,
    size: f32,
    color: Color,
    bold: bool,
) -> DisplayCommand {
    let weight = if bold {
        p::WEIGHT_STRONG
    } else {
        p::WEIGHT_REGULAR
    };
    text_cmd(text, x, y, size, color, weight, false)
}

fn rounded(color: Color, rect: Rect, radius: f32) -> DisplayCommand {
    DisplayCommand::RoundedRect(color, rect, CornerRadii::uniform(radius))
}

fn line_height(size: f32) -> f32 {
    (size * 1.5).round()
}

// ---------------------------------------------------------------- icons ---
//
// The renderer has no paths, so icons are stamped: small squares along the
// strokes of the design's 14 px SVG icons.

fn stroke(points: &[(f32, f32)], x: f32, y: f32, width: f32, color: Color) -> Vec<DisplayCommand> {
    let mut commands = Vec::new();
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        let steps = ((x1 - x0).hypot(y1 - y0) * 2.0).ceil().max(1.0) as usize;
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            commands.push(DisplayCommand::SolidRect(
                color,
                Rect {
                    x: x + x0 + (x1 - x0) * t - width / 2.0,
                    y: y + y0 + (y1 - y0) * t - width / 2.0,
                    width,
                    height: width,
                },
            ));
        }
    }
    commands
}

/// Green check (done), 14 × 14 at (x, y).
fn check_icon(x: f32, y: f32, color: Color) -> Vec<DisplayCommand> {
    stroke(&[(2.5, 7.5), (5.5, 10.5), (11.5, 3.5)], x, y, 2.0, color)
}

/// Red cross (refused / failed), 14 × 14.
fn cross_icon(x: f32, y: f32, color: Color) -> Vec<DisplayCommand> {
    let mut commands = stroke(&[(3.0, 3.0), (11.0, 11.0)], x, y, 2.0, color);
    commands.extend(stroke(&[(11.0, 3.0), (3.0, 11.0)], x, y, 2.0, color));
    commands
}

/// A ring (`fill` = None) or a dot of `diameter`, centred in a `box` square.
fn ring_icon(
    x: f32,
    y: f32,
    size: f32,
    diameter: f32,
    color: Color,
    inside: Option<Color>,
) -> Vec<DisplayCommand> {
    let r = Rect {
        x: x + (size - diameter) / 2.0,
        y: y + (size - diameter) / 2.0,
        width: diameter,
        height: diameter,
    };
    let mut commands = vec![rounded(color, r, diameter / 2.0)];
    if let Some(inside) = inside {
        commands.push(rounded(
            inside,
            Rect {
                x: r.x + 2.0,
                y: r.y + 2.0,
                width: diameter - 4.0,
                height: diameter - 4.0,
            },
            diameter / 2.0 - 2.0,
        ));
    }
    commands
}

fn pause_icon(x: f32, y: f32, size: f32, color: Color) -> [DisplayCommand; 2] {
    let bar = |dx: f32| {
        rounded(
            color,
            Rect {
                x: x + dx,
                y,
                width: size * 0.3,
                height: size,
            },
            1.0,
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

// The 32 px soft-orange circle at the head of a card that needs the person,
// holding "!" (approval) or a padlock (password handoff).
fn card_badge(x: f32, y: f32, lock: bool) -> Vec<DisplayCommand> {
    let mut commands = vec![rounded(
        p::PERSON_SOFT,
        Rect {
            x,
            y,
            width: 32.0,
            height: 32.0,
        },
        16.0,
    )];
    let (cx, cy) = (x + 16.0, y + 16.0);
    if lock {
        commands.extend(framed_rect(
            Rect {
                x: cx - 5.0,
                y: cy - 1.0,
                width: 10.0,
                height: 7.0,
            },
            1.5,
            p::INK,
            p::PERSON_SOFT,
        ));
        commands.extend(stroke(
            &[
                (-2.5, -1.0),
                (-2.5, -3.5),
                (-1.5, -5.0),
                (1.5, -5.0),
                (2.5, -3.5),
                (2.5, -1.0),
            ],
            cx,
            cy,
            1.6,
            p::INK,
        ));
    } else {
        commands.push(rounded(
            p::INK,
            Rect {
                x: cx - 1.0,
                y: cy - 6.0,
                width: 2.0,
                height: 8.0,
            },
            1.0,
        ));
        commands.push(rounded(
            p::INK,
            Rect {
                x: cx - 1.0,
                y: cy + 4.0,
                width: 2.0,
                height: 2.0,
            },
            1.0,
        ));
    }
    commands
}

// --------------------------------------------------------- page overlay ---

/// Badges and the target outline, in window coordinates. `scroll_y` is the
/// page's current scroll; `page_bottom` the window height. `note` labels the
/// target ("The agent wants to press this"). Nothing is drawn over the
/// browser chrome.
pub fn page_overlay(
    elements: &[ElementObservation],
    target: Option<NodeId>,
    note: Option<&str>,
    scroll_y: f32,
    page_bottom: f32,
) -> Vec<DisplayCommand> {
    let to_window = |rect: Rect| Rect {
        y: rect.y - scroll_y + CHROME_HEIGHT,
        ..rect
    };
    let visible = |r: &Rect| r.y + r.height > CHROME_HEIGHT && r.y < page_bottom;

    let mut commands = Vec::new();
    for element in elements {
        let r = to_window(element.rect);
        if !visible(&r) {
            continue;
        }
        let label = format!("e{}", element.id.raw());
        let width = text_width(&label, BADGE_FONT, true, 600) + BADGE_PAD * 2.0;
        // Sit just left of the element when the margin has room, so small
        // controls (checkboxes, short links) stay visible; otherwise pin to
        // its top-left corner. Never under the chrome.
        let x = if r.x - width - 3.0 >= 0.0 {
            r.x - width - 3.0
        } else {
            r.x
        };
        let y = r.y.max(CHROME_HEIGHT);
        let badge = Rect {
            x,
            y,
            width,
            height: BADGE_HEIGHT,
        };
        commands.push(rounded(p::AGENT, badge, 4.0));
        commands.push(text_cmd(
            label,
            x + BADGE_PAD,
            centered_text_y(y, BADGE_HEIGHT, BADGE_FONT),
            BADGE_FONT,
            p::PAGE,
            600,
            true,
        ));
    }
    // The target's outline goes over the badges: on a small control
    // (a checkbox at the page margin) the badge would otherwise hide it.
    let mut note_at = None;
    if let Some(target) = target
        && let Some(element) = elements.iter().find(|e| e.id == target)
    {
        let r = to_window(element.rect);
        if visible(&r) {
            // 2 px orange, 2 px clear of the element (board 1 "target").
            // Square corners: the renderer can't stroke a rounded outline.
            commands.extend(outline(
                Rect {
                    x: r.x - 2.0,
                    y: r.y - 2.0,
                    width: r.width + 4.0,
                    height: r.height + 4.0,
                },
                2.0,
                p::PERSON,
            ));
            note_at = Some(r);
        }
    }
    if let (Some(r), Some(note)) = (note_at, note) {
        // A soft-orange label: what the person is asked about.
        let width = text_width(note, SMALL, false, 700) + 16.0;
        let height = 24.0;
        // Right of the target; the panel (painted later) covers any overflow.
        let (x, y) = (r.x + r.width + 10.0, r.y + (r.height - height) / 2.0);
        let chip = Rect {
            x,
            y: y.max(CHROME_HEIGHT),
            width,
            height,
        };
        commands.push(rounded(p::PERSON_SOFT, chip, 6.0));
        commands.push(text(
            note,
            chip.x + 8.0,
            centered_text_y(chip.y, height, SMALL),
            SMALL,
            p::INK,
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

// ------------------------------------------------------------ the pill ---

/// The chrome pill of an agent window (boards 2, 6, 7), painted into the
/// accessory slot `chrome::accessory_rect` reserves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pill {
    /// An agent is driving: "Agent attached · ask".
    Attached(ApproveMode),
    /// `--http` and no agent has called yet.
    NoAgent,
    /// The person paused the agent.
    Paused,
    /// `mb-mcp replay --window`: the page only replays.
    Replaying,
}

impl Pill {
    fn label(self) -> String {
        match self {
            Pill::Attached(mode) => format!("Agent attached · {}", mode_word(mode)),
            Pill::NoAgent => "No agent".into(),
            Pill::Paused => "Agent paused".into(),
            Pill::Replaying => "Read-only · replaying".into(),
        }
    }

    fn bold(self) -> bool {
        !matches!(self, Pill::NoAgent | Pill::Replaying)
    }

    /// Width to reserve in the chrome for this pill.
    pub fn width(self) -> f32 {
        let icon = if self == Pill::Replaying { 0.0 } else { 16.0 };
        let weight = if self.bold() { 700 } else { 400 };
        12.0 + icon + text_width(&self.label(), BODY, false, weight) + 14.0
    }

    /// The pill at its slot right of the address field.
    pub fn commands(self, page_width: f32) -> Vec<DisplayCommand> {
        let rect = accessory_rect(page_width, self.width());
        let (border, fill, ink) = match self {
            Pill::Attached(_) => (p::AGENT_BORDER, p::AGENT_TINT, p::AGENT_INK),
            Pill::NoAgent => (p::LINE, p::PAGE, p::MUTED),
            Pill::Paused => (p::PERSON_BORDER, p::PERSON_TINT, p::INK),
            Pill::Replaying => (p::LINE, p::PANEL, p::MUTED),
        };
        let mut commands = framed_rect(rect, rect.height / 2.0, border, fill).to_vec();
        let icon_x = rect.x + 12.0;
        let mid = rect.y + rect.height / 2.0;
        match self {
            Pill::Attached(_) => {
                commands.extend(ring_icon(icon_x, mid - 4.0, 8.0, 8.0, p::AGENT, None))
            }
            Pill::NoAgent => commands.extend(ring_icon(
                icon_x,
                mid - 4.0,
                8.0,
                8.0,
                p::FAINT,
                Some(p::PAGE),
            )),
            Pill::Paused => commands.extend(pause_icon(icon_x - 1.0, mid - 5.0, 10.0, p::PERSON)),
            Pill::Replaying => {}
        }
        let label_x = if self == Pill::Replaying {
            rect.x + 12.0
        } else {
            icon_x + 16.0
        };
        commands.push(text(
            self.label(),
            label_x,
            centered_text_y(rect.y, rect.height, BODY),
            BODY,
            ink,
            self.bold(),
        ));
        commands
    }
}

fn mode_word(mode: ApproveMode) -> &'static str {
    match mode {
        ApproveMode::Ask => "ask",
        ApproveMode::Deny => "refuse",
        ApproveMode::Auto => "allow",
    }
}

// ------------------------------------------------------------ the panel ---

pub struct PanelInfo<'a> {
    pub approve: ApproveMode,
    /// The person paused the agent: the call waiting for them to resume
    /// (`Some(None)` = paused, nothing waiting).
    pub paused: Option<Option<String>>,
    pub allow_origins: &'a [String],
    pub timeline: &'a [TimelineEntry],
    /// A request waiting for the person (approval / password handoff).
    pub waiting: Option<&'a Waiting>,
    /// How long that request has waited (the card's "Waiting m:ss").
    pub waited: Duration,
    /// The agent's pending `wait_for`: what, time left, whole timeout.
    pub page_wait: Option<(String, Duration, Duration)>,
    /// `--http`: where an agent attaches (G3b).
    pub attach: Option<&'a Attach>,
    /// The setup command was copied (the button says so).
    pub copied: bool,
}

impl PanelInfo<'_> {
    /// No agent has called yet (`--http` before anyone attached). A stdio
    /// agent started us, so it is always there.
    pub fn no_agent_yet(&self) -> bool {
        self.attach.is_some()
            && self.timeline.is_empty()
            && self.waiting.is_none()
            && self.page_wait.is_none()
            && !matches!(self.paused, Some(Some(_)))
    }
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

/// How a button looks (boards 1, 3, 4, 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    /// Filled: green Approve / Done, ink Resume / Next, blue Copy.
    Filled(Color),
    /// White with a line border.
    Plain,
    /// White, red label: Deny.
    Danger,
    /// Greyed: nothing to do right now.
    Disabled,
}

fn button(rect: Rect, label: &str, look: Look) -> Vec<DisplayCommand> {
    let (border, fill, ink, bold) = match look {
        Look::Filled(color) => (color, color, p::PAGE, true),
        Look::Plain => (p::LINE, p::PAGE, p::INK, false),
        Look::Danger => (p::STOP_BORDER, p::PAGE, p::STOP_TEXT, true),
        Look::Disabled => (p::LINE, p::DISABLED_BG, p::FAINT, false),
    };
    let mut commands = framed_rect(rect, 8.0, border, fill).to_vec();
    let size = if rect.height >= 40.0 { 14.0 } else { BODY };
    let weight = if bold { 700 } else { 400 };
    let label_width = text_width(label, size, false, weight);
    commands.push(text(
        label.to_string(),
        rect.x + (rect.width - label_width).max(0.0) / 2.0,
        centered_text_y(rect.y, rect.height, size),
        size,
        ink,
        bold,
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
            // A long URL breaks after a '/' when one falls in the back half
            // of the line, so a path segment isn't split mid-name.
            let chars: Vec<char> = word.chars().collect();
            let take = chars[..max]
                .iter()
                .rposition(|&c| c == '/')
                .filter(|&i| i + 1 > max / 2)
                .map_or(max, |i| i + 1);
            let head: String = chars[..take].iter().collect();
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            lines.push(head);
            word = chars[take..].iter().collect();
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

/// `wrap` by pixel width: greedy on words, measured in the face that
/// paints them. At most `limit` lines.
pub fn wrap_px(s: &str, width: f32, size: f32, mono: bool, limit: usize) -> Vec<String> {
    // Average glyph width in this face, to turn the pixel budget into the
    // char budget `wrap` splits long words by.
    let sample = "abcdefghijklmnopqrstuvwxyz0123456789";
    let per_char = text_width(sample, size, mono, 400) / sample.len() as f32;
    let max_chars = ((width / per_char.max(1.0)) as usize).max(4);
    let mut lines: Vec<String> = Vec::new();
    for line in wrap(s, max_chars, usize::MAX) {
        // `wrap`'s char budget is an average; re-split a line that still
        // measures too wide at its last space.
        let mut rest = line;
        while text_width(&rest, size, mono, 400) > width {
            let Some(cut) = rest.rfind(' ') else { break };
            let tail = rest.split_off(cut);
            lines.push(rest);
            rest = tail.trim_start().to_string();
        }
        lines.push(rest);
    }
    if lines.len() > limit {
        lines.truncate(limit);
        if let Some(last) = lines.last_mut() {
            last.push('…');
        }
    }
    lines
}

// Wrapped paragraph at (x, *y); advances *y.
#[allow(clippy::too_many_arguments)]
fn paragraph(
    commands: &mut Vec<DisplayCommand>,
    y: &mut f32,
    s: &str,
    x: f32,
    width: f32,
    size: f32,
    color: Color,
    limit: usize,
) {
    for line in wrap_px(s, width, size, false, limit) {
        commands.push(text(line, x, *y, size, color, false));
        *y += line_height(size);
    }
}

// A panel-grey box of monospace lines (the action, the held call).
fn mono_box(commands: &mut Vec<DisplayCommand>, y: &mut f32, s: &str, x: f32, width: f32) {
    let lines = wrap_px(s, width - 24.0, BODY, true, 3);
    let height = 16.0 + lines.len() as f32 * line_height(BODY);
    commands.push(rounded(
        p::PANEL,
        Rect {
            x,
            y: *y,
            width,
            height,
        },
        8.0,
    ));
    let mut ly = *y + 8.0;
    for line in lines {
        commands.push(text_cmd(line, x + 12.0, ly, BODY, p::INK, 400, true));
        ly += line_height(BODY);
    }
    *y += height;
}

// A card: white with a border, radius 14. `border_width` 2 for the cards
// that need the person (board 3), 1 otherwise.
fn card_frame(rect: Rect, border: Color, border_width: f32) -> Vec<DisplayCommand> {
    vec![
        rounded(border, rect, 14.0),
        rounded(
            p::PAGE,
            Rect {
                x: rect.x + border_width,
                y: rect.y + border_width,
                width: rect.width - 2.0 * border_width,
                height: rect.height - 2.0 * border_width,
            },
            14.0 - border_width,
        ),
    ]
}

// Lays a card's content out at `top` with `body`, then puts the frame
// behind it once its height is known. Returns the card's bottom.
fn framed_card(
    commands: &mut Vec<DisplayCommand>,
    x: f32,
    width: f32,
    top: f32,
    border: Color,
    border_width: f32,
    body: impl FnOnce(&mut Vec<DisplayCommand>, f32, f32, f32) -> f32,
) -> f32 {
    let mut inner = Vec::new();
    let bottom = body(
        &mut inner,
        x + CARD_PAD,
        width - 2.0 * CARD_PAD,
        top + CARD_PAD,
    ) + CARD_PAD;
    commands.extend(card_frame(
        Rect {
            x,
            y: top,
            width,
            height: bottom - top,
        },
        border,
        border_width,
    ));
    commands.extend(inner);
    bottom
}

fn minutes(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

// The card asking the person something (board 3 approval, board 4
// password handoff). Returns its bottom.
fn question_card(
    commands: &mut Vec<DisplayCommand>,
    buttons: &mut Vec<Button>,
    waiting: &Waiting,
    waited: Duration,
    x: f32,
    width: f32,
    top: f32,
) -> f32 {
    framed_card(
        commands,
        x,
        width,
        top,
        p::PERSON,
        2.0,
        |c, left, inner, mut y| {
            let (lock, title) = match waiting {
                Waiting::Approval { action } if action.starts_with("POST") => {
                    (false, "Submit this form?")
                }
                Waiting::Approval { action } if action.starts_with("Navigation to") => {
                    (false, "Go to another site?")
                }
                Waiting::Approval { .. } => (false, "Allow this action?"),
                Waiting::Handoff { .. } => (true, "Type the password yourself"),
            };
            c.extend(card_badge(left, y, lock));
            c.push(text(
                title,
                left + 42.0,
                centered_text_y(y, 32.0, 16.0),
                16.0,
                p::INK,
                true,
            ));
            y += 32.0 + 12.0;
            match waiting {
                Waiting::Approval { action } => {
                    let shown = action.strip_prefix("Navigation to ").unwrap_or(action);
                    mono_box(c, &mut y, shown, left, inner);
                    y += 10.0;
                    let note = if action.starts_with("POST") {
                        "Nothing has been sent yet."
                    } else {
                        "The page hasn't changed yet."
                    };
                    paragraph(c, &mut y, note, left, inner, BODY, p::MUTED, 2);
                    y += 6.0;
                    let half = (inner - 8.0) / 2.0;
                    let row = [
                        ("Approve", Answer::Approve, Look::Filled(p::DONE), left),
                        ("Deny", Answer::Deny, Look::Danger, left + half + 8.0),
                    ];
                    for (label, answer, look, bx) in row {
                        let rect = Rect {
                            x: bx,
                            y,
                            width: half,
                            height: BUTTON_HEIGHT,
                        };
                        c.extend(button(rect, label, look));
                        buttons.push(Button {
                            rect,
                            action: PanelAction::Answer(answer),
                        });
                    }
                    y += BUTTON_HEIGHT + 8.0;
                    let rect = Rect {
                        x: left,
                        y,
                        width: inner,
                        height: 40.0,
                    };
                    c.extend(button(rect, "I'll do it myself", Look::Plain));
                    buttons.push(Button {
                        rect,
                        action: PanelAction::Answer(Answer::TakeOver),
                    });
                    y += 40.0 + 10.0;
                    c.push(text(
                        format!(
                            "Waiting {} · the agent checks back every 45 s",
                            minutes(waited)
                        ),
                        left,
                        y,
                        SMALL,
                        p::MUTED,
                        false,
                    ));
                    y + line_height(SMALL) - 4.0
                }
                Waiting::Handoff { id } => {
                    paragraph(
                        c,
                        &mut y,
                        &format!(
                            "Field {id} is outlined on the page. Click it, type, then press Done. \
                         The agent never sees what you type — only how many characters."
                        ),
                        left,
                        inner,
                        BODY,
                        p::MUTED,
                        4,
                    );
                    y += 6.0;
                    let half = (inner - 8.0) / 2.0;
                    let row = [
                        ("Done", Answer::HandoffDone, Look::Filled(p::DONE), left),
                        (
                            "Cancel",
                            Answer::HandoffCancel,
                            Look::Plain,
                            left + half + 8.0,
                        ),
                    ];
                    for (label, answer, look, bx) in row {
                        let rect = Rect {
                            x: bx,
                            y,
                            width: half,
                            height: BUTTON_HEIGHT,
                        };
                        c.extend(button(rect, label, look));
                        buttons.push(Button {
                            rect,
                            action: PanelAction::Answer(answer),
                        });
                    }
                    y + BUTTON_HEIGHT
                }
            }
        },
    )
}

// The card while the agent is paused (board 7): the person has the page,
// the one call waiting for them, and Resume.
fn paused_card(
    commands: &mut Vec<DisplayCommand>,
    buttons: &mut Vec<Button>,
    held: Option<&str>,
    x: f32,
    width: f32,
    top: f32,
) -> f32 {
    framed_card(
        commands,
        x,
        width,
        top,
        p::PERSON_BORDER,
        1.0,
        |c, left, inner, mut y| {
            c.extend(pause_icon(left, y + 3.0, 12.0, p::PERSON));
            c.push(text(
                "Agent paused · you have the wheel",
                left + 20.0,
                y,
                14.0,
                p::INK,
                true,
            ));
            y += line_height(14.0) + 4.0;
            paragraph(
                c,
                &mut y,
                "The agent's calls wait until you resume — nothing it asks for happens meanwhile. \
             The page is yours.",
                left,
                inner,
                SMALL,
                p::MUTED,
                3,
            );
            y += 6.0;
            match held {
                Some(call) => {
                    c.push(text(
                        "1 call is waiting for you to resume",
                        left,
                        y,
                        BODY,
                        p::INK,
                        true,
                    ));
                    y += line_height(BODY) + 2.0;
                    mono_box(c, &mut y, call, left, inner);
                    y += 6.0;
                    paragraph(
                        c,
                        &mut y,
                        "On resume it runs against the page as it is then — ids are re-checked first.",
                        left,
                        inner,
                        SMALL,
                        p::MUTED,
                        3,
                    );
                }
                None => {
                    c.push(text("No call is waiting.", left, y, BODY, p::MUTED, false));
                    y += line_height(BODY);
                }
            }
            y += 8.0;
            let rect = Rect {
                x: left,
                y,
                width: inner,
                height: BUTTON_HEIGHT,
            };
            c.extend(button(rect, "Resume agent", Look::Filled(p::INK)));
            buttons.push(Button {
                rect,
                action: PanelAction::Control(Control::Resume),
            });
            y + BUTTON_HEIGHT
        },
    )
}

// `--http` (board 4): where an agent attaches and the copy button.
fn attach_card(
    commands: &mut Vec<DisplayCommand>,
    buttons: &mut Vec<Button>,
    attach: &Attach,
    copied: bool,
    x: f32,
    width: f32,
    top: f32,
) -> f32 {
    framed_card(
        commands,
        x,
        width,
        top,
        p::LINE,
        1.0,
        |c, left, inner, mut y| {
            c.push(text(
                "Let an agent drive this window",
                left,
                y,
                TITLE,
                p::INK,
                true,
            ));
            y += line_height(TITLE) + 4.0;
            for (label, value) in [
                ("Endpoint", attach.url.clone()),
                ("Token", crate::http::masked(&attach.token)),
            ] {
                c.push(text(label, left, y, SMALL, p::MUTED, false));
                y += line_height(SMALL);
                c.push(text_cmd(value, left, y, BODY, p::INK, 400, true));
                y += line_height(BODY) + 4.0;
            }
            y += 4.0;
            let rect = Rect {
                x: left,
                y,
                width: inner,
                height: BUTTON_HEIGHT,
            };
            let label = if copied {
                "Copied — paste it in a terminal"
            } else {
                "Copy setup command"
            };
            c.extend(button(rect, label, Look::Filled(p::AGENT)));
            buttons.push(Button {
                rect,
                action: PanelAction::CopySetup,
            });
            y += BUTTON_HEIGHT + 10.0;
            paragraph(
                c,
                &mut y,
                "Paste it in a terminal. The token stays on your clipboard, never on screen in full. \
             Only this machine can connect.",
                left,
                inner,
                SMALL,
                p::MUTED,
                4,
            );
            y - 4.0
        },
    )
}

// The agent's `wait_for` (board 4): what it waits for, a progress bar.
fn wait_card(
    commands: &mut Vec<DisplayCommand>,
    (what, left_time, total): &(String, Duration, Duration),
    x: f32,
    width: f32,
    top: f32,
) -> f32 {
    framed_card(
        commands,
        x,
        width,
        top,
        p::AGENT_BORDER,
        1.0,
        |c, left, inner, mut y| {
            c.push(text(
                clip(&format!("Waiting for {what}"), 40),
                left,
                y,
                14.0,
                p::INK,
                true,
            ));
            y += line_height(14.0) + 4.0;
            let track = Rect {
                x: left,
                y,
                width: inner,
                height: 6.0,
            };
            c.push(rounded(p::LINE, track, 3.0));
            let done = progress(*left_time, *total);
            if done > 0.0 {
                c.push(rounded(
                    p::AGENT,
                    Rect {
                        width: (inner * done).max(6.0),
                        ..track
                    },
                    3.0,
                ));
            }
            y += 6.0 + 10.0;
            c.push(text(
                format!(
                    "{} s left · you can keep using the page",
                    left_time.as_secs()
                ),
                left,
                y,
                BODY,
                p::MUTED,
                false,
            ));
            y + line_height(BODY) - 4.0
        },
    )
}

/// Share of a wait that has elapsed, 0..=1.
pub fn progress(left: Duration, total: Duration) -> f32 {
    if total.is_zero() {
        return 1.0;
    }
    (1.0 - left.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
}

// Chips under the header: what the agent may do, which sites (board 3).
fn policy_chips(info: &PanelInfo<'_>) -> Vec<String> {
    let mut chips: Vec<String> = match info.approve {
        ApproveMode::Ask => vec![
            "Asks before POST".into(),
            "Asks before other origins".into(),
        ],
        ApproveMode::Deny => vec!["Refuses POST".into(), "Refuses other origins".into()],
        ApproveMode::Auto => vec!["Allows POST and other origins".into()],
    };
    chips.push(match info.allow_origins {
        [] => "Sites: any".into(),
        [one] => format!("Sites: {one}"),
        many => format!("Sites: {} origins", many.len()),
    });
    chips
}

// A row of the action timeline: icon, label, outcome.
fn timeline_row(
    commands: &mut Vec<DisplayCommand>,
    x: f32,
    y: f32,
    width: f32,
    icon: Vec<DisplayCommand>,
    label: &str,
    bold: bool,
) {
    commands.extend(icon);
    let max = (width - 22.0) / 6.8;
    commands.push(text(
        clip(label, max as usize),
        x + 22.0,
        y + 1.0,
        BODY,
        p::INK,
        bold,
    ));
}

/// The agent panel occupying `[x, x + width) × [0, height)`, plus the
/// buttons of any card in it.
pub fn panel(
    info: &PanelInfo<'_>,
    x: f32,
    width: f32,
    height: f32,
) -> (Vec<DisplayCommand>, Vec<Button>) {
    let mut commands = vec![
        DisplayCommand::SolidRect(
            p::PANEL,
            Rect {
                x,
                y: 0.0,
                width,
                height,
            },
        ),
        DisplayCommand::SolidRect(
            p::LINE,
            Rect {
                x,
                y: 0.0,
                width: 1.0,
                height,
            },
        ),
    ];
    let left = x + PANEL_PAD;
    let inner = width - 2.0 * PANEL_PAD;
    let mut buttons = Vec::new();
    let mut y = PANEL_PAD;

    // Header: who is driving, and how they are attached.
    let (title, sub) = if info.no_agent_yet() {
        ("No agent attached", "")
    } else if info.paused.is_some() {
        ("Agent · paused", "")
    } else if info.attach.is_some() {
        ("Agent", "attached over HTTP")
    } else {
        ("Agent", "attached over stdio")
    };
    let dot = if info.no_agent_yet() {
        ring_icon(left, y + 4.0, 8.0, 8.0, p::FAINT, Some(p::PANEL))
    } else if info.paused.is_some() {
        ring_icon(left, y + 4.0, 8.0, 8.0, p::PERSON, None)
    } else {
        ring_icon(left, y + 4.0, 8.0, 8.0, p::AGENT, None)
    };
    commands.extend(dot);
    commands.push(text(title, left + 16.0, y - 1.0, TITLE, p::INK, true));
    if !sub.is_empty() {
        let title_width = text_width(title, TITLE, false, 700);
        commands.push(text(
            sub,
            left + 16.0 + title_width + 8.0,
            y + 2.0,
            SMALL,
            p::MUTED,
            false,
        ));
    }
    y += line_height(TITLE) + 6.0;

    // Policy chips, wrapping.
    let mut cx = left;
    for chip in policy_chips(info) {
        let chip_width = text_width(&chip, SMALL, false, 400) + 18.0;
        if cx > left && cx + chip_width > left + inner {
            cx = left;
            y += 28.0;
        }
        let rect = Rect {
            x: cx,
            y,
            width: chip_width.min(inner),
            height: 22.0,
        };
        commands.extend(framed_rect(rect, 11.0, p::LINE, p::PAGE));
        commands.push(text(
            chip,
            rect.x + 9.0,
            centered_text_y(rect.y, rect.height, SMALL),
            SMALL,
            p::INK,
            false,
        ));
        cx += chip_width + 6.0;
    }
    y += 22.0 + 8.0;
    paragraph(
        &mut commands,
        &mut y,
        "You can use this window too — only the agent's actions are gated. Change the rules in the menu at the top right.",
        left,
        inner,
        SMALL,
        p::MUTED,
        3,
    );
    y += 10.0;

    if let Some(held) = &info.paused {
        y = paused_card(&mut commands, &mut buttons, held.as_deref(), left, inner, y) + 14.0;
    }
    if let Some(waiting) = info.waiting {
        y = question_card(
            &mut commands,
            &mut buttons,
            waiting,
            info.waited,
            left,
            inner,
            y,
        ) + 14.0;
    }
    if let Some(wait) = &info.page_wait {
        y = wait_card(&mut commands, wait, left, inner, y) + 14.0;
    }
    if let Some(attach) = info.attach {
        y = attach_card(
            &mut commands,
            &mut buttons,
            attach,
            info.copied,
            left,
            inner,
            y,
        ) + 14.0;
    }

    commands.push(text("Agent actions", left, y, BODY, p::MUTED, true));
    y += line_height(BODY) + 4.0;
    // Newest at the bottom, as many as fit; the running wait_for last.
    let running = info.page_wait.is_some() as usize;
    let rows = ((height - y - PANEL_PAD) / ROW).max(0.0) as usize;
    if info.timeline.is_empty() && running == 0 {
        commands.push(text(
            "(waiting for the agent)",
            left,
            y,
            BODY,
            p::MUTED,
            false,
        ));
    }
    let start = info
        .timeline
        .len()
        .saturating_sub(rows.saturating_sub(running));
    for entry in &info.timeline[start..] {
        let (icon, label) = match &entry.error {
            None => (check_icon(left, y + 2.0, p::DONE), entry.label.clone()),
            Some(code) => (
                cross_icon(left, y + 2.0, p::STOP),
                format!("{} · {code}", entry.label),
            ),
        };
        timeline_row(&mut commands, left, y, inner, icon, &label, false);
        y += ROW;
    }
    if running == 1 {
        let icon = ring_icon(left, y + 2.0, 14.0, 10.0, p::AGENT, Some(p::PANEL));
        timeline_row(
            &mut commands,
            left,
            y,
            inner,
            icon,
            "wait_for · in progress",
            true,
        );
    }
    (commands, buttons)
}

// ------------------------------------------------------ the replay panel ---

/// The replay panel (`mb-mcp replay --window`, board 5) at `[x, x + width)`.
pub fn replay_panel(
    info: &ReplayPanelInfo,
    x: f32,
    width: f32,
    height: f32,
) -> (Vec<DisplayCommand>, Vec<Button>) {
    let mut commands = vec![
        DisplayCommand::SolidRect(
            p::PANEL,
            Rect {
                x,
                y: 0.0,
                width,
                height,
            },
        ),
        DisplayCommand::SolidRect(
            p::LINE,
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
    let mut y = PANEL_PAD;

    commands.push(text(
        clip(&format!("Replay · {}", info.file), (inner / 7.5) as usize),
        left,
        y,
        TITLE,
        p::INK,
        true,
    ));
    y += line_height(TITLE);
    let stopped = info
        .steps
        .iter()
        .any(|(mark, _)| matches!(mark, StepMark::Differs | StepMark::NeedsPerson));
    let progress_line = if stopped {
        format!(
            "Stopped at step {} of {} · {} matched",
            info.position, info.total, info.matched
        )
    } else {
        format!(
            "step {} of {} · {} matched",
            info.position, info.total, info.matched
        )
    };
    commands.push(text(progress_line, left, y, BODY, p::MUTED, false));
    y += line_height(BODY) + 8.0;

    // One segment per step, colored by outcome.
    commands.extend(progress_segments(&info.steps, left, y, inner));
    y += 8.0 + 12.0;

    // Back / Next / Run all / Restart
    let can_step = info.next.is_some();
    let controls = [
        ("Back", ReplayControl::Back, Look::Plain),
        // Nothing to step when the replay is over or stopped.
        (
            "Next",
            ReplayControl::Next,
            if can_step {
                Look::Filled(p::INK)
            } else {
                Look::Disabled
            },
        ),
        (
            "Run all",
            ReplayControl::RunAll,
            if can_step {
                Look::Plain
            } else {
                Look::Disabled
            },
        ),
        ("Restart", ReplayControl::Restart, Look::Plain),
    ];
    let gap = 6.0;
    let button_width = (inner - gap * 3.0) / 4.0;
    let mut buttons = Vec::new();
    for (i, (label, control, look)) in controls.into_iter().enumerate() {
        let rect = Rect {
            x: left + i as f32 * (button_width + gap),
            y,
            width: button_width,
            height: 40.0,
        };
        commands.extend(button(rect, label, look));
        buttons.push(Button {
            rect,
            action: PanelAction::Replay(control),
        });
    }
    y += 40.0 + 16.0;

    // The result card is drawn last but needs its room reserved.
    let result_lines = info
        .last
        .as_ref()
        .map_or(0, |(_, lines)| lines.len().min(12) + 3);
    let next_lines = usize::from(info.next.as_ref().is_some_and(|(_, outlined)| *outlined));
    let reserved = (result_lines + next_lines) as f32 * line_height(BODY) + 2.0 * CARD_PAD;
    let rows = (((height - y - PANEL_PAD - reserved) / ROW).max(3.0)) as usize;

    // A window of rows around the next step.
    let focus = info.position.min(info.steps.len().saturating_sub(1));
    let start = focus
        .saturating_sub(rows / 2)
        .min(info.steps.len().saturating_sub(rows));
    for (mark, label) in info.steps.iter().skip(start).take(rows) {
        let (icon, ink, bold) = match mark {
            StepMark::Matched => (check_icon(left, y + 3.0, p::DONE), p::INK, false),
            StepMark::Differs => (cross_icon(left, y + 3.0, p::STOP), p::INK, true),
            StepMark::NeedsPerson => (
                ring_icon(left, y + 3.0, 14.0, 10.0, p::PERSON, None),
                p::INK,
                true,
            ),
            StepMark::Next => (
                ring_icon(left, y + 3.0, 14.0, 10.0, p::AGENT, Some(p::PANEL)),
                p::INK,
                true,
            ),
            StepMark::Pending => (Vec::new(), p::FAINT, false),
        };
        // The step that stopped the replay is tinted.
        let tint = match mark {
            StepMark::Differs => Some(p::STOP_TINT),
            StepMark::NeedsPerson => Some(p::PERSON_TINT),
            _ => None,
        };
        if let Some(tint) = tint {
            commands.push(rounded(
                tint,
                Rect {
                    x: left - 6.0,
                    y: y - 1.0,
                    width: inner + 12.0,
                    height: ROW,
                },
                6.0,
            ));
        }
        if *mark == StepMark::Pending {
            commands.push(text("·", left + 5.0, y + 1.0, BODY, p::FAINT, false));
        }
        commands.extend(icon);
        commands.push(text(
            clip(label, ((inner - 22.0) / 6.8) as usize),
            left + 22.0,
            y + 2.0,
            BODY,
            ink,
            bold,
        ));
        y += ROW;
    }
    y += 8.0;

    if let Some((_, true)) = &info.next {
        commands.push(text(
            "The next step's target is outlined on the page.",
            left,
            y,
            SMALL,
            p::MUTED,
            false,
        ));
        y += line_height(BODY);
    }
    if let Some((label, lines)) = &info.last {
        y += 4.0;
        framed_card(
            &mut commands,
            left,
            inner,
            y,
            p::LINE,
            1.0,
            |c, cl, cw, mut cy| {
                c.push(text(capitalize(label), cl, cy, 14.0, p::INK, true));
                cy += line_height(14.0) + 4.0;
                let extra = lines.len().saturating_sub(12);
                for result in lines.iter().take(12) {
                    let trimmed = result.trim_start();
                    let diff = if trimmed.starts_with("- ") {
                        Some((p::STOP_TINT, p::STOP_INK))
                    } else if trimmed.starts_with("+ ") {
                        Some((p::DONE_TINT, p::DONE_INK))
                    } else {
                        None
                    };
                    match diff {
                        Some((tint, ink)) => {
                            c.push(rounded(
                                tint,
                                Rect {
                                    x: cl,
                                    y: cy - 1.0,
                                    width: cw,
                                    height: line_height(SMALL) + 4.0,
                                },
                                4.0,
                            ));
                            c.push(text_cmd(
                                clip(trimmed, (cw / 7.3) as usize),
                                cl + 6.0,
                                cy + 1.0,
                                SMALL,
                                ink,
                                400,
                                true,
                            ));
                            cy += line_height(SMALL) + 6.0;
                        }
                        None => {
                            let color = if trimmed == "matches the recording" {
                                p::DONE_INK
                            } else {
                                p::MUTED
                            };
                            c.push(text(result.clone(), cl, cy, BODY, color, false));
                            cy += line_height(BODY);
                        }
                    }
                }
                if extra > 0 {
                    c.push(text(
                        format!("… {extra} more"),
                        cl,
                        cy,
                        SMALL,
                        p::MUTED,
                        false,
                    ));
                    cy += line_height(SMALL);
                }
                cy - 4.0
            },
        );
    }
    (commands, buttons)
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Board 5's progress strip: one rounded segment per step (green matched,
/// red differs, orange needs the person, grey not run). Long logs share
/// segments so they never get thinner than 3 px.
pub fn progress_segments(
    steps: &[(StepMark, String)],
    x: f32,
    y: f32,
    width: f32,
) -> Vec<DisplayCommand> {
    if steps.is_empty() {
        return Vec::new();
    }
    let gap = 4.0;
    let max_segments = ((width + gap) / (3.0 + gap)).floor().max(1.0) as usize;
    let segments = steps.len().min(max_segments);
    let segment_width = (width - gap * (segments - 1) as f32) / segments as f32;
    (0..segments)
        .map(|i| {
            // The steps this segment stands for; the "worst" mark wins.
            let from = i * steps.len() / segments;
            let to = ((i + 1) * steps.len() / segments).max(from + 1);
            let marks = &steps[from..to];
            let has = |m: StepMark| marks.iter().any(|(mark, _)| *mark == m);
            let color = if has(StepMark::Differs) {
                p::STOP
            } else if has(StepMark::NeedsPerson) {
                p::PERSON
            } else if marks.iter().all(|(mark, _)| *mark == StepMark::Matched) {
                p::DONE
            } else {
                p::FRAME
            };
            rounded(
                color,
                Rect {
                    x: x + i as f32 * (segment_width + gap),
                    y,
                    width: segment_width,
                    height: 8.0,
                },
                4.0,
            )
        })
        .collect()
}

// --------------------------------------------------------- the ⋮ menu ---

pub const MENU_WIDTH: f32 = 330.0;
const MENU_PAD: f32 = 8.0;
const MENU_ROW: f32 = 40.0;
const MENU_FONT: f32 = 14.0;
const MENU_HEADER: f32 = 11.0;
const SEGMENT_HEIGHT: f32 = 38.0;

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

/// The ⋮ menu (board 2b), hanging under the menu button at the right of
/// the page area. Agent section first (pause, approval mode): the controls
/// a watcher needs. Items that aren't built yet are listed greyed out.
pub fn menu(info: &MenuInfo<'_>, page_width: f32) -> Menu {
    let button = mb_runtime::chrome::menu_button_rect(page_width);
    let right = button.x + button.width;
    let x = (right - MENU_WIDTH).max(4.0);
    let width = MENU_WIDTH.min(page_width - 8.0).max(0.0);
    let top = button.y + button.height + 4.0;
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
            p::MUTED,
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
        if let Some(fill) = tint.or(hovered.then_some(p::PANEL)) {
            commands.push(rounded(fill, rect, 8.0));
        }
        let ink = if enabled { p::INK } else { p::DISABLED };
        commands.push(text(
            label,
            rect.x + 34.0,
            centered_text_y(rect.y, MENU_ROW, MENU_FONT),
            MENU_FONT,
            ink,
            false,
        ));
        if !value.is_empty() {
            let value_width = text_width(value, SMALL, false, 400);
            commands.push(text(
                value,
                rect.x + rect.width - 10.0 - value_width,
                centered_text_y(rect.y, MENU_ROW, SMALL),
                SMALL,
                if enabled { p::MUTED } else { p::DISABLED },
                false,
            ));
        }
        *y += MENU_ROW + 2.0;
        rect
    };
    let rule = |commands: &mut Vec<DisplayCommand>, y: &mut f32| {
        *y += 4.0;
        commands.push(DisplayCommand::SolidRect(
            p::LINE,
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
        ("Resume the agent", Control::Resume, p::PERSON_TINT)
    } else {
        ("Pause the agent", Control::Pause, p::AGENT_TINT)
    };
    let rect = row(&mut commands, &mut y, label, "", true, Some(tint));
    if info.paused {
        commands.extend(play_icon(rect.x + 11.0, rect.y + 13.0, 14.0, p::AGENT));
    } else {
        commands.extend(pause_icon(rect.x + 11.0, rect.y + 13.0, 14.0, p::AGENT));
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
        BODY,
        p::MUTED,
        false,
    ));
    y += 24.0;
    let track = Rect {
        x: inner_x + 10.0,
        y,
        width: inner_width - 20.0,
        height: SEGMENT_HEIGHT,
    };
    commands.push(rounded(p::PANEL, track, 8.0));
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
            commands.push(rounded(p::PAGE, rect, 6.0));
        } else if info.hover.is_some_and(|p| contains(&rect, p)) {
            commands.push(rounded(p::LINE, rect, 6.0));
        }
        let weight = if selected { 700 } else { 400 };
        let label_width = text_width(label, BODY, false, weight);
        commands.push(text(
            label,
            rect.x + (rect.width - label_width).max(0.0) / 2.0,
            centered_text_y(rect.y, rect.height, BODY),
            BODY,
            if selected { p::INK } else { p::MUTED },
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
    // No shadow (board 1 "drawable by our engine"): a frame-grey border
    // sets the menu off the page.
    let mut all = framed_rect(rect, 12.0, p::FRAME, p::PAGE).to_vec();
    all.extend(commands);
    Menu {
        commands: all,
        buttons,
        rect,
    }
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
    use mb_runtime::chrome::accessory_rect;

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
        let top = page_overlay(&elements, None, None, 0.0, 700.0);
        assert_eq!(badge_texts(&top), vec!["e5"]);
        let scrolled = page_overlay(&elements, None, None, 600.0, 700.0);
        assert_eq!(badge_texts(&scrolled), vec!["e9"]);
        // e9 at document y 900, scrolled 600 → window y 300 + chrome; the
        // element starts at x 10, too close to the edge for a left badge.
        let DisplayCommand::RoundedRect(_, badge, _) = &scrolled[0] else {
            panic!("badge background first");
        };
        assert_eq!(badge.y, 300.0 + CHROME_HEIGHT);
        assert_eq!(badge.x, 10.0);
    }

    #[test]
    fn badges_move_left_of_the_element_when_there_is_room() {
        let mut far = element(5, 20.0);
        far.rect.x = 200.0;
        let commands = page_overlay(&[far], None, None, 0.0, 700.0);
        let DisplayCommand::RoundedRect(_, badge, _) = &commands[0] else {
            panic!("badge background first");
        };
        assert!(
            badge.x + badge.width < 200.0,
            "badge clear of the element: {badge:?}"
        );
    }

    #[test]
    fn target_gets_an_outline() {
        let elements = [element(5, 20.0)];
        let plain = page_overlay(&elements, None, None, 0.0, 700.0);
        let targeted = page_overlay(&elements, Some(NodeId::from_raw(5)), None, 0.0, 700.0);
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
                waited: Duration::ZERO,
                page_wait: None,
                attach: None,
                copied: false,
            },
            900.0,
            300.0,
            700.0,
        );
        let texts = badge_texts(&commands);
        assert!(texts.contains(&"click e24".to_string()), "{texts:?}");
        assert!(
            texts.contains(&"click e30 · approval_denied".to_string()),
            "{texts:?}"
        );
        // Deny mode reads as chips under the header.
        assert!(texts.contains(&"Refuses POST".to_string()), "{texts:?}");
        assert!(texts.contains(&"Sites: any".to_string()), "{texts:?}");
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
                waited: Duration::from_secs(38),
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
            texts.contains(&"Submit this form?".to_string()),
            "{texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.starts_with("Waiting 0:38")),
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
                waited: Duration::from_secs(38),
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
        // URLs break after a '/', not inside a path segment.
        assert_eq!(
            wrap("http://localhost:8790/about.html", 31, 3),
            ["http://localhost:8790/", "about.html"]
        );
    }

    #[test]
    fn narrow_windows_drop_the_panel() {
        assert_eq!(layout(1280), (940, 340));
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
                waited: Duration::ZERO,
                page_wait: Some((
                    "\"This page\"".into(),
                    Duration::from_secs(42),
                    Duration::from_secs(60),
                )),
                attach: Some(&attach),
                copied: false,
            },
            900.0,
            300.0,
            700.0,
        );
        let texts = badge_texts(&commands);
        assert!(texts.iter().any(|t| t.contains("127.0.0.1:7333/mcp")));
        assert!(texts.iter().any(|t| t == "0123…cdef"), "{texts:?}");
        assert!(!texts.iter().any(|t| t.contains(&attach.token)));
        assert!(texts.contains(&"Waiting for \"This page\"".to_string()));
        assert!(texts.contains(&"42 s left · you can keep using the page".to_string()));
        // The question comes first (it is what the person must act on),
        // then the attach card's copy button below it.
        assert_eq!(buttons.len(), 3);
        assert_eq!(buttons[2].action, PanelAction::CopySetup);
        assert!(buttons[2].rect.y > buttons[0].rect.y + buttons[0].rect.height);
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
            texts.contains(&"Stopped at step 2 of 3 · 1 matched".to_string()),
            "{texts:?}"
        );
        assert!(texts.contains(&"1 navigate".to_string()));
        assert!(texts.contains(&"2 click button \"Add\"".to_string()));
        let color_of = |needle: &str| {
            commands.iter().find_map(|c| match c {
                DisplayCommand::Text(t) if t.text.contains(needle) => Some(t.color),
                _ => None,
            })
        };
        assert_eq!(color_of("- button"), Some(p::STOP_INK));
        assert_eq!(color_of("+ button"), Some(p::DONE_INK));
        // Nothing to step: Next and Run all are greyed out.
        let next_label = commands.iter().find_map(|c| match c {
            DisplayCommand::Text(t) if t.text == "Next" => Some(t.color),
            _ => None,
        });
        assert_eq!(next_label, Some(p::FAINT));
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
            waited: Duration::ZERO,
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
        assert!(texts.contains(&"click e26".to_string()), "{texts:?}");
        assert_eq!(
            buttons.iter().map(|b| b.action).collect::<Vec<_>>(),
            [PanelAction::Control(Control::Resume)]
        );
        let (_, buttons) = panel(&info(None), 900.0, 300.0, 700.0);
        assert!(buttons.is_empty(), "running: no Resume button");
    }

    #[test]
    fn pills_fill_the_chrome_accessory_slot() {
        for pill in [
            Pill::Attached(ApproveMode::Ask),
            Pill::NoAgent,
            Pill::Paused,
            Pill::Replaying,
        ] {
            let slot = accessory_rect(900.0, pill.width());
            let commands = pill.commands(900.0);
            let DisplayCommand::RoundedRect(_, outer, _) = &commands[0] else {
                panic!("pill frame first");
            };
            assert_eq!(*outer, slot, "{pill:?}");
            // Everything stays inside the slot, which ends before ⋮.
            for command in &commands {
                match command {
                    DisplayCommand::RoundedRect(_, r, _) | DisplayCommand::SolidRect(_, r) => {
                        assert!(r.x >= slot.x && r.x + r.width <= slot.x + slot.width + 0.01);
                    }
                    _ => {}
                }
            }
            assert!(slot.x + slot.width < mb_runtime::chrome::menu_button_rect(900.0).x);
        }
        let texts = badge_texts(&Pill::Attached(ApproveMode::Deny).commands(900.0));
        assert_eq!(texts, ["Agent attached · refuse"]);
        assert_eq!(badge_texts(&Pill::Paused.commands(900.0)), ["Agent paused"]);
    }

    #[test]
    fn a_waiting_target_gets_a_note_beside_its_outline() {
        let elements = [element(5, 20.0)];
        let commands = page_overlay(
            &elements,
            Some(NodeId::from_raw(5)),
            Some("The agent wants to press this"),
            0.0,
            700.0,
        );
        let texts = badge_texts(&commands);
        assert!(texts.contains(&"The agent wants to press this".to_string()));
        // The note starts right of the element (x 10, width 80) and its outline.
        let note_box = commands.iter().find_map(|c| match c {
            DisplayCommand::RoundedRect(color, r, _) if *color == p::PERSON_SOFT => Some(*r),
            _ => None,
        });
        assert!(
            note_box.is_some_and(|r| r.x >= 10.0 + 80.0 + 4.0),
            "{note_box:?}"
        );
    }

    #[test]
    fn progress_segments_color_each_step_and_never_get_too_thin() {
        let steps: Vec<(StepMark, String)> =
            [StepMark::Matched, StepMark::Differs, StepMark::Pending]
                .into_iter()
                .map(|m| (m, String::new()))
                .collect();
        let segments = progress_segments(&steps, 0.0, 0.0, 300.0);
        let colors: Vec<Color> = segments
            .iter()
            .map(|c| match c {
                DisplayCommand::RoundedRect(color, _, _) => *color,
                _ => panic!("segments are rounded rects"),
            })
            .collect();
        assert_eq!(colors, [p::DONE, p::STOP, p::FRAME]);
        // 300 steps in 300 px: merged so each is ≥ 3 px, and a failure
        // anywhere in a merged segment still shows red.
        let mut many: Vec<(StepMark, String)> = (0..300)
            .map(|_| (StepMark::Matched, String::new()))
            .collect();
        many[150].0 = StepMark::Differs;
        let segments = progress_segments(&many, 0.0, 0.0, 300.0);
        assert!(segments.len() < 300);
        let mut reds = 0;
        for c in &segments {
            let DisplayCommand::RoundedRect(color, r, _) = c else {
                panic!()
            };
            assert!(r.width >= 3.0, "{r:?}");
            reds += usize::from(*color == p::STOP);
        }
        assert_eq!(reds, 1);
    }

    #[test]
    fn wait_progress_is_the_elapsed_share() {
        let s = Duration::from_secs;
        assert_eq!(progress(s(30), s(60)), 0.5);
        assert_eq!(progress(s(60), s(60)), 0.0);
        assert_eq!(progress(s(0), s(0)), 1.0);
    }

    #[test]
    fn wrap_px_keeps_lines_inside_the_width() {
        let lines = wrap_px(
            "The agent's calls wait until you resume — nothing it asks for happens meanwhile.",
            120.0,
            12.0,
            false,
            10,
        );
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(
                text_width(line, 12.0, false, 400) <= 120.0 || !line.contains(' '),
                "{line}"
            );
        }
    }

    #[test]
    fn http_panel_says_no_agent_until_the_first_call() {
        let attach = Attach {
            url: "http://127.0.0.1:7333/mcp".into(),
            token: "0123456789abcdef0123456789abcdef".into(),
        };
        let timeline = [TimelineEntry {
            label: "observe".into(),
            error: None,
        }];
        let info = |timeline: &'static [TimelineEntry]| PanelInfo {
            approve: ApproveMode::Ask,
            paused: None,
            allow_origins: &[],
            timeline,
            waiting: None,
            waited: Duration::ZERO,
            page_wait: None,
            attach: Some(&attach),
            copied: false,
        };
        let before = badge_texts(&panel(&info(&[]), 900.0, 340.0, 700.0).0);
        assert!(
            before.contains(&"No agent attached".to_string()),
            "{before:?}"
        );
        let leaked: &'static [TimelineEntry] = Box::leak(Box::new(timeline));
        let after = badge_texts(&panel(&info(leaked), 900.0, 340.0, 700.0).0);
        assert!(
            after.contains(&"attached over HTTP".to_string()),
            "{after:?}"
        );
    }
}
