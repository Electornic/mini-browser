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
use crate::worker::{Answer, Refusal, TimelineEntry, Waiting};

/// Width of the agent panel on the right. Below `MIN_WINDOW_FOR_PANEL` the
/// panel is dropped so the page keeps a usable width.
pub const PANEL_WIDTH: usize = 340;
const MIN_WINDOW_FOR_PANEL: usize = PANEL_WIDTH + 400;

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

// An eye (board 2b "What the agent sees"), 16 × 16.
fn eye_icon(x: f32, y: f32, color: Color) -> Vec<DisplayCommand> {
    let mut commands = stroke(
        &[
            (1.5, 8.0),
            (4.0, 5.0),
            (8.0, 3.5),
            (12.0, 5.0),
            (14.5, 8.0),
            (12.0, 11.0),
            (8.0, 12.5),
            (4.0, 11.0),
            (1.5, 8.0),
        ],
        x,
        y,
        1.5,
        color,
    );
    commands.extend(ring_icon(x + 5.0, y + 5.0, 6.0, 5.0, color, None));
    commands
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

// ------------------------------------------------------ badge placement ---
//
// Where an id badge goes (docs/concept.md "배지 자리"). Since inline
// elements share line boxes there is no left margin to park badges in, so
// each badge tries a few spots around its element: full size above / left /
// below / right, then the same four compact.
//
// * Hard rules — a spot is dropped if it leaves the page area (never over
//   the chrome or the panel), touches another element's box (a badge must
//   never read as its neighbour's) or another badge.
// * Of the spots left, the one hiding the least wins: overlap area with the
//   badge's own element (past the allowed tuck) ×4 and page text ×1; the
//   first spot in the order above wins ties, so one that hides nothing is
//   taken as soon as it comes up.
// * No spot left: compact, inside the element's own on-page box (a corner
//   that's free of other badges). An element with no on-page area, or no
//   free corner, gets no badge rather than one somewhere unrelated.
//
// Badges are placed greedily in document order. The work is bounded: at
// most `MAX_BADGES` badges, and every overlap test only looks at rects in
// the same `INDEX_ROW` px band of the page (`RowIndex`).

/// The page's part of the window: below the chrome, left of the panel.
pub fn page_area(page_width: usize, window_height: usize) -> Rect {
    Rect {
        x: 0.0,
        y: CHROME_HEIGHT,
        width: page_width as f32,
        height: (window_height as f32 - CHROME_HEIGHT).max(0.0),
    }
}

/// Size and type of one badge variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BadgeStyle {
    pub font: f32,
    pub height: f32,
    pub pad: f32,
    pub radius: f32,
}

impl BadgeStyle {
    /// Board 1: mono 11 semibold, 16 high, r4.
    pub const FULL: BadgeStyle = BadgeStyle {
        font: 11.0,
        height: 16.0,
        pad: 4.0,
        radius: 4.0,
    };
    /// For tight spots (a checkbox at the body margin, stacked list rows).
    pub const COMPACT: BadgeStyle = BadgeStyle {
        font: 9.0,
        height: 12.0,
        pad: 2.0,
        radius: 3.0,
    };

    /// (width, height) of a badge showing `label`.
    fn size(self, label: &str) -> (f32, f32) {
        (
            text_width(label, self.font, true, 600) + self.pad * 2.0,
            self.height,
        )
    }
}

/// One element's badge to place: its box in window coordinates, whether it
/// carries the orange target outline, and the badge's (width, height) in
/// both styles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BadgeRequest {
    pub element: Rect,
    pub outlined: bool,
    pub full: (f32, f32),
    pub compact: (f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedBadge {
    pub rect: Rect,
    pub compact: bool,
}

/// Space between a badge beside an element and the element.
const BADGE_GAP: f32 = 2.0;
/// How far a badge above / below may tuck into the element's box: a line
/// box has leading above and below the glyphs, so 2 px hides no ink.
const BADGE_TUCK: f32 = 2.0;
/// The target outline's outer edge, past the element's box: 2 px clear + a
/// 2 px line. The outline is painted over the badges, so the target's badge
/// keeps outside it when it can.
const OUTLINE_REACH: f32 = 4.0;
const HIDES_OWN: f32 = 4.0;
const HIDES_TEXT: f32 = 1.0;
/// Most badges drawn in one frame (document order; the target always gets
/// one). A screenful rarely holds more, and past it badges are noise anyway.
pub const MAX_BADGES: usize = 300;

/// Badge spots for `requests` (in order) inside `page`; `None` = no badge
/// (see the section comment). `ink` is where page text is.
pub fn place_badges(
    requests: &[BadgeRequest],
    ink: &[Rect],
    page: Rect,
) -> Vec<Option<PlacedBadge>> {
    let mut elements = RowIndex::new(page);
    for (i, r) in requests.iter().enumerate() {
        elements.insert(i, &r.element);
    }
    let mut text = RowIndex::new(page);
    for (i, t) in ink.iter().enumerate() {
        text.insert(i, t);
    }
    let mut badges = RowIndex::new(page);
    let mut placed_rects: Vec<Rect> = Vec::new();
    let mut near = Vec::new();

    let mut out = Vec::with_capacity(requests.len());
    for (i, request) in requests.iter().enumerate() {
        let own = request.element;
        // What the badge should not cover of its own element: the box past
        // the allowed tuck, or the whole outline around the target.
        let keep_clear = if request.outlined {
            inset(own, -OUTLINE_REACH)
        } else {
            inset(own, BADGE_TUCK)
        };
        let mut spots = Vec::new();
        if request.outlined {
            spots.extend(badge_spots(
                inset(own, -OUTLINE_REACH),
                0.0,
                request.full,
                request.compact,
            ));
        }
        spots.extend(badge_spots(own, BADGE_TUCK, request.full, request.compact));

        let mut best: Option<(f32, PlacedBadge)> = None;
        for spot in spots {
            let r = spot.rect;
            if !within(&page, &r)
                || elements.hits(&r, &mut near, |j| {
                    j != i && overlap(&r, &requests[j].element) > 0.0
                })
                || badges.hits(&r, &mut near, |j| {
                    overlap(&r, &inset(placed_rects[j], -1.0)) > 0.0
                })
            {
                continue;
            }
            let mut cost = HIDES_OWN * overlap(&r, &keep_clear);
            text.for_near(&r, &mut near, |j| cost += HIDES_TEXT * overlap(&r, &ink[j]));
            if best.is_none_or(|(b, _)| cost < b) {
                best = Some((cost, spot));
            }
            if cost == 0.0 {
                break;
            }
        }
        let badge = best.map(|(_, b)| b).or_else(|| {
            // Off other badges always; off other elements too if a corner
            // allows (a box narrower than the badge can't keep it all in).
            let corners: Vec<Rect> = inside_own_box(own, request.compact, page)
                .into_iter()
                .filter(|r| {
                    !badges.hits(r, &mut near, |j| {
                        overlap(r, &inset(placed_rects[j], -1.0)) > 0.0
                    })
                })
                .collect();
            let clear = corners.iter().find(|r| {
                !elements.hits(r, &mut near, |j| {
                    j != i && overlap(r, &requests[j].element) > 0.0
                })
            });
            clear.or(corners.first()).map(|&rect| PlacedBadge {
                rect,
                compact: true,
            })
        });
        if let Some(b) = badge {
            badges.insert(placed_rects.len(), &b.rect);
            placed_rects.push(b.rect);
        }
        out.push(badge);
    }
    out
}

/// Candidate spots around `r` in preference order (see the section
/// comment); above / below tuck `tuck` px into `r`.
fn badge_spots(r: Rect, tuck: f32, full: (f32, f32), compact: (f32, f32)) -> Vec<PlacedBadge> {
    let around = |(w, h): (f32, f32)| {
        // Beside: level with a one-line element, at the top of a tall one.
        let beside_y = if r.height <= h * 2.0 {
            r.y + (r.height - h) / 2.0
        } else {
            r.y
        };
        [
            (r.x, r.y - h + tuck),
            (r.x - w - BADGE_GAP, beside_y),
            (r.x, r.y + r.height - tuck),
            (r.x + r.width + BADGE_GAP, beside_y),
        ]
        .map(|(x, y)| Rect {
            x,
            y,
            width: w,
            height: h,
        })
    };
    let mut spots: Vec<PlacedBadge> = around(full)
        .into_iter()
        .map(|rect| PlacedBadge {
            rect,
            compact: false,
        })
        .collect();
    spots.extend(around(compact).into_iter().map(|rect| PlacedBadge {
        rect,
        compact: true,
    }));
    spots
}

/// Last resort: a `(w, h)` badge in a corner of the part of `r` that's on
/// the page (top-left first), pulled onto the page if the box is smaller
/// than the badge — so it always touches `r`. Empty when `r` is off-page.
fn inside_own_box(r: Rect, (w, h): (f32, f32), page: Rect) -> Vec<Rect> {
    let Some(clip) = intersection(&r, &page) else {
        return Vec::new();
    };
    let (left, right) = (clip.x, clip.x + clip.width - w);
    let (top, bottom) = (clip.y, clip.y + clip.height - h);
    [(left, top), (right, top), (left, bottom), (right, bottom)]
        .into_iter()
        .map(|(x, y)| Rect {
            x: x.min(page.x + page.width - w).max(page.x),
            y: y.min(page.y + page.height - h).max(page.y),
            width: w,
            height: h,
        })
        .collect()
}

fn intersection(a: &Rect, b: &Rect) -> Option<Rect> {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    let right = (a.x + a.width).min(b.x + b.width);
    let bottom = (a.y + a.height).min(b.y + b.height);
    (right > x && bottom > y).then_some(Rect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    })
}

