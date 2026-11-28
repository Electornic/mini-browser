// CSS Grid layout — track sizing, named lines, grid-area placement,
// occupancy-based auto-flow.

use crate::{
    css::{GridLine, TrackBreadth, TrackSize, Unit, Value},
    style::StyledNode,
};

use super::inline::{layout_grid_item, layout_inline_block_node, layout_out_of_flow};
use super::{
    LayoutBox, clamp_width, containing_block_height, is_display_none, is_layout_whitespace_text,
    is_out_of_flow, length_value, outer_rect, shift_layout_subtree,
};

pub(super) fn is_grid_container(node: &StyledNode) -> bool {
    matches!(node.value("display"), Some(Value::Keyword(keyword)) if keyword == "grid")
}

#[derive(Debug, Clone, Copy)]
struct GridArea {
    row_start: usize,
    row_end: usize, // exclusive
    col_start: usize,
    col_end: usize, // exclusive
}

/// Grid lines past this are clamped, like Blink's implicit-grid cap. The
/// occupancy map and the track tables allocate one entry per line, so an
/// author value like `grid-row: 2000000000` must not reach them as is.
const MAX_GRID_LINES: usize = 1000;

#[derive(Debug, Clone, Copy)]
struct AxisHint {
    explicit_start: Option<usize>, // 0-indexed cell
    span: usize,                   // always >= 1
}