/// Rect indices bucketed into `INDEX_ROW` px bands of the page, so an overlap
/// test only looks at what's in the same bands — a page with thousands of
/// elements or text runs costs per badge what its neighbourhood costs.
/// Rects off the page are not kept (spots never leave the page).
struct RowIndex {
    top: f32,
    rows: Vec<Vec<usize>>,
}

const INDEX_ROW: f32 = 64.0;

impl RowIndex {
    fn new(page: Rect) -> Self {
        let count = (page.height / INDEX_ROW).ceil().max(0.0) as usize + 1;
        RowIndex {
            top: page.y,
            rows: vec![Vec::new(); count],
        }
    }

    /// The bands `r` spans, clamped to the page; empty when it's off-page.
    fn rows_of(&self, r: &Rect) -> std::ops::Range<usize> {
        let last = self.rows.len() as f32 - 1.0;
        let first = ((r.y - self.top) / INDEX_ROW).floor();
        let end = ((r.y + r.height - self.top) / INDEX_ROW).floor();
        if end < 0.0 || first > last || r.height < 0.0 {
            return 0..0;
        }
        first.max(0.0) as usize..end.min(last) as usize + 1
    }

    fn insert(&mut self, i: usize, r: &Rect) {
        for row in self.rows_of(r) {
            self.rows[row].push(i);
        }
    }

    /// Calls `f` once for each rect sharing a band with `r`.
    fn for_near(&self, r: &Rect, scratch: &mut Vec<usize>, mut f: impl FnMut(usize)) {
        scratch.clear();
        for row in self.rows_of(r) {
            scratch.extend_from_slice(&self.rows[row]);
        }
        scratch.sort_unstable();
        scratch.dedup();
        scratch.iter().for_each(|&i| f(i));
    }

    /// Whether `test` holds for any rect sharing a band with `r`.
    fn hits(&self, r: &Rect, scratch: &mut Vec<usize>, test: impl Fn(usize) -> bool) -> bool {
        let mut hit = false;
        self.for_near(r, scratch, |i| hit = hit || test(i));
        hit
    }
}

fn overlap(a: &Rect, b: &Rect) -> f32 {
    let w = (a.x + a.width).min(b.x + b.width) - a.x.max(b.x);
    let h = (a.y + a.height).min(b.y + b.height) - a.y.max(b.y);
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

fn within(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}

/// `r` shrunk by `by` on every side (grown when negative).
fn inset(r: Rect, by: f32) -> Rect {
    Rect {
        x: r.x + by,
        y: r.y + by,
        width: (r.width - 2.0 * by).max(0.0),
        height: (r.height - 2.0 * by).max(0.0),
    }
}

/// Where `commands` (a frame from `human_frame`, window coordinates) paint
/// page text inside `page`: one box per text run, cut to the glyphs' ink
/// band (a line box has leading above and below), so a badge may sit in
/// the gap between two lines.
pub fn text_obstacles(commands: &[DisplayCommand], page: Rect) -> Vec<Rect> {
    commands
        .iter()
        .filter_map(|c| match c {
            DisplayCommand::Text(t) => Some(t),
            _ => None,
        })
        .filter(|t| !t.text.trim().is_empty())
        // Measuring is the cost (a frame carries every run of the document,
        // most off screen): drop runs that can't reach the page first.
        .filter(|t| could_reach(t, &page))
        .filter_map(|t| {
            let (width, lines) = mb_runtime::render::measure_text_wrap_with_family(
                &t.text,
                t.font_size,
                t.wrap_width,
                t.font_family.as_deref(),
                t.font_weight,
            );
            let width = t.wrap_width.map_or(width, |w| width.min(w.max(0.0)));
            let line = t.font_size * 1.2;
            let r = Rect {
                x: t.x,
                y: t.y + t.font_size * 0.15,
                width,
                height: line * lines as f32 - t.font_size * 0.25,
            };
            (overlap(&r, &page) > 0.0).then_some(r)
        })
        .collect()
}

/// Whether text run `t` might paint inside `page`, without measuring it:
/// it starts above the page's bottom edge and, if it starts above the top,
/// its tallest possible wrap (no glyph wider than 1.5 em, or one character
/// per line when the wrap width is narrower than that) reaches the page.
fn could_reach(t: &TextCommand, page: &Rect) -> bool {
    if t.y >= page.y + page.height || t.x >= page.x + page.width {
        return false;
    }
    if t.y >= page.y {
        return true;
    }
    let chars = t.text.chars().count() as f32;
    let widest = 1.5 * t.font_size;
    let lines = match t.wrap_width {
        Some(w) if w >= widest => (chars * widest / w).ceil() + 1.0,
        Some(_) => chars,
        None => 1.0,
    };
    t.y + lines.min(chars).max(1.0) * t.font_size * 1.2 > page.y
}

// --------------------------------------------------------- page overlay ---

/// Badges and the target outline, in window coordinates. `scroll_y` is the
/// page's current scroll; `page` the page's area of the window (below the
/// chrome, left of the panel); `ink` where the page paints text
/// ([`text_obstacles`]), so badges can stay off it. `note` labels the target
/// ("The agent wants to press this"). Nothing is drawn over the browser
/// chrome.
pub fn page_overlay(
    elements: &[ElementObservation],
    target: Option<NodeId>,
    note: Option<&str>,
    scroll_y: f32,
    page: Rect,
    ink: &[Rect],
) -> Vec<DisplayCommand> {
    let to_window = |rect: Rect| Rect {
        y: rect.y - scroll_y + CHROME_HEIGHT,
        ..rect
    };
    let visible = |r: &Rect| intersection(r, &page).is_some();

    // Only what's on screen, and at most MAX_BADGES of it (the target
    // always): the placement and the label measuring are per badge.
    let shown: Vec<(String, Rect, bool)> = elements
        .iter()
        .map(|e| (e, to_window(e.rect)))
        .filter(|(_, r)| visible(r))
        .enumerate()
        .filter(|(n, (e, _))| *n < MAX_BADGES || Some(e.id) == target)
        .map(|(_, (e, r))| (format!("e{}", e.id.raw()), r, Some(e.id) == target))
        .collect();
    let wanted: Vec<BadgeRequest> = shown
        .iter()
        .map(|(label, r, outlined)| BadgeRequest {
            element: *r,
            outlined: *outlined,
            full: BadgeStyle::FULL.size(label),
            compact: BadgeStyle::COMPACT.size(label),
        })
        .collect();
    let mut commands = Vec::new();
    let placed = place_badges(&wanted, ink, page);
    for ((label, _, _), placed) in shown.iter().zip(placed) {
        let Some(placed) = placed else { continue };
        let style = if placed.compact {
            BadgeStyle::COMPACT
        } else {
            BadgeStyle::FULL
        };
        commands.push(rounded(p::AGENT, placed.rect, style.radius));
        commands.push(text_cmd(
            label.clone(),
            placed.rect.x + style.pad,
            centered_text_y(placed.rect.y, placed.rect.height, style.font),
            style.font,
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
    /// The session log is being written (board 7): "REC 02:41", seconds
    /// since recording started.
    Recording(u64),
}

impl Pill {
    fn label(self) -> String {
        match self {
            Pill::Attached(mode) => format!("Agent attached · {}", mode_word(mode)),
            Pill::NoAgent => "No agent".into(),
            Pill::Paused => "Agent paused".into(),
            Pill::Replaying => "Read-only · replaying".into(),
            Pill::Recording(secs) => format!("REC {}", clock(Duration::from_secs(secs))),
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
        self.commands_at(accessory_rect(page_width, self.width()))
    }

    fn commands_at(self, rect: Rect) -> Vec<DisplayCommand> {
        let (border, fill, ink) = match self {
            Pill::Attached(_) => (p::AGENT_BORDER, p::AGENT_TINT, p::AGENT_INK),
            Pill::NoAgent => (p::LINE, p::PAGE, p::MUTED),
            Pill::Paused => (p::PERSON_BORDER, p::PERSON_TINT, p::INK),
            Pill::Replaying => (p::LINE, p::PANEL, p::MUTED),
            Pill::Recording(_) => (p::STOP_BORDER, p::STOP_TINT, p::STOP_INK),
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
            Pill::Recording(_) => {
                commands.extend(ring_icon(icon_x, mid - 4.0, 8.0, 8.0, p::STOP, None))
            }
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

const PILL_GAP: f32 = 8.0;

/// Width to reserve for several pills side by side (agent + REC).
pub fn pills_width(pills: &[Pill]) -> f32 {
    let widths: f32 = pills.iter().map(|pill| pill.width()).sum();
    widths + PILL_GAP * pills.len().saturating_sub(1) as f32
}

/// Several pills in the accessory slot, left to right.
pub fn pills(pills: &[Pill], page_width: f32) -> Vec<DisplayCommand> {
    let slot = accessory_rect(page_width, pills_width(pills));
    let mut x = slot.x;
    let mut commands = Vec::new();
    for pill in pills {
        let width = pill.width();
        commands.extend(pill.commands_at(Rect { x, width, ..slot }));
        x += width + PILL_GAP;
    }
    commands
}

/// `02:41` — minutes can pass 99.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
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
    /// The session log being written (board 7's recording card).
    pub recording: Option<RecordingCard>,
    /// The receipt of the person's last refusal (board 9).
    pub refusal: Option<&'a Refusal>,
}

/// What the recording card says.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordingCard {
    /// The file, or the directory it will be created in.
    pub file: String,
    /// The file exists (the agent called since recording started).
    pub created: bool,
    pub steps: u64,
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
    /// Pause / resume / approval mode / recording — the person running
    /// the agent.
    Control(Control),
    /// The window's own views ("What the agent sees"): nothing reaches the
    /// worker or the agent.
    Window(WindowAction),
}

/// The "What the agent sees" viewer and the menu item that opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    ShowAgentView,
    Scope(ViewScope),
    CopyAgentView,
    CloseAgentView,
}

/// What the viewer shows (board 8's toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewScope {
    /// The observation the agent last received, as it was sent.
    #[default]
    OnScreen,
    /// A fresh look at the whole page (never sent, issues no ids).
    WholePage,
}

/// What the person can do to the agent from the ⋮ menu or the panel.
/// Window input only: no MCP tool maps to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Pause,
    Resume,
    Approve(ApproveMode),
    /// "Record this session": start / stop the session log.
    StartRecording,
    StopRecording,
    /// "Forget this refusal" on the receipt a refusal card left.
    ForgetRefusal,
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
                // (A taken-over step is drawn by `turn_card`.)
                Waiting::Approval { .. } | Waiting::TakenOver { .. } => {
                    (false, "Allow this action?")
                }
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
                Waiting::TakenOver { .. } => y,
            }
        },
    )
}

// "Your turn" (board 9): the person took the step over. The agent's call
// waits until they hand back (or say they won't do it).
fn turn_card(
    commands: &mut Vec<DisplayCommand>,
    buttons: &mut Vec<Button>,
    action: &str,
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
            c.push(text("Your turn", left, y, 16.0, p::INK, true));
            y += line_height(16.0) + 6.0;
            let shown = action.strip_prefix("Navigation to ").unwrap_or(action);
            mono_box(c, &mut y, shown, left, inner);
            y += 10.0;
            paragraph(
                c,
                &mut y,
                "Do this step in the page yourself. The agent's call waits; when you hand back, \
             it gets the page as it is then, with what you changed.",
                left,
                inner,
                BODY,
                p::MUTED,
                4,
            );
            y += 6.0;
            let rect = Rect {
                x: left,
                y,
                width: inner,
                height: BUTTON_HEIGHT,
            };
            c.extend(button(
                rect,
                "I'm done — hand back to the agent",
                Look::Filled(p::INK),
            ));
            buttons.push(Button {
                rect,
                action: PanelAction::Answer(Answer::HandBack),
            });
            y += BUTTON_HEIGHT + 8.0;
            let rect = Rect {
                x: left,
                y,
                width: inner,
                height: 36.0,
            };
            c.extend(button(rect, "I won't do it", Look::Plain));
            buttons.push(Button {
                rect,
                action: PanelAction::Answer(Answer::WontDo),
            });
            y += 36.0 + 10.0;
            c.push(text(
                "The agent checks back every 45 s",
                left,
                y,
                SMALL,
                p::MUTED,
                false,
            ));
            y + line_height(SMALL) - 4.0
        },
    )
}

// The receipt a refusal leaves (board 9): what was refused, that nothing
// happened, and "Forget this refusal".
fn refusal_card(
    commands: &mut Vec<DisplayCommand>,
    buttons: &mut Vec<Button>,
    refusal: &Refusal,
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
            c.extend(cross_icon(left, y + 1.0, p::STOP));
            c.push(text("Refused", left + 22.0, y, 14.0, p::INK, true));
            y += line_height(14.0) + 4.0;
            let action = refusal.action.as_str();
            mono_box(
                c,
                &mut y,
                action.strip_prefix("Navigation to ").unwrap_or(action),
                left,
                inner,
            );
            y += 8.0;
            let what = if refusal.taken_over {
                "You took this over and didn't do it."
            } else if action.starts_with("Navigation to") {
                "The page didn't change."
            } else {
                "Nothing was sent."
            };
            paragraph(
                c,
                &mut y,
                &format!(
                    "{what} The agent was told approval_denied and won't ask for this again this session."
                ),
                left,
                inner,
                BODY,
                p::MUTED,
                6,
            );
            y += 6.0;
            let rect = Rect {
                x: left,
                y,
                width: text_width("Forget this refusal", BODY, false, 400) + 24.0,
                height: 36.0,
            };
            c.extend(button(rect, "Forget this refusal", Look::Plain));
            buttons.push(Button {
                rect,
                action: PanelAction::Control(Control::ForgetRefusal),
            });
            y + rect.height
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

// Board 7: the session log being written, with Stop.
fn recording_card(
    commands: &mut Vec<DisplayCommand>,
    buttons: &mut Vec<Button>,
    card: &RecordingCard,
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
            c.extend(ring_icon(left, y + 4.0, 8.0, 8.0, p::STOP, None));
            c.push(text(
                "Recording this session",
                left + 16.0,
                y,
                14.0,
                p::INK,
                true,
            ));
            y += line_height(14.0) + 4.0;
            let steps = match card.steps {
                1 => "1 step".to_string(),
                n => format!("{n} steps"),
            };
            let line = format!("{} · {steps}", card.file);
            // A path has no spaces to wrap at: keep its end (the file name).
            let per_char = text_width("0000000000", SMALL, true, 400) / 10.0;
            let max_chars = (inner / per_char.max(1.0)) as usize;
            c.push(text_cmd(
                clip_start(&line, max_chars),
                left,
                y,
                SMALL,
                p::MUTED,
                400,
                true,
            ));
            y += line_height(SMALL) + 4.0;
            let note = if card.created {
                "Only the agent's steps are recorded, not your typing. Password fields are always redacted."
            } else {
                "The file is created at the agent's next call. Only the agent's steps are recorded, not your typing; password fields are always redacted."
            };
            paragraph(c, &mut y, note, left, inner, SMALL, p::MUTED, 4);
            y += 6.0;
            let rect = Rect {
                x: left,
                y,
                width: inner,
                height: 36.0,
            };
            c.extend(button(rect, "Stop recording", Look::Plain));
            buttons.push(Button {
                rect,
                action: PanelAction::Control(Control::StopRecording),
            });
            y + rect.height
        },
    )
}