pub(super) fn layout_grid_children<'a>(
    container: &'a StyledNode,
    children: &'a [StyledNode],
    content_x: f32,
    content_y: f32,
    content_width: f32,
) -> (Vec<LayoutBox>, f32) {
    // Passes:
    //   0: give each in-flow item a GridArea — a `grid-area` name from the
    //      template, explicit `grid-column`/`grid-row`, or row-major
    //      auto-flow around the cells already claimed.
    //   1: measure natural outer widths, only for items starting in a column
    //      whose size depends on its content (`auto`, `min-content`,
    //      `max-content`, `minmax()` with one of those). Spanning items
    //      contribute to their start column only (toy simplification).
    //   2: resolve the column tracks against the width left after the
    //      column gaps (`resolve_tracks`).
    //   3: lay every item out across its spanned columns — stretched to the
    //      span unless it has its own width (grid items stretch); a measured
    //      item that already fits is reused instead of laid out again.
    //   4: rows — natural heights per row, `grid-template-rows`, row gaps;
    //      shift items down and grow them to cover their rows.
    //
    // Out-of-flow children skip the grid entirely — they sit at the container
    // origin and the absolute reposition pass at the tree root moves them to
    // their containing block.
    let column_tracks = effective_column_tracks(container);
    let n_cols = column_tracks.len().max(1);
    let column_gap = gap_value(container, "column-gap", content_width);
    let row_gap = gap_value(container, "row-gap", 0.0);

    let mut boxes: Vec<Option<LayoutBox>> = Vec::with_capacity(children.len());
    // For each in-flow item: its area + boxes index + source styled node.
    let mut cell_assignments: Vec<(GridArea, usize, &'a StyledNode)> = Vec::new();
    let mut occupancy = Occupancy::new(n_cols);
    let mut cursor = (0usize, 0usize);

    for child in children {
        if is_display_none(child) {
            continue;
        }
        // Inter-element whitespace text (preserved by the parser to keep
        // inline runs spaced) must not consume a grid cell. Without this
        // filter, a grid container with HTML like `<div></div>\n<div></div>`
        // sees one extra whitespace item per gap and the named placement
        // / auto-flow / track sizing all drift accordingly.
        if is_layout_whitespace_text(child) {
            continue;
        }
        if is_out_of_flow(child) {
            let abs_box = layout_out_of_flow(child, content_x, content_y, content_width);
            boxes.push(Some(abs_box));
            continue;
        }
        let col_hint = axis_hint_from(child.value("grid-column"));
        let row_hint = axis_hint_from(child.value("grid-row"));
        // grid-area: <name> wins over both auto-flow and grid-column/-row when
        // the container declares a matching template-area rectangle.
        let template_area = grid_area_from_template(container, child);
        let area = if let Some(rect) = template_area {
            occupancy.mark(
                rect.row_start,
                rect.row_end - rect.row_start,
                rect.col_start,
                rect.col_end - rect.col_start,
            );
            rect
        } else {
            place_grid_item(&mut occupancy, &mut cursor, n_cols, col_hint, row_hint)
        };
        cell_assignments.push((area, boxes.len(), child));
        boxes.push(None);
    }

    // Pass 1. Indexing below is in bounds by construction: `box_idx` was
    // pushed with its assignment; every `col_start < n_cols ==
    // columns.len()` (place_grid_item clamps starts to `n_cols - span`,
    // template areas widen the tracks in effective_column_tracks);
    // `row_start < row_end <= n_rows` and resolve_tracks returns one size
    // per requested track.
    let mut natural_max_per_col = vec![0.0_f32; n_cols];
    if !column_tracks.is_empty() {
        for &(area, box_idx, child) in &cell_assignments {
            if !column_tracks
                .get(area.col_start)
                .is_some_and(sizes_to_content)
            {
                continue;
            }
            let measured = layout_inline_block_node(child, content_x, content_y, content_width);
            let w = outer_rect(&measured).width;
            if w > natural_max_per_col[area.col_start] {
                natural_max_per_col[area.col_start] = w;
            }
            boxes[box_idx] = Some(measured);
        }
    }

    // Pass 2. With no declaration a bare `display: grid` is one full-width
    // track.
    let columns = if column_tracks.is_empty() {
        vec![content_width]
    } else {
        let gaps = column_gap * (n_cols - 1) as f32;
        resolve_tracks(
            &column_tracks,
            n_cols,
            &natural_max_per_col,
            Some((content_width - gaps).max(0.0)),
            Some(content_width),
        )
    };
    let col_offsets = track_offsets(&columns, column_gap);

    // Pass 3.
    for &(area, box_idx, child) in &cell_assignments {
        let span_width = span_size(&columns, column_gap, area.col_start, area.col_end);
        let target_outer_x = content_x + col_offsets[area.col_start];
        let measured = boxes[box_idx]
            .take()
            .filter(|measured| outer_rect(measured).width <= span_width + 0.5);
        let item = match measured {
            Some(mut item) => {
                let dx = target_outer_x - outer_rect(&item).x;
                if dx != 0.0 {
                    shift_layout_subtree(&mut item, dx, 0.0);
                }
                if !matches!(child.value("width"), Some(Value::Length(_, _))) {
                    let edges = outer_rect(&item).width - item.dimensions.content.width;
                    let target = clamp_width(child, (span_width - edges).max(0.0), span_width);
                    if item.dimensions.content.width < target {
                        item.dimensions.content.width = target;
                    }
                }
                item
            }
            None => layout_grid_item(child, target_outer_x, content_y, span_width),
        };
        boxes[box_idx] = Some(item);
    }

    // Pass 4: natural row heights = max(item outer height) per row, with
    // spanned items contributing to row_start only.
    let n_rows = cell_assignments
        .iter()
        .map(|&(area, _, _)| area.row_end)
        .max()
        .unwrap_or(0);
    let mut natural_row_heights = vec![0.0_f32; n_rows];
    for &(area, box_idx, _) in &cell_assignments {
        let h = boxes[box_idx]
            .as_ref()
            .map_or(0.0, |b| outer_rect(b).height);
        if h > natural_row_heights[area.row_start] {
            natural_row_heights[area.row_start] = h;
        }
    }

    let row_template = match container.value("grid-template-rows") {
        Some(Value::TrackList(tracks)) => tracks.as_slice(),
        _ => &[],
    };
    // The container's own definite height (installed for its child layout,
    // step 1p) — `None` when auto.
    let container_explicit_height = containing_block_height();
    let row_gaps = row_gap * n_rows.saturating_sub(1) as f32;
    let row_heights = resolve_tracks(
        row_template,
        n_rows,
        &natural_row_heights,
        container_explicit_height.map(|h| (h - row_gaps).max(0.0)),
        container_explicit_height,
    );
    let row_offsets = track_offsets(&row_heights, row_gap);
    for &(area, box_idx, child) in &cell_assignments {
        let Some(item) = boxes[box_idx].as_mut() else {
            continue;
        };
        let dy = row_offsets[area.row_start];
        if dy != 0.0 {
            shift_layout_subtree(item, 0.0, dy);
        }
        // Fill content to span the row range when no explicit height. This
        // covers single-row and multi-row spans uniformly: an item without
        // its own height takes the row's resolved height (or the sum across
        // a multi-row span). Items with explicit height keep their declared
        // size — this matches the column-track post-hoc fill on the main
        // axis.
        if !matches!(child.value("height"), Some(Value::Length(_, _))) {
            let span_height = span_size(&row_heights, row_gap, area.row_start, area.row_end);
            let edges = outer_rect(item).height - item.dimensions.content.height;
            let target = (span_height - edges).max(0.0);
            if item.dimensions.content.height < target {
                item.dimensions.content.height = target;
            }
        }
    }

    let auto_content_height: f32 = row_heights.iter().sum::<f32>() + row_gaps;
    (boxes.into_iter().flatten().collect(), auto_content_height)
}

/// `column-gap` / `row-gap`, falling back to the `gap` value (one length);
/// a percentage resolves against `base`.
fn gap_value(container: &StyledNode, name: &str, base: f32) -> f32 {
    length_value(container, name, base)
        .or_else(|| length_value(container, "gap", base))
        .unwrap_or(0.0)
        .max(0.0)
}

/// Start offset of each track, gaps included.
fn track_offsets(sizes: &[f32], gap: f32) -> Vec<f32> {
    let mut offsets = Vec::with_capacity(sizes.len());
    let mut acc = 0.0;
    for size in sizes {
        offsets.push(acc);
        acc += size + gap;
    }
    offsets
}

/// Size of tracks `start..end` plus the gaps between them.
fn span_size(sizes: &[f32], gap: f32, start: usize, end: usize) -> f32 {
    let end = end.min(sizes.len());
    let tracks = sizes.get(start..end).unwrap_or(&[]);
    tracks.iter().sum::<f32>() + gap * tracks.len().saturating_sub(1) as f32
}

/// `grid-template-columns`, widened to the column count
/// `grid-template-areas` implies. Areas may name more columns than the track
/// list declares (or the track list may use a value we can't parse — Vector
/// on Wikipedia does both); every named cell needs a track or placement
/// indexes past the table. Spec-wise the extra columns are implicit `auto`
/// tracks; with no track list at all we split the width evenly instead,
/// since this toy doesn't stretch `auto` tracks into free space.
fn effective_column_tracks(container: &StyledNode) -> Vec<TrackSize> {
    let mut tracks = match container.value("grid-template-columns") {
        Some(Value::TrackList(tracks)) => tracks.clone(),
        _ => Vec::new(),
    };
    let area_columns = match container.value("grid-template-areas") {
        Some(Value::TemplateAreas(rows)) => rows.iter().map(Vec::len).max().unwrap_or(0),
        _ => 0,
    };
    let filler = if tracks.is_empty() {
        TrackSize::Fraction(1.0)
    } else {
        TrackSize::Auto
    };
    while tracks.len() < area_columns {
        tracks.push(filler);
    }
    tracks
}

fn grid_area_from_template(container: &StyledNode, item: &StyledNode) -> Option<GridArea> {
    let area_name = match item.value("grid-area") {
        Some(Value::Keyword(name)) => name.as_str(),
        _ => return None,
    };
    let rows = match container.value("grid-template-areas") {
        Some(Value::TemplateAreas(rows)) => rows,
        _ => return None,
    };
    // Sweep the map and build a bounding rectangle for cells matching the
    // area name. CSS spec actually requires the area to be rectangular and
    // contiguous; toy is lenient — we just take the bbox even if the named
    // cells are non-contiguous.
    let mut min_row: Option<usize> = None;
    let mut max_row: usize = 0;
    let mut min_col: Option<usize> = None;
    let mut max_col: usize = 0;
    for (r, row) in rows.iter().enumerate() {
        for (c, cell) in row.iter().enumerate() {
            if cell.as_deref() == Some(area_name) {
                min_row = Some(min_row.map_or(r, |m| m.min(r)));
                if r > max_row {
                    max_row = r;
                }
                min_col = Some(min_col.map_or(c, |m| m.min(c)));
                if c > max_col {
                    max_col = c;
                }
            }
        }
    }
    Some(GridArea {
        row_start: min_row?,
        row_end: max_row + 1,
        col_start: min_col?,
        col_end: max_col + 1,
    })
}

fn axis_hint_from(value: Option<&Value>) -> AxisHint {
    let placement = match value {
        Some(Value::GridPlacement(p)) => *p,
        _ => {
            return AxisHint {
                explicit_start: None,
                span: 1,
            };
        }
    };
    // Lines are 1-based (the parser rejects <= 0). Clamp the start so at
    // least one cell fits, then the span so start + span stays in bounds.
    let start = |s: u32| (s.max(1) as usize - 1).min(MAX_GRID_LINES - 1);
    let span = |from: usize, n: usize| n.clamp(1, MAX_GRID_LINES - from);
    let hint = match (placement.start, placement.end) {
        (GridLine::Index(s), GridLine::Index(e)) if e > s => AxisHint {
            explicit_start: Some(start(s)),
            span: (e - s) as usize,
        },
        (GridLine::Index(s), GridLine::Span(n)) => AxisHint {
            explicit_start: Some(start(s)),
            span: n as usize,
        },
        (GridLine::Index(s), GridLine::Auto) => AxisHint {
            explicit_start: Some(start(s)),
            span: 1,
        },
        // `span <n>` as the start side: no explicit anchor, but the item
        // claims n cells when auto-placed.
        (GridLine::Span(n), _) => AxisHint {
            explicit_start: None,
            span: n as usize,
        },
        // Anything else (e.g. `auto / 3` — end-only) falls back to a
        // single-cell auto placement. Toy doesn't try to honor end-only
        // line constraints because they need a back-search through the
        // already-placed items.
        _ => AxisHint {
            explicit_start: None,
            span: 1,
        },
    };
    AxisHint {
        span: span(hint.explicit_start.unwrap_or(0), hint.span),
        ..hint
    }
}

#[derive(Debug)]
struct Occupancy {
    cells: Vec<Vec<bool>>,
    n_cols: usize,
}

impl Occupancy {
    fn new(n_cols: usize) -> Self {
        Self {
            cells: Vec::new(),
            n_cols,
        }
    }

    fn ensure_rows(&mut self, target_rows: usize) {
        while self.cells.len() < target_rows {
            self.cells.push(vec![false; self.n_cols]);
        }
    }

    fn is_free(&self, row: usize, row_span: usize, col: usize, col_span: usize) -> bool {
        for r in row..row + row_span {
            for c in col..col + col_span {
                if r < self.cells.len() && c < self.n_cols && self.cells[r][c] {
                    return false;
                }
            }
        }
        true
    }

    fn mark(&mut self, row: usize, row_span: usize, col: usize, col_span: usize) {
        self.ensure_rows(row + row_span);
        for r in row..row + row_span {
            for c in col..col + col_span {
                if c < self.n_cols {
                    self.cells[r][c] = true;
                }
            }
        }
    }
}

fn place_grid_item(
    occupancy: &mut Occupancy,
    cursor: &mut (usize, usize),
    n_cols: usize,
    col_hint: AxisHint,
    row_hint: AxisHint,
) -> GridArea {
    // Clamp spans to the declared track count: a cell range overflowing the
    // explicit grid would create implicit tracks per spec, but the toy
    // ignores those.
    let col_span = col_hint.span.clamp(1, n_cols.max(1));
    let row_span = row_hint.span.max(1);

    let (row_s, col_s) = match (row_hint.explicit_start, col_hint.explicit_start) {
        // Both axes anchored — drop the item exactly at (rs, cs), clamped.
        (Some(rs), Some(cs)) => {
            let cs = cs.min(n_cols.saturating_sub(col_span));
            (rs, cs)
        }
        // Column anchored — scan rows for one where the item's spanned
        // columns are all free for `row_span` rows.
        (None, Some(cs)) => {
            let cs = cs.min(n_cols.saturating_sub(col_span));
            let mut row = 0usize;
            while !occupancy.is_free(row, row_span, cs, col_span) {
                row += 1;
            }
            (row, cs)
        }
        // Row anchored — scan columns at that row for the first free run.
        (Some(rs), None) => {
            let mut col = 0usize;
            while col + col_span <= n_cols && !occupancy.is_free(rs, row_span, col, col_span) {
                col += 1;
            }
            // Items wider than n_cols overflow at col 0 (toy fallback).
            let col = col.min(n_cols.saturating_sub(col_span));
            (rs, col)
        }
        // No anchors — walk the auto-flow cursor row-major, wrapping when
        // we'd exceed n_cols and skipping cells already occupied by the
        // explicit-placed items above.
        (None, None) => {
            let (mut row, mut col) = *cursor;
            loop {
                if col + col_span > n_cols {
                    row += 1;
                    col = 0;
                    continue;
                }
                if occupancy.is_free(row, row_span, col, col_span) {
                    break;
                }
                col += 1;
            }
            *cursor = (row, col + col_span);
            if cursor.1 >= n_cols {
                cursor.0 += 1;
                cursor.1 = 0;
            }
            (row, col)
        }
    };

    occupancy.mark(row_s, row_span, col_s, col_span);
    GridArea {
        row_start: row_s,
        row_end: row_s + row_span,
        col_start: col_s,
        col_end: col_s + col_span,
    }
}

/// Whether a track's size depends on the natural width of its items (the
/// pre-pass measures only items starting in such a column).
fn sizes_to_content(track: &TrackSize) -> bool {
    match track {
        TrackSize::Auto | TrackSize::MinContent | TrackSize::MaxContent => true,
        TrackSize::MinMax(min, max) => {
            matches!(min, TrackBreadth::MinContent | TrackBreadth::MaxContent)
                || matches!(
                    max,
                    TrackBreadth::Auto | TrackBreadth::MinContent | TrackBreadth::MaxContent
                )
        }
        TrackSize::Length(_, _) | TrackSize::Fraction(_) => false,
    }
}

/// How one track takes space.
#[derive(Debug, Clone, Copy)]
enum Plan {
    Fixed(f32),
    /// `minmax(<min>, <length>)`: starts at `floor`, grows to `cap` from
    /// free space before any `fr` track gets some.
    Capped {
        floor: f32,
        cap: f32,
    },
    /// `fr` (and `minmax(<min>, <n>fr)`): a share of what is left.
    Flex {
        weight: f32,
        floor: f32,
    },
}

/// A breadth in px: a percentage needs a definite `base` (`None` = behaves
/// as `auto`); the content keywords are the measured `natural` size.
fn breadth_px(breadth: TrackBreadth, natural: f32, base: Option<f32>) -> Option<f32> {
    match breadth {
        TrackBreadth::Length(v, Unit::Percent) => base.map(|b| v / 100.0 * b),
        // em/rem/ch/pt are resolved to px at style time.
        TrackBreadth::Length(v, _) => Some(v),
        TrackBreadth::MinContent | TrackBreadth::MaxContent => Some(natural),
        TrackBreadth::Auto | TrackBreadth::Fraction(_) => None,
    }
}

fn plan(track: TrackSize, natural: f32, base: Option<f32>) -> Plan {
    let as_breadth = |track| match track {
        TrackSize::Length(v, unit) => TrackBreadth::Length(v, unit),
        TrackSize::Fraction(w) => TrackBreadth::Fraction(w),
        TrackSize::MinContent => TrackBreadth::MinContent,
        TrackSize::MaxContent => TrackBreadth::MaxContent,
        TrackSize::Auto | TrackSize::MinMax(..) => TrackBreadth::Auto,
    };
    let (min, max) = match track {
        TrackSize::MinMax(min, max) => (min, max),
        // A plain `fr` keeps the toy's 0 floor (spec: `minmax(auto, 1fr)`).
        TrackSize::Fraction(w) => (
            TrackBreadth::Length(0.0, Unit::Px),
            TrackBreadth::Fraction(w),
        ),
        other => (as_breadth(other), as_breadth(other)),
    };
    // An `auto` minimum is 0 here — the toy has no separate min-content
    // measurement (the natural size is max-content-like).
    let floor = breadth_px(min, natural, base).unwrap_or(0.0).max(0.0);
    match max {
        TrackBreadth::Fraction(weight) => Plan::Flex {
            weight: weight.max(0.0),
            floor,
        },
        TrackBreadth::Length(..) => match breadth_px(max, natural, base) {
            // A max below the min is the min (css-grid-1 §7.2.3).
            Some(cap) if matches!(track, TrackSize::MinMax(..)) => Plan::Capped {
                floor,
                cap: cap.max(floor),
            },
            Some(size) => Plan::Fixed(size.max(0.0)),
            // A percentage without a definite base behaves as `auto`.
            None => Plan::Fixed(natural.max(floor)),
        },
        TrackBreadth::Auto | TrackBreadth::MinContent | TrackBreadth::MaxContent => {
            Plan::Fixed(natural.max(floor))
        }
    }
}

/// Resolve `count` tracks (template first, `auto` past its end) to sizes.
/// `available` is the space for the tracks (gaps already taken out), `None`
/// when indefinite (auto-height rows): flexible and capped tracks then size
/// to their content, as Chrome does for `fr` rows in an auto-height grid.
/// `base` is what percentages resolve against.
fn resolve_tracks(
    template: &[TrackSize],
    count: usize,
    natural: &[f32],
    available: Option<f32>,
    base: Option<f32>,
) -> Vec<f32> {
    let plans: Vec<Plan> = (0..count)
        .map(|i| {
            let track = template.get(i).copied().unwrap_or(TrackSize::Auto);
            plan(track, natural.get(i).copied().unwrap_or(0.0), base)
        })
        .collect();
    let Some(available) = available else {
        return plans
            .iter()
            .zip(natural.iter().chain(std::iter::repeat(&0.0)))
            .map(|(plan, &natural)| match *plan {
                Plan::Fixed(size) => size,
                Plan::Capped { floor, cap } => natural.clamp(floor, cap),
                Plan::Flex { floor, .. } => natural.max(floor),
            })
            .collect();
    };
    let mut sizes: Vec<f32> = plans
        .iter()
        .map(|plan| match *plan {
            Plan::Fixed(size) => size,
            Plan::Capped { floor, .. } | Plan::Flex { floor, .. } => floor,
        })
        .collect();
    let mut free = available - sizes.iter().sum::<f32>();
    // Capped tracks grow first, evenly, each up to its cap. Every round
    // either uses up the free space or caps at least one track, so this
    // runs at most `count` times.
    for _ in 0..plans.len() {
        let growable: Vec<usize> = (0..plans.len())
            .filter(|&i| matches!(plans[i], Plan::Capped { cap, .. } if sizes[i] < cap))
            .collect();
        if free <= 0.0 || growable.is_empty() {
            break;
        }
        let share = free / growable.len() as f32;
        for i in growable {
            if let Plan::Capped { cap, .. } = plans[i] {
                let add = share.min(cap - sizes[i]);
                sizes[i] += add;
                free -= add;
            }
        }
    }
    // Then `fr` tracks split what is left, by css-grid-1 "find the size of
    // an fr": a track whose floor beats its share keeps its floor and
    // leaves the split, which is redone without it (`minmax(300px,1fr)
    // 1fr` in 400px is 300 / 100, not 300 / 200). A weight sum below 1
    // counts as 1, so `0.5fr` alone takes half. Each round fixes at least
    // one track, so this runs at most `count` times.
    let mut flexible: Vec<usize> = (0..plans.len())
        .filter(|&i| matches!(plans[i], Plan::Flex { .. }))
        .collect();
    if !flexible.is_empty() {
        let mut space = free.max(0.0) + flexible.iter().map(|&i| sizes[i]).sum::<f32>();
        loop {
            let weights: f32 = flexible
                .iter()
                .map(|&i| match plans[i] {
                    Plan::Flex { weight, .. } => weight,
                    _ => 0.0,
                })
                .sum();
            let fr = space / weights.max(1.0);
            let (stuck, rest): (Vec<usize>, Vec<usize>) =
                flexible.iter().partition(|&&i| match plans[i] {
                    Plan::Flex { weight, floor } => floor > fr * weight,
                    _ => false,
                });
            if stuck.is_empty() {
                for &i in &flexible {
                    if let Plan::Flex { weight, .. } = plans[i] {
                        sizes[i] = fr * weight;
                    }
                }
                break;
            }
            // `sizes` already holds the floors of the stuck tracks.
            space -= stuck.iter().map(|&i| sizes[i]).sum::<f32>();
            flexible = rest;
            if flexible.is_empty() {
                break;
            }
        }
    }
    sizes
}

#[cfg(test)]
mod tests {
    use super::{AxisHint, MAX_GRID_LINES, axis_hint_from};
    use crate::css::{GridLine, GridPlacement, Value};

    fn hint(start: GridLine, end: GridLine) -> AxisHint {
        axis_hint_from(Some(&Value::GridPlacement(GridPlacement { start, end })))
    }

    #[test]
    fn huge_grid_lines_and_spans_are_clamped() {
        // `grid-row: 2000000000` / `span 1000000000` made the occupancy map
        // and the row table allocate one entry per line — gigabytes, then
        // an abort. Browsers cap the implicit grid (Blink: 1000 lines).
        let far = hint(GridLine::Index(2_000_000_000), GridLine::Auto);
        assert!(far.explicit_start.unwrap() < MAX_GRID_LINES);
        let wide = hint(GridLine::Span(1_000_000_000), GridLine::Auto);
        assert!(wide.span <= MAX_GRID_LINES);
        let both = hint(GridLine::Index(1), GridLine::Index(u32::MAX));
        assert!(both.span <= MAX_GRID_LINES);
        let anchored = hint(GridLine::Index(u32::MAX), GridLine::Span(u32::MAX));
        assert!(anchored.explicit_start.unwrap() + anchored.span <= MAX_GRID_LINES);
    }
}