// Keep the end of `s` (`…/mb-runs/run-1-2.jsonl · 3 steps`).
fn clip_start(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out = String::from("…");
    out.extend(s.chars().skip(count - keep));
    out
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
    match info.waiting {
        Some(Waiting::TakenOver { action }) => {
            y = turn_card(&mut commands, &mut buttons, action, left, inner, y) + 14.0;
        }
        Some(waiting) => {
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
        None => {}
    }
    if let Some(refusal) = info.refusal {
        y = refusal_card(&mut commands, &mut buttons, refusal, left, inner, y) + 14.0;
    }
    if let Some(wait) = &info.page_wait {
        y = wait_card(&mut commands, wait, left, inner, y) + 14.0;
    }
    if let Some(recording) = &info.recording {
        y = recording_card(&mut commands, &mut buttons, recording, left, inner, y) + 14.0;
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
    /// Recording the session: for how long.
    pub recording: Option<Duration>,
    /// The last "Record this session" couldn't create its directory.
    pub record_failed: bool,
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

    // Record: start / stop the session log (person-only, like pause).
    header(&mut commands, &mut y, "RECORD");
    let (label, control, value) = match info.recording {
        Some(elapsed) => (
            "Stop recording",
            Control::StopRecording,
            format!("REC {}", clock(elapsed)),
        ),
        None if info.record_failed => (
            "Record this session",
            Control::StartRecording,
            "couldn't start".to_string(),
        ),
        None => (
            "Record this session",
            Control::StartRecording,
            String::new(),
        ),
    };
    let rect = row(&mut commands, &mut y, label, "", true, None);
    if info.recording.is_some() {
        // Stop: a red square.
        commands.push(rounded(
            p::STOP,
            Rect {
                x: rect.x + 13.0,
                y: rect.y + 15.0,
                width: 10.0,
                height: 10.0,
            },
            2.0,
        ));
    } else {
        commands.extend(ring_icon(
            rect.x + 10.0,
            rect.y + 12.0,
            16.0,
            10.0,
            p::STOP,
            None,
        ));
    }
    if !value.is_empty() {
        let value_width = text_width(&value, SMALL, false, 400);
        commands.push(text(
            value,
            rect.x + rect.width - 10.0 - value_width,
            centered_text_y(rect.y, MENU_ROW, SMALL),
            SMALL,
            p::STOP_TEXT,
            false,
        ));
    }
    buttons.push(Button {
        rect,
        action: PanelAction::Control(control),
    });
    rule(&mut commands, &mut y);

    // View: the observation text, read-only.
    header(&mut commands, &mut y, "VIEW");
    let rect = row(&mut commands, &mut y, "What the agent sees", "", true, None);
    commands.extend(eye_icon(rect.x + 10.0, rect.y + 12.0, p::INK));
    buttons.push(Button {
        rect,
        action: PanelAction::Window(WindowAction::ShowAgentView),
    });
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

// ------------------------------------------- "What the agent sees" ---

const VIEW_MAX_WIDTH: f32 = 760.0;
const VIEW_MARGIN: f32 = 16.0;
const VIEW_HEADER: f32 = 64.0;
const VIEW_FOOTER: f32 = 40.0;
const VIEW_PAD: f32 = 20.0;
const VIEW_MONO: f32 = 12.0;
const VIEW_LINE: f32 = 18.0;
const VIEW_LABEL: f32 = 24.0;
const VIEW_GAP: f32 = 14.0;
const VIEW_BOX_PAD: f32 = 8.0;

/// What the viewer shows (board 8).
pub struct AgentViewInfo<'a> {
    pub scope: ViewScope,
    /// (trusted browser block, untrusted page block), verbatim.
    pub blocks: Option<(&'a str, &'a str)>,
    /// One sentence under the title: where these blocks come from.
    pub source: &'a str,
    /// First row shown (clamped by the layout).
    pub scroll: usize,
    pub copied: bool,
    pub hover: Option<(f32, f32)>,
}

/// The painted viewer: commands, its buttons, the dialog rect (input over
/// it never reaches the page) and the scroll it ended up at.
pub struct AgentView {
    pub commands: Vec<DisplayCommand>,
    pub buttons: Vec<Button>,
    pub rect: Rect,
    /// `info.scroll`, clamped to what can scroll.
    pub scroll: usize,
    /// Rows a page-down moves.
    pub page_rows: usize,
    /// Text lines below the visible part ("… N more lines").
    #[cfg(test)]
    pub below: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewBlock {
    Browser,
    Page,
}

/// One row of the viewer's scrolling body.
#[derive(Debug, Clone, PartialEq)]
enum ViewRow {
    Label(String, ViewBlock),
    /// Box padding: top (true) or bottom edge of a block's box.
    Pad(ViewBlock, bool),
    Line(ViewBlock, String),
    Gap,
}

impl ViewRow {
    fn height(&self) -> f32 {
        match self {
            ViewRow::Label(..) => VIEW_LABEL,
            ViewRow::Pad(..) => VIEW_BOX_PAD,
            ViewRow::Line(..) => VIEW_LINE,
            ViewRow::Gap => VIEW_GAP,
        }
    }
}

/// `# page content (untrusted, origin=X)` → X (the browser wrote it from
/// the URL; the rest of that block is the page's).
fn page_origin(page: &str) -> &str {
    page.lines()
        .next()
        .and_then(|line| line.split_once("origin="))
        .map(|(_, rest)| rest.trim_end_matches(')'))
        .unwrap_or("the page")
}

// The two blocks as rows: a label over each, then its lines hard-wrapped
// at `columns` (monospace, so a char count is a width).
fn view_rows(browser: &str, page: &str, columns: usize) -> Vec<ViewRow> {
    let columns = columns.max(8);
    let mut rows = Vec::new();
    let blocks = [
        (
            ViewBlock::Browser,
            "Browser — trusted, written by mini-browser".to_string(),
            browser,
        ),
        (
            ViewBlock::Page,
            format!("Page content — untrusted, written by {}", page_origin(page)),
            page,
        ),
    ];
    for (i, (block, label, body)) in blocks.into_iter().enumerate() {
        if i > 0 {
            rows.push(ViewRow::Gap);
        }
        rows.push(ViewRow::Label(label, block));
        rows.push(ViewRow::Pad(block, true));
        for line in body.trim_end_matches('\n').split('\n') {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                rows.push(ViewRow::Line(block, String::new()));
            }
            for chunk in chars.chunks(columns) {
                rows.push(ViewRow::Line(block, chunk.iter().collect()));
            }
        }
        rows.push(ViewRow::Pad(block, false));
    }
    rows
}

// The largest first row that still fills the body.
fn max_scroll(rows: &[ViewRow], body: f32) -> usize {
    let mut used = 0.0;
    for (i, row) in rows.iter().enumerate().rev() {
        used += row.height();
        if used > body {
            return i + 1;
        }
    }
    0
}

/// The read-only "What the agent sees" dialog (board 8) over the page
/// area `page`. Long text scrolls (`info.scroll`, in rows); what doesn't
/// fit is counted in the footer.
pub fn agent_view(info: &AgentViewInfo<'_>, page: Rect) -> AgentView {
    let width = (page.width - 2.0 * VIEW_MARGIN).clamp(0.0, VIEW_MAX_WIDTH);
    let rect = Rect {
        x: page.x + ((page.width - width) / 2.0).max(0.0),
        y: page.y + VIEW_MARGIN,
        width,
        height: (page.height - 2.0 * VIEW_MARGIN).max(VIEW_HEADER + VIEW_FOOTER),
    };
    let mut commands = framed_rect(rect, 14.0, p::FRAME, p::PAGE).to_vec();
    let mut buttons = Vec::new();
    let left = rect.x + VIEW_PAD;
    let inner = rect.width - 2.0 * VIEW_PAD;
    let right = rect.x + rect.width - VIEW_PAD;
    let hovered = |r: &Rect| info.hover.is_some_and(|point| contains(r, point));

    // Header: eye, title, scope toggle, Copy, close.
    let row_y = rect.y + (VIEW_HEADER - 32.0) / 2.0;
    commands.extend(eye_icon(left, rect.y + (VIEW_HEADER - 16.0) / 2.0, p::INK));
    commands.push(text(
        "What the agent sees",
        left + 26.0,
        centered_text_y(rect.y, VIEW_HEADER, 16.0),
        16.0,
        p::INK,
        true,
    ));
    let close = Rect {
        x: right - 32.0,
        y: row_y,
        width: 32.0,
        height: 32.0,
    };
    if hovered(&close) {
        commands.push(rounded(p::PANEL, close, 8.0));
    }
    commands.extend(cross_icon(close.x + 9.0, close.y + 9.0, p::MUTED));
    buttons.push(Button {
        rect: close,
        action: PanelAction::Window(WindowAction::CloseAgentView),
    });
    let copy_label = if info.copied { "Copied" } else { "Copy" };
    let copy_width = text_width(copy_label, BODY, false, 400) + 24.0;
    let copy = Rect {
        x: close.x - 8.0 - copy_width,
        y: row_y,
        width: copy_width,
        height: 32.0,
    };
    let look = if info.blocks.is_some() {
        Look::Plain
    } else {
        Look::Disabled
    };
    commands.extend(button(copy, copy_label, look));
    if info.blocks.is_some() {
        buttons.push(Button {
            rect: copy,
            action: PanelAction::Window(WindowAction::CopyAgentView),
        });
    }
    let segments = [
        ("On screen", ViewScope::OnScreen),
        ("Whole page", ViewScope::WholePage),
    ];
    let segment_widths: Vec<f32> = segments
        .iter()
        .map(|(label, _)| text_width(label, BODY, false, 700) + 24.0)
        .collect();
    let track_width = segment_widths.iter().sum::<f32>() + 3.0 * 3.0;
    let track = Rect {
        x: copy.x - 12.0 - track_width,
        y: row_y - 1.0,
        width: track_width,
        height: 34.0,
    };
    commands.push(rounded(p::PANEL, track, 8.0));
    let mut sx = track.x + 3.0;
    for ((label, scope), w) in segments.into_iter().zip(segment_widths) {
        let segment = Rect {
            x: sx,
            y: track.y + 3.0,
            width: w,
            height: 28.0,
        };
        let selected = info.scope == scope;
        if selected {
            commands.push(rounded(p::PAGE, segment, 6.0));
        } else if hovered(&segment) {
            commands.push(rounded(p::LINE, segment, 6.0));
        }
        let weight = if selected { 700 } else { 400 };
        let label_width = text_width(label, BODY, false, weight);
        commands.push(text(
            label,
            segment.x + (w - label_width) / 2.0,
            centered_text_y(segment.y, segment.height, BODY),
            BODY,
            if selected { p::INK } else { p::MUTED },
            selected,
        ));
        buttons.push(Button {
            rect: segment,
            action: PanelAction::Window(WindowAction::Scope(scope)),
        });
        sx += w + 3.0;
    }
    let rule = |commands: &mut Vec<DisplayCommand>, y: f32| {
        commands.push(DisplayCommand::SolidRect(
            p::LINE,
            Rect {
                x: rect.x + 1.0,
                y,
                width: rect.width - 2.0,
                height: 1.0,
            },
        ));
    };
    rule(&mut commands, rect.y + VIEW_HEADER);

    // Where the text comes from.
    let mut y = rect.y + VIEW_HEADER + 14.0;
    paragraph(
        &mut commands,
        &mut y,
        info.source,
        left,
        inner,
        BODY,
        p::MUTED,
        2,
    );
    y += 8.0;

    // The scrolling body.
    let footer_top = rect.y + rect.height - VIEW_FOOTER;
    let body_top = y;
    let body_height = (footer_top - 8.0 - body_top).max(0.0);
    let box_inner = inner - 28.0;
    let per_char = text_width("0000000000", VIEW_MONO, true, 400) / 10.0;
    let columns = (box_inner / per_char.max(1.0)) as usize;
    let rows = match info.blocks {
        Some((browser, page_block)) => view_rows(browser, page_block, columns),
        None => Vec::new(),
    };
    let scroll = info.scroll.min(max_scroll(&rows, body_height));
    let mut shown = 0;
    for row in &rows[scroll..] {
        if y + row.height() > body_top + body_height {
            break;
        }
        let (fill, border) = match row {
            ViewRow::Pad(ViewBlock::Browser, _) | ViewRow::Line(ViewBlock::Browser, _) => {
                (p::AGENT_TINT, p::AGENT_BORDER)
            }
            _ => (p::PANEL, p::LINE),
        };
        let strip = Rect {
            x: left,
            y,
            width: inner,
            height: row.height(),
        };
        match row {
            ViewRow::Label(label, block) => {
                let ink = match block {
                    ViewBlock::Browser => p::AGENT_INK,
                    ViewBlock::Page => p::MUTED,
                };
                commands.push(text(
                    clip(label, (inner / 6.5) as usize),
                    left,
                    y + 4.0,
                    SMALL,
                    ink,
                    true,
                ));
            }
            ViewRow::Pad(_, top) => {
                // Rounded on the box's outer edge; a 1 px border all round.
                let radius = |r: f32| {
                    if *top {
                        CornerRadii {
                            tl: r,
                            tr: r,
                            br: 0.0,
                            bl: 0.0,
                        }
                    } else {
                        CornerRadii {
                            tl: 0.0,
                            tr: 0.0,
                            br: r,
                            bl: r,
                        }
                    }
                };
                commands.push(DisplayCommand::RoundedRect(border, strip, radius(10.0)));
                let (dy, dh) = if *top { (1.0, -1.0) } else { (0.0, -1.0) };
                commands.push(DisplayCommand::RoundedRect(
                    fill,
                    Rect {
                        x: strip.x + 1.0,
                        y: strip.y + dy,
                        width: strip.width - 2.0,
                        height: strip.height + dh,
                    },
                    radius(9.0),
                ));
            }
            ViewRow::Line(_, line) => {
                commands.push(DisplayCommand::SolidRect(border, strip));
                commands.push(DisplayCommand::SolidRect(
                    fill,
                    Rect {
                        x: strip.x + 1.0,
                        width: strip.width - 2.0,
                        ..strip
                    },
                ));
                if !line.is_empty() {
                    commands.push(text_cmd(
                        line.clone(),
                        left + 14.0,
                        centered_text_y(y, VIEW_LINE, VIEW_MONO),
                        VIEW_MONO,
                        p::INK,
                        400,
                        true,
                    ));
                }
            }
            ViewRow::Gap => {}
        }
        y += row.height();
        shown += 1;
    }
    if rows.is_empty() {
        commands.push(text(
            "Nothing yet — switch to Whole page to look at this page as the agent would.",
            left,
            body_top,
            BODY,
            p::FAINT,
            false,
        ));
    }
    let lines = |rows: &[ViewRow]| {
        rows.iter()
            .filter(|row| matches!(row, ViewRow::Line(..)))
            .count()
    };
    let above = lines(&rows[..scroll]);
    let below = lines(&rows[(scroll + shown).min(rows.len())..]);

    // Footer: read-only note, and how much is out of view.
    rule(&mut commands, footer_top);
    let footer_y = centered_text_y(footer_top, VIEW_FOOTER, SMALL);
    commands.push(text(
        "Read-only · nothing here is sent to the agent",
        left,
        footer_y,
        SMALL,
        p::MUTED,
        false,
    ));
    let more = match (above, below) {
        (0, 0) => String::new(),
        (0, n) => format!("… {n} more line{} — scroll", if n == 1 { "" } else { "s" }),
        (a, 0) => format!("{a} line{} above", if a == 1 { "" } else { "s" }),
        (a, n) => format!(
            "{a} above · … {n} more line{}",
            if n == 1 { "" } else { "s" }
        ),
    };
    if !more.is_empty() {
        let more_width = text_width(&more, SMALL, false, 700);
        commands.push(text(
            more,
            right - more_width,
            footer_y,
            SMALL,
            p::INK,
            true,
        ));
    }
    AgentView {
        commands,
        buttons,
        rect,
        scroll,
        page_rows: shown.saturating_sub(1).max(1),
        #[cfg(test)]
        below,
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
        let page = page_area(900, 700);
        let top = page_overlay(&elements, None, None, 0.0, page, &[]);
        assert_eq!(badge_texts(&top), vec!["e5"]);
        let scrolled = page_overlay(&elements, None, None, 600.0, page, &[]);
        assert_eq!(badge_texts(&scrolled), vec!["e9"]);
        // e9 at document y 900, scrolled 600 → window y 300 + chrome; with
        // nothing around it the badge sits on its top-left corner, outside.
        let DisplayCommand::RoundedRect(_, badge, _) = &scrolled[0] else {
            panic!("badge background first");
        };
        assert_eq!(badge.x, 10.0);
        assert_eq!(
            badge.y,
            300.0 + CHROME_HEIGHT - BadgeStyle::FULL.height + BADGE_TUCK
        );
    }

    // ---- badge placement (pure, sizes given) ----

    const FULL: (f32, f32) = (28.0, 16.0);
    const COMPACT: (f32, f32) = (20.0, 12.0);

    fn rect(x: f32, y: f32, width: f32, height: f32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    fn request(element: Rect) -> BadgeRequest {
        BadgeRequest {
            element,
            outlined: false,
            full: FULL,
            compact: COMPACT,
        }
    }

    fn page() -> Rect {
        page_area(900, 700)
    }

    /// `place_badges` where every element is expected to get a badge.
    fn place_all(requests: &[BadgeRequest], ink: &[Rect], page: Rect) -> Vec<PlacedBadge> {
        place_badges(requests, ink, page)
            .into_iter()
            .enumerate()
            .map(|(i, b)| b.unwrap_or_else(|| panic!("element {i} got no badge")))
            .collect()
    }

    /// No badge hides an element (past the tuck), the text, or another badge.
    fn assert_nothing_hidden(requests: &[BadgeRequest], ink: &[Rect], placed: &[PlacedBadge]) {
        for (i, badge) in placed.iter().enumerate() {
            let b = badge.rect;
            for (j, r) in requests.iter().enumerate() {
                let keep = if i == j {
                    inset(r.element, BADGE_TUCK)
                } else {
                    r.element
                };
                assert_eq!(
                    overlap(&b, &keep),
                    0.0,
                    "badge {i} hides element {j}: {b:?}"
                );
            }
            for t in ink {
                assert_eq!(overlap(&b, t), 0.0, "badge {i} hides text {t:?}: {b:?}");
            }
            for other in &placed[i + 1..] {
                assert_eq!(overlap(&b, &other.rect), 0.0, "badges overlap: {b:?}");
            }
            assert!(within(&page(), &b), "badge {i} off the page: {b:?}");
        }
    }

    /// The badge touches (or nearly touches) its element.
    fn assert_next_to(badge: Rect, element: Rect) {
        let gap_x =
            (element.x - (badge.x + badge.width)).max(badge.x - (element.x + element.width));
        let gap_y =
            (element.y - (badge.y + badge.height)).max(badge.y - (element.y + element.height));
        assert!(
            gap_x.max(gap_y) <= OUTLINE_REACH,
            "badge {badge:?} too far from {element:?}"
        );
    }

    #[test]
    fn adjacent_inline_links_get_badges_above_without_covering_each_other() {
        // "About this demo · Sign in · Report" on one line, the paragraph
        // gap above it free: every badge goes on top of its link, full size.
        let y = CHROME_HEIGHT + 200.0;
        let links = [
            request(rect(24.0, y, 110.0, 20.0)),
            request(rect(144.0, y, 44.0, 20.0)),
            request(rect(199.0, y, 45.0, 20.0)),
        ];
        let placed = place_all(&links, &[], page());
        assert_nothing_hidden(&links, &[], &placed);
        for (badge, link) in placed.iter().zip(&links) {
            assert!(!badge.compact);
            assert_eq!(badge.rect.x, link.element.x);
            assert!(badge.rect.y < link.element.y, "above: {badge:?}");
        }
    }

    #[test]
    fn links_packed_tighter_than_a_badge_never_stack_badges() {
        // Short links 6 px apart: the full badge above the first one also
        // reaches over the second, so the second must go somewhere else.
        let y = CHROME_HEIGHT + 200.0;
        let links = [
            request(rect(24.0, y, 14.0, 20.0)),
            request(rect(44.0, y, 14.0, 20.0)),
            request(rect(64.0, y, 14.0, 20.0)),
        ];
        let placed = place_all(&links, &[], page());
        assert_nothing_hidden(&links, &[], &placed);
        for (badge, link) in placed.iter().zip(&links) {
            assert_next_to(badge.rect, link.element);
        }
    }

    #[test]
    fn a_line_of_text_above_a_link_pushes_its_badge_to_a_free_spot() {
        // "1 of 2 done" right above "About this demo · Sign in", 12 px
        // gap; the next line holds another link: a full badge fits
        // nowhere around the first link, a compact one fits the gap above.
        let y = CHROME_HEIGHT + 200.0;
        let ink = [
            rect(24.0, y - 30.0, 74.0, 18.0), // "1 of 2 done"
            rect(136.0, y + 2.0, 6.0, 15.0),  // " · "
        ];
        let links = [
            request(rect(24.0, y, 110.0, 20.0)),
            request(rect(144.0, y, 44.0, 20.0)),
            request(rect(24.0, y + 31.0, 130.0, 20.0)),
        ];
        let placed = place_all(&links, &ink, page());
        assert_nothing_hidden(&links, &ink, &placed);
        assert!(placed[0].compact, "{:?}", placed[0]);
        assert!(
            placed[0].rect.y < links[0].element.y,
            "above: {:?}",
            placed[0]
        );
        assert_next_to(placed[0].rect, links[0].element);
    }

    #[test]
    fn checkbox_and_its_label_both_stay_visible() {
        // A list of checkboxes at the 24 px body margin, each followed by
        // its label text, the "New item" row right above: a full badge fits
        // nowhere, a compact one in the margin or the gap between rows.
        let top = CHROME_HEIGHT + 160.0;
        let ink = [
            rect(24.0, top + 4.0, 63.0, 15.0),   // "New item"
            rect(47.0, top + 31.0, 108.0, 15.0), // "Water the plants"
            rect(47.0, top + 54.0, 140.0, 15.0), // "Read the concept doc"
            rect(24.0, top + 81.0, 74.0, 15.0),  // "1 of 2 done"
        ];
        let requests = [
            request(rect(90.0, top, 210.0, 21.0)),       // the textbox
            request(rect(24.0, top + 29.0, 16.0, 16.0)), // checkbox 1
            request(rect(24.0, top + 52.0, 16.0, 16.0)), // checkbox 2
        ];
        let placed = place_all(&requests, &ink, page());
        assert_nothing_hidden(&requests, &ink, &placed);
        for (badge, checkbox) in placed[1..].iter().zip(&requests[1..]) {
            assert!(badge.compact, "{badge:?}");
            assert_next_to(badge.rect, checkbox.element);
        }
    }

    #[test]
    fn element_at_the_top_of_the_viewport_keeps_its_badge_off_the_chrome() {
        // Right under the chrome: "above" would be over the toolbar.
        let flush = [request(rect(200.0, CHROME_HEIGHT, 80.0, 20.0))];
        let placed = place_all(&flush, &[], page());
        assert!(placed[0].rect.y >= CHROME_HEIGHT, "{:?}", placed[0]);
        assert_nothing_hidden(&flush, &[], &placed);
        assert_next_to(placed[0].rect, flush[0].element);

        // Half scrolled under the chrome, at the left edge: every spot
        // around it leaves the page except below.
        let tucked = [request(rect(0.0, CHROME_HEIGHT - 10.0, 80.0, 20.0))];
        let placed = place_all(&tucked, &[], page());
        assert!(placed[0].rect.y >= CHROME_HEIGHT, "{:?}", placed[0]);
        assert!(within(&page(), &placed[0].rect));
    }

    #[test]
    fn nowhere_to_go_falls_back_to_the_inside_corner_on_the_page() {
        // The element fills the whole page area.
        let area = rect(0.0, CHROME_HEIGHT, 100.0, 30.0);
        let full = [request(area)];
        let placed = place_all(&full, &[], area);
        assert!(placed[0].compact);
        assert_eq!((placed[0].rect.x, placed[0].rect.y), (0.0, CHROME_HEIGHT));
    }

    #[test]
    fn a_spot_touching_another_element_is_never_used() {
        // A small button boxed in on every side by other controls, at the
        // page's left edge: every spot around it touches a neighbour, so the
        // badge goes inside its own box rather than on top of a neighbour's.
        let y = CHROME_HEIGHT + 200.0;
        let small = rect(0.0, y, 24.0, 16.0);
        let requests = [
            request(small),
            request(rect(0.0, y - 20.0, 200.0, 19.0)), // above
            request(rect(25.0, y - 2.0, 200.0, 20.0)), // right
            request(rect(0.0, y + 17.0, 200.0, 19.0)), // below
        ];
        let placed = place_badges(&requests, &[], page());
        let badge = placed[0].expect("a badge for the checkbox");
        for other in &requests[1..] {
            assert_eq!(overlap(&badge.rect, &other.element), 0.0, "{badge:?}");
        }
        assert!(badge.compact);
        assert!(overlap(&badge.rect, &small) > 0.0, "touches its own box");
    }

    #[test]
    fn the_fallback_stays_on_its_own_element_and_off_other_badges() {
        // A big element filling the page with a smaller one nested in its
        // top-left corner: both fall back inside their own boxes, and the
        // nested one takes another corner instead of stacking on the first.
        let area = rect(0.0, CHROME_HEIGHT, 300.0, 60.0);
        let inner = rect(0.0, CHROME_HEIGHT, 80.0, 40.0);
        let placed = place_badges(&[request(area), request(inner)], &[], area);
        let (outer_badge, inner_badge) = (placed[0].unwrap(), placed[1].unwrap());
        assert_eq!(overlap(&outer_badge.rect, &inner_badge.rect), 0.0);
        assert!(within(&inner, &inner_badge.rect), "{inner_badge:?}");
    }

    #[test]
    fn an_element_with_no_on_page_area_gets_no_badge() {
        let below = rect(10.0, 2000.0, 80.0, 20.0);
        let under_chrome = rect(10.0, 10.0, 80.0, 20.0);
        let placed = place_badges(&[request(below), request(under_chrome)], &[], page());
        assert_eq!(placed, [None, None]);
    }

    #[test]
    fn no_corner_free_of_other_badges_means_no_badge() {
        // Two elements with the same box that fills the page: the second
        // has no spot and every corner of its box is taken by the first's.
        let area = rect(0.0, CHROME_HEIGHT, 20.0, 12.0);
        let placed = place_badges(&[request(area), request(area)], &[], area);
        assert!(placed[0].is_some());
        assert_eq!(placed[1], None);
    }

    #[test]
    fn a_crowded_page_draws_at_most_max_badges_but_always_the_target() {
        // 2000 visible links in a grid, the target last in document order.
        let elements: Vec<ElementObservation> = (0..2000)
            .map(|i| {
                let mut e = element(i, (i / 20) as f32 * 6.0);
                e.rect.x = (i % 20) as f32 * 44.0;
                e.rect.width = 40.0;
                e.rect.height = 5.0;
                e
            })
            .collect();
        let target = NodeId::from_raw(1999);
        let commands = page_overlay(&elements, Some(target), None, 0.0, page_area(900, 700), &[]);
        let labels = badge_texts(&commands);
        assert!(labels.len() <= MAX_BADGES + 1, "{}", labels.len());
        assert!(
            labels.contains(&"e1999".to_string()),
            "the target keeps its badge"
        );
    }

    #[test]
    fn text_far_off_the_page_is_skipped_before_measuring() {
        let page = page();
        let run = |y: f32, text: &str, wrap: Option<f32>| TextCommand {
            text: text.into(),
            x: 8.0,
            y,
            color: p::INK,
            font_size: 16.0,
            wrap_width: wrap,
            font_family: None,
            font_weight: 400,
        };
        // Below the page, or one line well above it: never measured.
        assert!(!could_reach(
            &run(page.y + page.height, "below", Some(400.0)),
            &page
        ));
        assert!(!could_reach(&run(page.y - 100.0, "above", None), &page));
        // On the page; or above it but long enough to wrap down into it.
        assert!(could_reach(
            &run(page.y + 10.0, "on it", Some(400.0)),
            &page
        ));
        let long = "word ".repeat(200);
        assert!(could_reach(&run(page.y - 100.0, &long, Some(400.0)), &page));
        // A wrap narrower than a glyph: one character per line at most.
        assert!(!could_reach(&run(page.y - 100.0, "abc", Some(4.0)), &page));
        assert!(could_reach(&run(page.y - 30.0, "abc", Some(4.0)), &page));
    }

    #[test]
    fn row_index_finds_rects_in_the_same_bands_once() {
        let page = page();
        let mut index = RowIndex::new(page);
        let tall = rect(0.0, CHROME_HEIGHT, 10.0, 200.0); // spans several bands
        let far = rect(0.0, CHROME_HEIGHT + 500.0, 10.0, 10.0);
        let off = rect(0.0, 5000.0, 10.0, 10.0);
        index.insert(0, &tall);
        index.insert(1, &far);
        index.insert(2, &off);
        let mut scratch = Vec::new();
        let mut seen = Vec::new();
        let probe = rect(0.0, CHROME_HEIGHT + 60.0, 10.0, 80.0);
        index.for_near(&probe, &mut scratch, |i| seen.push(i));
        assert_eq!(seen, [0]);
        assert!(
            index.rows.iter().all(|row| !row.contains(&2)),
            "off-page rect kept"
        );
    }

    #[test]
    fn the_target_badge_stays_outside_its_outline() {
        let y = CHROME_HEIGHT + 200.0;
        let mut target = request(rect(300.0, y, 120.0, 20.0));
        target.outlined = true;
        let placed = place_all(&[target], &[], page());
        let ring = inset(target.element, -OUTLINE_REACH);
        assert_eq!(overlap(&placed[0].rect, &ring), 0.0, "{:?}", placed[0]);
        assert_next_to(placed[0].rect, target.element);
    }

    #[test]
    fn text_obstacles_cover_page_text_only_and_skip_the_leading() {
        let page = page();
        let label = |s: &str, y: f32| {
            DisplayCommand::Text(TextCommand {
                text: s.into(),
                x: 24.0,
                y,
                color: p::INK,
                font_size: 16.0,
                wrap_width: Some(400.0),
                font_family: None,
                font_weight: 400,
            })
        };
        let commands = [
            label("tab title", 10.0),    // chrome
            label("1 of 2 done", 300.0), // page
            label("   ", 330.0),         // nothing to cover
        ];
        let ink = text_obstacles(&commands, page);
        assert_eq!(ink.len(), 1, "{ink:?}");
        // Inside the 19.2 px line box, off its top and bottom edges.
        assert!(ink[0].y > 300.0 && ink[0].y + ink[0].height < 300.0 + 19.2);
        assert!(ink[0].width > 0.0 && ink[0].width <= 400.0);
    }

    #[test]
    fn target_gets_an_outline() {
        let elements = [element(5, 20.0)];
        let page = page_area(900, 700);
        let plain = page_overlay(&elements, None, None, 0.0, page, &[]);
        let targeted = page_overlay(&elements, Some(NodeId::from_raw(5)), None, 0.0, page, &[]);
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
                refusal: None,
                recording: None,
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
                refusal: None,
                recording: None,
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
                refusal: None,
                recording: None,
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

    fn panel_with(
        waiting: Option<&Waiting>,
        refusal: Option<&Refusal>,
    ) -> (Vec<String>, Vec<Button>) {
        let (commands, buttons) = panel(
            &PanelInfo {
                approve: ApproveMode::Ask,
                paused: None,
                allow_origins: &[],
                timeline: &[],
                waiting,
                waited: Duration::from_secs(5),
                page_wait: None,
                attach: None,
                copied: false,
                recording: None,
                refusal,
            },
            900.0,
            340.0,
            900.0,
        );
        (badge_texts(&commands), buttons)
    }

    #[test]
    fn the_your_turn_card_hands_back_or_declines() {
        let waiting = Waiting::TakenOver {
            action: "POST http://127.0.0.1:8765/signup".into(),
        };
        let (texts, buttons) = panel_with(Some(&waiting), None);
        assert!(texts.contains(&"Your turn".to_string()), "{texts:?}");
        assert!(texts.iter().any(|t| t.contains("/signup")), "{texts:?}");
        assert!(
            texts.contains(&"I'm done — hand back to the agent".to_string()),
            "{texts:?}"
        );
        // Not the approval card any more: no Approve / Deny.
        assert!(!texts.contains(&"Approve".to_string()), "{texts:?}");
        let answers: Vec<PanelAction> = buttons.iter().map(|b| b.action).collect();
        assert_eq!(
            answers,
            [Answer::HandBack, Answer::WontDo].map(PanelAction::Answer)
        );
        assert!(buttons[0].rect.y + buttons[0].rect.height <= buttons[1].rect.y);
    }

    #[test]
    fn a_refusal_leaves_a_receipt_with_forget() {
        let denied = Refusal::sample("POST http://127.0.0.1:8765/signup", false);
        let (texts, buttons) = panel_with(None, Some(&denied));
        assert!(texts.contains(&"Refused".to_string()), "{texts:?}");
        let body = texts.join(" ");
        assert!(body.contains("Nothing was sent."), "{body}");
        assert!(body.contains("approval_denied"), "{body}");
        assert!(body.contains("this session"), "{body}");
        assert_eq!(
            buttons.iter().map(|b| b.action).collect::<Vec<_>>(),
            [PanelAction::Control(Control::ForgetRefusal)]
        );

        // A navigation says the page didn't change; "I won't do it" says so.
        let nav = Refusal::sample("Navigation to http://localhost:8765/x", false);
        let (texts, _) = panel_with(None, Some(&nav));
        let body = texts.join(" ");
        assert!(body.contains("The page didn't change."), "{body}");
        assert!(
            texts.iter().any(|t| t.starts_with("http://localhost")),
            "{texts:?}"
        );
        assert!(!body.contains("Navigation to"), "{body}");
        let wont = Refusal::sample("POST http://127.0.0.1:8765/signup", true);
        let (texts, _) = panel_with(None, Some(&wont));
        assert!(texts.join(" ").contains("You took this over"), "{texts:?}");

        // Under a question that is up meanwhile, both cards are answerable.
        let asking = Waiting::Approval {
            action: "POST http://127.0.0.1:8765/login".into(),
        };
        let (_, buttons) = panel_with(Some(&asking), Some(&denied));
        let actions: Vec<PanelAction> = buttons.iter().map(|b| b.action).collect();
        assert_eq!(actions.len(), 4, "{actions:?}");
        assert_eq!(actions[3], PanelAction::Control(Control::ForgetRefusal));
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
                refusal: None,
                recording: None,
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
    fn the_menu_offers_pause_the_three_approval_modes_record_and_view() {
        let menu = menu(
            &MenuInfo {
                paused: false,
                approve: ApproveMode::Ask,
                allow_origins: &[],
                recording: None,
                record_failed: false,
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
                Control::StartRecording,
            ]
        );
        let texts = badge_texts(&menu.commands);
        for label in [
            "AGENT",
            "Pause the agent",
            "Ask me",
            "Refuse",
            "Allow",
            "RECORD",
            "Record this session",
            "VIEW",
            "What the agent sees",
        ] {
            assert!(texts.contains(&label.to_string()), "{label}: {texts:?}");
        }
        // The viewer is the window's own: no worker control behind it.
        assert_eq!(
            menu.buttons.last().map(|b| b.action),
            Some(PanelAction::Window(WindowAction::ShowAgentView))
        );
        // Allowed sites isn't built yet: listed, but nothing to click.
        assert_eq!(menu.buttons.len(), 6);
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
                recording: None,
                record_failed: false,
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
                recording: None,
                record_failed: false,
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
            refusal: None,
            recording: None,
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
            page_area(900, 700),
            &[],
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
            refusal: None,
            recording: None,
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

    #[test]
    fn a_recording_menu_offers_stop_with_the_clock_and_a_failure_says_so() {
        let info = |recording, record_failed| MenuInfo {
            paused: false,
            approve: ApproveMode::Ask,
            allow_origins: &[],
            recording,
            record_failed,
            hover: None,
        };
        let on = menu(&info(Some(Duration::from_secs(161)), false), 900.0);
        assert!(controls(&on).contains(&Control::StopRecording));
        assert!(!controls(&on).contains(&Control::StartRecording));
        let texts = badge_texts(&on.commands);
        assert!(texts.contains(&"Stop recording".to_string()), "{texts:?}");
        assert!(texts.contains(&"REC 02:41".to_string()), "{texts:?}");
        let failed = menu(&info(None, true), 900.0);
        assert!(controls(&failed).contains(&Control::StartRecording));
        assert!(badge_texts(&failed.commands).contains(&"couldn't start".to_string()));
    }

    #[test]
    fn the_rec_pill_sits_after_the_agent_pill_in_the_reserved_slot() {
        let pair = [
            Pill::Attached(ApproveMode::Ask),
            Pill::Recording(3 * 60 + 5),
        ];
        let width = pills_width(&pair);
        assert_eq!(width, pair[0].width() + PILL_GAP + pair[1].width());
        let commands = pills(&pair, 1000.0);
        let texts = badge_texts(&commands);
        assert_eq!(texts, ["Agent attached · ask", "REC 03:05"]);
        // Both inside the slot the chrome reserved, agent first.
        let slot = accessory_rect(1000.0, width);
        let xs: Vec<f32> = commands
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::Text(t) => Some(t.x),
                _ => None,
            })
            .collect();
        assert!(xs[0] > slot.x && xs[0] < xs[1] && xs[1] < slot.x + slot.width);
        assert_eq!(Pill::Recording(6001).label(), "REC 100:01");
    }

    #[test]
    fn the_recording_card_shows_the_file_steps_and_stop() {
        let card = |file: &str, created, steps| PanelInfo {
            approve: ApproveMode::Ask,
            paused: None,
            allow_origins: &[],
            timeline: &[],
            waiting: None,
            waited: Duration::ZERO,
            page_wait: None,
            attach: None,
            copied: false,
            refusal: None,
            recording: Some(RecordingCard {
                file: file.into(),
                created,
                steps,
            }),
        };
        let (commands, buttons) = panel(&card("runs/r-1.jsonl", true, 6), 900.0, 340.0, 800.0);
        let texts = badge_texts(&commands);
        assert!(texts.contains(&"Recording this session".to_string()));
        // The path keeps its end when it doesn't fit (test fonts are wide).
        assert!(
            texts.iter().any(|t| t.ends_with("1.jsonl · 6 steps")),
            "{texts:?}"
        );
        assert!(texts.contains(&"Stop recording".to_string()));
        assert_eq!(
            buttons.iter().map(|b| b.action).collect::<Vec<_>>(),
            [PanelAction::Control(Control::StopRecording)]
        );
        // Before the first call: the directory, and when the file appears.
        let (commands, _) = panel(&card("mb-runs/", false, 0), 900.0, 340.0, 800.0);
        let texts = badge_texts(&commands).join(" ");
        assert!(texts.contains("mb-runs/ · 0 steps"), "{texts}");
        assert!(texts.contains("The file is created"), "{texts}");
        // A long path keeps its end.
        assert_eq!(clip_start("abcdefgh", 5), "…efgh");
    }

    fn view(blocks: Option<(&str, &str)>, scroll: usize, page: Rect) -> AgentView {
        agent_view(
            &AgentViewInfo {
                scope: ViewScope::OnScreen,
                blocks,
                source: "Exactly the text the agent's last result carried (observe).",
                scroll,
                copied: false,
                hover: None,
            },
            page,
        )
    }

    const BROWSER_BLOCK: &str =
        "# browser (trusted)\nurl: http://127.0.0.1:8765/\nelements: 1 shown\n";
    const PAGE_BLOCK: &str =
        "# page content (untrusted, origin=http://127.0.0.1:8765)\n[e26] button \"Sign up\"\n";

    #[test]
    fn the_viewer_keeps_trusted_and_page_text_apart() {
        let rows = view_rows(BROWSER_BLOCK, PAGE_BLOCK, 80);
        let labels: Vec<&ViewRow> = rows
            .iter()
            .filter(|r| matches!(r, ViewRow::Label(..)))
            .collect();
        assert_eq!(
            labels,
            [
                &ViewRow::Label(
                    "Browser — trusted, written by mini-browser".into(),
                    ViewBlock::Browser
                ),
                &ViewRow::Label(
                    "Page content — untrusted, written by http://127.0.0.1:8765".into(),
                    ViewBlock::Page
                ),
            ]
        );
        // Every line is in its own block, verbatim, in order.
        let lines = |block| -> Vec<String> {
            rows.iter()
                .filter_map(|r| match r {
                    ViewRow::Line(b, line) if *b == block => Some(line.clone()),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(lines(ViewBlock::Browser).join("\n") + "\n", BROWSER_BLOCK);
        assert_eq!(lines(ViewBlock::Page).join("\n") + "\n", PAGE_BLOCK);
        // A line longer than the box wraps at the column count.
        let wrapped = view_rows("# browser (trusted)\n0123456789abcdef\n", PAGE_BLOCK, 8);
        assert!(wrapped.contains(&ViewRow::Line(ViewBlock::Browser, "01234567".into())));
        assert!(wrapped.contains(&ViewRow::Line(ViewBlock::Browser, "89abcdef".into())));

        // Painted: the two boxes in their own colours, buttons for the
        // toggle, Copy and close — all the window's, none the worker's.
        let page = Rect {
            x: 0.0,
            y: 96.0,
            width: 900.0,
            height: 700.0,
        };
        let painted = view(Some((BROWSER_BLOCK, PAGE_BLOCK)), 0, page);
        assert!(within(&page, &painted.rect));
        let fills: Vec<Color> = painted
            .commands
            .iter()
            .filter_map(|c| match c {
                DisplayCommand::SolidRect(color, _) => Some(*color),
                _ => None,
            })
            .collect();
        assert!(fills.contains(&p::AGENT_TINT) && fills.contains(&p::PANEL));
        let actions: Vec<PanelAction> = painted.buttons.iter().map(|b| b.action).collect();
        assert_eq!(
            actions,
            [
                PanelAction::Window(WindowAction::CloseAgentView),
                PanelAction::Window(WindowAction::CopyAgentView),
                PanelAction::Window(WindowAction::Scope(ViewScope::OnScreen)),
                PanelAction::Window(WindowAction::Scope(ViewScope::WholePage)),
            ]
        );
        for b in &painted.buttons {
            let (x, y) = centre(b);
            assert_eq!(
                painted.buttons.iter().filter(|o| o.contains(x, y)).count(),
                1
            );
            assert!(contains(&painted.rect, (x, y)));
        }
        assert_eq!(painted.below, 0);
        let texts = badge_texts(&painted.commands);
        assert!(
            texts.contains(&"[e26] button \"Sign up\"".to_string()),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|t| t.contains("more line")));
        // Nothing received yet: no Copy to press.
        let empty = view(None, 0, page);
        assert!(
            !empty
                .buttons
                .iter()
                .any(|b| b.action == PanelAction::Window(WindowAction::CopyAgentView))
        );
    }

    #[test]
    fn a_long_observation_scrolls_and_counts_what_is_hidden() {
        let page_block: String =
            std::iter::once("# page content (untrusted, origin=http://127.0.0.1:8765)".to_string())
                .chain((0..200).map(|i| format!("[e{i}] link \"Item {i}\"")))
                .collect::<Vec<_>>()
                .join("\n");
        let page = Rect {
            x: 0.0,
            y: 96.0,
            width: 900.0,
            height: 500.0,
        };
        let top = view(Some((BROWSER_BLOCK, &page_block)), 0, page);
        assert!(top.below > 150, "{}", top.below);
        let footer = badge_texts(&top.commands).join(" | ");
        assert!(
            footer.contains(&format!("… {} more lines — scroll", top.below)),
            "{footer}"
        );
        // Scrolled past the end: clamped so the last line shows.
        let end = view(Some((BROWSER_BLOCK, &page_block)), 10_000, page);
        assert_eq!(end.below, 0);
        assert!(end.scroll > 0 && end.scroll < 10_000);
        let texts = badge_texts(&end.commands);
        assert!(texts.contains(&"[e199] link \"Item 199\"".to_string()));
        assert!(
            texts.iter().any(|t| t.ends_with("lines above")),
            "{texts:?}"
        );
        // Nothing drawn below the footer rule.
        for c in &end.commands {
            if let DisplayCommand::Text(t) = c {
                assert!(t.y < end.rect.y + end.rect.height, "{t:?}");
            }
        }
    }
}
